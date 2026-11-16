//! The play loop: follow the server's `Next`, run the game, hand over the room code and
//! upload the replays.
//!
//! What keeps a live game safe:
//! - A game is stopped only when the server offers a different set, or the player is out
//!   of the event. A set in review keeps its game running; the replay is uploaded when it
//!   ends (the server takes it as evidence).
//! - The game the client started is recorded in `{work_dir}/session.json`, and only one
//!   `play` may use a work directory (`play.lock`). A restarted `play` takes over a game
//!   still running instead of starting another, and uploads the replays of one that ended
//!   while `play` was not running, before anything else.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use proto::{Next, Role};
use reqwest::StatusCode;

use crate::api::{is_retryable, Api, ApiError, NoShowKind};
use crate::engine::{Engine, GameParams, RunExit, DISC_CHECK_EXIT_CODE, USAGE_EXIT_CODE};
use crate::rules::{describe_ruleset, valid_relay, valid_room_code};
use crate::session::{self, Loaded, Record, SessionFile};

/// How long the host waits for the game to print its room code.
const ROOM_CODE_WAIT: Duration = Duration::from_secs(60);
/// How often the host checks for the room code while waiting.
const ROOM_CODE_SLICE: Duration = Duration::from_millis(50);
/// Consecutive runs of one game that may end without a replay before `play` gives up.
pub const MAX_FAILED_RUNS: u32 = 4;
/// Consecutive host runs of one game that may end with nobody joining before `play`
/// reports a no-show.
pub const NO_SHOW_STRIKES: u32 = 2;
/// How many times a no-show report refused for a fresh room code is tried again, at least;
/// it is tried for as long as the grace period the server names lasts.
const NO_SHOW_RETRIES: u32 = 8;
/// Added to the grace period the server names, which it measures in whole seconds.
const GRACE_MARGIN: Duration = Duration::from_secs(2);

/// Timing of the play loop.
#[derive(Clone, Debug)]
pub struct PlayOptions {
    /// Time between checks with the server.
    pub poll: Duration,
    /// Wait before relaunching after the first run that ended without a replay; tripled
    /// for each further one (5 s, 15 s, 45 s by default).
    pub launch_backoff: Duration,
    /// Longest wait between retries while the server is unreachable.
    pub retry_cap: Duration,
    /// A guest with no usable room code for this long reports a no-show; `None` never does.
    pub no_code_wait: Option<Duration>,
    /// Wait before trying again a no-show report refused because a room code was posted
    /// moments ago.
    pub no_show_retry: Duration,
    /// Accept a `HOST:PORT` room code (direct connect).
    pub allow_direct_connect: bool,
    /// Wait before hosting a game again after the server refused its room code because a
    /// game of the set is in progress (unless the server offers a different game first).
    pub in_progress_wait: Duration,
    /// Longest a refused no-show report is retried for the grace period the server names;
    /// after it (and the fixed retries), `play` hosts again.
    pub max_grace_wait: Duration,
}

impl PlayOptions {
    pub fn new(poll: Duration) -> Self {
        PlayOptions {
            poll,
            launch_backoff: Duration::from_secs(5),
            retry_cap: Duration::from_secs(30),
            no_code_wait: Some(Duration::from_secs(
                crate::config::DEFAULT_NO_CODE_WAIT_SECS,
            )),
            no_show_retry: Duration::from_secs(15),
            allow_direct_connect: false,
            in_progress_wait: Duration::from_secs(30),
            max_grace_wait: Duration::from_secs(15 * 60),
        }
    }
}

/// The run the engine was started for.
#[derive(Clone, Debug, PartialEq)]
struct Launch {
    event: i64,
    set_id: u32,
    game_no: u8,
    host: bool,
    /// The code a guest joined with.
    code: Option<String>,
}

impl Launch {
    fn of(record: &Record) -> Self {
        Launch {
            event: record.event,
            set_id: record.set_id,
            game_no: record.game_no,
            host: record.host,
            code: None,
        }
    }

    fn is(&self, set_id: u32, game_no: u8) -> bool {
        self.set_id == set_id && self.game_no == game_no
    }
}

/// A replay collected from a finished run and not yet accepted or refused by the server.
struct Pending {
    launch: Launch,
    path: PathBuf,
}

/// State that outlives a failed loop, for telling the player what was left unfinished,
/// and its copy on disk.
#[derive(Default)]
struct Session {
    launched: Option<Launch>,
    pending: Vec<Pending>,
    file: Option<SessionFile>,
    /// What `file` holds.
    record: Option<Record>,
}

impl Session {
    /// Records a run just started, if the engine can say which process it is.
    fn started(&mut self, engine: &dyn Engine, launch: Launch) {
        if let (Some(file), Some(run_dir), Some(process)) =
            (&self.file, engine.run_dir(), engine.process())
        {
            let record = Record {
                event: launch.event,
                set_id: launch.set_id,
                game_no: launch.game_no,
                host: launch.host,
                run_dir,
                process,
                code_posted: false,
                handled: Vec::new(),
            };
            if let Err(e) = file.save(&record) {
                eprintln!("warning: {e:#}");
            }
            self.record = Some(record);
        }
        self.launched = Some(launch);
    }

