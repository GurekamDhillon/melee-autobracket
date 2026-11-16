//! Tournament bracket server: signed-request HTTP API over a SQLite store.

use std::sync::Arc;
use std::time::Duration;

use std::net::SocketAddr;

use anyhow::Context;
use axum::extract::ConnectInfo;
use axum::Router;
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::{Notify, Semaphore};
use tower::ServiceExt;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;

pub mod auth;
pub mod config;
pub mod db;
pub mod judge;
pub mod lifecycle;
pub mod logging;
pub mod net;
pub mod resim;
pub mod results;
pub mod review;
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
/// How long a connection may take to send a request's headers, and how long an idle
/// keep-alive connection is kept open.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Validates `cfg`, opens the store (running migrations), assembles the router, and spawns the background
/// tasks (the judging worker when `cfg.worker_enabled`, and rate-limiter trimming). Their
/// abort handles are kept in `AppState::tasks`.
pub async fn build_app(cfg: Config, resim: Arc<dyn Resim>) -> anyhow::Result<(Router, AppState)> {
    cfg.validate().context("invalid configuration")?;
    worker::check_verifier_build(&cfg, resim.as_ref())?;
    std::fs::create_dir_all(&cfg.replay_dir)
        .with_context(|| format!("creating replay directory {}", cfg.replay_dir.display()))?;
    let db = db::open(&cfg.db_path)
        .await
        .with_context(|| format!("opening database {}", cfg.db_path.display()))?;
    resim
        .attach(&db)
        .await
        .context("attaching the verifier to the store")?;

    // Replenish one request every 1/rate_per_sec seconds, per client address (`validate`
    // checked that both rates are positive and that `trusted_proxies` parses).
    let trusted = net::parse_cidrs(&cfg.trusted_proxies).context("trusted_proxies")?;
    let governor = GovernorConfigBuilder::default()
        .per_nanosecond(1_000_000_000 / cfg.rate_per_sec)
        .burst_size(cfg.rate_burst)
        .key_extractor(net::ClientKey {
            trusted: trusted.into(),
        })
        .finish()
        .context("invalid rate limit: rate_per_sec and rate_burst must be positive")?;

    let state = AppState {
        cfg: Arc::new(cfg),
        db,
        resim,
        wake: Arc::new(Notify::new()),
        tasks: Arc::default(),
        rejections: Arc::default(),
        started_at: db::now_unix(),
        peer_only_alarm: Arc::default(),
    };

    if state.cfg.worker_enabled {
        let st = state.clone();
        let worker = tokio::spawn(async move {
            if let Err(e) = worker::recover(&st).await {
                tracing::error!(
                    error = %logging::detail(format_args!("{e:#}")),
                    "recovering interrupted judging failed"
                );
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
        // Outermost, so the rate limiter's refusals are JSON too.
        .layer(axum::middleware::map_response(routes::json_errors))
        .with_state(state.clone());
    Ok((app, state))
}

/// Serves `app` on `listener` until the listener fails: HTTP/1.1, at most `max_connections`
/// connections at once (further ones wait in the listen backlog), and a connection that takes
/// longer than [`HEADER_READ_TIMEOUT`] to send a request's headers, or sits idle that long
/// between requests, is closed. Each request carries the peer address as
/// `ConnectInfo<SocketAddr>`.
pub async fn serve(listener: TcpListener, app: Router, max_connections: usize) {
    let slots = Arc::new(Semaphore::new(max_connections));
    loop {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            return; // The semaphore is never closed.
        };
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                // E.g. out of file descriptors: back off instead of spinning.
                tracing::warn!(error = %logging::detail(&e), "accepting a connection failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            let service = app.map_request(move |mut req: hyper::Request<Incoming>| {
                req.extensions_mut().insert(ConnectInfo::<SocketAddr>(peer));
                req
            });
            let served = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT)
                .serve_connection(TokioIo::new(stream), TowerToHyperService::new(service))
                .await;
            if let Err(e) = served {
                tracing::debug!(error = %logging::detail(&e), "connection ended with an error");
            }
            drop(slot);
        });
    }
}
