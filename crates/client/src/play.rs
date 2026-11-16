//! The play loop: follow the server's `Next`, run the game, hand over the room code and
//! upload the replays.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use proto::{Next, Role};
use reqwest::StatusCode;

use crate::api::{is_retryable, Api, ApiError};
use crate::engine::{Engine, GameParams, RunExit, DISC_CHECK_EXIT_CODE, USAGE_EXIT_CODE};

/// How long the host waits for the game to print its room code.
const ROOM_CODE_WAIT: Duration = Duration::from_secs(60);
/// How often the host checks for the room code while waiting.
const ROOM_CODE_SLICE: Duration = Duration::from_millis(50);
/// Consecutive runs of one game that may end without a replay before `play` gives up.
pub const MAX_FAILED_RUNS: u32 = 4;
/// Consecutive host runs of one game that may end with nobody joining before `play`
/// reports a no-show.
pub const NO_SHOW_STRIKES: u32 = 2;

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
}

impl PlayOptions {
    pub fn new(poll: Duration) -> Self {
        PlayOptions {
            poll,
            launch_backoff: Duration::from_secs(5),
            retry_cap: Duration::from_secs(30),
        }
    }
}

/// The run the engine was started for.
#[derive(Clone, Copy)]
struct Launch {
    set_id: u32,
    game_no: u8,
    host: bool,
}

/// A replay collected from a finished run and not yet accepted or refused by the server.
struct Pending {
    launch: Launch,
    path: PathBuf,
}

