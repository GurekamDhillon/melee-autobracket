//! The judging worker: finds games whose uploads are in, judges them (re-simulating
//! when the pair agrees), and applies the verdict.
//!
//! Claiming a game (`uploading` => `judging`) is one short write transaction; the
//! re-simulation runs with no transaction held; applying the verdict is one more
//! transaction ([`lifecycle::apply_verdict`]), a no-op unless the game is still `judging`.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Context;
use proto::{ResimOutcome, Ruleset, Verdict};
use tracing::{error, info, warn};

use crate::config::{Config, VerifierMode};
use crate::db::{begin_write, now_unix};
use crate::judge::{self, Pre};
use crate::lifecycle::{self, verdict_name};
use crate::logging::{detail, label, resim_kind, Journal};
use crate::resim::Resim;
use crate::review::{self, STALLED};
use crate::state::AppState;

/// How often the worker looks for due games when nothing wakes it.
const TICK: Duration = Duration::from_secs(1);
/// Least time between two looks for stalled sets ([`flag_stalled_sets`]).
const STALL_CHECK_PERIOD: Duration = Duration::from_secs(30);

/// Judges due games forever: on every wake-up and at least once per [`TICK`]. Looks for
/// stalled sets on the first tick and then at most once per [`STALL_CHECK_PERIOD`].
pub async fn run(st: AppState) {
    let mut last_stall_check: Option<Instant> = None;
    loop {
        if let Err(e) = judge_due(&st).await {
            error!(error = %detail(format_args!("{e:#}")), "judging failed");
        }
        if last_stall_check.is_none_or(|t| t.elapsed() >= STALL_CHECK_PERIOD) {
            last_stall_check = Some(Instant::now());
            if let Err(e) = flag_stalled_sets(&st).await {
                error!(
                    error = %detail(format_args!("{e:#}")),
                    "looking for stalled sets failed"
                );
            }
        }
        tokio::select! {
            () = st.wake.notified() => {}
            () = tokio::time::sleep(TICK) => {}
        }
    }
}

/// Refuses an in-process verifier whose engine build is not on a non-empty
/// `accepted_builds` (see [`Config::build_accepted`]): it would re-simulate the players'
/// games on another build than theirs.
pub fn check_verifier_build(cfg: &Config, resim: &dyn Resim) -> anyhow::Result<()> {
    if cfg.verifier.mode != VerifierMode::InProcess {
        return Ok(());
    }
    let build = resim.build();
    if !cfg.build_accepted(&build) {
        anyhow::bail!(
            "the verifier's engine build {:?} is not in accepted_builds {:?}",
            label(&build),
            cfg.accepted_builds
        );
    }
    Ok(())
}

/// Games left in `judging` by an interrupted run go back to `uploading`, to be judged again.
pub async fn recover(st: &AppState) -> anyhow::Result<()> {
    sqlx::query("UPDATE games SET state = 'uploading' WHERE state = 'judging'")
        .execute(&st.db)
        .await
        .context("returning judging games to uploading")?;
    Ok(())
}

