//! Running the game. [`Engine`] is what the play loop needs; [`CommandEngine`] runs the
//! real `melee` binary.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use regex::Regex;

use crate::config::ClientConfig;

/// The set's rules and relay, the same for both peers.
pub struct GameParams<'a> {
    pub relay: &'a str,
    pub stocks: u8,
    pub timer_secs: u32,
}

/// The line the engine prints on stderr when it was hosting and no guest joined within its
/// `--np-join-wait`.
pub const NO_OPPONENT_LINE: &str = "play: netplay: no opponent joined";
/// The engine's exit status in that case.
pub const NO_OPPONENT_EXIT_CODE: i32 = 4;

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunExit {
    /// The exit status; `None` when the process was ended by a signal.
    pub code: Option<i32>,
    /// The game gave up because nobody joined: it exited with [`NO_OPPONENT_EXIT_CODE`] or
    /// printed [`NO_OPPONENT_LINE`].
    pub no_opponent: bool,
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
}

/// Where the game's output lines are forwarded.
type Output = Arc<dyn Fn(&str) + Send + Sync>;

/// How often the log is checked for new output.
const TAIL_INTERVAL: Duration = Duration::from_millis(50);

struct Run {
    dir: PathBuf,
    /// `None` once the process has exited.
    child: Option<Child>,
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
        let (tx, codes) = mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let tail = Tail {
            file: log_reader,
            regex: self.code_regex.clone(),
            output: self.output.clone(),
            codes: tx,
            done: done.clone(),
        };
        std::thread::spawn(move || tail.run());
        self.run = Some(Run {
            dir,
            child: Some(child),
            codes,
            code: None,
            done,
        });
        Ok(())
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
        let Some(child) = run.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                if !status.success() {
                    eprintln!("the game exited with {status}");
                }
                run.child = None;
                run.done.store(true, Ordering::Release);
                self.last_exit = Some(run_exit(status.code(), &run.dir));
                false
            }
            Err(e) => {
                eprintln!("checking on the game: {e}");
                run.child = None;
                run.done.store(true, Ordering::Release);
                self.last_exit = Some(run_exit(None, &run.dir));
                false
            }
        }
    }

    fn take_replays(&mut self) -> anyhow::Result<Vec<PathBuf>> {
        if self.running() {
            return Ok(Vec::new());
        }
        let Some(run) = self.run.take() else {
            return Ok(Vec::new());
        };
        let mut replays = Vec::new();
        for entry in
            std::fs::read_dir(&run.dir).with_context(|| format!("reading {}", run.dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "slp") && path.is_file() {
                replays.push(path);
            }
        }
        // `Game_<UTC timestamp>.slp`, so name order is time order.
        replays.sort();
        Ok(replays)
    }

    fn stop(&mut self) {
        self.last_exit = None;
        if let Some(mut run) = self.run.take() {
            if let Some(child) = run.child.as_mut() {
                kill_group(child);
                let _ = child.wait();
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
}

/// How a run in `dir` that exited with `code` ended, from its code and its `engine.log`.
fn run_exit(code: Option<i32>, dir: &Path) -> RunExit {
    let printed = std::fs::read(dir.join("engine.log")).is_ok_and(|log| {
        String::from_utf8_lossy(&log)
            .lines()
            .any(|l| l.trim_end() == NO_OPPONENT_LINE)
    });
    RunExit {
        code,
        no_opponent: code == Some(NO_OPPONENT_EXIT_CODE) || printed,
    }
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
        if let Some(c) = self.regex.captures(line) {
            if let Some(code) = c.get(1).or_else(|| c.get(0)) {
                let _ = self.codes.send(code.as_str().to_string());
            }
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
