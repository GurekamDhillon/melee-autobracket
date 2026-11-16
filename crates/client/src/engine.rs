//! Running the game. [`Engine`] is what the play loop needs; [`CommandEngine`] runs the
//! real `melee` binary.
//!
//! How a run ended is read from what the engine printed (checked against the engine at
//! commit 0cd085d, branch port/candidate12, `rust/melee/src/play.rs` and `play_net.rs`):
//! - A host nobody joined within `--np-timeout` (default 60 s) prints
//!   `play: netplay host: no connection after N s (...)` and exits 1. Builds with
//!   `--np-join-wait` (engine branch port/np-nameentry) print `play: netplay: no opponent
//!   joined` and exit 5. Either line means nobody joined; the exit code alone never does.
//! - A guest whose code names no room prints `play: netplay guest: no room with that code`;
//!   one whose host closed the room, `play: netplay guest: The host closed the room`. Both
//!   exit 1. Neither is the guest's own failure.
//! - Exit 4 only with `--verify-disc`: the disc image is not NTSC 1.02 Melee.
//! - Exit 2: a usage error (no `--iso`, a bad display option, a bad offline `--match`).
//!   Unknown flags are ignored, not refused.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use regex::Regex;

use crate::config::ClientConfig;
use crate::session::{self, ProcId, Record};

/// The set's rules and relay, the same for both peers.
pub struct GameParams<'a> {
    pub relay: &'a str,
    pub stocks: u8,
    pub timer_secs: u32,
}

/// The line an engine with `--np-join-wait` prints when it was hosting and no guest joined
/// in time (it then exits 5).
pub const NO_OPPONENT_LINE: &str = "play: netplay: no opponent joined";
/// The start of the line the installed engine (0cd085d) prints when it was hosting and no
/// guest connected within `--np-timeout` (it then exits 1): `play_net.rs:1154`.
pub const HOST_NO_CONNECTION_PREFIX: &str = "play: netplay host: no connection after ";
/// Lines a guest's engine prints when the room it was sent to is gone: `melee_relay`'s
/// `ERR no room with that code`, and the host's `BYE`.
pub const HOST_GONE_LINES: [&str; 2] = [
    "play: netplay guest: no room with that code",
    "play: netplay guest: The host closed the room",
];
/// The engine's exit status when `--verify-disc` found the disc image unusable.
pub const DISC_CHECK_EXIT_CODE: i32 = 4;
/// The engine's exit status for a usage error.
pub const USAGE_EXIT_CODE: i32 = 2;
/// How many lines of the log [`RunSetup::log_tail`] holds.
const LOG_TAIL_LINES: usize = 10;

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunExit {
    /// The exit status; `None` when the process was ended by a signal, or was adopted from
    /// an earlier `play` (its status cannot be known).
    pub code: Option<i32>,
    /// A host run that ended because nobody joined (one of the engine's lines for it).
    pub no_opponent: bool,
    /// A guest run that ended because the room was gone (one of [`HOST_GONE_LINES`]).
    pub host_gone: bool,
}

/// What the last run was started with, for explaining a failure caused by the player's
/// own setup.
#[derive(Clone, Debug, Default)]
pub struct RunSetup {
    /// The disc image path given to the game.
    pub iso: Option<String>,
    /// The argument template the run used (`host_args` or `join_args`).
    pub args: Vec<String>,
    /// Where the run's log is, and its last lines.
    pub log: Option<PathBuf>,
    pub log_tail: Vec<String>,
}

pub trait Engine {
    /// Starts a run hosting a netplay room.
    fn host(&mut self, p: &GameParams) -> anyhow::Result<()>;
    /// Starts a run joining the room `code`.
    fn join(&mut self, p: &GameParams, code: &str) -> anyhow::Result<()>;
    /// The host's room code, waiting up to `wait` for it to appear.
    fn room_code(&mut self, wait: Duration) -> anyhow::Result<Option<String>>;
    fn running(&mut self) -> bool;
    /// After the run has exited: its `.slp` files, oldest first. Each run's files are
    /// returned once; empty while running or when there is nothing new.
    fn take_replays(&mut self) -> anyhow::Result<Vec<PathBuf>>;
    /// Abandons the run: kills the process, which then writes no replay.
    fn stop(&mut self);
    /// Where the current run's replays are written, if known.
    fn run_dir(&self) -> Option<PathBuf> {
        None
    }
    /// How the last run that exited on its own ended; `None` before then, while a run is
    /// going, or after [`Engine::stop`].
    fn last_exit(&mut self) -> Option<RunExit> {
        None
    }
    /// What the last run was started with; empty if not known.
    fn last_setup(&self) -> RunSetup {
        RunSetup::default()
    }
    /// The directory run directories go in, if this engine has one; `play` keeps its lock
    /// and session file there.
    fn work_dir(&self) -> Option<PathBuf> {
        None
    }
    /// The current run's process, for the session file.
    fn process(&self) -> Option<ProcId> {
        None
    }
    /// Takes over the still-running game `record` describes, as if this engine had started
    /// it. `false` (and nothing changes) unless it is provably that process.
    fn adopt(&mut self, _record: &Record) -> anyhow::Result<bool> {
        Ok(false)
    }
}

