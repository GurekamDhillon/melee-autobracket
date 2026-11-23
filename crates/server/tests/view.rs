use std::path::Path;

use autobracket_server::config::VerifierMode;
use autobracket_server::testkit::TestApp;
use autobracket_server::Config;
use axum::http::StatusCode;
use bracket::Format;

async fn fetch(app: &TestApp, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", app.url))
        .send()
        .await
        .unwrap()
}

/// Every public page, on a server with one event and one registered player.
async fn all_pages(app: &TestApp) -> Vec<String> {
    let event = app.create_event(Format::SingleElim).await;
    app.player("Mango").await;
    vec![
        "/".to_owned(),
        "/stream".to_owned(),
        "/schedule".to_owned(),
        "/history".to_owned(),
        "/players".to_owned(),
        "/players/Mango".to_owned(),
        format!("/events/{event}"),
    ]
}

#[tokio::test]
async fn schedule_and_history_pages_fetch_their_data() {
    let app = TestApp::spawn(|_| {}).await;
    let schedule = fetch(&app, "/schedule").await.text().await.unwrap();
    for needle in [
        "/v1/events",
        "Happening now",
        "Coming up",
        "time to be announced",
        "autobracket-client register",
    ] {
        assert!(schedule.contains(needle), "/schedule has {needle:?}");
    }
    let history = fetch(&app, "/history").await.text().await.unwrap();
    for needle in [
        "/v1/history",
        "No completed events yet",
        "and ",
        "AB.ordinal(",
    ] {
        assert!(history.contains(needle), "/history has {needle:?}");
    }
}

#[tokio::test]
async fn pages_serve_html() {
    let app = TestApp::spawn(|_| {}).await;
    for path in all_pages(&app).await {
        let res = fetch(&app, &path).await;
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let ctype = res.headers()["content-type"].to_str().unwrap().to_string();
        assert!(
            ctype.starts_with("text/html"),
            "{path}: content-type {ctype:?}"
        );
        let body = res.text().await.unwrap();
        assert!(body.contains("<title>"), "{path}");
        assert_eq!(body.matches("<script>").count(), 1, "{path}: one <script>");
        assert_eq!(body.matches("<style>").count(), 1, "{path}: one <style>");
    }
}

