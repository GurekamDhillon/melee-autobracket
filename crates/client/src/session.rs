//! What `play` keeps on disk so that a restart never starts a second game: an exclusive
//! lock on the work directory (`play.lock`), and `session.json`, the game the client last
//! started and which of its replays have been dealt with.
//!
//! A recorded game is only adopted if it is provably the same process: the pid, the
//! process's start time (from `/proc/PID/stat`), the boot it started in, and, when adopting,
//! its working directory (each game runs in its own run directory). A recycled pid never
//! matches all of these.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

/// Holds the work directory for one `play` process; released when dropped or when the
/// process ends, however it ends.
pub struct WorkLock {
    _file: File,
}

/// Takes `work_dir/play.lock`, or fails if another `play` holds it.
pub fn lock(work_dir: &Path) -> anyhow::Result<WorkLock> {
    std::fs::create_dir_all(work_dir)
        .with_context(|| format!("creating {}", work_dir.display()))?;
    let path = work_dir.join("play.lock");
    // std opens files close-on-exec, so a game started later never inherits the lock.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: `flock` on a descriptor this function owns; no memory is involved.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            let mut holder = String::new();
            let _ = file.read_to_string(&mut holder);
            let holder = holder
                .trim()
                .parse::<u32>()
                .map_or_else(|_| String::new(), |pid| format!(" (pid {pid})"));
            bail!(
                "another `autobracket-client play`{holder} is already running with the work \
                 directory {}. Use that one, or stop it first (its game, if any, keeps running \
                 and is picked up by the next `play`)",
                work_dir.display()
            );
        }
        return Err(err).with_context(|| format!("locking {}", path.display()));
    }
    // Only for the message above; the lock itself is what counts.
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = writeln!(file, "{}", std::process::id());
    Ok(WorkLock { _file: file })
}

/// Fails unless `stat` (`/proc/self/stat` for `play`) can be read. Without `/proc` a game
/// is never recorded, so a restarted `play` would start a second one.
pub fn require_proc(stat: &Path) -> anyhow::Result<()> {
    std::fs::read_to_string(stat).with_context(|| {
        format!(
            "`play` needs Linux: it reads /proc to recognise a game still running when it is \
             restarted, and {} cannot be read",
            stat.display()
        )
    })?;
    Ok(())
}

/// A process identified beyond its pid.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProcId {
    pub pid: i32,
    /// Its process group (the game leads its own).
    pub pgid: i32,
    /// Field 22 of `/proc/PID/stat`: clock ticks after boot when it started.
    pub start_time: u64,
    /// `/proc/sys/kernel/random/boot_id` when it was recorded.
    pub boot_id: String,
}

/// `(state, pgid, start time)` from `/proc/PID/stat`.
fn stat(pid: i32) -> Option<(char, i32, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may contain anything, so read after the last `)`.
    let (_, rest) = text.rsplit_once(')')?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let pgid = fields.get(2)?.parse().ok()?;
    let start = fields.get(19)?.parse().ok()?;
    Some((state, pgid, start))
}

fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

impl ProcId {
    /// The process `pid` as it is now, if it exists.
    pub fn of(pid: u32) -> Option<ProcId> {
        let pid = i32::try_from(pid).ok()?;
        let (_, pgid, start_time) = stat(pid)?;
        Some(ProcId {
            pid,
            pgid,
            start_time,
            boot_id: boot_id(),
        })
    }

    /// Whether this exact process is still running: same pid, start time and boot, and not
    /// a zombie.
    pub fn is_running(&self) -> bool {
        if self.boot_id != boot_id() {
            return false;
        }
        stat(self.pid).is_some_and(|(state, _, start)| {
            start == self.start_time && !matches!(state, 'Z' | 'X' | 'x')
        })
    }

    /// Its working directory, as the kernel resolved it (no symlinks, no `..`).
    pub fn cwd(&self) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", self.pid)).ok()
    }

    /// [`ProcId::is_running`], and its working directory is `dir` (compared resolved, so a
    /// `dir` reached through a symlink or `..` still matches).
    pub fn is_running_in(&self, dir: &Path) -> bool {
        self.is_running()
            && self.cwd().is_some_and(|cwd| {
                cwd == dir || std::fs::canonicalize(dir).is_ok_and(|dir| cwd == dir)
            })
    }
}

/// The game the client last started, as written to `session.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub event: i64,
    pub set_id: u32,
    pub game_no: u8,
    pub host: bool,
    pub run_dir: PathBuf,
    pub process: ProcId,
    /// The host's room code reached the server.
    #[serde(default)]
    pub code_posted: bool,
    /// Replays of this run already uploaded (or refused by the server).
    #[serde(default)]
    pub handled: Vec<PathBuf>,
}

/// What [`SessionFile::load`] found.
#[derive(Debug)]
pub enum Loaded {
    None,
    Record(Record),
    /// A file that could not be read as a record; the reason.
    Corrupt(String),
}

/// `work_dir/session.json`.
#[derive(Clone, Debug)]
pub struct SessionFile {
    path: PathBuf,
}

impl SessionFile {
    pub fn new(work_dir: &Path) -> Self {
        SessionFile {
            path: work_dir.join("session.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Loaded {
        match std::fs::read(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded::None,
            Err(e) => Loaded::Corrupt(e.to_string()),
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(r) => Loaded::Record(r),
                Err(e) => Loaded::Corrupt(e.to_string()),
            },
        }
    }

    /// Writes `record` atomically (beside the file, then renamed over it).
    pub fn save(&self, record: &Record) -> anyhow::Result<()> {
        let json = serde_json::to_vec_pretty(record).context("serialising the session")?;
        let tmp = self.path.with_extension("json.tmp");
        let written = (|| {
            let mut f = File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written.with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The `.slp` files directly in `dir`, oldest first (`Game_<UTC timestamp>.slp`).
pub fn replays_in(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut replays = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "slp") && path.is_file() {
            replays.push(path);
        }
    }
    replays.sort();
    Ok(replays)
}
