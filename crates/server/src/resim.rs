//! Re-simulation of uploaded replays against the game engine.

use std::path::Path;

use async_trait::async_trait;
use proto::ResimOutcome;

#[async_trait]
pub trait Resim: Send + Sync {
    /// Re-simulates the replay at `slp` and compares it with the recording.
    async fn check(&self, slp: &Path, replay_last_frame: i32) -> ResimOutcome;
    /// Identifies the engine build doing the checking.
    fn build(&self) -> String;
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
