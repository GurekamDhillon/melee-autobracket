//! The judging worker: finds games whose uploads are in, judges them (re-simulating
//! when the pair agrees), and applies the verdict. Also flags stalled sets, on a timer of
//! its own.
//!
//! Up to `verifier.jobs` games are judged at once, each in its own task. Claiming a game
//! (`uploading` => `judging`) is one short write transaction, and is what keeps two tasks off
//! the same game; the re-simulation runs with no transaction held; applying the verdict is one
//! more transaction ([`lifecycle::apply_verdict`]), a no-op unless the game is still
//! `judging`. A judging attempt that fails (an internal error, or a panic) hands the game
//! back; after [`MAX_JUDGE_ATTEMPTS`] the game goes to the organiser as `judging-failed`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Context;
use proto::{ResimOutcome, Ruleset, Verdict};
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tracing::{error, info, warn};

use crate::config::{Config, VerifierMode};
use crate::db::{begin_write, now_unix};
use crate::judge::{self, Pre, UploadMeta};
use crate::lifecycle::{self, verdict_name, LifecycleError};
use crate::logging::{clean, detail, label, resim_kind, Journal, MAX_DETAIL_CHARS};
use crate::resim::Resim;
use crate::review::{self, STALLED};
use crate::state::AppState;

/// How often the worker looks for due games when nothing wakes it.
const TICK: Duration = Duration::from_secs(1);
/// Longest time between two looks for stalled sets ([`flag_stalled_sets`]); shorter when
/// `set_stall_secs` is.
const STALL_CHECK_PERIOD: Duration = Duration::from_secs(30);
/// Failed attempts after which a game is no longer retried but sent to the organiser.
pub const MAX_JUDGE_ATTEMPTS: i64 = 5;
/// Queue reason for a game that could not be judged ([`MAX_JUDGE_ATTEMPTS`] failed attempts).
pub const JUDGING_FAILED: &str = "judging-failed";
/// Pause before a game whose judging failed is tried again, per failed attempt.
const RETRY_BACKOFF: Duration = Duration::from_secs(2);

/// A spawned task that is aborted when this is dropped, so aborting the task that owns it
/// stops it too.
struct Owned<T>(JoinHandle<T>);

impl<T> Drop for Owned<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs `fut` in a task of its own, so a panic in it is an `Err` here instead of ending the
/// caller; aborted with the caller.
async fn contained<T: Send + 'static>(
    fut: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<T, JoinError> {
    let mut task = Owned(tokio::spawn(fut));
    (&mut task.0).await
}

/// The worker: judges due games forever, and looks for stalled sets on a timer task of its
/// own, so a long judging never delays it. Survives panics in either.
pub async fn run(st: AppState) {
    let _stall_checks = Owned(tokio::spawn(stall_checks(st.clone())));
    loop {
        let ended = contained(judge_forever(st.clone())).await;
        // `judge_forever` only ends by panicking. Its judging tasks were aborted with it:
        // hand their games back.
        if let Err(e) = ended {
            error!(error = %detail(&e), "judging worker failed; restarting it");
        }
        if let Err(e) = recover(&st).await {
            error!(error = %detail(format_args!("{e:#}")), "returning judging games failed");
        }
        tokio::time::sleep(TICK).await;
    }
}

/// How often [`stall_checks`] looks: every [`STALL_CHECK_PERIOD`], or every
/// `set_stall_secs` when that is shorter (at least once a second).
fn stall_check_period(cfg: &Config) -> Duration {
    Duration::from_secs(cfg.set_stall_secs.max(1)).min(STALL_CHECK_PERIOD)
}