    fn update(&mut self, change: impl FnOnce(&mut Record)) {
        if let (Some(file), Some(record)) = (&self.file, self.record.as_mut()) {
            change(record);
            if let Err(e) = file.save(record) {
                eprintln!("warning: {e:#}");
            }
        }
    }

    fn code_posted(&self) -> bool {
        self.record.as_ref().is_some_and(|r| r.code_posted)
    }

    /// Forgets the recorded run: it was stopped, or everything it left is dealt with.
    fn clear(&mut self) {
        if let Some(file) = &self.file {
            file.clear();
        }
        self.record = None;
    }
}

/// Prints each status line once, until it changes.
#[derive(Default)]
struct Status {
    state: Option<String>,
    detail: Option<String>,
}

impl Status {
    fn state(&mut self, line: String) {
        if self.state.as_ref() != Some(&line) {
            println!("{line}");
            self.state = Some(line);
            self.detail = None;
        }
    }

    fn detail(&mut self, line: String) {
        if self.detail.as_ref() != Some(&line) {
            println!("{line}");
            self.detail = Some(line);
        }
    }
}

/// Retries calls that fail retryably, with doubling waits, reporting when the server goes
/// away and when it comes back.
struct Net {
    first_delay: Duration,
    cap: Duration,
    down: bool,
}

