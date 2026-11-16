//! Re-simulation of uploaded replays against the game engine.
//!
//! [`Slpcheck`] runs the engine's `slpcheck` command as a subprocess and reads its verdict from
//! the CSV the engine writes; the CSV, not the exit code or the log lines, is the contract.

use std::collections::HashMap;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use proto::{ResimOutcome, VerifyResult};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::task::Id as TaskId;

#[async_trait]
pub trait Resim: Send + Sync {
    /// Re-simulates the replay at `slp` and compares it with the recording.
    async fn check(&self, slp: &Path, replay_last_frame: i32) -> ResimOutcome;
    /// Identifies the engine build doing the checking.
    fn build(&self) -> String;

    /// Hands the verifier the opened store; `build_app` calls it once. Only verifiers that
    /// keep state in the store ([`ExternalResim`]) need it.
    fn attach(&self, _db: &SqlitePool) {}

    /// Tells the verifier an external result was just recorded (by the result route).
    fn result_posted(&self) {}
}

/// A verifier that checks nothing.
pub struct NoResim;

#[async_trait]
impl Resim for NoResim {
    async fn check(&self, _slp: &Path, _replay_last_frame: i32) -> ResimOutcome {
        ResimOutcome::Skipped
    }

    fn build(&self) -> String {
        "none".into()
    }
}

/// How many frames short of the replay's last frame the engine may stop and still count as having
/// run to the end. The engine's `last_frame` equals the replay reader's for a complete run.
pub const EARLY_END_SLACK_FRAMES: i32 = 0;

/// Environment variable the engine needs to spawn characters the way the vanilla game does.
const SPAWN_ENV: &str = "SLPCHECK_VANILLA_SPAWN";
/// Bytes of the engine's stderr kept for error messages.
const STDERR_TAIL_BYTES: usize = 2048;

/// Re-simulates replays by running `{engine} slpcheck` against a disc image.
pub struct Slpcheck {
    engine: PathBuf,
    iso: PathBuf,
    timeout: Duration,
    build: String,
}

impl Slpcheck {
    /// Checks that the engine runs and records its build: the first line of `{engine} version`,
    /// `+`, and the first 12 hex digits of the binary's SHA-256.
    pub async fn new(engine: PathBuf, iso: PathBuf, timeout: Duration) -> anyhow::Result<Self> {
        let version = Command::new(&engine)
            .arg("version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let version = tokio::time::timeout(timeout, version)
            .await
            .with_context(|| format!("`{} version` timed out", engine.display()))?
            .with_context(|| format!("running `{} version`", engine.display()))?;
        if !version.status.success() {
            bail!("`{} version` failed: {}", engine.display(), version.status);
        }
        let stdout = String::from_utf8_lossy(&version.stdout);
        let line = stdout.lines().next().unwrap_or("").trim();
        if line.is_empty() {
            bail!("`{} version` printed nothing", engine.display());
        }

        let path = engine.clone();
        let digest = tokio::task::spawn_blocking(move || -> io::Result<Vec<u8>> {
            let mut file = std::fs::File::open(&path)?;
            let mut hasher = Sha256::new();
            io::copy(&mut file, &mut hasher)?;
            Ok(hasher.finalize().to_vec())
        })
        .await
        .context("hashing the engine binary")?
        .with_context(|| format!("hashing {}", engine.display()))?;
        let build = format!("{line}+{}", hex::encode(&digest[..6]));

        Ok(Self {
            engine,
            iso,
            timeout,
            build,
        })
    }
}

#[async_trait]
impl Resim for Slpcheck {
    async fn check(&self, slp: &Path, replay_last_frame: i32) -> ResimOutcome {
        // `--csv` appends and only writes a header to an empty file: every run gets a fresh one.
        let dir = match ScratchDir::create() {
            Ok(dir) => dir,
            Err(e) => return ResimOutcome::Unavailable(format!("cannot create temp dir: {e}")),
        };
        let csv_path = dir.path().join("slpcheck.csv");

        let mut child = match Command::new(&self.engine)
            .arg("slpcheck")
            .arg("--slp")
            .arg(slp)
            .arg("--iso")
            .arg(&self.iso)
            .arg("--csv")
            .arg(&csv_path)
            .env(SPAWN_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return ResimOutcome::Unavailable(format!("cannot start engine: {e}")),
        };

        let stderr = child.stderr.take();
        let finished = tokio::time::timeout(self.timeout, async {
            tokio::join!(child.wait(), read_tail(stderr, STDERR_TAIL_BYTES))
        })
        .await;
        let (status, stderr_tail) = match finished {
            Ok((Ok(status), tail)) => (status, tail),
            Ok((Err(e), _)) => {
                let _ = child.kill().await;
                return ResimOutcome::Crashed(format!("waiting for engine: {e}"));
            }
            Err(_) => {
                let _ = child.kill().await;
                return ResimOutcome::Crashed("timeout".into());
            }
        };

        if status.code() == Some(2) {
            return ResimOutcome::Unavailable(stderr_tail);
        }
        let csv = match tokio::fs::read(&csv_path).await {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::new(),
        };
        if last_row(&csv).is_none() {
            return ResimOutcome::Crashed(format!("{status}; no result row"));
        }
        outcome_from_csv(&csv, replay_last_frame)
    }

