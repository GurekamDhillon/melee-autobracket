use std::path::Path;

use autobracket_server::config::VerifierMode;
use autobracket_server::testkit::TestApp;
use autobracket_server::Config;
use axum::http::StatusCode;

async fn fetch_root(app: &TestApp) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}/", app.url))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn root_serves_html() {
    let app = TestApp::spawn(|_| {}).await;
    let res = fetch_root(&app).await;
    assert_eq!(res.status(), StatusCode::OK);
    let ctype = res.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ctype.starts_with("text/html"), "content-type {ctype:?}");
    let body = res.text().await.unwrap();
    assert!(body.contains("<title>"));
    assert!(body.contains("/v1/events"));
}

#[tokio::test]
async fn page_has_no_external_urls() {
    let app = TestApp::spawn(|_| {}).await;
    let res = fetch_root(&app).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.text().await.unwrap();
    assert!(!body.contains("http://"), "page references http://");
    assert!(!body.contains("https://"), "page references https://");
}

#[test]
fn example_config_parses() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/autobracket.example.toml");
    let cfg = Config::load(&path).unwrap();
    let defaults = Config::new(cfg.to_keys.clone());
    assert_eq!(
        cfg.db_path,
        Path::new("/var/lib/autobracket/autobracket.sqlite")
    );
    assert_eq!(cfg.replay_dir, Path::new("/var/lib/autobracket/replays"));
    assert_eq!(cfg.bind, defaults.bind);
    assert_eq!(cfg.relay, defaults.relay);
    assert_eq!(cfg.rate_per_sec, defaults.rate_per_sec);
    assert_eq!(cfg.rate_burst, defaults.rate_burst);
    assert_eq!(cfg.verifier.mode, VerifierMode::Off);
    assert_eq!(cfg.ruleset, defaults.ruleset);
}
