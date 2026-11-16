use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::Notify;

use crate::config::Config;
use crate::resim::Resim;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: SqlitePool,
    pub resim: Arc<dyn Resim>,
    /// Signalled when background work (judging, re-simulation) may be ready.
    pub wake: Arc<Notify>,
}
