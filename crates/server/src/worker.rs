//! The judging worker: finds games whose uploads are in, judges them (re-simulating
//! when the pair agrees), and applies the verdict.
//!
//! Claiming a game (`uploading` => `judging`) is one short write transaction; the
//! re-simulation runs with no transaction held; applying the verdict is one more
//! transaction ([`lifecycle::apply_verdict`]), a no-op unless the game is still `judging`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use proto::{ResimOutcome, Ruleset, Verdict};

use crate::db::{begin_write, now_unix};
use crate::judge::{self, Pre};
use crate::lifecycle;
use crate::state::AppState;

/// How often the worker looks for due games when nothing wakes it.
const TICK: Duration = Duration::from_secs(1);

/// Judges due games forever: on every wake-up and at least once per [`TICK`].
pub async fn run(st: AppState) {
    loop {
        if let Err(e) = judge_due(&st).await {
            eprintln!("worker: judging failed: {e:#}");
        }
        tokio::select! {
            () = st.wake.notified() => {}
            () = tokio::time::sleep(TICK) => {}
        }
    }
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
                    eprintln!("worker: game {game_id}: returning it to uploading failed: {he}");
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

/// What judging one game needs, read when it was claimed.
struct Claim {
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
    let ruleset_json: String = sqlx::query_scalar(
        "SELECT e.ruleset_json FROM games g JOIN events e ON e.id = g.event_id WHERE g.id = ?",
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
    Ok(Some(Claim { ruleset, paths }))
}

/// Judges a claimed game and applies the verdict. Holds no transaction while re-simulating.
async fn judge_claimed(st: &AppState, game_id: i64, claim: Claim) -> anyhow::Result<()> {
    let mut files: [Option<Vec<u8>>; 2] = [None, None];
    for (file, path) in files.iter_mut().zip(&claim.paths) {
        let Some(path) = path else { continue };
        match tokio::fs::read(path).await {
            Ok(bytes) => *file = Some(bytes),
            // A lost file is a missing upload: the game goes to the organiser, never counts.
            Err(e) => eprintln!("worker: game {game_id}: reading {}: {e}", path.display()),
        }
    }
    let ruleset = claim.ruleset;
    let pre = tokio::task::spawn_blocking(move || {
        let [a, b] = &files;
        judge::pre_judge([a.as_deref(), b.as_deref()], &ruleset)
    })
    .await
    .context("pre-judging")?;

    let (verdict, winner_slot, resim) = match pre {
        Pre::Final(v) => (v, None, ResimOutcome::Skipped),
        Pre::NeedsResim {
            winner_port,
            last_frame,
        } => {
            let host = claim.paths[0]
                .as_deref()
                .context("agreeing pair without a host upload")?;
            let outcome = st.resim.check(host, last_frame).await;
            if let ResimOutcome::Unavailable(detail) = &outcome {
                eprintln!(
                    "worker: game {game_id}: verifier unavailable, judging peer-only: {detail}"
                );
            }
            let v: Verdict = judge::finish(&outcome, st.cfg.strict_resim);
            (v, Some(judge::winner_slot(winner_port)), outcome)
        }
    };
    lifecycle::apply_verdict(st, game_id, &verdict, winner_slot, &resim).await?;
    Ok(())
}
