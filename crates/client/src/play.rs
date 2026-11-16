//! The play loop: follow the server's `Next`, run the game, hand over the room code and
//! upload the replays.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;
use proto::{Next, Role};
use reqwest::StatusCode;

use crate::api::{Api, ApiError};
use crate::engine::{Engine, GameParams};

/// How long the host waits for the game to print its room code.
const ROOM_CODE_WAIT: Duration = Duration::from_secs(60);
/// How often the host checks for the room code while waiting.
const ROOM_CODE_SLICE: Duration = Duration::from_millis(50);

/// The run the engine was started for.
struct Launch {
    set_id: u32,
    game_no: u8,
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

/// Plays `event` until this player is out of it, polling the server every `poll`.
/// Returns the terminal state: `Eliminated`, `Champion`, or `NotEntered`.
pub async fn play_event(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    poll: Duration,
) -> anyhow::Result<Next> {
    let mut launched: Option<Launch> = None;
    let mut status = Status::default();
    loop {
        let next = api.next(event).await?;
        let play_set = match &next {
            Next::Play { set_id, .. } => Some(*set_id),
            _ => None,
        };
        if launched
            .as_ref()
            .is_some_and(|l| Some(l.set_id) != play_set)
        {
            engine.stop();
            launched = None;
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
                if engine.running() {
                    // Playing; nothing to do until the game exits.
                } else if let Some(run) = launched.take() {
                    upload(api, engine, event, &run).await?;
                    // Ask again before doing anything else: the server now knows about it.
                } else if *awaiting_verdict {
                    status.detail(format!("Game {game_no} uploaded; waiting for the verdict"));
                } else if *role == Role::Host {
                    println!("Starting game {game_no} as host");
                    engine.host(&params)?;
                    launched = Some(Launch {
                        set_id: *set_id,
                        game_no: *game_no,
                    });
                    match wait_for_code(engine).await? {
                        Some(code) => {
                            api.post_room(event, *set_id, &code).await?;
                            println!("Room code {code} sent");
                        }
                        None if engine.running() => {
                            println!(
                                "No room code from the game after {} s; stopping it",
                                ROOM_CODE_WAIT.as_secs()
                            );
                            engine.stop();
                            launched = None;
                        }
                        // Exited before printing a code; collected on the next poll.
                        None => {}
                    }
                } else if let Some(code) = room_code {
                    println!("Joining room {code} for game {game_no}");
                    engine.join(&params, code)?;
                    launched = Some(Launch {
                        set_id: *set_id,
                        game_no: *game_no,
                    });
                } else {
                    status.detail(format!("Waiting for {opponent_tag} to send the room code"));
                }
            }
        }
        tokio::time::sleep(poll).await;
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

/// Uploads every replay of the run that just exited as its game. A refused upload (`409`)
/// is reported and skipped.
async fn upload(
    api: &Api,
    engine: &mut dyn Engine,
    event: i64,
    run: &Launch,
) -> anyhow::Result<()> {
    let n = run.game_no;
    let replays = engine.take_replays()?;
    if replays.is_empty() {
        println!("Game {n}: no replay produced");
    }
    for path in replays {
        let slp = read(&path).await?;
        match api.upload_replay(event, run.set_id, n, slp).await {
            Ok(_) => println!("Uploaded game {n}"),
            Err(e) => match e.downcast_ref::<ApiError>() {
                Some(err) if err.status == StatusCode::CONFLICT => {
                    println!("Game {n}: {} not accepted: {}", path.display(), err.message);
                }
                _ => return Err(e.context(format!("uploading {}", path.display()))),
            },
        }
    }
    Ok(())
}

async fn read(path: &Path) -> anyhow::Result<Vec<u8>> {
    tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))
}