/// Runs `{melee}` with `host_args` or `join_args`. Each argument has `{iso}`, `{relay}`,
/// `{code}` (empty when hosting), `{mrpl}`, `{stocks}` and `{timer_secs}` replaced; no
/// shell is involved; `{iso}` and `{mrpl}` are absolute paths. Every run gets a fresh
/// `{work_dir}/run-{n}/`, which is the game's working directory and `MELEE_SLP_DIR` and
/// holds `game.mrpl` and `engine.log` (the game's stdout and stderr, which the client
/// follows and forwards); run directories are never deleted.
pub struct CommandEngine {
    melee: PathBuf,
    iso: String,
    work_dir: PathBuf,
    host_args: Vec<String>,
    join_args: Vec<String>,
    code_regex: Regex,
    output: Output,
    run: Option<Run>,
    last_exit: Option<RunExit>,
    /// The last run's directory and argument template.
    last_run: Option<(PathBuf, Vec<String>)>,
}

/// Where the game's output lines are forwarded.
type Output = Arc<dyn Fn(&str) + Send + Sync>;

/// How often the log is checked for new output.
const TAIL_INTERVAL: Duration = Duration::from_millis(50);

/// The game process of a run.
enum Proc {
    /// Started by this engine.
    Child(Child),
    /// Started by an earlier `play` and taken over; not this process's child, so its exit
    /// status is unknown and it is watched through `/proc`.
    Adopted,
}

struct Run {
    dir: PathBuf,
    /// `None` once the process has exited.
    proc: Option<Proc>,
    /// The process, as recorded in the session file.
    id: Option<ProcId>,
    /// Room codes found in the process's output.
    codes: Receiver<String>,
    code: Option<String>,
    /// Tells the log reader that no more output is coming (or wanted): it reads what is
    /// left and stops.
    done: Arc<AtomicBool>,
}

impl Drop for Run {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

impl CommandEngine {
    pub fn new(cfg: &ClientConfig) -> anyhow::Result<Self> {
        let code_regex = Regex::new(&cfg.code_regex)
            .with_context(|| format!("invalid code_regex {:?}", cfg.code_regex))?;
        // The game runs in its run directory, so relative paths would point elsewhere. A
        // `melee` that is a bare name is left for a PATH lookup.
        let melee = if cfg.melee.components().count() > 1 {
            absolute(&cfg.melee)?
        } else {
            cfg.melee.clone()
        };
        Ok(CommandEngine {
            melee,
            iso: utf8(&absolute(&cfg.iso)?)?.to_string(),
            work_dir: absolute(&cfg.work_dir)?,
            host_args: cfg.host_args.clone(),
            join_args: cfg.join_args.clone(),
            code_regex,
            output: Arc::new(|line: &str| eprintln!("{line}")),
            run: None,
            last_exit: None,
            last_run: None,
        })
    }

    /// Sends the game's output lines to `sink` instead of the client's stderr.
    pub fn set_output(&mut self, sink: impl Fn(&str) + Send + Sync + 'static) {
        self.output = Arc::new(sink);
    }

