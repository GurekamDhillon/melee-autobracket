//! The play loop: follow the server's `Next`, run the game, hand over the room code and
//! upload the replays.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use proto::{Next, Role};
use reqwest::StatusCode;

use crate::api::{is_retryable, Api, ApiError};
use crate::engine::{Engine, GameParams};

/// How long the host waits for the game to print its room code.
const ROOM_CODE_WAIT: Duration = Duration::from_secs(60);
/// How often the host checks for the room code while waiting.
const ROOM_CODE_SLICE: Duration = Duration::from_millis(50);
/// Consecutive runs of one game that may end without a replay before `play` gives up.
pub const MAX_FAILED_RUNS: u32 = 4;

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

/// Counts consecutive runs of one game that ended without a replay, and spaces out
/// relaunches after them.
struct Failures {
    base: Duration,
    game: Option<(u32, u8)>,
    count: u32,
    retry_at: Option<Instant>,
}

impl Failures {
    /// Records how a run of `launch` ended. Errors after [`MAX_FAILED_RUNS`] in a row.
    fn record(&mut self, launch: Launch, produced_replay: bool, relay: &str) -> anyhow::Result<()> {
        let game = (launch.set_id, launch.game_no);
        if produced_replay {
            self.game = None;
            self.count = 0;
            self.retry_at = None;
            return Ok(());
        }
        if self.game != Some(game) {
            self.game = Some(game);
            self.count = 0;
        }
        self.count += 1;
        if self.count >= MAX_FAILED_RUNS {
            bail!(
                "the game ended without a replay {MAX_FAILED_RUNS} times in a row (set {}, \
                 game {}). Check that `melee` and `iso` in the client config point at a working \
                 engine and game image, and that the relay {relay} is reachable, then run \
                 `play` again",
                launch.set_id,
                launch.game_no
            );
        }
        let wait = self.base * 3u32.pow(self.count - 1);
        self.retry_at = Some(Instant::now() + wait);
        println!(
            "The game ended without a replay ({} of {MAX_FAILED_RUNS}); trying again in {wait:?}",
            self.count
        );
        Ok(())
    }

    /// Whether a relaunch of this game must still wait.
    fn waiting(&self, set_id: u32, game_no: u8) -> bool {
        self.game == Some((set_id, game_no)) && self.retry_at.is_some_and(|t| Instant::now() < t)
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
    let mut session = Session::default();
    let result = run(api, engine, event, opts, &mut session).await;
    if result.is_err() {
        report_unfinished(engine, event, &mut session);
    }
    result
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
                };
                if engine.running() {
                    // Playing; nothing to do until the game exits.
                } else if let Some(launch) = s.launched.take() {
                    let produced = collect(engine, launch, s)?;
                    failures.record(launch, produced, relay)?;
                    upload_pending(api, &mut net, event, s).await?;
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
                            failures.record(this_game, false, relay)?;
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
        let Launch { set_id, game_no: n } = p.launch;
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
