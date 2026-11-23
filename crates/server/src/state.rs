use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::config::Config;
use crate::logging::Rejections;
use crate::resim::Resim;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: SqlitePool,
    pub resim: Arc<dyn Resim>,
    /// Signalled when background work (judging, re-simulation) may be ready.
    pub wake: Arc<Notify>,
    /// The background tasks `build_app` spawned.
    pub tasks: Arc<Tasks>,
    /// Requests refused for their signature, summarised in the log.
    pub rejections: Arc<Rejections>,
    /// When this process started, unix seconds. The stall check counts a set's inactivity
    /// from here at the earliest: time the server was down is not inactivity it observed.
    pub started_at: i64,
    /// Set once a game counted peer-only because the configured verifier was unavailable has
    /// been logged at error level: that happens once per process, every such game after it
    /// only gets its own warning.
    pub peer_only_alarm: Arc<AtomicBool>,
    /// The live relay's streams (display only; never read for results).
    pub live: Arc<crate::live::LiveHub>,
}

/// Abort handles of spawned background tasks, so an embedder (e.g. a test) can stop them.
#[derive(Default)]
pub struct Tasks(Mutex<Vec<AbortHandle>>);

impl Tasks {
    pub fn add(&self, handle: AbortHandle) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
    }

    /// Aborts every task added so far.
    pub fn abort_all(&self) {
        for handle in self.0.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            handle.abort();
        }
    }
}