    fn start(&mut self, host: bool, p: &GameParams, code: &str) -> anyhow::Result<()> {
        if self.running() {
            bail!("the game is already running");
        }
        self.last_exit = None;
        let dir = self.new_run_dir()?;
        let mrpl = dir.join("game.mrpl");
        let mrpl = utf8(&mrpl)?;
        let (stocks, timer_secs) = (p.stocks.to_string(), p.timer_secs.to_string());
        let template = if host {
            &self.host_args
        } else {
            &self.join_args
        };
        self.last_run = Some((dir.clone(), template.clone()));
        let args: Vec<String> = template
            .iter()
            .map(|a| {
                a.replace("{iso}", &self.iso)
                    .replace("{relay}", p.relay)
                    .replace("{code}", code)
                    .replace("{mrpl}", mrpl)
                    .replace("{stocks}", &stocks)
                    .replace("{timer_secs}", &timer_secs)
            })
            .collect();

        // The game writes its output to a file in its run directory, not to a pipe into
        // this process, and runs in its own process group: if the client exits or is
        // interrupted, the game keeps running and still writes its replay.
        let log_path = dir.join("engine.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("creating {}", log_path.display()))?;
        let log_reader =
            File::open(&log_path).with_context(|| format!("opening {}", log_path.display()))?;
        let child = Command::new(&self.melee)
            .args(&args)
            .env("MELEE_SLP_DIR", &dir)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().context("duplicating the log file handle")?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", self.melee.display()))?;
        // Read before anything waits on the child, so even a run that has already exited
        // is still there to read (as a zombie).
        let id = ProcId::of(child.id());
        let (codes, done) = self.follow(log_reader);
        self.run = Some(Run {
            dir,
            proc: Some(Proc::Child(child)),
            id,
            codes,
            code: None,
            done,
        });
        Ok(())
    }

    /// Starts a thread following `log` from its current position.
    fn follow(&self, log: File) -> (Receiver<String>, Arc<AtomicBool>) {
        let (tx, codes) = mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let tail = Tail {
            file: log,
            regex: self.code_regex.clone(),
            output: self.output.clone(),
            codes: tx,
            done: done.clone(),
        };
        std::thread::spawn(move || tail.run());
        (codes, done)
    }

    /// Creates the first free `{work_dir}/run-{n}/`, counting from 1.
    fn new_run_dir(&self) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(&self.work_dir)
            .with_context(|| format!("creating {}", self.work_dir.display()))?;
        for n in 1u64.. {
            let dir = self.work_dir.join(format!("run-{n}"));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
            }
        }
        unreachable!("u64 run numbers exhausted")
    }
}

impl Engine for CommandEngine {
    fn host(&mut self, p: &GameParams) -> anyhow::Result<()> {
        self.start(true, p, "")
    }

    fn join(&mut self, p: &GameParams, code: &str) -> anyhow::Result<()> {
        self.start(false, p, code)
    }

    fn room_code(&mut self, wait: Duration) -> anyhow::Result<Option<String>> {
        let Some(run) = self.run.as_mut() else {
            return Ok(None);
        };
        if run.code.is_none() {
            // Nothing yet, or the run ended without a code: `None` either way.
            run.code = if wait.is_zero() {
                run.codes.try_recv().ok()
            } else {
                run.codes.recv_timeout(wait).ok()
            };
        }
        Ok(run.code.clone())
    }

    fn running(&mut self) -> bool {
        let Some(run) = self.run.as_mut() else {
            return false;
        };
        let code = match run.proc.as_mut() {
            None => return false,
            Some(Proc::Child(child)) => match child.try_wait() {
                Ok(None) => return true,
                Ok(Some(status)) => {
                    if !status.success() {
                        eprintln!("the game exited with {status}");
                    }
                    status.code()
                }
                Err(e) => {
                    eprintln!("checking on the game: {e}");
                    None
                }
            },
            Some(Proc::Adopted) => {
                if run.id.as_ref().is_some_and(ProcId::is_running) {
                    return true;
                }
                None
            }
        };
        run.proc = None;
        run.done.store(true, Ordering::Release);
        self.last_exit = Some(run_exit(code, &run.dir));
        false
    }

    fn take_replays(&mut self) -> anyhow::Result<Vec<PathBuf>> {
        if self.running() {
            return Ok(Vec::new());
        }
        let Some(run) = self.run.take() else {
            return Ok(Vec::new());
        };
        session::replays_in(&run.dir)
    }

    fn stop(&mut self) {
        self.last_exit = None;
        if let Some(mut run) = self.run.take() {
            match run.proc.as_mut() {
                Some(Proc::Child(child)) => {
                    kill_group(child);
                    let _ = child.wait();
                }
                Some(Proc::Adopted) => {
                    if let Some(id) = run.id.as_ref().filter(|id| id.is_running()) {
                        // SAFETY: `killpg` only sends a signal. The identity was checked
                        // just above, so this is still the game's own group.
                        unsafe {
                            libc::killpg(id.pgid, libc::SIGKILL);
                        }
                        // Not this process's child: wait for it to go, briefly.
                        let deadline = Instant::now() + Duration::from_secs(2);
                        while id.is_running() && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                }
                None => {}
            }
        }
    }

    fn run_dir(&self) -> Option<PathBuf> {
        self.run.as_ref().map(|r| r.dir.clone())
    }

    fn last_exit(&mut self) -> Option<RunExit> {
        if self.running() {
            return None;
        }
        self.last_exit
    }

    fn last_setup(&self) -> RunSetup {
        let Some((dir, args)) = &self.last_run else {
            return RunSetup {
                iso: Some(self.iso.clone()),
                ..RunSetup::default()
            };
        };
        let log = dir.join("engine.log");
        let text = std::fs::read(&log).map_or_else(
            |_| String::new(),
            |b| String::from_utf8_lossy(&b).into_owned(),
        );
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(LOG_TAIL_LINES)..]
            .iter()
            .map(|l| l.to_string())
            .collect();
        RunSetup {
            iso: Some(self.iso.clone()),
            args: args.clone(),
            log: Some(log),
            log_tail: tail,
        }
    }