/// State that outlives a failed loop, for telling the player what was left unfinished.
#[derive(Default)]
struct Session {
    launched: Option<Launch>,
    pending: Vec<Pending>,
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

/// How a finished run ended, as far as relaunching is concerned.
enum Outcome {
    Replay,
    /// A host run that ended because nobody joined.
    NoOpponent,
    /// Ended without a replay for any other reason; how it exited, if known.
    Failed(Option<RunExit>),
}

impl Outcome {
    fn of(launch: Launch, produced_replay: bool, exit: Option<RunExit>) -> Self {
        if produced_replay {
            Outcome::Replay
        } else if launch.host && exit.is_some_and(|e| e.no_opponent) {
            Outcome::NoOpponent
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
    /// Report the stuck set with `detail`, then end `play` with `error`.
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
    /// Records how a run of `launch` ended: a no-show to report after [`NO_SHOW_STRIKES`]
    /// runs nobody joined, and giving up after [`MAX_FAILED_RUNS`] other failed runs in a row.
    fn record(&mut self, launch: Launch, outcome: Outcome, relay: &str) -> After {
        let game = (launch.set_id, launch.game_no);
        if self.game != Some(game) {
            self.game = Some(game);
            self.count = 0;
            self.no_shows = 0;
        }
        let exit = match outcome {
            Outcome::Replay => {
                self.game = None;
                self.count = 0;
                self.no_shows = 0;
                self.retry_at = None;
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
                // Worded as this client's failure, not the opponent's.
                detail: format!(
                    "client could not complete a game: {MAX_FAILED_RUNS} runs without a \
                     replay, {last}"
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

/// An exit that means the player's own setup is broken and will not fix itself: the
/// engine's disc check failed, or it refused its arguments. `play` stops at once, and
/// nothing is reported to the organiser.
fn setup_error(engine: &dyn Engine, exit: Option<RunExit>) -> Option<anyhow::Error> {
    let code = exit?.code?;
    let setup = engine.last_setup();
    if code == DISC_CHECK_EXIT_CODE {
        let iso = setup.iso.as_deref().unwrap_or("(unknown)");
        return Some(anyhow::anyhow!(
            "the engine's disc check failed (exit code {code}) for the disc image {iso}. \
             Check that `iso` in the client config points at your own NTSC 1.02 Melee \
             image, then run `play` again"
        ));
    }
    if code == USAGE_EXIT_CODE {
        let log = match &setup.log {
            Some(path) if !setup.log_tail.is_empty() => format!(
                "\nLast lines of {}:\n  {}",
                path.display(),
                setup.log_tail.join("\n  ")
            ),
            _ => String::new(),
        };
        return Some(anyhow::anyhow!(
            "the engine refused its arguments (exit code {code}, a usage error). It was \
             started with the template {:?} (`host_args` or `join_args` in the client \
             config); fix it, then run `play` again.{log}",
            setup.args
        ));
    }
    None
}

/// Acts on [`After`] for `launch`'s set.
async fn settle(
    api: &Api,
    net: &mut Net,
    event: i64,
    launch: Launch,
    after: After,
) -> anyhow::Result<()> {
    match after {
        After::Continue => Ok(()),
        After::ReportNoShow(detail) => {
            if report_no_show(api, net, event, launch.set_id, &detail).await? {
                println!(
                    "Nobody joined set {} game {} after {NO_SHOW_STRIKES} tries; reported a \
                     no-show to the organiser",
                    launch.set_id, launch.game_no
                );
            }
            Ok(())
        }
        After::GiveUp { error, detail } => {
            match report_no_show(api, net, event, launch.set_id, &detail).await {
                Ok(true) => println!("Reported set {} to the organiser: {detail}", launch.set_id),
                Ok(false) => {}
                Err(e) => eprintln!(
                    "Could not report set {} to the organiser: {e:#}",
                    launch.set_id
                ),
            }
            Err(error)
        }
    }
}

/// Reports a no-show, retrying while the server is unreachable. Whether the server took it:
/// a `409` (the set is not in a state for it) or `404` (an older server) is logged and not
/// an error.
async fn report_no_show(
    api: &Api,
    net: &mut Net,
    event: i64,
    set_id: u32,
    detail: &str,
) -> anyhow::Result<bool> {
    match net
        .retry(|| api.report_no_show(event, set_id, Some(detail)))
        .await
    {
        Ok(()) => Ok(true),
        Err(e) => match e.downcast_ref::<ApiError>() {
            Some(err) if err.status == Some(StatusCode::CONFLICT) => {
                println!(
                    "The server did not take the no-show report for set {set_id}: {}",
                    err.message
                );
                Ok(false)
            }
            Some(err) if err.status == Some(StatusCode::NOT_FOUND) => {
                println!(
                    "This server cannot take no-show reports ({}); tell the organiser about set {set_id}",
                    err.message
                );
                Ok(false)
            }
            _ => Err(e.context(format!("reporting a no-show for set {set_id}"))),
        },
    }
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
pub async fn play_event_until(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    opts: &PlayOptions,
    stop: impl Future<Output = ()>,
) -> anyhow::Result<Option<Next>> {
    let mut session = Session::default();
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

async fn run(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    opts: &PlayOptions,
    s: &mut Session,
) -> anyhow::Result<Next> {
    let mut status = Status::default();
    let mut net = Net {
        first_delay: opts.poll.max(Duration::from_millis(10)).min(opts.retry_cap),
        cap: opts.retry_cap,
        down: false,
    };
    let mut failures = Failures {
        base: opts.launch_backoff,
        game: None,
        count: 0,
        no_shows: 0,
        retry_at: None,
    };
    loop {
        let next = net.retry(|| api.next(event)).await?;
        let play_set = match &next {
            Next::Play { set_id, .. } => Some(*set_id),
            _ => None,
        };
        if let Some(launch) = s.launched.filter(|l| Some(l.set_id) != play_set) {
            if engine.running() {
                engine.stop();
            } else {
                // Finished before the set moved on: its replays still belong to that game.
                collect(engine, launch, s)?;
                upload_pending(api, &mut net, event, s).await?;
            }
            s.launched = None;
        }

        match &next {
            Next::NotEntered | Next::Eliminated | Next::Champion => {
                println!("{}", describe(&next));
                return Ok(next);
            }
            Next::Waiting | Next::Review { .. } => status.state(describe(&next)),
            Next::Play {
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
            } => {
                status.state(describe(&next));
                let params = GameParams {
                    relay,
                    stocks: *stocks,
                    timer_secs: *timer_secs,
                };
                let this_game = Launch {
                    set_id: *set_id,
                    game_no: *game_no,
                    host: *role == Role::Host,
                };
                if engine.running() {
                    // Playing; nothing to do until the game exits.
                } else if let Some(launch) = s.launched.take() {
                    let produced = collect(engine, launch, s)?;
                    let exit = engine.last_exit();
                    if !produced {
                        if let Some(e) = setup_error(engine, exit) {
                            return Err(e);
                        }
                    }
                    let outcome = Outcome::of(launch, produced, exit);
                    let after = failures.record(launch, outcome, relay);
                    upload_pending(api, &mut net, event, s).await?;
                    settle(api, &mut net, event, launch, after).await?;
                    // Ask again before doing anything else: the server now knows about it.
                } else if *awaiting_verdict {
                    status.detail(format!("Game {game_no} uploaded; waiting for the verdict"));
                } else if failures.waiting(*set_id, *game_no) {
                    status.detail(format!("Waiting before starting game {game_no} again"));
                } else if *role == Role::Host {
                    println!("Starting game {game_no} as host");
                    engine.host(&params)?;
                    s.launched = Some(this_game);
                    match wait_for_code(engine).await? {
                        Some(code) => {
                            net.retry(|| api.post_room(event, *set_id, &code)).await?;
                            println!("Room code {code} sent");
                        }
                        None if engine.running() => {
                            println!(
                                "No room code from the game after {} s; stopping it",
                                ROOM_CODE_WAIT.as_secs()
                            );
                            engine.stop();
                            s.launched = None;
                            let after = failures.record(this_game, Outcome::Failed(None), relay);
                            settle(api, &mut net, event, this_game, after).await?;
                        }
                        // Exited before printing a code; collected on the next poll.
                        None => {}
                    }
                } else if let Some(code) = room_code {
                    println!("Joining room {code} for game {game_no}");
                    engine.join(&params, code)?;
                    s.launched = Some(this_game);
                } else {
                    status.detail(format!("Waiting for {opponent_tag} to send the room code"));
                }
            }
        }
        tokio::time::sleep(opts.poll).await;
    }
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
fn collect(engine: &mut dyn Engine, launch: Launch, s: &mut Session) -> anyhow::Result<bool> {
    let replays = engine.take_replays()?;
    if replays.is_empty() {
        println!("Game {}: no replay produced", launch.game_no);
    }
    let produced = !replays.is_empty();
    s.pending
        .extend(replays.into_iter().map(|path| Pending { launch, path }));
    Ok(produced)
}

/// Uploads the queued replays in order. A refused upload (`409`) is reported and dropped;
/// a retryable failure is retried until it lands; any other failure leaves the replay
/// queued and ends `play`.
async fn upload_pending(
    api: &Api,
    net: &mut Net,
    event: i64,
    s: &mut Session,
) -> anyhow::Result<()> {
    while let Some(p) = s.pending.first() {
        let Launch {
            set_id, game_no: n, ..
        } = p.launch;
        let slp = read(&p.path).await?;
        let sent = net
            .retry(|| api.upload_replay(event, set_id, n, slp.clone()))
            .await;
        match sent {
            Ok(_) => println!("Uploaded game {n}"),
            Err(e) => match e.downcast_ref::<ApiError>() {
                Some(err) if err.status == Some(StatusCode::CONFLICT) => {
                    println!(
                        "Game {n}: {} not accepted: {}",
                        p.path.display(),
                        err.message
                    );
                }
                _ => return Err(e.context(format!("uploading {}", p.path.display()))),
            },
        }
        s.pending.remove(0);
    }
    Ok(())
}

/// Tells the player about a game left running and replays not uploaded, with the command
/// to upload each by hand.
fn report_unfinished(engine: &mut dyn Engine, event: i64, s: &mut Session) {
    if let Some(launch) = s.launched.take() {
        if engine.running() {
            let dir = engine.run_dir().map_or_else(String::new, |d| {
                format!(" (its replay goes in {})", d.display())
            });
            eprintln!(
                "The game for set {} game {} is still running{dir}. When it ends, upload its \
                 replay with: autobracket-client upload {event} {} {} FILE",
                launch.set_id, launch.game_no, launch.set_id, launch.game_no
            );
        } else {
            let dir = engine.run_dir();
            match engine.take_replays() {
                Ok(replays) => s
                    .pending
                    .extend(replays.into_iter().map(|path| Pending { launch, path })),
                Err(e) => eprintln!(
                    "Could not list the replays of set {} game {} ({e:#}); look for .slp files \
                     in {} and upload them with: autobracket-client upload {event} {} {} FILE",
                    launch.set_id,
                    launch.game_no,
                    dir.map_or_else(|| "its run directory".into(), |d| d.display().to_string()),
                    launch.set_id,
                    launch.game_no
                ),
            }
        }
    }
    for p in &s.pending {
        eprintln!(
            "Replay for set {} game {} not uploaded: autobracket-client upload {event} {} {} {}",
            p.launch.set_id,
            p.launch.game_no,
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
