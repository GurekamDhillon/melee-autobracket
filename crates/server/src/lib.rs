//! Tournament bracket server: signed-request HTTP API over a SQLite store.

use std::sync::Arc;
use std::time::Duration;

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
pub mod verify_worker;
pub mod worker;

pub use config::Config;
pub use resim::{ExternalResim, NoResim, Resim, Slpcheck};
pub use state::AppState;

/// How often the rate limiter forgets peers whose quota has fully replenished.
const RATE_LIMIT_TRIM_PERIOD: Duration = Duration::from_secs(60);

/// Opens the store (running migrations), assembles the router, and spawns the background
/// tasks (the judging worker when `cfg.worker_enabled`, and rate-limiter trimming). Their
/// abort handles are kept in `AppState::tasks`.
pub async fn build_app(cfg: Config, resim: Arc<dyn Resim>) -> anyhow::Result<(Router, AppState)> {
    std::fs::create_dir_all(&cfg.replay_dir)
        .with_context(|| format!("creating replay directory {}", cfg.replay_dir.display()))?;
    let db = db::open(&cfg.db_path)
        .await
        .with_context(|| format!("opening database {}", cfg.db_path.display()))?;
    resim.attach(&db);

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
        tasks: Arc::default(),
    };

    if state.cfg.worker_enabled {
        let st = state.clone();
        let worker = tokio::spawn(async move {
            if let Err(e) = worker::recover(&st).await {
                eprintln!("worker: recovering interrupted judging failed: {e:#}");
            }
            worker::run(st).await;
        });
        state.tasks.add(worker.abort_handle());
    }
    let limiter = governor.limiter().clone();
    let trim = tokio::spawn(async move {
        let mut tick = tokio::time::interval(RATE_LIMIT_TRIM_PERIOD);
        loop {
            tick.tick().await;
            limiter.retain_recent();
        }
    });
    state.tasks.add(trim.abort_handle());

    let app = routes::router()
        .layer(GovernorLayer::new(governor))
        .with_state(state.clone());
    Ok((app, state))
}