/// Judges every game that is due: `uploading` with both uploads in, or with one upload
/// whose `upload_timeout_secs` has passed. Returns how many games were judged.
pub async fn judge_due(st: &AppState) -> anyhow::Result<usize> {
    let timeout = i64::try_from(st.cfg.upload_timeout_secs).unwrap_or(i64::MAX);
    let due: Vec<i64> = sqlx::query_scalar(
        "SELECT g.id FROM games g
         WHERE g.state = 'uploading'
           AND ((SELECT COUNT(*) FROM uploads u WHERE u.game_id = g.id) >= 2
                OR (EXISTS (SELECT 1 FROM uploads u WHERE u.game_id = g.id)
                    AND g.first_upload_at IS NOT NULL
                    AND g.first_upload_at <= ? - ?))
         ORDER BY g.id",
    )
    .bind(now_unix())
    .bind(timeout)
    .fetch_all(&st.db)
    .await
    .context("finding due games")?;

    let mut judged = 0;
    let mut first_err = None;
    for game_id in due {
        let claim = match claim(st, game_id).await {
            Ok(Some(claim)) => claim,
            Ok(None) => continue,
            Err(e) => {
                // Nothing was committed, so the game is still `uploading`; carry on.
                first_err.get_or_insert(e.context(format!("claiming game {game_id}")));
                continue;
            }
        };
        match judge_claimed(st, game_id, claim).await {
            Ok(()) => judged += 1,
            Err(e) => {
                // Hand the game back so a later pass (or a restart) retries it, and carry on
                // with the others.
                let handed_back = sqlx::query(
                    "UPDATE games SET state = 'uploading' WHERE id = ? AND state = 'judging'",
                )
                .bind(game_id)
                .execute(&st.db)
                .await;
                if let Err(he) = handed_back {
                    error!(game_id, error = %detail(&he), "returning a game to uploading failed");
                }
                first_err.get_or_insert(e.context(format!("judging game {game_id}")));
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(judged),
    }
}

/// Sends every stalled set to the organiser queue (reason `stalled`), as a no-show report
/// would: a `ready` set of a `running` event with no activity for `set_stall_secs` (since
/// `sets.updated_at`, or since this process started if that is later, see
/// [`AppState::started_at`]) and no game uploading or awaiting a verdict. Does nothing when
/// `set_stall_secs` is 0. Returns how many sets were flagged.
pub async fn flag_stalled_sets(st: &AppState) -> anyhow::Result<usize> {
    let stall_secs = st.cfg.set_stall_secs;
    if stall_secs == 0 {
        return Ok(0);
    }
    let now = now_unix();
    let cutoff = now.saturating_sub(i64::try_from(stall_secs).unwrap_or(i64::MAX));
    let mut journal = Journal::default();
    let mut flagged = Vec::new();
    let mut tx = begin_write(&st.db).await?;
    // Inactivity counts from the last change or from this process's start, whichever is
    // later: time the server was down is not inactivity it observed.
    let idle: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT s.event_id, s.set_id, MAX(s.updated_at, ?) AS since
         FROM sets s JOIN events e ON e.id = s.event_id
         WHERE s.state = 'ready' AND e.state = 'running' AND MAX(s.updated_at, ?) < ?
         ORDER BY s.event_id, s.set_id",
    )
    .bind(st.started_at)
    .bind(st.started_at)
    .bind(cutoff)
    .fetch_all(&mut *tx)
    .await
    .context("finding idle sets")?;
    for (event_id, set_id, since) in idle {
        if review::game_in_progress(&mut tx, event_id, set_id).await? {
            continue;
        }
        let set = u32::try_from(set_id).with_context(|| format!("sets row set_id {set_id}"))?;
        let idle_secs = now - since;
        let note = format!("no activity for {idle_secs} s");
        review::send_to_review(&mut tx, event_id, set, STALLED, Some(&note), &mut journal).await?;
        flagged.push((event_id, set, idle_secs));
    }
    tx.commit().await?;
    for &(event_id, set, idle_secs) in &flagged {
        info!(event = event_id, set, idle_secs, "stalled set flagged");
    }
    journal.emit();
    Ok(flagged.len())
}

/// What judging one game needs, read when it was claimed.
struct Claim {
    event_id: i64,
    set_id: i64,
    game_no: i64,
    ruleset: Ruleset,
    /// Stored replay path per slot (0 is the host).
    paths: [Option<PathBuf>; 2],
}

/// Moves the game from `uploading` to `judging`, reading its uploads. `None` if another
/// pass got there first.
async fn claim(st: &AppState, game_id: i64) -> anyhow::Result<Option<Claim>> {
    let mut tx = begin_write(&st.db).await?;
    let claimed =
        sqlx::query("UPDATE games SET state = 'judging' WHERE id = ? AND state = 'uploading'")
            .bind(game_id)
            .execute(&mut *tx)
            .await?;
    if claimed.rows_affected() == 0 {
        return Ok(None);
    }
    let (event_id, set_id, game_no, ruleset_json): (i64, i64, i64, String) = sqlx::query_as(
        "SELECT g.event_id, g.set_id, g.game_no, e.ruleset_json
         FROM games g JOIN events e ON e.id = g.event_id WHERE g.id = ?",
    )
    .bind(game_id)
    .fetch_one(&mut *tx)
    .await?;
    let ruleset = serde_json::from_str(&ruleset_json).context("event ruleset_json")?;
    let uploads: Vec<(i64, String)> =
        sqlx::query_as("SELECT slot, path FROM uploads WHERE game_id = ?")
            .bind(game_id)
            .fetch_all(&mut *tx)
            .await?;
    // Validated before committing: on failure the claim rolls back and the game stays
    // `uploading`.
    let mut paths = [None, None];
    for (slot, path) in uploads {
        let slot = usize::try_from(slot)
            .ok()
            .filter(|s| *s < 2)
            .with_context(|| format!("upload slot {slot}"))?;
        paths[slot] = Some(PathBuf::from(path));
    }
    tx.commit().await?;
    Ok(Some(Claim {
        event_id,
        set_id,
        game_no,
        ruleset,
        paths,
    }))
}

/// Judges a claimed game and applies the verdict. Holds no transaction while re-simulating.
async fn judge_claimed(st: &AppState, game_id: i64, claim: Claim) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut files: [Option<Vec<u8>>; 2] = [None, None];
    for (file, path) in files.iter_mut().zip(&claim.paths) {
        let Some(path) = path else { continue };
        match tokio::fs::read(path).await {
            Ok(bytes) => *file = Some(bytes),
            // A lost file is a missing upload: the game goes to the organiser, never counts.
            Err(e) => warn!(
                game_id,
                path = %detail(path.display()),
                error = %detail(&e),
                "stored replay unreadable; judged as missing"
            ),
        }
    }
    let ruleset = claim.ruleset;
    let pre = tokio::task::spawn_blocking(move || {
        let [a, b] = &files;
        judge::pre_judge([a.as_deref(), b.as_deref()], &ruleset)
    })
    .await
    .context("pre-judging")?;

    let mut resim_ms = 0;
    let (verdict, winner_slot, resim, verifier_build) = match pre {
        // Decided before any re-simulation (`Single` included: the lone upload is not run
        // through the engine), so no verifier build checked it.
        Pre::Final(v) => (v, None, ResimOutcome::Skipped, None),
        Pre::NeedsResim {
            winner_port,
            last_frame,
        } => {
            let host = claim.paths[0]
                .as_deref()
                .context("agreeing pair without a host upload")?;
            let resim_started = Instant::now();
            let (mut outcome, build) = st
                .resim
                .check_with_build(host, last_frame)
                .await
                .context("re-simulating")?;
            resim_ms = resim_started.elapsed().as_millis();
            // A verifier on another engine build than the accepted ones checked nothing the
            // players ran (only an external one can be: an in-process one refuses to start).
            if engine_ran(&outcome) && !st.cfg.build_accepted(&build) {
                outcome = ResimOutcome::Unavailable(format!(
                    "verifier build not accepted: {}",
                    label(&build)
                ));
            }
            let verifier_on = st.cfg.verifier.mode != VerifierMode::Off;
            let v: Verdict = judge::finish(&outcome, st.cfg.strict_resim, verifier_on);
            if let ResimOutcome::Unavailable(why) = &outcome {
                warn!(
                    game_id,
                    event = claim.event_id,
                    set = claim.set_id,
                    verdict = %verdict_display(&v),
                    detail = %detail(why),
                    "verifier unavailable"
                );
                if verifier_on
                    && v == Verdict::PeerOnly
                    && !st.peer_only_alarm.swap(true, Ordering::Relaxed)
                {
                    error!(
                        game_id,
                        detail = %detail(why),
                        "verifier unavailable: games are counting peer-only, without \
                         re-simulation (set strict_resim to send them to the organiser instead)"
                    );
                }
            }
            // A build is recorded only when the engine actually ran on the game.
            let build = engine_ran(&outcome).then_some(build);
            (v, Some(judge::winner_slot(winner_port)), outcome, build)
        }
    };
    lifecycle::apply_verdict(
        st,
        game_id,
        &verdict,
        winner_slot,
        &resim,
        verifier_build.as_deref(),
    )
    .await?;
    info!(
        event = claim.event_id,
        set = claim.set_id,
        game = claim.game_no,
        game_id,
        verdict = %verdict_display(&verdict),
        resim = %resim_kind(&resim),
        verifier_build = %verifier_build.as_deref().map_or_else(|| "-".to_owned(), label),
        judging_ms = started.elapsed().as_millis(),
        resim_ms,
        "verdict"
    );
    Ok(())
}

/// Whether a re-simulation outcome means the engine ran on the replay: `Skipped` (verifier
/// off) and `Unavailable` (no verifier, or it could not start the game) mean it did not.
fn engine_ran(outcome: &ResimOutcome) -> bool {
    match outcome {
        ResimOutcome::Match
        | ResimOutcome::Diverged { .. }
        | ResimOutcome::EarlyEnd { .. }
        | ResimOutcome::Crashed(_) => true,
        ResimOutcome::Unavailable(_) | ResimOutcome::Skipped => false,
    }
}

/// A verdict as logged: its name, with the failed rule of an `Illegal` one.
fn verdict_display(v: &Verdict) -> String {
    match v {
        Verdict::Illegal(rule) => format!("Illegal({rule})"),
        other => verdict_name(other).to_owned(),
    }
}