    fn work_dir(&self) -> Option<PathBuf> {
        Some(self.work_dir.clone())
    }

    fn process(&self) -> Option<ProcId> {
        self.run.as_ref().and_then(|r| r.id.clone())
    }

    fn adopt(&mut self, record: &Record) -> anyhow::Result<bool> {
        if self.running() || !record.process.is_running_in(&record.run_dir) {
            return Ok(false);
        }
        let log_path = record.run_dir.join("engine.log");
        let mut log =
            File::open(&log_path).with_context(|| format!("opening {}", log_path.display()))?;
        // What it printed before: only its room code is wanted (it may not have reached
        // the server yet). Then follow the log from here on.
        let mut before = Vec::new();
        log.read_to_end(&mut before)
            .with_context(|| format!("reading {}", log_path.display()))?;
        let code = String::from_utf8_lossy(&before)
            .lines()
            .find_map(|l| capture_code(&self.code_regex, l));
        log.seek(SeekFrom::End(0))
            .with_context(|| format!("reading {}", log_path.display()))?;
        let (codes, done) = self.follow(log);
        self.last_exit = None;
        self.last_run = Some((record.run_dir.clone(), Vec::new()));
        self.run = Some(Run {
            dir: record.run_dir.clone(),
            proc: Some(Proc::Adopted),
            id: Some(record.process.clone()),
            codes,
            code,
            done,
        });
        Ok(true)
    }
}

/// How a run in `dir` that exited with `code` ended, from its `engine.log`.
fn run_exit(code: Option<i32>, dir: &Path) -> RunExit {
    let log = std::fs::read(dir.join("engine.log"))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let mut exit = RunExit {
        code,
        no_opponent: false,
        host_gone: false,
    };
    for line in log.lines().map(str::trim_end) {
        if line == NO_OPPONENT_LINE || line.starts_with(HOST_NO_CONNECTION_PREFIX) {
            exit.no_opponent = true;
        }
        if HOST_GONE_LINES.contains(&line) {
            exit.host_gone = true;
        }
    }
    exit
}

impl Drop for CommandEngine {
    /// Never kills a running game: a match being played must not be lost because the
    /// client exits. The game finishes on its own and writes its replay as usual.
    fn drop(&mut self) {
        if self.running() {
            if let Some(run) = &self.run {
                eprintln!(
                    "Leaving the game running; its replay will be written to {}",
                    run.dir.display()
                );
            }
        }
    }
}

/// Kills the game and everything else in its process group (the game leads its own
/// group, so its pgid is its pid), such as a helper it started.
fn kill_group(child: &mut Child) {
    match libc::pid_t::try_from(child.id()) {
        // SAFETY: `killpg` only sends a signal; it has no memory-safety requirements.
        Ok(pgid) => unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        },
        Err(_) => {
            let _ = child.kill();
        }
    }
}

fn capture_code(regex: &Regex, line: &str) -> Option<String> {
    let c = regex.captures(line)?;
    c.get(1)
        .or_else(|| c.get(0))
        .map(|m| m.as_str().to_string())
}

/// Follows a run's `engine.log`: forwards each line and sends each room code found.
struct Tail {
    file: File,
    regex: Regex,
    output: Output,
    codes: Sender<String>,
    done: Arc<AtomicBool>,
}

impl Tail {
    fn run(mut self) {
        let mut pending = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            // Checked before reading: once set, everything the game wrote is already in
            // the file, so this last pass reads it all.
            let finished = self.done.load(Ordering::Acquire);
            loop {
                match self.file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        while let Some(i) = pending.iter().position(|&b| b == b'\n') {
                            let line: Vec<u8> = pending.drain(..=i).collect();
                            self.line(&line);
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => {
                        eprintln!("reading the game's log: {e}");
                        return;
                    }
                }
            }
            if finished {
                if !pending.is_empty() {
                    self.line(&pending);
                }
                return;
            }
            std::thread::sleep(TAIL_INTERVAL);
        }
    }

    fn line(&self, raw: &[u8]) {
        let line = String::from_utf8_lossy(raw);
        let line = line.trim_end_matches(['\n', '\r']);
        (self.output)(line);
        if let Some(code) = capture_code(&self.regex, line) {
            let _ = self.codes.send(code);
        }
    }
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))
}

fn utf8(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("path {} is not UTF-8", path.display()))
}
