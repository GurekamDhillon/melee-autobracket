//! Running the game. [`Engine`] is what the play loop needs; [`CommandEngine`] runs the
//! real `melee` binary.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
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
}

/// Runs `{melee}` with `host_args` or `join_args`. Each argument has `{iso}`, `{relay}`,
/// `{code}` (empty when hosting), `{mrpl}`, `{stocks}` and `{timer_secs}` replaced; no
/// shell is involved. Every run gets a fresh `{work_dir}/run-{n}/`, which is the
/// `MELEE_SLP_DIR` and holds `game.mrpl`; run directories are never deleted.
pub struct CommandEngine {
    melee: PathBuf,
    iso: String,
    work_dir: PathBuf,
    host_args: Vec<String>,
    join_args: Vec<String>,
    code_regex: Regex,
    run: Option<Run>,
}

struct Run {
    dir: PathBuf,
    /// `None` once the process has exited.
    child: Option<Child>,
    /// Room codes found on the process's stderr.
    codes: Receiver<String>,
    code: Option<String>,
}

impl CommandEngine {
    pub fn new(cfg: &ClientConfig) -> anyhow::Result<Self> {
        let code_regex = Regex::new(&cfg.code_regex)
            .with_context(|| format!("invalid code_regex {:?}", cfg.code_regex))?;
        Ok(CommandEngine {
            melee: cfg.melee.clone(),
            iso: utf8(&cfg.iso)?.to_string(),
            work_dir: cfg.work_dir.clone(),
            host_args: cfg.host_args.clone(),
            join_args: cfg.join_args.clone(),
            code_regex,
            run: None,
        })
    }

    fn start(&mut self, host: bool, p: &GameParams, code: &str) -> anyhow::Result<()> {
        if self.running() {
            bail!("the game is already running");
        }
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

        let mut child = Command::new(&self.melee)
            .args(&args)
            .env("MELEE_SLP_DIR", &dir)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", self.melee.display()))?;
        let stderr = child
            .stderr
            .take()
            .context("the game's stderr was not captured")?;
        let (tx, codes) = mpsc::channel();
        let regex = self.code_regex.clone();
        std::thread::spawn(move || {
            // Forwards the engine's output to ours and picks out room codes.
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            while matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches(['\n', '\r']);
                eprintln!("{line}");
                if let Some(c) = regex.captures(line) {
                    let code = c.get(1).or_else(|| c.get(0)).map(|m| m.as_str());
                    if let Some(code) = code {
                        let _ = tx.send(code.to_string());
                    }
                }
                buf.clear();
            }
        });
        self.run = Some(Run {
            dir,
            child: Some(child),
            codes,
            code: None,
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
            // Nothing yet, or stderr closed without a code: `None` either way.
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
                false
            }
            Err(e) => {
                eprintln!("checking on the game: {e}");
                run.child = None;
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
        if let Some(mut run) = self.run.take() {
            if let Some(child) = run.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn utf8(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("path {} is not UTF-8", path.display()))
}