impl Net {
    async fn retry<T, F, Fut>(&mut self, mut call: F) -> anyhow::Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = anyhow::Result<T>>,
    {
        let mut delay = self.first_delay;
        loop {
            match call().await {
                Ok(v) => {
                    if self.down {
                        println!("Server is reachable again");
                        self.down = false;
                    }
                    return Ok(v);
                }
                Err(e) if is_retryable(&e) => {
                    if !self.down {
                        println!("Server unavailable ({e}); retrying");
                        self.down = true;
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(self.cap);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// The server refused the request (a 4xx other than 401, which means this client's key or
/// clock is wrong and nothing will work until it is fixed).
fn refused(e: &anyhow::Error) -> Option<&ApiError> {
    e.downcast_ref::<ApiError>().filter(|err| {
        err.status
            .is_some_and(|s| s.is_client_error() && s != StatusCode::UNAUTHORIZED)
    })
}

/// How a finished run ended, as far as relaunching is concerned.
enum Outcome {
    Replay,
    /// A host run that ended because nobody joined.
    NoOpponent,
    /// A guest run that ended because the host's room was gone.
    HostGone,
    /// Ended without a replay for any other reason; how it exited, if known.
    Failed(Option<RunExit>),
}

impl Outcome {
    fn of(launch: &Launch, produced_replay: bool, exit: Option<RunExit>) -> Self {
        if produced_replay {
            Outcome::Replay
        } else if launch.host && exit.is_some_and(|e| e.no_opponent) {
            Outcome::NoOpponent
        } else if !launch.host && exit.is_some_and(|e| e.host_gone) {
            Outcome::HostGone
        } else {
            Outcome::Failed(exit)
        }
    }
}

/// What to do about the set after a run.
enum After {
    Continue,
    /// Report a no-show with this detail, then keep going.
    ReportNoShow(String),
    /// Report the stuck set with `detail` (this client's own failure), then end `play`
    /// with `error`.
    GiveUp {
        error: anyhow::Error,
        detail: String,
    },
}

/// Counts consecutive runs of one game that ended without a replay (and, separately, host
/// runs that nobody joined), and spaces out relaunches after them.
struct Failures {
    base: Duration,
    game: Option<(u32, u8)>,
    count: u32,
    no_shows: u32,
    retry_at: Option<Instant>,
}

impl Failures {
    fn new(base: Duration) -> Self {
        Failures {
            base,
            game: None,
            count: 0,
            no_shows: 0,
            retry_at: None,
        }
    }

    fn for_game(&mut self, set_id: u32, game_no: u8) {
        if self.game != Some((set_id, game_no)) {
            *self = Failures::new(self.base);
            self.game = Some((set_id, game_no));
        }
    }

    /// Forgets every failure: the set went to the organiser.
    fn reset(&mut self) {
        *self = Failures::new(self.base);
    }

    /// Holds off relaunching this game until `until`, without counting a failure.
    fn hold(&mut self, set_id: u32, game_no: u8, until: Instant) {
        self.for_game(set_id, game_no);
        self.retry_at = Some(until);
    }

    /// Records how a run of `launch` ended: a no-show to report after [`NO_SHOW_STRIKES`]
    /// runs nobody joined, and giving up after [`MAX_FAILED_RUNS`] other failed runs in a row.
    fn record(&mut self, launch: &Launch, outcome: Outcome, relay: &str) -> After {
        self.for_game(launch.set_id, launch.game_no);
        let exit = match outcome {
            Outcome::Replay => {
                self.reset();
                return After::Continue;
            }
            Outcome::HostGone => {
                println!(
                    "The room for game {} is gone; waiting for a new room code",
                    launch.game_no
                );
                return After::Continue;
            }
            Outcome::NoOpponent => {
                self.no_shows += 1;
                self.retry_at = Some(Instant::now() + self.base);
                if self.no_shows >= NO_SHOW_STRIKES {
                    self.no_shows = 0;
                    return After::ReportNoShow(format!(
                        "no opponent joined ({NO_SHOW_STRIKES} attempts)"
                    ));
                }
                println!(
                    "Nobody joined game {} ({} of {NO_SHOW_STRIKES}); hosting again in {:?}",
                    launch.game_no, self.no_shows, self.base
                );
                return After::Continue;
            }
            Outcome::Failed(exit) => exit,
        };
        self.no_shows = 0;
        self.count += 1;
        if self.count >= MAX_FAILED_RUNS {
            let last = match exit.and_then(|e| e.code) {
                Some(code) => format!("last exit code {code}"),
                None => "no exit code from the last run".to_string(),
            };
            return After::GiveUp {
                error: anyhow::anyhow!(
                    "the game ended without a replay {MAX_FAILED_RUNS} times in a row (set {}, \
                     game {}). Check that `melee` and `iso` in the client config point at a \
                     working engine and game image, and that the relay {relay} is reachable, \
                     then run `play` again",
                    launch.set_id,
                    launch.game_no
                ),
                detail: format!(
                    "the reporting player's client could not complete game {}: \
                     {MAX_FAILED_RUNS} runs without a replay, {last}",
                    launch.game_no
                ),
            };
        }
        let wait = self.base * 3u32.pow(self.count - 1);
        self.retry_at = Some(Instant::now() + wait);
        println!(
            "The game ended without a replay ({} of {MAX_FAILED_RUNS}); trying again in {wait:?}",
            self.count
        );
        After::Continue
    }

    /// Whether a relaunch of this game must still wait.
    fn waiting(&self, set_id: u32, game_no: u8) -> bool {
        self.game == Some((set_id, game_no)) && self.retry_at.is_some_and(|t| Instant::now() < t)
    }
}

/// A no-show report the server refused because a room code was posted moments ago; tried
/// again later, while no new game is started.
struct LaterReport {
    set_id: u32,
    game_no: u8,
    detail: String,
    at: Instant,
    tries: u32,
    /// Until when refusals are expected: the server's grace period has certainly passed
    /// by then. `None` when it named none; then only [`NO_SHOW_RETRIES`] are made.
    until: Option<Instant>,
}

/// How long a guest has been without a usable room code, for its automatic report.
struct CodeWait {
    set_id: u32,
    game_no: u8,
    since: Instant,
}

/// An exit that means the player's own setup is broken and will not fix itself: the
/// engine's disc check failed, it refused its arguments, or it does not know the preset
/// fighter. `play` stops at once, and nothing is reported to the organiser.
fn setup_error(engine: &dyn Engine, exit: Option<RunExit>) -> Option<anyhow::Error> {
    let exit = exit?;
    let setup = engine.last_setup();
    let log = match &setup.log {
        Some(path) if !setup.log_tail.is_empty() => format!(
            "\nLast lines of {}:\n  {}",
            path.display(),
            setup.log_tail.join("\n  ")
        ),
        _ => String::new(),
    };
    if setup
        .log_tail
        .iter()
        .any(|l| l.contains("--match: unknown fighter"))
    {
        return Some(anyhow::anyhow!(
            "the engine does not know the fighter or stage given with `--match` in \
             `host_args` / `join_args` of the client config; fix it (e.g. `falcon`, `fox`, \
             `marth`), then run `play` again.{log}"
        ));
    }
    let code = exit.code?;
    if code == DISC_CHECK_EXIT_CODE {
        let iso = setup.iso.as_deref().unwrap_or("(unknown)");
        return Some(anyhow::anyhow!(
            "the engine's disc check failed (exit code {code}) for the disc image {iso}. \
             Check that `iso` in the client config points at your own NTSC 1.02 Melee \
             image, then run `play` again.{log}"
        ));
    }
    if code == USAGE_EXIT_CODE {
        return Some(anyhow::anyhow!(
            "the engine refused its arguments (exit code {code}, a usage error). It was \
             started with the template {:?} (`host_args` or `join_args` in the client \
             config); fix it, then run `play` again.{log}",
            setup.args
        ));
    }
    None
}

/// What became of a report.
enum Reported {
    Taken,
    /// Refused because a room code was posted moments ago: worth trying again shortly. The
    /// grace period the server named, if it could be read.
    TooSoon(Option<Duration>),
    /// Refused for another reason, or the server has no such route.
    NotTaken,
}

/// Reports the set to the organiser, retrying while the server is unreachable. A `409`,
/// `404` (an older server), `403` or `400` is printed and not an error.
async fn report(
    api: &Api,
    net: &mut Net,
    event: i64,
    set_id: u32,
    detail: &str,
    kind: NoShowKind,
) -> anyhow::Result<Reported> {
    match net
        .retry(|| api.report_no_show(event, set_id, Some(detail), kind))
        .await
    {
        Ok(()) => Ok(Reported::Taken),
        Err(e) => match e.downcast_ref::<ApiError>() {
            Some(err) if err.status == Some(StatusCode::CONFLICT) => {
                println!(
                    "The server did not take the report for set {set_id}: {}",
                    err.message
                );
                if kind == NoShowKind::NoShow && err.message.contains("room code was posted") {
                    Ok(Reported::TooSoon(grace_in(&err.message)))
                } else {
                    Ok(Reported::NotTaken)
                }
            }
            Some(err) if err.status == Some(StatusCode::NOT_FOUND) => {
                println!(
                    "This server cannot take no-show reports ({}); tell the organiser about set {set_id}",
                    err.message
                );
                Ok(Reported::NotTaken)
            }
            Some(err)
                if err.status == Some(StatusCode::FORBIDDEN)
                    || err.status == Some(StatusCode::BAD_REQUEST) =>
            {
                println!(
                    "The server refused the report for set {set_id} ({}); tell the organiser \
                     about the set",
                    err.message
                );
                Ok(Reported::NotTaken)
            }
            _ => Err(e.context(format!("reporting set {set_id} to the organiser"))),
        },
    }
}

/// The grace period in the server's refusal `a room code was posted less than N seconds
/// ago`.
fn grace_in(message: &str) -> Option<Duration> {
    let (_, rest) = message.split_once("less than ")?;
    let mut words = rest.split_whitespace();
    let secs = words.next()?.parse().ok()?;
    (words.next() == Some("seconds")).then(|| Duration::from_secs(secs))
}

/// Plays `event` until this player is out of it, polling the server every `poll`.
/// Returns the terminal state: `Eliminated`, `Champion`, or `NotEntered`.
pub async fn play_event(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    poll: Duration,
) -> anyhow::Result<Next> {
    play_event_with(api, engine, event, &PlayOptions::new(poll)).await
}

/// [`play_event`] with all timings given. When it fails, it first prints what was left
/// unfinished — a game still running, or replays not uploaded — and how to upload them.
pub async fn play_event_with(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    opts: &PlayOptions,
) -> anyhow::Result<Next> {
    let ended = play_event_until(api, engine, event, opts, std::future::pending()).await?;
    ended.context("play stopped without being asked to")
}

/// [`play_event_with`] that also stops when `stop` completes (on a signal, say): it stops
/// polling, leaves a running game running, prints what was left unfinished as on a
/// failure, and returns `None`.
///
/// Fails at once if another `play` holds the engine's work directory.
pub async fn play_event_until(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    opts: &PlayOptions,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<Option<Next>> {
    let work_dir = engine.work_dir();
    let _lock = work_dir.as_deref().map(session::lock).transpose()?;
    let mut session = Session {
        file: work_dir.as_deref().map(SessionFile::new),
        ..Session::default()
    };
    let result = tokio::select! {
        r = run(api, &mut *engine, event, opts, &mut session) => Some(r),
        () = stop => None,
    };
    match result {
        Some(Ok(next)) => Ok(Some(next)),
        Some(Err(e)) => {
            report_unfinished(engine, event, &mut session);
            Err(e)
        }
        None => {
            report_unfinished(engine, event, &mut session);
            Ok(None)
        }
    }
}

/// Before the first poll: takes over the game an earlier `play` left running, or uploads
/// the replays of one that ended since.
async fn recover(
    api: &Api,
    net: &mut Net,
    engine: &mut dyn Engine,
    event: i64,
    s: &mut Session,
) -> anyhow::Result<()> {
    let Some(file) = s.file.clone() else {
        return Ok(());
    };
    let record = match file.load() {
        Loaded::None => return Ok(()),
        Loaded::Corrupt(why) => {
            eprintln!(
                "warning: ignoring {}, which cannot be read ({why}); starting afresh",
                file.path().display()
            );
            file.clear();
            return Ok(());
        }
        Loaded::Record(r) => r,
    };
    let launch = Launch::of(&record);
    let what = format!(
        "the game for event {} set {} game {} ({})",
        record.event,
        record.set_id,
        record.game_no,
        record.run_dir.display()
    );
    if record.process.is_running_in(&record.run_dir) {
        if record.event != event {
            bail!(
                "{what} from an earlier `play` is still running; run `autobracket-client play \
                 {}` to finish it first",
                record.event
            );
        }
        if engine.adopt(&record)? {
            println!(
                "Taking over {what}, still running from an earlier `play`; its replay is \
                 uploaded when it ends"
            );
            s.record = Some(record);
            s.launched = Some(launch);
            return Ok(());
        }
    } else if record.process.is_running() {
        // The recorded process (pid, start time and boot all match) is alive but not in its
        // run directory. It may well be the game: starting another, or reporting this one
        // as ended, is worse than stopping here.
        let cwd = record
            .process
            .cwd()
            .map_or_else(|| "(unknown)".to_string(), |c| c.display().to_string());
        bail!(
            "{what} may still be running: its process (pid {}) is alive, but its working \
             directory is {cwd}, not the recorded run directory {}. Nothing was started. Your \
             game may still be running: let it finish, then run `play` again, which uploads its \
             replay. If that process is not your game, delete {} and run `play` again",
            record.process.pid,
            record.run_dir.display(),
            file.path().display()
        );
    }
    let replays: Vec<PathBuf> = match session::replays_in(&record.run_dir) {
        Ok(r) => r
            .into_iter()
            .filter(|p| !record.handled.contains(p))
            .collect(),
        Err(e) => {
            eprintln!("warning: {e:#}");
            Vec::new()
        }
    };
    if replays.is_empty() {
        println!("{what} ended while `play` was not running, with no replay left to upload");
    } else {
        println!("{what} ended while `play` was not running; uploading its replay");
    }
    s.record = Some(record);
    s.pending.extend(replays.into_iter().map(|path| Pending {
        launch: launch.clone(),
        path,
    }));
    upload_pending(api, net, s).await?;
    s.clear();
    Ok(())
}

/// The play loop's state between polls.
struct Loop {
    status: Status,
    net: Net,
    failures: Failures,
    later: Option<LaterReport>,
    code_wait: Option<CodeWait>,
    /// The code a guest's run failed with, per game: not joined again.
    tried: Option<(u32, u8, String)>,
    /// The set whose rules were last printed.
    announced: Option<u32>,
}

async fn run(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    opts: &PlayOptions,
    s: &mut Session,
) -> anyhow::Result<Next> {
    let mut l = Loop {
        status: Status::default(),
        net: Net {
            first_delay: opts.poll.max(Duration::from_millis(10)).min(opts.retry_cap),
            cap: opts.retry_cap,
            down: false,
        },
        failures: Failures::new(opts.launch_backoff),
        later: None,
        code_wait: None,
        tried: None,
        announced: None,
    };

    recover(api, &mut l.net, engine, event, s).await?;

    loop {
        let next = l.net.retry(|| api.next(event)).await?;
        stop_if_moved_on(engine, &next, s);
        let collected = finish_run(api, engine, opts, &next, s, &mut l).await?;
        match &next {
            Next::NotEntered | Next::Eliminated | Next::Champion => {
                println!("{}", describe(&next));
                return Ok(next);
            }
            Next::Waiting => l.status.state(describe(&next)),
            Next::Review { .. } => {
                l.status.state(describe(&next));
                // Whatever went wrong before, the organiser has it now.
                l.failures.reset();
                l.later = None;
                l.code_wait = None;
                if let Some(launch) = s.launched.as_ref().filter(|_| engine.running()) {
                    l.status.detail(format!(
                        "Game {} is still running; its replay is uploaded when it ends",
                        launch.game_no
                    ));
                }
            }
            Next::Play { .. } if collected => {
                // Ask again before starting anything: the server knows about the run now.
                l.status.state(describe(&next));
            }
            Next::Play { .. } => play(api, engine, event, opts, &next, s, &mut l).await?,
        }
        tokio::time::sleep(opts.poll).await;
    }
}

/// Stops the launched game if the server has moved on from its set: it offers a different
/// set, or the player is out of the event. A set in review or a wait keeps the game.
fn stop_if_moved_on(engine: &mut dyn Engine, next: &Next, s: &mut Session) {
    let Some(launch) = s.launched.clone() else {
        return;
    };
    let moved_on = match next {
        Next::Play { set_id, .. } => *set_id != launch.set_id,
        Next::NotEntered | Next::Eliminated | Next::Champion => true,
        Next::Waiting | Next::Review { .. } => false,
    };
    if moved_on && engine.running() {
        println!(
            "Stopping the game for set {} game {}: the server has moved on",
            launch.set_id, launch.game_no
        );
        engine.stop();
        s.launched = None;
        s.clear();
    }
}

/// If the launched run has ended: uploads its replays (whatever the set's state), then, if
/// the server still has that set in play, counts the run and reports what needs reporting.
/// Whether there was such a run.
async fn finish_run(
    api: &Api,
    engine: &mut dyn Engine,
    opts: &PlayOptions,
    next: &Next,
    s: &mut Session,
    l: &mut Loop,
) -> anyhow::Result<bool> {
    let Some(launch) = s.launched.clone().filter(|_| !engine.running()) else {
        return Ok(false);
    };
    s.launched = None;
    let produced = collect(engine, &launch, s)?;
    let exit = engine.last_exit();
    if !produced {
        if let Some(e) = setup_error(engine, exit) {
            return Err(e);
        }
    }
    upload_pending(api, &mut l.net, s).await?;
    s.clear();
    let Next::Play { set_id, relay, .. } = next else {
        return Ok(true);
    };
    if *set_id != launch.set_id {
        return Ok(true);
    }
    if !produced && !launch.host {
        if let Some(code) = &launch.code {
            l.tried = Some((launch.set_id, launch.game_no, code.clone()));
        }
    }
    match l
        .failures
        .record(&launch, Outcome::of(&launch, produced, exit), relay)
    {
        After::Continue => {}
        After::ReportNoShow(detail) => {
            let sent = report(
                api,
                &mut l.net,
                launch.event,
                launch.set_id,
                &detail,
                NoShowKind::NoShow,
            )
            .await?;
            match sent {
                Reported::Taken => println!(
                    "Nobody joined set {} game {} after {NO_SHOW_STRIKES} tries; reported a \
                     no-show to the organiser",
                    launch.set_id, launch.game_no
                ),
                Reported::TooSoon(grace) => {
                    let at = Instant::now() + opts.no_show_retry;
                    l.failures.hold(launch.set_id, launch.game_no, at);
                    l.later = Some(LaterReport {
                        set_id: launch.set_id,
                        game_no: launch.game_no,
                        detail,
                        at,
                        tries: 1,
                        // Capped, so an absurd period cannot overflow the clock or wait forever.
                        until: grace.and_then(|g| {
                            let wait = g.saturating_add(GRACE_MARGIN).min(opts.max_grace_wait);
                            Instant::now().checked_add(wait)
                        }),
                    });
                }
                Reported::NotTaken => {}
            }
        }
        After::GiveUp { error, detail } => {
            let kind = NoShowKind::ClientFailure;
            match report(api, &mut l.net, launch.event, launch.set_id, &detail, kind).await {
                Ok(Reported::Taken) => {
                    println!("Reported set {} to the organiser: {detail}", launch.set_id)
                }
                Ok(_) => {}
                Err(e) => eprintln!(
                    "Could not report set {} to the organiser: {e:#}",
                    launch.set_id
                ),
            }
            return Err(error);
        }
    }
    Ok(true)
}

/// Acts on `Next::Play`: starts or joins the game, or waits.
async fn play(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    opts: &PlayOptions,
    next: &Next,
    s: &mut Session,
    l: &mut Loop,
) -> anyhow::Result<()> {
    let Next::Play {
        set_id,
        opponent_tag,
        role,
        relay,
        room_code,
        game_no,
        stocks,
        timer_secs,
        awaiting_verdict,
        ..
    } = next
    else {
        return Ok(());
    };
    let (set_id, game_no) = (*set_id, *game_no);
    l.status.state(describe(next));
    if l.announced != Some(set_id) {
        l.announced = Some(set_id);
        match l.net.retry(|| api.event(event)).await {
            Ok(view) => println!("{}", describe_ruleset(&view.ruleset)),
            Err(e) => eprintln!("Could not fetch the event's rules: {e:#}"),
        }
    }
    if l.later
        .as_ref()
        .is_some_and(|r| !(r.set_id == set_id && r.game_no == game_no))
    {
        l.later = None;
    }
    let params = GameParams {
        relay,
        stocks: *stocks,
        timer_secs: *timer_secs,
    };
    let this_game = Launch {
        event,
        set_id,
        game_no,
        host: *role == Role::Host,
        code: None,
    };
    if let Some(launch) = s.launched.clone() {
        // Playing, or the run ended after `finish_run` looked: it is collected on the next
        // poll, and nothing new starts before then. A game taken over from an earlier
        // `play` may not have had its room code sent yet.
        if engine.running() && launch.host && launch.is(set_id, game_no) && !s.code_posted() {
            if let Some(code) = engine.room_code(Duration::ZERO)? {
                let sent = send_code(api, &mut l.net, s, &launch, &code).await?;
                if sent != CodeSent::Taken {
                    stop_run(engine, s);
                    l.failures.hold(set_id, game_no, sent.hold_until(opts));
                }
            }
        }
        return Ok(());
    }
    if *awaiting_verdict {
        l.status
            .detail(format!("Game {game_no} uploaded; waiting for the verdict"));
        return Ok(());
    }
    if let Some(later) = l.later.as_mut() {
        if Instant::now() < later.at {
            l.status.detail(format!(
                "Reporting the no-show for game {game_no} shortly (the server asks to wait \
                 after a room code)"
            ));
            return Ok(());
        }
        let detail = later.detail.clone();
        match report(api, &mut l.net, event, set_id, &detail, NoShowKind::NoShow).await? {
            Reported::TooSoon(_)
                if later.tries < NO_SHOW_RETRIES
                    || later.until.is_some_and(|u| Instant::now() < u) =>
            {
                later.tries += 1;
                later.at = Instant::now() + opts.no_show_retry;
                l.failures.hold(set_id, game_no, later.at);
                return Ok(());
            }
            Reported::Taken => {
                println!(
                    "Nobody joined set {set_id} game {game_no}; reported a no-show to the \
                     organiser"
                );
                l.later = None;
                return Ok(());
            }
            _ => l.later = None,
        }
    }
    if l.failures.waiting(set_id, game_no) {
        l.status
            .detail(format!("Waiting before starting game {game_no} again"));
        return Ok(());
    }
    if !valid_relay(relay) {
        bail!(
            "the server gave the relay address {relay:?}, which is not HOST[:PORT]; tell the \
             organiser"
        );
    }
    if this_game.host {
        println!("Starting game {game_no} as host");
        engine.host(&params)?;
        s.started(engine, this_game.clone());
        match wait_for_code(engine).await? {
            Some(code) => {
                let sent = send_code(api, &mut l.net, s, &this_game, &code).await?;
                if sent != CodeSent::Taken {
                    stop_run(engine, s);
                    l.failures.hold(set_id, game_no, sent.hold_until(opts));
                }
            }
            None if engine.running() => {
                println!(
                    "No room code from the game after {} s; stopping it",
                    ROOM_CODE_WAIT.as_secs()
                );
                stop_run(engine, s);
                if let After::GiveUp { error, .. } =
                    l.failures.record(&this_game, Outcome::Failed(None), relay)
                {
                    return Err(error);
                }
            }
            // Exited before printing a code; collected on the next poll.
            None => {}
        }
        return Ok(());
    }

    // A guest: join a valid code not already tried for this game.
    let usable = room_code
        .as_deref()
        .filter(|c| valid_room_code(c, opts.allow_direct_connect));
    if let Some(code) = room_code.as_deref().filter(|_| usable.is_none()) {
        l.status.detail(format!(
            "The server passed on a room code that is not valid ({code:?}); not joining it"
        ));
    }
    let stale = |c: &&str| {
        l.tried
            .as_ref()
            .is_some_and(|(s, g, t)| *s == set_id && *g == game_no && t == c)
    };
    match usable.filter(|c| !stale(c)) {
        Some(code) => {
            println!("Joining room {code} for game {game_no}");
            engine.join(&params, code)?;
            s.started(
                engine,
                Launch {
                    code: Some(code.to_string()),
                    ..this_game
                },
            );
            l.code_wait = None;
        }
        None => {
            if usable.is_some() {
                l.status.detail(format!(
                    "Waiting for {opponent_tag} to send a new room code (the last one did not \
                     work)"
                ));
            } else if room_code.is_none() {
                l.status
                    .detail(format!("Waiting for {opponent_tag} to send the room code"));
            }
            guest_waits(api, opts, event, set_id, game_no, l).await?;
        }
    }
    Ok(())
}

fn stop_run(engine: &mut dyn Engine, s: &mut Session) {
    engine.stop();
    s.launched = None;
    s.clear();
}

/// A guest without a usable room code: after [`PlayOptions::no_code_wait`] for this game,
/// reports a no-show, then waits as long again before reporting again.
async fn guest_waits(
    api: &Api,
    opts: &PlayOptions,
    event: i64,
    set_id: u32,
    game_no: u8,
    l: &mut Loop,
) -> anyhow::Result<()> {
    let Some(limit) = opts.no_code_wait else {
        return Ok(());
    };
    let since = match &l.code_wait {
        Some(w) if w.set_id == set_id && w.game_no == game_no => w.since,
        _ => {
            l.code_wait = Some(CodeWait {
                set_id,
                game_no,
                since: Instant::now(),
            });
            return Ok(());
        }
    };
    if since.elapsed() < limit {
        return Ok(());
    }
    let detail = format!(
        "no room code from the host for game {game_no} after {} s",
        limit.as_secs()
    );
    let sent = report(api, &mut l.net, event, set_id, &detail, NoShowKind::NoShow).await?;
    if let Reported::Taken = sent {
        println!(
            "No room code for set {set_id} game {game_no}; reported a no-show to the organiser"
        );
    }
    l.code_wait = Some(CodeWait {
        set_id,
        game_no,
        since: Instant::now(),
    });
    Ok(())
}

/// What became of a room code sent to the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeSent {
    Taken,
    /// Refused (the set is not ready for it, say); the run should be stopped.
    Refused,
    /// Refused because a game of the set is still uploading or being judged; the run should
    /// be stopped, and the game not hosted again for a while.
    GameInProgress,
}

impl CodeSent {
    /// Until when not to host this game again after the refusal.
    fn hold_until(self, opts: &PlayOptions) -> Instant {
        let wait = match self {
            CodeSent::GameInProgress => opts.in_progress_wait,
            _ => opts.launch_backoff,
        };
        Instant::now() + wait
    }
}

/// Hands the host's room code to the server.
async fn send_code(
    api: &Api,
    net: &mut Net,
    s: &mut Session,
    launch: &Launch,
    code: &str,
) -> anyhow::Result<CodeSent> {
    match net
        .retry(|| api.post_room(launch.event, launch.set_id, code))
        .await
    {
        Ok(()) => {
            println!("Room code {code} sent");
            s.update(|r| r.code_posted = true);
            Ok(CodeSent::Taken)
        }
        Err(e) => match refused(&e) {
            Some(err) => {
                println!(
                    "The server did not take room code {code} for game {}: {}; stopping that game",
                    launch.game_no, err.message
                );
                // The room route's answer while a game of the set is uploading or judged.
                if err.status == Some(StatusCode::CONFLICT) && err.message.contains("in progress") {
                    Ok(CodeSent::GameInProgress)
                } else {
                    Ok(CodeSent::Refused)
                }
            }
            None => Err(e),
        },
    }
}

/// Reports a no-show for this player's current set in `event` (the opponent did not show
/// up). Returns what to tell the player.
pub async fn report_current_no_show(
    api: &Api,
    event: i64,
    note: Option<&str>,
) -> anyhow::Result<String> {
    let set_id = match api.next(event).await? {
        Next::Play { set_id, .. } => set_id,
        Next::Review { set_id } => {
            return Ok(format!("Set {set_id} is already with the organiser"));
        }
        other => bail!(
            "you have no set in play in event {event} ({})",
            describe(&other)
        ),
    };
    let detail = note.unwrap_or("opponent did not show up (reported by hand)");
    api.report_no_show(event, set_id, Some(detail), NoShowKind::NoShow)
        .await
        .with_context(|| format!("reporting a no-show for set {set_id}"))?;
    Ok(format!(
        "Reported a no-show for set {set_id}; the organiser decides it"
    ))
}

/// One status line for a `Next`, e.g. `Set 4 vs TAG — game 2 of best-of-3 (1-0) — you host`.
pub fn describe(next: &Next) -> String {
    match next {
        Next::NotEntered => "Not entered in this event".into(),
        Next::Waiting => "Waiting for your next set".into(),
        Next::Review { set_id } => format!("Set {set_id} is being reviewed by the organiser"),
        Next::Eliminated => "Eliminated".into(),
        Next::Champion => "Champion".into(),
        Next::Play {
            set_id,
            opponent_tag,
            role,
            game_no,
            wins,
            best_of,
            ..
        } => {
            let you = if *role == Role::Host {
                "you host"
            } else {
                "you join"
            };
            format!(
                "Set {set_id} vs {opponent_tag} — game {game_no} of best-of-{best_of} ({}-{}) — {you}",
                wins[0], wins[1]
            )
        }
    }
}

/// Polls the host's engine for its room code, in short non-blocking checks, for up to
/// [`ROOM_CODE_WAIT`] or until the game exits.
async fn wait_for_code(engine: &mut dyn Engine) -> anyhow::Result<Option<String>> {
    let deadline = Instant::now() + ROOM_CODE_WAIT;
    loop {
        if let Some(code) = engine.room_code(Duration::ZERO)? {
            return Ok(Some(code));
        }
        if !engine.running() || Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(ROOM_CODE_SLICE).await;
    }
}

/// Queues the finished run's replays for upload as its game; whether there were any.
fn collect(engine: &mut dyn Engine, launch: &Launch, s: &mut Session) -> anyhow::Result<bool> {
    let replays = engine.take_replays()?;
    if replays.is_empty() {
        println!("Game {}: no replay produced", launch.game_no);
    }
    let produced = !replays.is_empty();
    s.pending.extend(replays.into_iter().map(|path| Pending {
        launch: launch.clone(),
        path,
    }));
    Ok(produced)
}

/// Uploads the queued replays in order. A retryable failure is retried until it lands; a
/// refusal (any 4xx but 401) is printed with the command to try again by hand, the file is
/// kept, and `play` goes on; anything else leaves the replay queued and ends `play`.
async fn upload_pending(api: &Api, net: &mut Net, s: &mut Session) -> anyhow::Result<()> {
    while let Some(p) = s.pending.first() {
        let (event, set_id, n) = (p.launch.event, p.launch.set_id, p.launch.game_no);
        let path = p.path.clone();
        let slp = read(&path).await?;
        let sent = net
            .retry(|| api.upload_replay(event, set_id, n, slp.clone()))
            .await;
        match sent {
            Ok(_) => println!("Uploaded game {n}"),
            Err(e) => match refused(&e) {
                Some(err) => println!(
                    "Game {n}: the server did not accept {}: {}. The file is kept; to try \
                     again: autobracket-client upload {event} {set_id} {n} {}",
                    path.display(),
                    err.message,
                    path.display()
                ),
                None => return Err(e.context(format!("uploading {}", path.display()))),
            },
        }
        s.pending.remove(0);
        s.update(|r| r.handled.push(path));
    }
    Ok(())
}

/// Tells the player about a game left running and replays not uploaded, with the command
/// to upload each by hand. The session file is left in place for the next `play`.
fn report_unfinished(engine: &mut dyn Engine, event: i64, s: &mut Session) {
    if let Some(launch) = s.launched.take() {
        if engine.running() {
            let dir = engine.run_dir().map_or_else(String::new, |d| {
                format!(" (its replay goes in {})", d.display())
            });
            eprintln!(
                "The game for set {} game {} is still running{dir}. Run `autobracket-client \
                 play {event}` again to take it over and upload its replay when it ends, or \
                 upload it by hand: autobracket-client upload {} {} {} FILE",
                launch.set_id, launch.game_no, launch.event, launch.set_id, launch.game_no
            );
        } else {
            let dir = engine.run_dir();
            match engine.take_replays() {
                Ok(replays) => s.pending.extend(replays.into_iter().map(|path| Pending {
                    launch: launch.clone(),
                    path,
                })),
                Err(e) => eprintln!(
                    "Could not list the replays of set {} game {} ({e:#}); look for .slp files \
                     in {} and upload them with: autobracket-client upload {} {} {} FILE",
                    launch.set_id,
                    launch.game_no,
                    dir.map_or_else(|| "its run directory".into(), |d| d.display().to_string()),
                    launch.event,
                    launch.set_id,
                    launch.game_no
                ),
            }
        }
    }
    for p in &s.pending {
        eprintln!(
            "Replay for set {} game {} not uploaded: autobracket-client upload {} {} {} {}",
            p.launch.set_id,
            p.launch.game_no,
            p.launch.event,
            p.launch.set_id,
            p.launch.game_no,
            p.path.display()
        );
    }
}

async fn read(path: &Path) -> anyhow::Result<Vec<u8>> {
    tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))
}
