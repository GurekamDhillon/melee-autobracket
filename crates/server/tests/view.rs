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
    // Sets decided without play are labelled rather than shown as 0-0.
    assert!(body.contains("decided_by"));
    assert!(body.contains("awarded"));
    // Games counted while the verifier was unavailable are mentioned under the header.
    assert!(body.contains("view.unverified_games"));
    assert!(body.contains("counted without engine verification"));
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
    assert_eq!(cfg.set_stall_secs, defaults.set_stall_secs);
    assert_eq!(cfg.verifier.mode, VerifierMode::Off);
    assert_eq!(cfg.ruleset, defaults.ruleset);
    assert_eq!(cfg.max_connections, defaults.max_connections);
    assert_eq!(cfg.trusted_proxies, defaults.trusted_proxies);
    assert_eq!(cfg.no_show_grace_secs, defaults.no_show_grace_secs);
    assert_eq!(cfg.allow_direct_connect, defaults.allow_direct_connect);
}

/// Standard base64 (with padding), as CSP hash sources use.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The text between the page's `<tag>` and `</tag>`.
fn inline_block<'a>(page: &'a str, tag: &str) -> &'a str {
    let open = format!("<{tag}>");
    let start = page.find(&open).unwrap() + open.len();
    let end = page[start..].find(&format!("</{tag}>")).unwrap() + start;
    assert_eq!(page.matches(&open).count(), 1, "one inline <{tag}>");
    &page[start..end]
}

#[tokio::test]
async fn page_has_security_headers() {
    use sha2::{Digest, Sha256};
    let app = TestApp::spawn(|_| {}).await;
    let res = fetch_root(&app).await;
    let header = |name: &str| res.headers()[name].to_str().unwrap().to_owned();
    assert_eq!(header("x-content-type-options"), "nosniff");
    let csp = header("content-security-policy");
    for directive in [
        "default-src 'none'",
        "connect-src 'self'",
        "img-src data:",
        "base-uri 'none'",
        "form-action 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(directive), "{directive} in {csp}");
    }
    assert!(!csp.contains("unsafe"), "{csp}");
    assert!(!csp.contains("http"), "no external origins: {csp}");
    let page = res.text().await.unwrap();
    for (tag, directive) in [("script", "script-src"), ("style", "style-src")] {
        let hash = base64(&Sha256::digest(inline_block(&page, tag).as_bytes()));
        let expected = format!("{directive} 'sha256-{hash}'");
        assert!(csp.contains(&expected), "{expected} in {csp}");
    }
}

/// Every route in the table of `docs/api.md` exists with its method.
#[tokio::test]
async fn documented_routes_exist() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api.md"))
            .unwrap();
    let app = TestApp::spawn(|_| {}).await;
    let client = reqwest::Client::new();
    let mut seen = 0;
    for line in doc.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let [_, method, path, ..] = cells[..] else {
            continue;
        };
        if !["GET", "POST", "DELETE"].contains(&method) {
            continue;
        }
        // `{name}` placeholders become 1.
        let mut concrete = String::new();
        let mut rest = path.trim_matches('`');
        while let Some((before, after)) = rest.split_once('{') {
            concrete.push_str(before);
            concrete.push('1');
            rest = after.split_once('}').unwrap().1;
        }
        concrete.push_str(rest);
        let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let res = client
            .request(method.clone(), format!("{}{concrete}", app.url))
            .send()
            .await
            .unwrap();
        let status = res.status();
        let body = res.text().await.unwrap();
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {concrete}"
        );
        assert!(
            !body.contains("no such route"),
            "{method} {concrete}: {body}"
        );
        seen += 1;
    }
    assert!(seen >= 21, "parsed {seen} routes from the table");
}