    fn build(&self) -> String {
        self.build.clone()
    }
}

/// How long [`ExternalResim::check`] waits for a verifier by default.
pub const EXTERNAL_RESULT_TIMEOUT: Duration = Duration::from_secs(600);
/// How often a waiting check re-reads its job, in case a wake-up was missed.
const EXTERNAL_RECHECK: Duration = Duration::from_secs(1);

/// Re-simulates on another machine: `check` queues a `resim_jobs` row, a `verify-worker`
/// claims it over the API and posts the result, and `check` returns that result.
///
/// `build()` is `"external"`, except that right after a `check` that got a result it returns
/// the build the verifier posted, once, to the same task. The judging path calls `check` and
/// then `build` from one task, so concurrent judging in other tasks cannot mix the builds up.
pub struct ExternalResim {
    db: OnceLock<SqlitePool>,
    posted: Notify,
    timeout: Duration,
    /// The build of the last result `check` returned, per calling task.
    builds: Mutex<HashMap<TaskId, String>>,
}

impl ExternalResim {
    /// `timeout` is how long `check` waits for a result before giving up
    /// (normally [`EXTERNAL_RESULT_TIMEOUT`]).
    pub fn new(timeout: Duration) -> Self {
        ExternalResim {
            db: OnceLock::new(),
            posted: Notify::new(),
            timeout,
            builds: Mutex::default(),
        }
    }

    fn builds(&self) -> std::sync::MutexGuard<'_, HashMap<TaskId, String>> {
        self.builds.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues a job and waits for its result; `None` when the wait timed out.
    async fn run_job(
        &self,
        db: &SqlitePool,
        slp: &Path,
        replay_last_frame: i32,
    ) -> anyhow::Result<Option<VerifyResult>> {
        let path = slp
            .to_str()
            .with_context(|| format!("non-UTF-8 replay path {slp:?}"))?;
        let game_id: Option<i64> = sqlx::query_scalar("SELECT game_id FROM uploads WHERE path = ?")
            .bind(path)
            .fetch_optional(db)
            .await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO resim_jobs (game_id, path, replay_last_frame, state)
             VALUES (?, ?, ?, 'queued') RETURNING id",
        )
        .bind(game_id.unwrap_or(0))
        .bind(path)
        .bind(replay_last_frame)
        .fetch_one(db)
        .await?;

        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            // Register for the wake-up before reading, so a result posted in between still wakes us.
            let posted = self.posted.notified();
            tokio::pin!(posted);
            posted.as_mut().enable();
            if let Some(result) = finished(db, id).await? {
                return Ok(Some(result));
            }
            tokio::select! {
                () = posted => {}
                () = tokio::time::sleep(EXTERNAL_RECHECK) => {}
                () = tokio::time::sleep_until(deadline) => break,
            }
        }

        // Give up, unless the result landed just now. Retiring the job stops it being offered.
        let mut tx = crate::db::begin_write(db).await?;
        let retired =
            sqlx::query("UPDATE resim_jobs SET state = 'done' WHERE id = ? AND state != 'done'")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        let result = if retired.rows_affected() == 0 {
            finished(&mut *tx, id).await?
        } else {
            None
        };
        tx.commit().await?;
        Ok(result)
    }
}

/// The posted result of job `id`, once it is `done`.
async fn finished<'e, E>(db: E, id: i64) -> anyhow::Result<Option<VerifyResult>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT state, result_json FROM resim_jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?;
    match row {
        None => bail!("resim job {id} disappeared"),
        Some((state, Some(json))) if state == "done" => Ok(Some(
            serde_json::from_str(&json).with_context(|| format!("resim job {id} result_json"))?,
        )),
        Some(_) => Ok(None),
    }
}

