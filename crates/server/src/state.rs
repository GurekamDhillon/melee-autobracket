use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::config::Config;
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