/// Flags stalled sets at once and then every [`stall_check_period`], each look in a task of
/// its own. Does nothing when `set_stall_secs` is 0.
async fn stall_checks(st: AppState) {
    if st.cfg.set_stall_secs == 0 {
        return;
    }
    let mut tick = tokio::time::interval(stall_check_period(&st.cfg));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let look = st.clone();
        match contained(async move { flag_stalled_sets(&look).await }).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => error!(
                error = %detail(format_args!("{e:#}")),
                "looking for stalled sets failed"
            ),
            Err(e) => error!(error = %detail(&e), "looking for stalled sets failed"),
        }
    }
}

/// Judges due games forever: on every wake-up, whenever a judging task ends, and at least
/// once per [`TICK`].
async fn judge_forever(st: AppState) {
    let mut pool = Judging::new(&st);
    loop {
        if let Err(e) = pool.start_due(&st).await {
            error!(error = %detail(format_args!("{e:#}")), "finding due games failed");
        }
        let done = tokio::select! {
            done = pool.next_done(), if !pool.is_empty() => done,
            () = st.wake.notified() => None,
            () = tokio::time::sleep(TICK) => None,
        };
        if let Some(done) = done {
            // A failure is logged and the game handed back by `finish`.
            let _ = pool.finish(&st, done).await;
        }
    }
}

/// Judging tasks in flight, at most `verifier.jobs` of them.
struct Judging {
    tasks: JoinSet<anyhow::Result<()>>,
    /// The game each task judges.
    games: HashMap<tokio::task::Id, i64>,
    limit: usize,
    /// Games whose last attempt failed, and when they may be tried again.
    retry_after: HashMap<i64, Instant>,
}

impl Judging {
    fn new(st: &AppState) -> Self {
        Judging {
            tasks: JoinSet::new(),
            games: HashMap::new(),
            limit: st.cfg.verifier.jobs.max(1),
            retry_after: HashMap::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    fn is_full(&self) -> bool {
        self.tasks.len() >= self.limit
    }

    /// Starts judging due games while there is room, skipping games waiting out a retry pause.
    async fn start_due(&mut self, st: &AppState) -> anyhow::Result<()> {
        if self.is_full() {
            return Ok(());
        }
        let now = Instant::now();
        self.retry_after.retain(|_, at| *at > now);
        for game_id in due_games(st).await? {
            if self.is_full() {
                break;
            }
            if !self.retry_after.contains_key(&game_id) {
                self.start(st, game_id).await?;
            }
        }
        Ok(())
    }

    /// Claims `game_id` and judges it in a task of its own. Does nothing if another task (or
    /// process) claimed it first.
    async fn start(&mut self, st: &AppState, game_id: i64) -> anyhow::Result<()> {
        if !claim(st, game_id)
            .await
            .with_context(|| format!("claiming game {game_id}"))?
        {
            return Ok(());
        }
        let task_st = st.clone();
        let handle = self.tasks.spawn(async move {
            let claim = load_claim(&task_st, game_id).await?;
            judge_claimed(&task_st, game_id, claim).await
        });
        self.games.insert(handle.id(), game_id);
        Ok(())
    }

    /// The next judging task to end; `None` when there is none.
    async fn next_done(
        &mut self,
    ) -> Option<Result<(tokio::task::Id, anyhow::Result<()>), JoinError>> {
        self.tasks.join_next_with_id().await
    }

    /// Accounts for an ended task: `Ok` with its game when the game was judged. A failed
    /// attempt (an error or a panic) is logged and counted, and the game handed back (or, after
    /// [`MAX_JUDGE_ATTEMPTS`], sent to the organiser); its error is returned.
    async fn finish(
        &mut self,
        st: &AppState,
        done: Result<(tokio::task::Id, anyhow::Result<()>), JoinError>,
    ) -> anyhow::Result<i64> {
        let (id, failure) = match done {
            Ok((id, Ok(()))) => (id, None),
            Ok((id, Err(e))) => (id, Some(e)),
            Err(e) => {
                let id = e.id();
                let why = if e.is_panic() {
                    anyhow::anyhow!(
                        "the judging task panicked: {}",
                        panic_message(e.into_panic())
                    )
                } else {
                    anyhow::anyhow!("the judging task was cancelled")
                };
                (id, Some(why))
            }
        };
        let game_id = self
            .games
            .remove(&id)
            .context("a judging task for no known game")?;
        let Some(e) = failure else {
            return Ok(game_id);
        };
        let e = e.context(format!("judging game {game_id}"));
        match failed_attempt(st, game_id, &format!("{e:#}")).await {
            Ok(Some(attempts)) if attempts < MAX_JUDGE_ATTEMPTS => {
                error!(
                    game_id,
                    attempts,
                    error = %detail(format_args!("{e:#}")),
                    "judging failed; the game will be judged again"
                );
                let pause = RETRY_BACKOFF * u32::try_from(attempts).unwrap_or(1);
                self.retry_after.insert(game_id, Instant::now() + pause);
            }
            Ok(Some(attempts)) => error!(
                game_id,
                attempts,
                error = %detail(format_args!("{e:#}")),
                "judging failed too often; the game goes to the organiser"
            ),
            Ok(None) => {}
            Err(he) => error!(
                game_id,
                error = %detail(format_args!("{he:#}")),
                "returning a game to uploading failed"
            ),
        }
        Err(e)
    }
}

/// The text of a panic payload.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "(no message)".to_owned()
    }
}

