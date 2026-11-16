//! Tournament bracket server: signed-request HTTP API over a SQLite store.

use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use tokio::sync::Notify;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;

pub mod auth;
pub mod config;
pub mod db;
pub mod judge;
pub mod lifecycle;
pub mod resim;
pub mod routes;
pub mod state;
#[cfg(feature = "testkit")]
pub mod testkit;

pub use config::Config;
pub use resim::{NoResim, Resim};
pub use state::AppState;

/// Opens the store (running migrations) and assembles the router. Spawns nothing.
pub async fn build_app(cfg: Config, resim: Arc<dyn Resim>) -> anyhow::Result<(Router, AppState)> {
    std::fs::create_dir_all(&cfg.replay_dir)
        .with_context(|| format!("creating replay directory {}", cfg.replay_dir.display()))?;
    let db = db::open(&cfg.db_path)
        .await
        .with_context(|| format!("opening database {}", cfg.db_path.display()))?;

    // Replenish one request every 1/rate_per_sec seconds, per peer IP.
    let governor = GovernorConfigBuilder::default()
        .per_nanosecond(1_000_000_000 / cfg.rate_per_sec.max(1))
        .burst_size(cfg.rate_burst)
        .finish()
        .context("invalid rate limit: rate_per_sec and rate_burst must be positive")?;

    let state = AppState {
        cfg: Arc::new(cfg),
        db,
        resim,
        wake: Arc::new(Notify::new()),
    };
    let app = routes::router()
        .layer(GovernorLayer::new(governor))
        .with_state(state.clone());
    Ok((app, state))
}