#[async_trait]
impl Resim for ExternalResim {
    async fn check(&self, slp: &Path, replay_last_frame: i32) -> ResimOutcome {
        let task = tokio::task::try_id();
        if let Some(task) = task {
            self.builds().remove(&task);
        }
        let Some(db) = self.db.get() else {
            return ResimOutcome::Unavailable("external verifier not attached to a store".into());
        };
        match self.run_job(db, slp, replay_last_frame).await {
            Ok(Some(VerifyResult { outcome, build })) => {
                if let Some(task) = task {
                    self.builds().insert(task, build);
                }
                outcome
            }
            Ok(None) => ResimOutcome::Unavailable("no verifier".into()),
            Err(e) => ResimOutcome::Unavailable(format!("external verifier: {e:#}")),
        }
    }

    fn build(&self) -> String {
        tokio::task::try_id()
            .and_then(|task| self.builds().remove(&task))
            .unwrap_or_else(|| "external".into())
    }

    fn attach(&self, db: &SqlitePool) {
        let _ = self.db.set(db.clone());
    }

    fn result_posted(&self) {
        self.posted.notify_waiters();
    }
}

/// Reads the verdict from the last data row of an `slpcheck` CSV, locating columns by header name.
pub fn outcome_from_csv(csv: &str, replay_last_frame: i32) -> ResimOutcome {
    let Some((header, row)) = last_row(csv) else {
        return ResimOutcome::Crashed("no result row".into());
    };
    let col = |name: &str| -> &str {
        header
            .iter()
            .position(|h| h.trim() == name)
            .and_then(|i| row.get(i))
            .map_or("", |v| v.trim())
    };

    let status = col("status");
    let word = status
        .split(|c: char| c.is_whitespace() || c == ':')
        .next()
        .unwrap_or("");
    match word {
        "match" => match col("last_frame").parse::<i32>() {
            Ok(engine_last)
                if engine_last.saturating_add(EARLY_END_SLACK_FRAMES) < replay_last_frame =>
            {
                ResimOutcome::EarlyEnd {
                    engine_last,
                    replay_last: replay_last_frame,
                }
            }
            Ok(_) => ResimOutcome::Match,
            Err(_) => ResimOutcome::Crashed(format!(
                "match with unreadable last_frame {:?}",
                col("last_frame")
            )),
        },
        "diverged" => ResimOutcome::Diverged {
            frame: col("first_frame").parse().unwrap_or(-1),
            port: col("first_port").parse().unwrap_or(0),
            field: col("first_field").to_string(),
            engine_value: col("engine_value").to_string(),
            replay_value: col("replay_value").to_string(),
        },
        _ if status.is_empty() => ResimOutcome::Crashed("empty status".into()),
        _ => ResimOutcome::Crashed(status.to_string()),
    }
}

/// The header and the last non-blank record after it, if there is one.
fn last_row(csv: &str) -> Option<(Vec<String>, Vec<String>)> {
    let mut records = parse_csv(csv).into_iter();
    let header = records.next()?;
    let row = records.last()?;
    Some((header, row))
}

/// Splits CSV text into records of fields. Double-quoted fields may contain commas, newlines and
/// `""` escapes. Blank lines are skipped.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    fn finish(record: &mut Vec<String>, field: &mut String, records: &mut Vec<Vec<String>>) {
        record.push(std::mem::take(field));
        let rec = std::mem::take(record);
        if !(rec.len() == 1 && rec[0].is_empty()) {
            records.push(rec);
        }
    }

    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
        } else {
            match c {
                '"' => quoted = true,
                ',' => record.push(std::mem::take(&mut field)),
                '\n' => finish(&mut record, &mut field, &mut records),
                '\r' => {}
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !record.is_empty() {
        finish(&mut record, &mut field, &mut records);
    }
    records
}

/// Reads `reader` to the end, keeping only the last `cap` bytes, as trimmed lossy UTF-8.
async fn read_tail<R: AsyncRead + Unpin>(reader: Option<R>, cap: usize) -> String {
    let Some(mut reader) = reader else {
        return String::new();
    };
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > 2 * cap {
                    kept.drain(..kept.len() - cap);
                }
            }
        }
    }
    if kept.len() > cap {
        kept.drain(..kept.len() - cap);
    }
    String::from_utf8_lossy(&kept).trim().to_string()
}

/// A private directory under the system temp dir, removed on drop.
pub(crate) struct ScratchDir(PathBuf);

impl ScratchDir {
    pub(crate) fn create() -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "autobracket-slpcheck-{}-{n}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