#[tokio::test]
async fn player_page_matches_tag_in_any_case_and_rejects_unknown() {
    let app = TestApp::spawn(|_| {}).await;
    app.player("Mango").await;
    for path in ["/players/MANGO", "/players/Ma%6Ego"] {
        let res = fetch(&app, path).await;
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let ctype = res.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(ctype.starts_with("text/html"), "{path}: {ctype}");
    }
    for path in ["/players/nobody", "/players/..%2Fetc"] {
        let res = fetch(&app, path).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        let ctype = res.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(ctype.starts_with("application/json"), "{path}: {ctype}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["error"], "no such player", "{path}");
    }
}

#[tokio::test]
async fn players_pages_fetch_their_data() {
    let app = TestApp::spawn(|_| {}).await;
    app.player("Mango").await;
    let body = fetch(&app, "/players").await.text().await.unwrap();
    assert!(body.contains("/v1/players"));
    assert!(body.contains(r#"type="search""#));
    assert!(body.contains("No players yet"));
    let body = fetch(&app, "/players/Mango").await.text().await.unwrap();
    assert!(body.contains("/v1/players/"));
    assert!(body.contains("Head to head"));
    assert!(body.contains("playing since"));
    // The tag comes from the path, decoded.
    assert!(body.contains("decodeURIComponent(location.pathname"));
}

#[tokio::test]
async fn event_page_is_the_bracket_viewer() {
    let app = TestApp::spawn(|_| {}).await;
    let path = all_pages(&app).await.pop().unwrap();
    assert!(path.starts_with("/events/"), "{path}");
    let body = fetch(&app, &path).await.text().await.unwrap();
    assert!(body.contains("/v1/events/"));
    // Sets decided without play are labelled rather than shown as 0-0.
    assert!(body.contains("decided_by"));
    assert!(body.contains("Awarded"));
    // Games counted while the verifier was unavailable are mentioned under the header.
    assert!(body.contains("view.unverified_games"));
    assert!(body.contains("counted without engine verification"));
    // The event id comes from the path, not a hash.
    assert!(body.contains("location.pathname"));
}

#[tokio::test]
async fn bracket_page_has_a_vertical_layout() {
    let app = TestApp::spawn(|_| {}).await;
    let id = app.create_event(Format::SingleElim).await;
    let page = fetch(&app, &format!("/events/{id}"))
        .await
        .text()
        .await
        .unwrap();
    for s in [
        "(orientation: portrait)",
        r#"[data-layout="vertical"]"#,
        ">Horizontal<",
        ">Vertical<",
        ">Jump to live<",
        "aria-pressed",
        r#"try { return localStorage.getItem(LAYOUT_KEY); } catch"#,
        r#"try { localStorage.setItem(LAYOUT_KEY, value); } catch"#,
        r#"behavior: "auto""#,
        // A click wins for the visit even where storage refused to remember it.
        "chosen || storedLayout() ||",
    ] {
        assert!(page.contains(s), "{s} in the bracket page");
    }
    // Storage is touched only inside the two guarded functions.
    assert_eq!(page.matches("localStorage").count(), 2);
    // Everything the vertical layout shows is as wide as the page, never wider.
    assert!(page
        .contains(r#".board[data-layout="vertical"] .bracket { display: block; width: auto; }"#));
}

#[tokio::test]
async fn pages_have_no_external_urls() {
    let app = TestApp::spawn(|_| {}).await;
    for path in all_pages(&app).await {
        let res = fetch(&app, &path).await;
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let body = res.text().await.unwrap();
        assert!(!body.contains("http://"), "{path} references http://");
        assert!(!body.contains("https://"), "{path} references https://");
    }
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
    assert_eq!(cfg.stream_url, None);
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
async fn pages_have_security_headers() {
    use sha2::{Digest, Sha256};
    let app = TestApp::spawn(|_| {}).await;
    for path in all_pages(&app).await {
        let res = fetch(&app, &path).await;
        let header = |name: &str| res.headers()[name].to_str().unwrap().to_owned();
        assert_eq!(header("x-content-type-options"), "nosniff", "{path}");
        let csp = header("content-security-policy");
        for directive in [
            "default-src 'none'",
            "connect-src 'self'",
            "img-src data:",
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(csp.contains(directive), "{path}: {directive} in {csp}");
        }
        // Fonts come from this server and nowhere else.
        assert!(csp.contains("font-src 'self';"), "{path}: {csp}");
        assert!(!csp.contains('*'), "{path}: no wildcard: {csp}");
        assert!(!csp.contains("unsafe"), "{path}: {csp}");
        assert!(!csp.contains("http"), "{path}: no external origins: {csp}");
        let page = res.text().await.unwrap();
        for (tag, directive) in [("script", "script-src"), ("style", "style-src")] {
            let hash = base64(&Sha256::digest(inline_block(&page, tag).as_bytes()));
            let expected = format!("{directive} 'sha256-{hash}'");
            assert!(csp.contains(&expected), "{path}: {expected} in {csp}");
        }
    }
}

#[tokio::test]
async fn stream_page_without_a_stream() {
    let app = TestApp::spawn(|_| {}).await;
    let info: serde_json::Value = fetch(&app, "/v1/stream").await.json().await.unwrap();
    assert_eq!(info, serde_json::json!({ "url": null }));

    let res = fetch(&app, "/stream").await;
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(!csp.contains("frame-src"), "{csp}");
    let body = res.text().await.unwrap();
    for needle in [
        "/v1/stream",
        "No stream is configured",
        "Now playing",
        r#"allow="autoplay; fullscreen""#,
        r#"referrerpolicy="no-referrer""#,
        r#"title="Live stream""#,
        r#"rel="noopener""#,
        "Nothing is being played right now.",
    ] {
        assert!(body.contains(needle), "{needle:?} in the stream page");
    }
}

#[tokio::test]
async fn stream_page_frames_only_the_configured_origin() {
    let app = TestApp::spawn(|c| {
        c.stream_url = Some("https://live.example.org:8889/melee/".into());
    })
    .await;
    let info: serde_json::Value = fetch(&app, "/v1/stream").await.json().await.unwrap();
    assert_eq!(
        info,
        serde_json::json!({ "url": "https://live.example.org:8889/melee/" })
    );

    let res = fetch(&app, "/stream").await;
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        csp.contains("; frame-src https://live.example.org:8889"),
        "{csp}"
    );
    assert!(!csp.contains("/melee"), "the origin only: {csp}");
    assert_eq!(csp.matches("frame-src").count(), 1, "{csp}");
    // The rest of the policy is the one every page gets.
    for directive in [
        "default-src 'none'",
        "connect-src 'self'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(directive), "{directive} in {csp}");
    }
    let body = res.text().await.unwrap();
    assert!(
        !body.contains("http"),
        "the URL reaches the page as JSON only"
    );

    for path in all_pages(&app).await {
        if path == "/stream" {
            continue;
        }
        let res = fetch(&app, &path).await;
        let csp = res.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(!csp.contains("frame-src"), "{path}: {csp}");
        assert!(!csp.contains("live.example.org"), "{path}: {csp}");
        assert!(!csp.contains("http"), "{path}: {csp}");
    }
}

#[tokio::test]
async fn event_page_unknown_or_bad_id_is_json() {
    let app = TestApp::spawn(|_| {}).await;
    // The id is decimal digits as written, which is what the page script reads from the
    // address bar; a sign, an escape or anything else is a 400, not a page with a dead script.
    for (path, status) in [
        ("/events/99999", StatusCode::NOT_FOUND),
        ("/events/0", StatusCode::NOT_FOUND),
        ("/events/-1", StatusCode::BAD_REQUEST),
        ("/events/+1", StatusCode::BAD_REQUEST),
        ("/events/%31", StatusCode::BAD_REQUEST),
        ("/events/1%20", StatusCode::BAD_REQUEST),
        ("/events/abc", StatusCode::BAD_REQUEST),
        ("/events/99999999999999999999", StatusCode::BAD_REQUEST),
    ] {
        let res = fetch(&app, path).await;
        assert_eq!(res.status(), status, "{path}");
        let ctype = res.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(ctype.starts_with("application/json"), "{path}: {ctype}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert!(body["error"].is_string(), "{path}: {body}");
        if status == StatusCode::NOT_FOUND {
            assert_eq!(body["error"], "no such event", "{path}");
        }
    }
}

/// The landing page sends `#N` bookmarks to `/events/N`, and has its three sections, the
/// how-to-enter commands, the stream link and the empty-server notice.
#[tokio::test]
async fn home_page_has_its_sections() {
    let app = TestApp::spawn(|_| {}).await;
    let body = fetch(&app, "/").await.text().await.unwrap();
    for needle in [
        r#"location.replace("/events/" +"#,
        "/v1/history",
        "/v1/stream",
        "Live now",
        "Next up",
        "Latest results",
        "No events yet",
        "autobracket-client register",
        "Watch the stream",
    ] {
        assert!(body.contains(needle), "the landing page has {needle:?}");
    }
}

/// The "A vs B · round · score" line comes from one shared helper: both pages that list live
/// sets call `AB.liveSetLines`, and the line's format appears once per page (in the shared
/// script), never again in the page's own.
#[tokio::test]
async fn live_set_lines_are_built_once() {
    let app = TestApp::spawn(|_| {}).await;
    for path in ["/", "/stream"] {
        let body = fetch(&app, path).await.text().await.unwrap();
        assert!(body.contains("liveSetLines(view)"), "{path}");
        assert_eq!(
            body.matches(r#"" vs ""#).count(),
            1,
            "{path}: one line builder"
        );
        assert_eq!(body.matches("function liveSetLines").count(), 1, "{path}");
    }
}

/// A `/#N` visit leaves the page before it polls anything.
#[tokio::test]
async fn home_hash_redirect_returns_early() {
    let app = TestApp::spawn(|_| {}).await;
    let body = fetch(&app, "/").await.text().await.unwrap();
    assert!(body.contains("location.replace(\"/events/\" + m[1]);\n    return;\n  }"));
}

/// Every hover lift added by the public pages is neutralised under reduced motion.
#[tokio::test]
async fn reduced_motion_removes_every_hover_lift() {
    let app = TestApp::spawn(|_| {}).await;
    let css = fetch(&app, "/").await.text().await.unwrap();
    let reduced = css.split("prefers-reduced-motion: reduce").nth(1).unwrap();
    for selector in [
        ".set:hover",
        ".nav a:hover",
        "a.card:hover",
        ".layout button:hover",
        ".jump:hover",
        "option:hover",
        ".pill:hover, .pill:focus-visible { translate: none; }",
    ] {
        assert!(
            reduced.contains(selector),
            "{selector} under reduced motion"
        );
    }
}

/// A start time of 0 is "not set", like a `created_at` of 0: every page that shows an
/// event's time goes through the shared `knownTime` / `eventDate` helpers and never
/// tests `starts_at` for null or number alone.
#[tokio::test]
async fn zero_start_time_is_unknown_everywhere() {
    let app = TestApp::spawn(|_| {}).await;
    let event = app.create_event(Format::SingleElim).await;
    app.player("Mango").await;
    let site = fetch(&app, "/").await.text().await.unwrap();
    assert!(
        site.contains("function knownTime(t) {\n    return typeof t === \"number\" && t > 0;\n  }")
    );
    assert!(site.contains("function eventDate(sum) {"));
    for path in [
        "/".to_owned(),
        "/schedule".to_owned(),
        "/history".to_owned(),
        "/players/Mango".to_owned(),
        format!("/events/{event}"),
    ] {
        let body = fetch(&app, &path).await.text().await.unwrap();
        for bad in [
            "starts_at ??",
            "starts_at === null",
            "typeof ev.starts_at",
            "typeof sum.starts_at",
            "typeof summary.starts_at",
            "typeof a.starts_at",
        ] {
            assert!(!body.contains(bad), "{path} has {bad:?}");
        }
        assert!(
            body.contains("knownTime(") || body.contains("eventDate("),
            "{path} uses the shared helpers"
        );
    }
}

/// Wide screens: the landing's three sections sit side by side (two when nothing is live),
/// and row lists stop at a readable width.
#[tokio::test]
async fn wide_layouts_use_the_width() {
    let app = TestApp::spawn(|_| {}).await;
    let css = fetch(&app, "/").await.text().await.unwrap();
    let wide = css.split("@media (min-width: 75rem)").nth(1).unwrap();
    assert!(wide.contains(".home { grid-template-columns: repeat(3, minmax(0, 1fr)); }"));
    assert!(wide.contains(
        ".home:has(#live[hidden]) { grid-template-columns: repeat(2, minmax(0, 1fr)); }"
    ));
    assert!(wide.contains("#live { grid-column: auto; }"));
    assert!(css.contains(".rows { display: grid; gap: var(--sp-2); max-width: 48rem;"));
}

/// A description that ends in a full stop is not followed by a second one before "Champion".
#[tokio::test]
async fn event_meta_joins_sentences_once() {
    let app = TestApp::spawn(|_| {}).await;
    let event = app.create_event(Format::SingleElim).await;
    let body = fetch(&app, &format!("/events/{event}"))
        .await
        .text()
        .await
        .unwrap();
    assert!(body.contains("if (sum.description) text += \". \" + sum.description;"));
    assert!(body.contains("meta.append((/[.!?]$/.test(text) ? \" \" : \". \") + \"Champion: \");"));
    assert!(!body.contains("meta.append(\". Champion: \")"));
}

/// Players read plain words; the client commands sit in a collapsed block for testers.
#[tokio::test]
async fn how_to_enter_is_plain_with_commands_for_testers() {
    let app = TestApp::spawn(|_| {}).await;
    for path in ["/", "/schedule"] {
        let body = fetch(&app, path).await.text().await.unwrap();
        for needle in [
            "Sign-ups are open. Joining from inside the game is coming soon.",
            r#"el("details")"#,
            r#"el("summary", null, "For testers: join from the command line")"#,
            "Ask an organiser for it",
            "autobracket-client register --server \" + location.origin + \" --tag YOURTAG",
            "autobracket-client signup \" + eventId",
        ] {
            assert!(body.contains(needle), "{path} has {needle:?}");
        }
        // The commands are appended to the details element, not the sticker itself.
        let howto = body.split("function howToEnter").nth(1).unwrap();
        let howto = howto.split("return box;").next().unwrap();
        assert_eq!(
            howto.matches("testers.append(el(\"code\"").count(),
            2,
            "{path}"
        );
        assert!(!howto.contains("box.append(el(\"code\""), "{path}");
    }
}

/// The font files the page's stylesheet asks for, as paths.
fn font_paths(page: &str) -> Vec<&str> {
    const PREFIX: &str = "/assets/fonts/";
    page.match_indices(PREFIX)
        .map(|(at, _)| {
            let rest = &page[at..];
            let end = rest.find(['"', '\'', ')', ' ']).unwrap();
            &rest[..end]
        })
        .collect()
}

#[tokio::test]
async fn page_fonts_are_served() {
    let app = TestApp::spawn(|_| {}).await;
    let page = fetch(&app, "/").await.text().await.unwrap();
    let paths = font_paths(&page);
    assert_eq!(paths.len(), 2, "one file per typeface: {paths:?}");
    for path in paths {
        let res = reqwest::get(format!("{}{path}", app.url)).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let header = |name: &str| res.headers()[name].to_str().unwrap().to_owned();
        assert_eq!(header("content-type"), "font/woff2", "{path}");
        assert_eq!(header("x-content-type-options"), "nosniff", "{path}");
        let cache = header("cache-control");
        assert!(cache.contains("immutable"), "{path}: {cache}");
        assert!(cache.contains("max-age=31536000"), "{path}: {cache}");
        let body = res.bytes().await.unwrap();
        assert!(body.starts_with(b"wOF2"), "{path} is a WOFF2 file");
    }
}

#[tokio::test]
async fn unknown_font_is_json_404() {
    let app = TestApp::spawn(|_| {}).await;
    for name in ["nope.woff2", "README.md", "..%2Fshell.html"] {
        let res = reqwest::get(format!("{}/assets/fonts/{name}", app.url))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{name}");
        let ctype = res.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(ctype.starts_with("application/json"), "{name}: {ctype}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert!(body["error"].is_string(), "{name}: {body}");
    }
}

/// The round names in `docs/api.md` ("Round names") are the ones the shared script uses.
///
/// This compares text, it does not run the script: for each side, the names the doc lists
/// counting back from the last round must appear in the script as that side's list, in the
/// same order, and the numbered and grand-final names must appear as written. It proves the
/// two cannot drift apart in wording or order; it does not prove the script picks the right
/// entry for a given round.
#[test]
fn round_names_match_the_doc() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(root.join("../../docs/api.md")).unwrap();
    let page = std::fs::read_to_string(root.join("assets/site.js")).unwrap();
    let section = doc.split_once("## 7. Round names").unwrap().1;
    let rows: Vec<[&str; 3]> = section
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            match cells[..] {
                [_, side, round, name, _] if side != "Side" && !side.starts_with('-') => {
                    Some([side, round, name])
                }
                _ => None,
            }
        })
        .collect();
    assert_eq!(rows.len(), 13, "{rows:?}");

    for (side, key) in [
        ("Winners", "Winners"),
        ("Losers", "Losers"),
        ("Single elimination", "Single"),
    ] {
        // "last", "last − 1", … in the order the doc gives them.
        let named: Vec<String> = rows
            .iter()
            .filter(|[s, round, _]| *s == side && round.starts_with("last"))
            .map(|[_, _, name]| format!("\"{name}\""))
            .collect();
        let list = format!("{key}: [{}],", named.join(", "));
        assert!(page.contains(&list), "the script has `{list}`");
        // The numbered rounds: "Winners round N" is built as "Winners round " + round.
        let [_, _, numbered] = rows
            .iter()
            .find(|[s, round, _]| *s == side && *round == "earlier")
            .unwrap();
        let stem = format!("\"{} \"", numbered.strip_suffix(" N").unwrap());
        assert!(page.contains(&stem), "the script has {stem}");
    }
    for [_, _, name] in rows.iter().filter(|[s, _, _]| *s == "Grand final") {
        assert!(
            page.contains(&format!("\"{name}\"")),
            "the script has {name:?}"
        );
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
    // Exact: a route added to the router must be added to the table too.
    assert_eq!(seen, 38, "routes in the table");
}

/// The final-review fixes that live in page scripts, pinned as text.
#[tokio::test]
async fn final_review_page_fixes() {
    let app = TestApp::spawn(|_| {}).await;
    let event = app.create_event(Format::SingleElim).await;

    // The landing keeps the shell's title and asks for one history entry; its live set lines
    // link to the bracket as the stream page's do.
    let home = fetch(&app, "/").await.text().await.unwrap();
    assert!(home.contains("<title>Autobracket</title>"));
    assert!(
        !home.contains("document.title"),
        "the landing keeps the shell's title"
    );
    assert!(home.contains(r#"getJson("/v1/history?limit=1")"#));
    let live_card = home.split("function liveCard").nth(1).unwrap();
    let live_card = live_card.split("function renderLive").next().unwrap();
    assert!(live_card.contains("a.href = eventHref(ev.id);"));

    // The Players nav item is current on a profile page.
    assert!(home.contains(
        r#"else if (href !== "/" && here.startsWith(href + "/")) a.setAttribute("aria-current", "true");"#
    ));
    assert!(home.contains(".nav a[aria-current] {"));

    // Tags on the bracket page link to the player's profile; the id comes from the path as
    // the server accepts it: decimal digits only.
    let bracket = fetch(&app, &format!("/events/{event}"))
        .await
        .text()
        .await
        .unwrap();
    assert!(bracket.contains("function tagLink(tag)"));
    assert!(bracket.contains("a.href = AB.playerHref(tag);"));
    assert_eq!(
        bracket.matches("tagLink(").count(),
        4,
        "defined once; set rows, entrants, champion"
    );
    assert!(bracket.contains(r#"/^\/events\/(\d+)$/.exec(location.pathname)"#));

    // The stream page drops a running event that vanished and keeps the rest.
    let stream = fetch(&app, "/stream").await.text().await.unwrap();
    assert!(stream.contains("if (err && err.status === 404) return null;"));

    // "and N more" counts the placings not shown, not entrants minus eight.
    let history = fetch(&app, "/history").await.text().await.unwrap();
    assert!(history.contains("const hidden = result.placings.length - rows.childElementCount;"));
    assert!(!history.contains("ev.entrants - PLACES_SHOWN"));
}