/// Records a failed judging attempt of a game in `judging`, in one transaction: counts it and
/// hands the game back to `uploading`, or, at [`MAX_JUDGE_ATTEMPTS`], gives up on it. Returns
/// the attempts so far, `None` if the game was no longer `judging`.
///
/// Giving up: the game goes to `review` with a `judging-failed` queue item carrying `error`
/// (cleaned) when its set is in play or already waiting for the organiser (a `ready` set goes
/// to `review`); for a set already decided the game is voided.
async fn failed_attempt(
    st: &AppState,
    game_id: i64,
    error: &str,
) -> Result<Option<i64>, LifecycleError> {
    let mut journal = Journal::default();
    let mut tx = begin_write(&st.db).await?;
    let row: Option<(i64, i64, i64)> = sqlx::query_as(
        "UPDATE games SET state = 'uploading', judge_attempts = judge_attempts + 1
         WHERE id = ? AND state = 'judging' RETURNING judge_attempts, event_id, set_id",
    )
    .bind(game_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((attempts, event_id, set_id)) = row else {
        return Ok(None);
    };
    if attempts >= MAX_JUDGE_ATTEMPTS {
        let set_state: Option<String> =
            sqlx::query_scalar("SELECT state FROM sets WHERE event_id = ? AND set_id = ?")
                .bind(event_id)
                .bind(set_id)
                .fetch_optional(&mut *tx)
                .await?;
        let open = matches!(set_state.as_deref(), Some("ready" | "review"));
        sqlx::query("UPDATE games SET state = ?, judged_at = ? WHERE id = ?")
            .bind(if open { "review" } else { "void" })
            .bind(now_unix())
            .bind(game_id)
            .execute(&mut *tx)
            .await?;
        if open {
            let set = u32::try_from(set_id)
                .map_err(|_| LifecycleError::Corrupt(format!("game {game_id} set_id {set_id}")))?;
            sqlx::query(
                "UPDATE sets SET state = 'review', updated_at = ? WHERE event_id = ? AND set_id = ?",
            )
            .bind(now_unix())
            .bind(event_id)
            .bind(set_id)
            .execute(&mut *tx)
            .await?;
            let detail = clean(error, MAX_DETAIL_CHARS);
            review::open_queue_item(
                &mut tx,
                event_id,
                set,
                Some(game_id),
                JUDGING_FAILED,
                Some(&detail),
                &mut journal,
            )
            .await?;
        }
    }
    tx.commit().await?;
    journal.emit();
    Ok(Some(attempts))
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

/// The games that are due for judging, oldest first: `uploading` with both uploads in, or
/// with one upload whose `upload_timeout_secs` has passed.
async fn due_games(st: &AppState) -> anyhow::Result<Vec<i64>> {
    let timeout = i64::try_from(st.cfg.upload_timeout_secs).unwrap_or(i64::MAX);
    sqlx::query_scalar(
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
    .context("finding due games")
}

/// Judges every game that is due now ([`due_games`]), up to `verifier.jobs` at once, and
/// waits for all of them. Returns how many games were judged, or the first failure (the
/// failed games are handed back, see [`MAX_JUDGE_ATTEMPTS`]). The running worker does the
/// same continuously; this one pass is for tests and tools.
pub async fn judge_due(st: &AppState) -> anyhow::Result<usize> {
    let mut pool = Judging::new(st);
    let mut judged = 0;
    let mut first_err = None;
    let mut account = |r: anyhow::Result<i64>| match r {
        Ok(_) => judged += 1,
        Err(e) => {
            first_err.get_or_insert(e);
        }
    };
    for game_id in due_games(st).await? {
        while pool.is_full() {
            if let Some(done) = pool.next_done().await {
                account(pool.finish(st, done).await);
            }
        }
        if let Err(e) = pool.start(st, game_id).await {
            account(Err(e));
        }
    }
    while let Some(done) = pool.next_done().await {
        account(pool.finish(st, done).await);
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
    /// What else is known about each slot's upload (the tag even without one).
    meta: [UploadMeta; 2],
}

/// Moves the game from `uploading` to `judging`: `false` if another task (or process) got
/// there first. Only the task that claimed a game judges it.
async fn claim(st: &AppState, game_id: i64) -> anyhow::Result<bool> {
    let claimed =
        sqlx::query("UPDATE games SET state = 'judging' WHERE id = ? AND state = 'uploading'")
            .bind(game_id)
            .execute(&st.db)
            .await?;
    Ok(claimed.rows_affected() == 1)
}

/// Reads what judging a claimed game needs, in one read transaction.
async fn load_claim(st: &AppState, game_id: i64) -> anyhow::Result<Claim> {
    let mut tx = st.db.begin().await?;
    let (event_id, set_id, game_no, ruleset_json): (i64, i64, i64, String) = sqlx::query_as(
        "SELECT g.event_id, g.set_id, g.game_no, e.ruleset_json
         FROM games g JOIN events e ON e.id = g.event_id WHERE g.id = ?",
    )
    .bind(game_id)
    .fetch_one(&mut *tx)
    .await?;
    let ruleset = serde_json::from_str(&ruleset_json).context("event ruleset_json")?;
    let uploads: Vec<(i64, String, String, String)> =
        sqlx::query_as("SELECT slot, path, sha256, client_build FROM uploads WHERE game_id = ?")
            .bind(game_id)
            .fetch_all(&mut *tx)
            .await?;
    let mut paths = [None, None];
    let mut meta: [UploadMeta; 2] = Default::default();
    for (slot, path, sha256, client_build) in uploads {
        let slot = usize::try_from(slot)
            .ok()
            .filter(|s| *s < 2)
            .with_context(|| format!("upload slot {slot}"))?;
        paths[slot] = Some(PathBuf::from(path));
        meta[slot].sha256 = sha256;
        meta[slot].client_build = client_build;
    }
    // For the evidence only: judging goes on without the tags.
    let tags = match lifecycle::load_bracket_on(&mut tx, event_id).await {
        Ok(bracket) => match usize::try_from(set_id)
            .ok()
            .and_then(|i| bracket.sets.get(i))
        {
            Some(set) => lifecycle::slot_tags(&mut tx, event_id, set.slots)
                .await
                .unwrap_or(None),
            None => None,
        },
        Err(_) => None,
    };
    if let Some(tags) = tags {
        for (m, tag) in meta.iter_mut().zip(tags) {
            m.tag = Some(tag);
        }
    }
    tx.commit().await?;
    Ok(Claim {
        event_id,
        set_id,
        game_no,
        ruleset,
        paths,
        meta,
    })
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
    let ruleset = claim.ruleset.clone();
    let meta = claim.meta.clone();
    let (pre, uploads, lone) = tokio::task::spawn_blocking(move || {
        let [a, b] = &files;
        let files = [a.as_deref(), b.as_deref()];
        let pre = judge::pre_judge(files, &ruleset);
        let mut uploads = Vec::new();
        let mut lone = None;
        for (slot, file) in (0u8..).zip(files) {
            if claim_has_upload(&meta[usize::from(slot)]) {
                uploads.push(judge::upload_evidence(
                    slot,
                    &meta[usize::from(slot)],
                    file,
                    &ruleset,
                ));
            }
            if pre == Pre::Final(Verdict::Single) {
                if let Some(frame) = file.and_then(|f| judge::evidence_resim_frame(f, &ruleset)) {
                    lone = Some((slot, frame));
                }
            }
        }
        (pre, uploads, lone)
    })
    .await
    .context("pre-judging")?;

    let mut resim_ms = 0;
    let mut resim_evidence = serde_json::Value::Null;
    let (verdict, winner_slot, resim, verifier_build) = match pre {
        // Decided before any re-simulation, so no verifier build checked it for counting. A
        // lone upload that could be checked is re-simulated as evidence for the organiser.
        Pre::Final(v) => {
            if let Some((slot, frame)) = lone {
                resim_evidence = evidence_resim(st, &claim, game_id, slot, frame).await;
            }
            (v, None, ResimOutcome::Skipped, None)
        }
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
            let v: Verdict = judge::finish(
                &outcome,
                st.cfg.strict_resim,
                verifier_on,
                claim.ruleset.any_client,
            );
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
            let (kind, text) = judge::resim_evidence(&outcome);
            resim_evidence = serde_json::json!({
                "outcome": kind,
                "detail": text,
                "build": build.as_deref().map(label),
                "slot": 0,
                "evidence_only": false,
            });
            (v, Some(judge::winner_slot(winner_port)), outcome, build)
        }
    };
    let evidence = serde_json::json!({
        "game_no": claim.game_no,
        "verdict": verdict_display(&verdict),
        "uploads": uploads,
        "resim": resim_evidence,
    });
    lifecycle::apply_verdict(
        st,
        game_id,
        &verdict,
        winner_slot,
        &resim,
        verifier_build.as_deref(),
        Some(&evidence.to_string()),
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

/// Whether the claim found an upload for this slot (every stored upload has a hash).
fn claim_has_upload(meta: &UploadMeta) -> bool {
    !meta.sha256.is_empty()
}

/// Re-simulates the lone upload in `slot` as evidence only: the outcome is shown to the
/// organiser and never counts. A failure to check is recorded in the evidence instead of
/// retrying the game, since nothing depends on it.
async fn evidence_resim(
    st: &AppState,
    claim: &Claim,
    game_id: i64,
    slot: u8,
    last_frame: i32,
) -> serde_json::Value {
    let Some(path) = claim.paths[usize::from(slot)].as_deref() else {
        return serde_json::Value::Null;
    };
    let (kind, text, build) = match st.resim.check_with_build(path, last_frame).await {
        Ok((outcome, build)) => {
            let build = engine_ran(&outcome).then_some(build);
            let (kind, text) = judge::resim_evidence(&outcome);
            (kind, text, build)
        }
        Err(e) => {
            warn!(game_id, error = %detail(format_args!("{e:#}")), "evidence re-simulation failed");
            ("Error", clean(&format!("{e:#}"), MAX_DETAIL_CHARS), None)
        }
    };
    serde_json::json!({
        "outcome": kind,
        "detail": text,
        "build": build.as_deref().map(label),
        "slot": slot,
        "evidence_only": true,
    })
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
