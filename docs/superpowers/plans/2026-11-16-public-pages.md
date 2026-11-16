# Public Pages (Phase 2a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A public, read-only web face for an autobracket server: landing, bracket per event, schedule, history, players, player profile and stream pages, with the JSON they need.

**Architecture:** Pages are assembled once at startup from a shared shell, one shared stylesheet and one shared script plus a per-page body and script, and served as self-contained HTML with a hash-pinned CSP per page (the existing `view.rs` mechanism, generalised). Results (placings, records, head-to-head) are computed from the stored brackets and set scores by pure functions in `bracket` and a new `results` module, exposed through three public JSON routes. Events gain an optional start time and description via one migration and a signed admin route.

**Tech Stack:** Rust stable, axum 0.8, sqlx 0.9 (SQLite), serde, clap 4; plain HTML/CSS/JS (no framework, no build step).

**Spec:** `docs/superpowers/specs/2026-11-16-public-pages.md`

## Global Constraints

- Licence AGPL-3.0-or-later; no new dependencies.
- No page's HTML may contain `http://` or `https://`; the only external origin a browser may contact is `stream_url`'s, and only from `/stream` via `frame-src`.
- Every page's CSP: `default-src 'none'; script-src 'sha256-<page script>'; style-src 'sha256-<page style>'; connect-src 'self'; font-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`; each page has exactly one `<script>` and one `<style>`.
- All data reaches the DOM via `textContent` (the `el()` helper); never `innerHTML` with data; tags in URLs go through `encodeURIComponent`.
- Same Envoy Party-pack look as `assets/bracket.html` today; responsive to 400 px (`@media (max-width: 40rem)`); `prefers-reduced-motion: reduce` removes every animation and transition.
- Limits: `description` at most 280 printable characters (`check_text`), empty/whitespace stored as NULL; `starts_at` in `0..=4102444800`.
- Every route added to `docs/api.md`'s table (the `documented_routes_exist` test); every new body type has a `<!-- checked: Type -->` example (the `api_doc` test).
- Tags match in any case everywhere (`COLLATE NOCASE`, prefer exact spelling then `tag_case_duplicate = 0`, as `remove_entrant` does).
- Commit after every task; trailer per the session's attribution rule.

## Review Focus

1. **A description with bidirectional controls, a newline, or 281 characters** — expected: 400 naming `description`; a 280-character one with accented letters is stored as sent. (Task 1)
2. **History and records for an event decided by awards (0-0 sets)** — expected: full placings, set counts include the awarded sets, game counts stay 0; a running event never appears in history. (Task 3)
3. **`/events/0`, `/events/99999`, `/events/-1`, `/events/abc`** — expected: 404 or 400 JSON `{"error": …}`, never a 500 or an HTML page for a missing event. (Task 4)
4. **`/players/MANGO`, `/players/Ma%6Ego`, `/players/..%2Fetc`** — expected: the first two serve the profile page (the JSON shows the canonical tag `Mango`), the third is a 404 JSON. (Task 7)
5. **`stream_url` with a port and a path** — expected: `/stream`'s `frame-src` is the origin only (`https://live.example.org:8889`), `/v1/stream` returns the full URL, no other page's CSP mentions it. (Task 8)
6. **A portrait phone (400 px) opening a double-elimination bracket** — expected: rounds stacked, no horizontal page scroll, the layout toggle remembered across reloads even where storage throws, "Jump to live" present while a set is ready. (Task 5)

---

## File Structure

```
crates/bracket/src/lib.rs                       + Bracket::placings
crates/bracket/tests/placings.rs                new
crates/proto/src/types.rs                       + fields, EventScheduleReq, Placing, EventResult, PlayerSummary,
                                                  PlayerEvent, HeadToHead, PlayerProfile, StreamInfo
crates/proto/tests/api_doc.rs                   + type arms and list entries
crates/server/migrations/0005_schedule.sql      new
crates/server/src/config.rs                     + stream_url, stream_origin()
crates/server/src/admin_cli.rs                  + flags, `event schedule`
crates/server/src/lib.rs                        + `pub mod results;`
crates/server/src/results.rs                    new: LoadedEvent, load_events, placings, player_summaries, player_profile
crates/server/src/routes/mod.rs                 + routes, MAX_EVENT_DESCRIPTION_CHARS
crates/server/src/routes/events.rs              summary() with the new fields
crates/server/src/routes/admin.rs               + schedule()
crates/server/src/routes/results.rs             new: history, players, player, stream
crates/server/src/routes/view.rs                page assembly and handlers
crates/server/assets/shell.html                 new (replaces bracket.html, which is deleted)
crates/server/assets/site.css, site.js          new
crates/server/assets/pages/{home,event,schedule,history,players,player,stream}.{html,js}
crates/server/tests/{migrations,events,results,view}.rs
docs/api.md, README.md, deploy/autobracket.example.toml
```

---

### Task 1: Event start time and description

**Files:**
- Create: `crates/server/migrations/0005_schedule.sql`
- Modify: `crates/proto/src/types.rs` (`EventCreateReq`, `EventSummary`, new `EventScheduleReq`)
- Modify: `crates/server/src/routes/mod.rs` (route, constant), `routes/events.rs` (`list`, `event_view`, `summary`), `routes/admin.rs` (`create_event`, new `schedule`)
- Modify: `crates/server/src/admin_cli.rs`, `crates/server/src/testkit.rs` (`create_event` builds the new fields as `None`)
- Modify: `docs/api.md`, `crates/proto/tests/api_doc.rs`
- Test: `crates/server/tests/migrations.rs`, `crates/server/tests/events.rs`

**Interfaces:**
- Produces: `EventSummary { id, name, state, entrants, starts_at: Option<i64>, description: Option<String>, created_at: i64 }`; `EventCreateReq` + `starts_at: Option<i64>`, `description: Option<String>` (both `#[serde(default)]`); `EventScheduleReq { #[serde(default)] starts_at: Option<i64>, #[serde(default)] description: Option<String> }`; `routes::MAX_EVENT_DESCRIPTION_CHARS: usize = 280`; `routes::MAX_STARTS_AT: i64 = 4_102_444_800`; `events::summary(id: i64, name: String, state: String, entrants: i64, starts_at: Option<i64>, description: Option<String>, created_at: i64) -> Result<EventSummary, ApiError>`; `pub(super) fn check_schedule(starts_at: Option<i64>, description: Option<String>) -> Result<Option<String>, ApiError>` in `routes/mod.rs` (validates both, returns the description to store: `None` for empty/whitespace).

- [ ] **Step 1: Write the failing migration test** in `crates/server/tests/migrations.rs`, after the 0003 test and in its style (`old.migrations[..4]`, `assert_eq!(old.migrations[3].version, 4)`), inserting one event row with the 0004 columns, then opening with `db::open`:

```rust
#[tokio::test]
async fn migration_0005_applies_to_a_populated_0004_database() {
    // … build at 0001-0004, insert events (id 1, 'Old', 'single_elim', '{}', 'signup', NULL, 1) …
    let pool = db::open(&path).await.unwrap();
    let (starts_at, description): (Option<i64>, Option<String>) =
        sqlx::query_as("SELECT starts_at, description FROM events").fetch_one(&pool).await.unwrap();
    assert_eq!((starts_at, description), (None, None));
    pool.close().await;
    db::open(&path).await.unwrap(); // a no-op the second time
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p autobracket-server --test migrations migration_0005 -- --nocapture`
Expected: FAIL (`no such column: starts_at`).

- [ ] **Step 3: Write `0005_schedule.sql`**: a header comment in the style of 0002-0004 ("Public pages (phase 2a). Columns only."), then `ALTER TABLE events ADD COLUMN starts_at INTEGER NULL;` and `ALTER TABLE events ADD COLUMN description TEXT NULL;`, each with a one-line comment giving its meaning and limit (spec, "Data model").

- [ ] **Step 4: Run the migration test to verify it passes**

Run: `cargo test -p autobracket-server --test migrations`
Expected: PASS (all three).

- [ ] **Step 5: Write the failing API tests** in `crates/server/tests/events.rs` (helpers `create_event`, `start` exist there; add `schedule(app, event, body: &Value) -> reqwest::Response` posting as `app.as_to()` to `/v1/admin/events/{event}/schedule`):

```rust
#[tokio::test]
async fn create_carries_schedule_fields() {
    let app = TestApp::spawn(|_| {}).await;
    let before = autobracket_server::db::now_unix();
    let res = app.as_to().post_json("/v1/admin/events", &json!({
        "name": "Friday", "format": "SingleElim", "ruleset": null,
        "starts_at": 1795000000, "description": "Bo3, Friday 8pm" })).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let s: EventSummary = res.json().await.unwrap();
    assert_eq!((s.starts_at, s.description.as_deref()), (Some(1795000000), Some("Bo3, Friday 8pm")));
    assert!(s.created_at >= before && s.created_at <= autobracket_server::db::now_unix() + 1);
    let list: Vec<EventSummary> = reqwest::get(format!("{}/v1/events", app.url)).await.unwrap().json().await.unwrap();
    assert_eq!(list[0].starts_at, Some(1795000000));
    let view: EventView = reqwest::get(format!("{}/v1/events/{}", app.url, s.id)).await.unwrap().json().await.unwrap();
    assert_eq!(view.summary.description.as_deref(), Some("Bo3, Friday 8pm"));
}

#[tokio::test]
async fn schedule_route_replaces_both_fields() {
    // create with neither; POST {"starts_at": 1795000000, "description": "x"} -> 204 and both set;
    // POST {} -> 204 and both None; POST {"description": "   "} -> stored as None;
    // a complete-state event is not needed: state does not matter, assert it works after `start` too.
}

#[tokio::test]
async fn schedule_rejects_bad_values() {
    // starts_at -1 -> 400 body mentions "starts_at"; starts_at 4102444801 -> 400;
    // description of 281 'a' -> 400 mentioning "description"; "a\nb" -> 400; "a\u{202e}b" -> 400;
    // 280 chars with 'é' -> 204; unknown event 999 -> 404; a registered player's key -> 403.
    // The same four 400s on POST /v1/admin/events (create).
}
```

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test -p autobracket-server --test events schedule create_carries`
Expected: FAIL (unknown fields / 404 `no such route`).

- [ ] **Step 7: Implement.** In `types.rs` add the fields and `EventScheduleReq`; update the unit test `event_create_req_best_of_is_optional` to also assert `starts_at == None`. In `routes/mod.rs` add the constants, `check_schedule`, and the route `.route("/v1/admin/events/{id}/schedule", post(admin::schedule))`. In `events.rs`: `list` selects `e.starts_at, e.description, e.created_at`; `event_view` selects them too; `summary` takes the new parameters. In `admin.rs`: `create_event` validates via `check_schedule` and inserts the two columns; `schedule(State, path: Result<Path<i64>, PathRejection>, to: To) -> Result<StatusCode, ApiError>` parses `EventScheduleReq`, validates, `UPDATE events SET starts_at = ?, description = ? WHERE id = ?`, 404 when `rows_affected() == 0`, logs `event`, `starts_at`, `to`, answers 204. Update `testkit::create_event` and `tests/events.rs::create_req` for the new fields.

- [ ] **Step 8: Admin CLI.** `EventCmd::Create` gains `#[arg(long, value_name = "UNIX")] starts_at: Option<i64>` (help: "When the event starts, unix seconds, e.g. `date -d '2026-11-20 20:00' +%s`") and `#[arg(long)] description: Option<String>`; new `EventCmd::Schedule { id: i64, #[arg(long, value_name = "UNIX")] starts_at: Option<i64>, #[arg(long)] description: Option<String> }` (doc: "Set or clear when an event starts and what it is about; an omitted flag clears that field") posting `EventScheduleReq` to `/v1/admin/events/{id}/schedule`. Add a unit test `schedule_request_clears_omitted_fields` in `admin_cli.rs`'s tests asserting `request(AdminCmd::Event(EventCmd::Schedule { id: 3, starts_at: None, description: Some("x".into()) }))` has path `/v1/admin/events/3/schedule` and body `{"starts_at":null,"description":"x"}`.

- [ ] **Step 9: Document.** `docs/api.md`: route row `POST /v1/admin/events/{id}/schedule | organiser | EventScheduleReq | 204, empty; replaces both fields, an omitted or null field clears it | 400; 404`; Limits table rows for `description` (at most 280 printable characters) and `starts_at` (unix seconds, 0 to 4102444800); `EventCreateReq` table rows and example gaining `"starts_at": 1795000000, "description": "Bo3, Friday 8pm"`; `EventSummary` text and example gaining the three fields (`"created_at": 1763200000`); the `EventView` example's summary likewise; a new `### EventScheduleReq` section with a checked example. `api_doc.rs`: arm `"EventScheduleReq" => round_trip::<EventScheduleReq>(&json, p)` and the list entry. README "Organiser quick start": show `--starts-at` and the `date -d` hint, and mention `event schedule`.

- [ ] **Step 10: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS (including `documented_routes_exist`, now parsing 22+ rows, and `api_doc`).

- [ ] **Step 11: Commit**

```bash
git add crates/server/migrations/0005_schedule.sql crates/proto crates/server/src crates/server/tests docs/api.md README.md
git commit -m "events: optional start time and description, schedule route, CLI flags"
```

---

### Task 2: Bracket placings

**Files:**
- Modify: `crates/bracket/src/lib.rs`
- Test: `crates/bracket/tests/placings.rs`

**Interfaces:**
- Produces: `impl Bracket { pub fn placings(&self) -> Option<Vec<u32>> }` — `None` unless `is_complete()`; else index = entrant id, value = 1-based place (spec, "Data model": champion 1st; others ranked by the highest `(side, round)` set they lost with Winners < Losers < GrandFinal; place = 1 + count of entrants ranked strictly higher; equal ranks share a place).

- [ ] **Step 1: Write the failing tests** in `crates/bracket/tests/placings.rs`, with a helper `fn play_out(b: &mut Bracket, pick: impl Fn(&Set) -> u8)` that reports `pick(set)` for each ready set until complete, and `fn counts(p: &[u32]) -> BTreeMap<u32, usize>`:

```rust
#[test] fn incomplete_bracket_has_no_placings() { let b = build(Format::SingleElim, 4).unwrap(); assert_eq!(b.placings(), None); }
#[test] fn single_elim_4_is_1_2_3_3() { /* slot 0 always wins; counts == {1:1, 2:1, 3:2}; placings[champion] == 1 */ }
#[test] fn single_elim_8_is_1_2_3_3_5x4() { /* counts == {1:1, 2:1, 3:2, 5:4} */ }
#[test] fn single_elim_3_with_a_bye_is_1_2_3() { /* counts == {1:1, 2:1, 3:1}, length 3 */ }
#[test] fn double_elim_4_is_1_2_3_4() { /* counts == {1:1, 2:1, 3:1, 4:1} */ }
#[test] fn double_elim_8_is_1_2_3_4_5_5_7_7() { /* counts == {1:1, 2:1, 3:1, 4:1, 5:2, 7:2} */ }
#[test] fn grand_final_reset_loser_is_second() {
    // DE 4: slot 0 wins every set except grand final round 1 (slot 1 wins, so the reset is played),
    // then slot 0 wins the reset. The reset's slot 0 entrant is 1st, its slot 1 entrant 2nd.
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bracket --test placings`
Expected: FAIL to compile (`no method named placings`).

- [ ] **Step 3: Implement `Bracket::placings`** in `lib.rs` next to `champion`. Rank type `(u8, u8)` from `(side as order, round)`; for each `Done` set with two entrants record the loser's rank as the max seen; champion gets `(u8::MAX, u8::MAX)`; place = 1 + number of entrants with a strictly greater rank. Document the rule in the doc comment with the two example sequences.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bracket`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/bracket
git commit -m "bracket: placings from a complete bracket, ties share a place"
```

---

### Task 3: Results API: history, players, player profile

**Files:**
- Create: `crates/server/src/results.rs`, `crates/server/src/routes/results.rs`
- Modify: `crates/server/src/lib.rs` (`pub mod results;`), `crates/server/src/routes/mod.rs` (routes), `crates/proto/src/types.rs`, `crates/proto/tests/api_doc.rs`, `docs/api.md`
- Test: `crates/server/tests/results.rs`

**Interfaces:**
- Consumes: `Bracket::placings`, `Bracket::was_played`, `Bracket::champion` (Task 2); `events::summary` (Task 1).
- Produces (proto): `Placing { place: u32, tag: String }`, `EventResult { summary: EventSummary, placings: Vec<Placing> }`, `PlayerSummary { tag: String, events: u32, titles: u32, sets: [u32; 2] }`, `PlayerEvent { event: EventSummary, seed: u32, place: Option<u32>, sets: [u32; 2], games: [u32; 2] }`, `HeadToHead { tag: String, sets: [u32; 2], games: [u32; 2] }`, `PlayerProfile { tag: String, since: i64, events: Vec<PlayerEvent>, head_to_head: Vec<HeadToHead> }`.
- Produces (server, `results.rs`): `pub struct LoadedEvent { pub summary: EventSummary, pub tags: Vec<String>, pub bracket: Option<Bracket>, pub scores: HashMap<u32, [u8; 2]> }`; `pub async fn load_events(db: &SqlitePool) -> Result<Vec<LoadedEvent>, ApiError>` (one deferred transaction: all events, all entrants joined to tags ordered by seed, all sets; oldest event first); `pub fn placings(ev: &LoadedEvent) -> Vec<Placing>` (empty unless complete; ordered by place then seed); `pub fn player_summaries(players: &[(String)], events: &[LoadedEvent]) -> Vec<PlayerSummary>` (every registered tag, ordered by tag ignoring case); `pub fn player_profile(tag: &str, since: i64, events: &[LoadedEvent]) -> PlayerProfile` (events newest first; head-to-head by sets played desc, then tag). Tag comparison inside uses the canonical tag strings from `tags` (exact), since `tags` come from the same `players` rows.
- Produces (routes): `GET /v1/history`, `GET /v1/players`, `GET /v1/players/{tag}` (404 `no such player`; lookup `SELECT tag, created_at FROM players WHERE tag = ? COLLATE NOCASE ORDER BY tag = ? DESC, tag_case_duplicate LIMIT 1`).

- [ ] **Step 1: Write the failing tests** in `crates/server/tests/results.rs`. Helper `async fn win_set(app: &TestApp, event: i64, set: u32, players: &[TestPlayer], winner_slot: u8)`: GET `/v1/events/{event}` as `EventView`, take `bracket.sets[set].slots` → entrant ids → `view.tags` → the `TestPlayer` with that tag; call `testkit::play_game(app, event, set, n, host, guest, winner_slot + 1)` for `n` in 1..=2 (default best of 3). Helper `async fn played_out_se4(app) -> (i64, Vec<TestPlayer>)`: players `Mango`, `Armada`, `Zain`, `Cody` via `app.player`, `app.started_event(Format::SingleElim, &refs)`, then for every `Ready` set (re-read the view each round) `win_set(.., 0)` until `summary.state == "complete"`.

```rust
#[tokio::test] async fn empty_server_has_empty_history_and_players() { /* GET /v1/history == [] ; GET /v1/players == [] */ }

#[tokio::test]
async fn history_lists_complete_events_with_placings() {
    let app = TestApp::spawn(|_| {}).await;
    let running = /* a second, started but unfinished 2-player event */;
    let (event, _) = played_out_se4(&app).await;
    let h: Vec<EventResult> = get_json(&app, "/v1/history").await;
    assert_eq!(h.len(), 1, "a running event is not history");
    assert_eq!(h[0].summary.id, event);
    let places: Vec<u32> = h[0].placings.iter().map(|p| p.place).collect();
    assert_eq!(places, vec![1, 2, 3, 3]);
    assert_eq!(h[0].placings[0].tag, "Mango"); // seed 1 won every set as slot 0
}

#[tokio::test]
async fn players_index_counts_events_titles_and_sets() {
    // after played_out_se4 plus a registered "Idle" who never entered:
    // 5 players ordered Armada, Cody, Idle, Mango, Zain; Mango: events 1, titles 1, sets [2, 0];
    // Idle: events 0, titles 0, sets [0, 0]; the two 3rd-placers: sets [0, 1].
}

#[tokio::test]
async fn player_profile_has_events_and_head_to_head() {
    // GET /v1/players/mango (lower case) -> 200, tag == "Mango", since within a minute of now,
    // events.len() == 1, events[0].place == Some(1), events[0].seed == 0, games == [4, 0], sets == [2, 0];
    // head_to_head has 2 opponents, each sets [1, 0], games [2, 0], ordered by tag;
    // GET /v1/players/nobody -> 404 JSON {"error": "no such player"}.
}

#[tokio::test]
async fn awarded_sets_count_as_sets_without_games() {
    // SE 4 started; award every set to slot 0 with lifecycle::award_set(&app.state, event, set, 0)
    // (loop over ready sets until complete). History shows places [1, 2, 3, 3]; the champion's
    // profile has sets [2, 0] and games [0, 0].
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p autobracket-server --test results`
Expected: FAIL to compile (missing proto types) or 404 `no such route`.

- [ ] **Step 3: Add the proto types** to `types.rs` with `Serialize, Deserialize, Clone, Debug, PartialEq` as the others.

- [ ] **Step 4: Implement `results.rs`** (pure functions over `LoadedEvent`; `load_events` is the only query) and `routes/results.rs` (`history`, `players`, `player` handlers: load once, map, 404 for an unknown tag). Register the routes in `routes/mod.rs` after `/v1/events/{id}`. Set-record rule: for each bracket set with `was_played(id)`, winner = `slots[winner]`, loser = the other; games from `scores[&id]` (`[0, 0]` if absent).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p autobracket-server --test results`
Expected: PASS.

- [ ] **Step 6: Document.** `docs/api.md`: three route rows (public, success shapes and ordering as in the spec table); sections `### EventResult`, `### PlayerSummary`, `### PlayerProfile` with the counting rules (spec, "Counting rules") and a checked example each (the `PlayerProfile` example includes one `PlayerEvent` and one `HeadToHead`); the signing section's list of public routes gains them. `api_doc.rs`: arms and list entries for `EventResult`, `PlayerSummary`, `PlayerProfile`.

- [ ] **Step 7: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/proto crates/server docs/api.md
git commit -m "api: history, players index and player profile from bracket results"
```

---

### Task 4: Page shell and the bracket page at /events/{id}

**Files:**
- Create: `crates/server/assets/shell.html`, `assets/site.css`, `assets/site.js`, `assets/pages/event.html`, `assets/pages/event.js`, `assets/pages/home.html`, `assets/pages/home.js` (a stub this task; Task 9 replaces it)
- Delete: `crates/server/assets/bracket.html`
- Modify: `crates/server/src/routes/view.rs`, `routes/mod.rs`, `docs/api.md`
- Test: `crates/server/tests/view.rs`

**Interfaces:**
- Produces (`view.rs`): `struct Page { html: String, csp: String }`; `fn assemble(title: &str, body: &str, script: &str) -> Page` (fills `shell.html`'s `{{title}}`, `{{style}}` = `site.css`, `{{body}}`, `{{script}}` = `site.js` + `"\n"` + page script; CSP = the base directives with this page's two hashes); `static PAGES: LazyLock<HashMap<&'static str, Page>>` built from `const SOURCES: &[(&str, &str, &str, &str)]` = (name, title, `include_str!` html, `include_str!` js); `fn page(name: &'static str) -> Response` (nosniff + CSP + `Html`); `pub async fn home()`, `pub async fn event(State, Path<i64>) -> Result<Response, ApiError>` (404 `no such event` when `SELECT 1 FROM events WHERE id = ?` is empty); `fn inline_hash(page: &str, tag: &str) -> String`. Fonts unchanged.
- Produces (`shell.html`): `<!doctype html>…<title>{{title}}</title>…<style>{{style}}</style>…<body>` with the `.scene`, the `<svg class="defs">` symbols and the two `<template>`s moved from `bracket.html`, then `<header class="top"><div><h1 id="title">{{title}}</h1><p class="meta" id="meta"></p></div><div id="head-side"></div></header><nav class="nav" aria-label="Site"><a href="/">Home</a><a href="/schedule">Schedule</a><a href="/history">History</a><a href="/players">Players</a><a href="/stream">Stream</a></nav>{{body}}<script>{{script}}</script></body></html>`.
- Produces (`site.js`, global `const AB = (function () { … return {…}; })();`): `el(tag, cls, text)`, `icon(name)`, `getJson(path)`, `setNotice(id, text)`, `STATE_LABEL`, `FORMAT_LABEL`, `ROUND_NAMES`, `roundName(side, round, last, format)`, `sideRounds(bracket, side)`, `slotEntrant`, `doneWinner`, `isHidden`, `fmtWhen(unix) -> { abs: string, rel: string, iso: string }` (`toLocaleString(undefined, { dateStyle: "full", timeStyle: "short" })`; rel: "in N days/hours/minutes", "today at HH:MM", "N hours ago"), `fmtDate(unix)` (`dateStyle: "medium"`), `ordinal(n)` ("1st", "2nd", "3rd", "4th", "11th", "21st"), `eventHref(id)`, `playerHref(tag)` (`"/players/" + encodeURIComponent(tag)`), `poll(fn, ms)` (runs `fn` now, every `ms`, and on `visibilitychange` to visible; one in flight at a time), `markNav()` (sets `aria-current="page"` on the nav link whose `href` equals `location.pathname`, or `/events` prefix → none), `howToEnter(eventId)` (a `.howto` sticker with two `<code>` lines: `autobracket-client register --server <location.origin> --tag YOURTAG` and `autobracket-client signup <eventId>`, built with `el`).
- Produces (`site.css`): the whole current `<style>` of `bracket.html` plus `.nav` (a row of pill stickers, wrapping; `[aria-current="page"]` in the sunny accent), `.cards`/`.card` (sticker list, `grid-template-columns: repeat(auto-fill, minmax(min(18rem, 100%), 1fr))`), `.rows`/`.row-line` (list rows for tables: `grid-template-columns: minmax(0, 1fr) repeat(var(--cols, 3), auto)`), `.podium`, `.howto code` (block, ink on cream, `overflow-x: auto`), `.player` (the 16:9 frame: `aspect-ratio: 16 / 9; width: 100%`), `.filter` (the search input styled as the select is). Reduced-motion block stays last and global.

- [ ] **Step 1: Rewrite the tests** in `crates/server/tests/view.rs`. Replace `fetch_root` with `async fn fetch(app: &TestApp, path: &str) -> reqwest::Response` and add `async fn all_pages(app: &TestApp) -> Vec<String>`: creates an event via `app.create_event(Format::SingleElim)`, registers `app.player("Mango")`, returns `["/", "/events/{id}"]` (later tasks append). Tests (keep the fonts, 404-font, config and documented-routes tests as they are, pointed at `fetch`):

```rust
#[tokio::test] async fn pages_serve_html() { /* every page: 200, text/html, body contains "<title>", exactly one "<script>" and one "<style>" */ }
#[tokio::test] async fn event_page_is_the_bracket_viewer() { /* /events/{id} body contains "/v1/events/", "decided_by", "Awarded", "view.unverified_games", "counted without engine verification", "location.pathname" */ }
#[tokio::test] async fn pages_have_no_external_urls() { /* every page: no "http://" nor "https://" */ }
#[tokio::test] async fn pages_have_security_headers() { /* the old assertions, for every page, including the two hashes */ }
#[tokio::test] async fn event_page_unknown_or_bad_id_is_json() {
    // /events/99999 -> 404 {"error":"no such event"}; /events/0 -> 404; /events/-1 -> 404; /events/abc -> 400 JSON
}
#[tokio::test] async fn home_redirects_old_hash_bookmarks() { /* "/" body contains `location.replace("/events/" +` */ }
#[test] fn round_names_match_the_doc() { /* unchanged, reading assets/site.js */ }
```
Delete `root_serves_html` and `no_events_view_removes_the_unverified_line` (spec, "Security and quality constraints").

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p autobracket-server --test view`
Expected: FAIL (`/events/1` is 404 `no such route`; site.js missing).

- [ ] **Step 3: Split `bracket.html`.** Move its `<style>` into `site.css`; its scene, defs and templates and header into `shell.html`; its `#unverified`, `#status`, `#strip`, `#board` and the footer into `pages/event.html` (the picker `<label class="pick">` too, inside `#head-side` via the page script appending it, or keep it in `event.html` and move it with `AB`); its script into `site.js` (the shared helpers listed above) and `pages/event.js` (the rest, using `AB.*`). Changes in `event.js`: the event id comes from `/^\/events\/(\d+)$/.exec(location.pathname)`; the picker's `change` does `location.href = AB.eventHref(picker.value)`; `renderView` appends `AB.fmtWhen(sum.starts_at).abs` and `sum.description` to the meta line when present; a 404 from `/v1/events/{id}` stops polling and shows the notice "This event no longer exists"; `renderNoEvents` is removed. Delete `bracket.html`.

- [ ] **Step 4: Write `home.html`/`home.js` stub**: body `<main id="main"></main>`; script: first line `const m = /^#(\d+)$/.exec(location.hash); if (m) { location.replace("/events/" + m[1]); }` then lists `/v1/events` newest first as `.cards` linking to each bracket (Task 9 replaces the body).

- [ ] **Step 5: Implement `view.rs`** as in Interfaces; `routes/mod.rs`: `.route("/", get(view::home)).route("/events/{id}", get(view::event))`. `inline_hash` now takes the page text. The `base64` unit test stays.

- [ ] **Step 6: Run the view tests to verify they pass**

Run: `cargo test -p autobracket-server --test view`
Expected: PASS.

- [ ] **Step 7: Document.** `docs/api.md` routes table: `GET / | public | — | 200 HTML landing page | —` and `GET /events/{id} | public | — | 200 HTML bracket page | 400, 404`; the signing section's public list gains `/events/{id}`. README: replace "a read-only bracket page" wording where it names `/` as the bracket with "the bracket page at `/events/{id}`".

- [ ] **Step 8: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/server docs/api.md README.md
git commit -m "pages: shared shell, stylesheet and script; bracket viewer at /events/{id}"
```

---

### Task 5: Vertical bracket layout for portrait screens

**Files:**
- Modify: `assets/pages/event.html`, `assets/pages/event.js`, `assets/site.css`
- Test: `crates/server/tests/view.rs`

**Interfaces:**
- Consumes: `#board`, `#head-side`, `.set.live`, the `renderBracket` output (Task 4).
- Produces (`event.js`): `const LAYOUT_KEY = "bracket-layout"`; `const PORTRAIT = matchMedia("(orientation: portrait)")`; `function storedLayout()` (`try { return localStorage.getItem(LAYOUT_KEY); } catch (e) { return null; }`, returning only `"horizontal"` or `"vertical"`, else `null`); `function storeLayout(value)` (`try { localStorage.setItem(LAYOUT_KEY, value); } catch (e) {}`); `function applyLayout()` (`board.dataset.layout = storedLayout() || (PORTRAIT.matches ? "vertical" : "horizontal")`, and `aria-pressed` on the two buttons); `PORTRAIT.addEventListener("change", applyLayout)`; `function jumpToLive()` (`document.querySelector(".set.live")?.scrollIntoView({ block: "center", behavior: "auto" })`); `#jump-live` is unhidden after each `renderBracket` when a `.set.live` exists.
- Produces (`event.html`, inside `#head-side` or just under the header): `<div class="layout" role="group" aria-label="Bracket layout"><button type="button" id="lay-h" aria-pressed="false">Horizontal</button><button type="button" id="lay-v" aria-pressed="false">Vertical</button></div><button type="button" id="jump-live" class="jump" hidden>Jump to live</button>`.
- Produces (`site.css`): `.board[data-layout="vertical"] .bracket { display: block; width: auto; }`, `.board[data-layout="vertical"] .side { display: block; }`, `.board[data-layout="vertical"] .round { width: auto; margin-bottom: var(--sp-5); }`, `.board[data-layout="vertical"] .sets { gap: var(--sp-3); }`, `.board[data-layout="vertical"] .set { width: 100%; min-height: 0; }`, `.board[data-layout="vertical"] .spacer { display: none; }`, `.board[data-layout="vertical"] .champ { margin: var(--sp-4) auto; }`, `.board[data-layout="vertical"] { overflow-x: clip; }`; `.layout button`, `.jump` styled as pill stickers (`[aria-pressed="true"]` in the sunny accent).

- [ ] **Step 1: Write the failing test** in `tests/view.rs`:

```rust
#[tokio::test]
async fn bracket_page_has_a_vertical_layout() {
    let app = TestApp::spawn(|_| {}).await;
    let id = app.create_event(Format::SingleElim).await;
    let page = fetch(&app, &format!("/events/{id}")).await.text().await.unwrap();
    for s in [
        "(orientation: portrait)",
        r#"[data-layout="vertical"]"#,
        ">Horizontal<", ">Vertical<", ">Jump to live<",
        "aria-pressed",
        r#"try { return localStorage.getItem(LAYOUT_KEY); } catch"#,
        r#"try { localStorage.setItem(LAYOUT_KEY, value); } catch"#,
        r#"behavior: "auto""#,
    ] {
        assert!(page.contains(s), "{s} in the bracket page");
    }
    // Storage is touched only inside the two guarded functions.
    assert_eq!(page.matches("localStorage").count(), 2);
    // Everything the vertical layout shows is as wide as the page, never wider.
    assert!(page.contains(r#".board[data-layout="vertical"] .bracket { display: block; width: auto; }"#));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p autobracket-server --test view bracket_page_has_a_vertical_layout`
Expected: FAIL on the first missing string.

- [ ] **Step 3: Implement** the markup, script and styles as in Interfaces. Call `applyLayout()` once at start and from the two buttons' `click` (`storeLayout(value); applyLayout();`). In `renderView`, after `renderBracket`, set `$("jump-live").hidden = !document.querySelector(".set.live")`; in the no-bracket (sign-up) branch hide it. The `(max-width: 40rem)` block keeps its current rules.

- [ ] **Step 4: Run the view tests**

Run: `cargo test -p autobracket-server --test view`
Expected: PASS.

- [ ] **Step 5: Manual check** at 400 px wide, portrait: the rounds stack, no horizontal page scroll (`document.documentElement.scrollWidth <= innerWidth`), the toggle switches and survives a reload, "Jump to live" lands on a live set. Fix in this task what the check finds.

- [ ] **Step 6: Commit**

```bash
git add crates/server
git commit -m "bracket page: vertical layout for portrait screens, layout toggle, jump to live"
```

---

### Task 6: Schedule and history pages

**Files:**
- Create: `assets/pages/schedule.html`, `schedule.js`, `history.html`, `history.js`
- Modify: `crates/server/src/routes/view.rs` (two `SOURCES` entries, handlers `schedule`, `history`), `routes/mod.rs`, `assets/site.css` (if a component is missing), `docs/api.md`
- Test: `crates/server/tests/view.rs`

**Interfaces:**
- Consumes: `AB.fmtWhen`, `AB.fmtDate`, `AB.ordinal`, `AB.howToEnter`, `AB.eventHref`, `AB.getJson`, `.cards`, `.rows` (Task 4); `/v1/history` (Task 3).
- Produces: `GET /schedule`, `GET /history`.

- [ ] **Step 1: Extend the tests.** `all_pages` returns `"/schedule"` and `"/history"` too. Add:

```rust
#[tokio::test] async fn schedule_and_history_pages_fetch_their_data() {
    // /schedule body contains "/v1/events", "Happening now", "Coming up", "time to be announced", "autobracket-client register";
    // /history body contains "/v1/history", "No completed events yet", "and ", "AB.ordinal("
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p autobracket-server --test view`
Expected: FAIL (404 `no such route`).

- [ ] **Step 3: Write the pages.** `schedule.js`: fetch `/v1/events`; `running` under `<h2>Happening now</h2>`, `signup` under `<h2>Coming up</h2>` sorted by `(starts_at ?? Infinity, id)`; each card: name link, `<time datetime=iso>` with `abs` and a quiet `rel` ("time to be announced" when null; "was due …" when past), description, "N entrants", `Sign-ups open`/`In progress` label; `AB.howToEnter(firstSignupId)` under "How to enter" (omitted when nothing is in sign-up); empty state "Nothing is scheduled." `history.js`: fetch `/v1/history`; each card: name link, `AB.fmtDate(starts_at ?? created_at)`, "N entrants", a `.rows` list of placings with `place <= 8` as `ordinal(place)` + tag (tag links to the player profile), then "and N more" linking to the bracket when `summary.entrants > 8`; empty "No completed events yet."

- [ ] **Step 4: Register** the two pages in `SOURCES` and routes `/schedule`, `/history`.

- [ ] **Step 5: Run the view tests**

Run: `cargo test -p autobracket-server --test view`
Expected: PASS.

- [ ] **Step 6: Document** the two route rows in `docs/api.md` (`200 HTML`), and the public list.

- [ ] **Step 7: Run the whole suite and commit**

```bash
cargo test --workspace
git add crates/server docs/api.md
git commit -m "pages: schedule and history"
```

---

### Task 7: Players index and profile pages

**Files:**
- Create: `assets/pages/players.html`, `players.js`, `player.html`, `player.js`
- Modify: `view.rs` (entries; handlers `players`, `player(State, Path<String>)`), `routes/mod.rs`, `docs/api.md`
- Test: `crates/server/tests/view.rs`

**Interfaces:**
- Consumes: `/v1/players`, `/v1/players/{tag}` (Task 3); `AB.playerHref`, `AB.ordinal`, `AB.fmtDate`, `.rows`, `.filter`.
- Produces: `GET /players`, `GET /players/{tag}` (404 `no such player` with the Task 3 lookup; the page's script takes the tag from `decodeURIComponent(location.pathname.slice("/players/".length))` and shows the canonical tag the JSON returns).

- [ ] **Step 1: Extend the tests.** `all_pages` adds `"/players"` and `"/players/Mango"`. Add:

```rust
#[tokio::test] async fn player_page_matches_tag_in_any_case_and_rejects_unknown() {
    // /players/MANGO -> 200 HTML; /players/Ma%6Ego -> 200 HTML; /players/nobody -> 404 JSON "no such player";
    // /players/..%2Fetc -> 404 JSON
}
#[tokio::test] async fn players_pages_fetch_their_data() {
    // /players body contains "/v1/players", `type="search"`, "No players yet";
    // /players/Mango body contains "/v1/players/", "Head to head", "playing since"
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p autobracket-server --test view`
Expected: FAIL (404 `no such route`).

- [ ] **Step 3: Write the pages.** `players.js`: fetch once; render `.rows` with a header line (Player, Events, Titles, Sets) and one `.row-line` per player (tag link, numbers, "W–L"); the `<input type="search" class="filter" aria-label="Filter players">` filters rows by `tag.toLowerCase().includes(q)` toggling `hidden`; empty "No players yet." `player.js`: fetch; `#title` and `document.title` = canonical tag; meta line "playing since <month year> · N events · T titles · sets W–L · games W–L"; `<h2>Events</h2>` rows (name link, date, "seed N", `ordinal(place)` or "in progress", sets, games); `<h2>Head to head</h2>` rows (tag link, sets, games) or "No sets played yet."

- [ ] **Step 4: Register** pages, handlers and routes (`/players`, `/players/{tag}`).

- [ ] **Step 5: Run the view tests**

Run: `cargo test -p autobracket-server --test view`
Expected: PASS.

- [ ] **Step 6: Document** the two route rows (`/players/{tag}`: `404`) and the public list.

- [ ] **Step 7: Run the whole suite and commit**

```bash
cargo test --workspace
git add crates/server docs/api.md
git commit -m "pages: players index and player profile"
```

---

### Task 8: Stream: configuration, /v1/stream and the stream page

**Files:**
- Modify: `crates/server/src/config.rs`, `deploy/autobracket.example.toml`, `crates/proto/src/types.rs` (`StreamInfo`), `crates/proto/tests/api_doc.rs`, `crates/server/src/routes/results.rs` (`stream` handler), `routes/mod.rs`, `routes/view.rs` (`stream(State)` with the dynamic CSP), `docs/api.md`, `README.md`
- Create: `assets/pages/stream.html`, `stream.js`
- Test: `crates/server/src/config.rs` (unit), `crates/server/tests/view.rs`

**Interfaces:**
- Produces: `Config { …, #[serde(default)] pub stream_url: Option<String> }`; `Config::stream_origin(&self) -> Option<String>` (`scheme://host[:port]` via `reqwest::Url`; `None` when unset); `validate()` bails `stream_url must be an http or https URL with a host, e.g. "https://live.example.org/melee/"` otherwise; `StreamInfo { url: Option<String> }`; `GET /v1/stream` → `StreamInfo { url: cfg.stream_url.clone() }`; `GET /stream` whose CSP is the page's base CSP plus `; frame-src <origin>` when configured; `Page` gains `pub fn csp_with(&self, extra: &str) -> String` or the handler formats it.

- [ ] **Step 1: Write the failing config tests** in `config.rs`'s test module:

```rust
#[test] fn stream_url_parses_and_gives_its_origin() {
    let cfg = Config::parse(&format!("to_keys = [\"{KEY}\"]\nstream_url = \"https://live.example.org:8889/melee/\"\n")).unwrap();
    assert_eq!(cfg.stream_origin().as_deref(), Some("https://live.example.org:8889"));
    assert_eq!(Config::new(vec![KEY.into()]).stream_origin(), None);
}
#[test] fn rejects_bad_stream_url() {
    for bad in ["ftp://live.example.org/", "live.example.org/melee", "https://", "https:///melee"] {
        let err = rejection(&format!("stream_url = \"{bad}\""));
        assert!(err.contains("stream_url"), "{bad}: {err}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p autobracket-server --lib config::tests::stream`
Expected: FAIL (unknown field `stream_url`).

- [ ] **Step 3: Implement** the field, `stream_origin`, the validation (in `validate_api` or beside it), `Config::new` sets `None`; `example_config_parses` in `tests/view.rs` gains `assert_eq!(cfg.stream_url, None)`; the example toml gains, before `[verifier]`:
  `# Live stream to embed on the /stream page (e.g. a MediaMTX WebRTC viewer). Unset: the page says no stream is configured. Only this URL's origin is allowed in the page's frame.` and `# stream_url = "https://live.example.org/melee/"`.

- [ ] **Step 4: Run the config tests**

Run: `cargo test -p autobracket-server --lib config`
Expected: PASS.

- [ ] **Step 5: Write the failing page tests** in `tests/view.rs` (`all_pages` adds `"/stream"`):

```rust
#[tokio::test] async fn stream_page_without_a_stream() {
    // default app: /v1/stream == {"url": null}; /stream CSP has no "frame-src"; body contains
    // "/v1/stream", "No stream is configured", "Now playing", `allow="autoplay; fullscreen"`, `referrerpolicy="no-referrer"`
}
#[tokio::test] async fn stream_page_frames_only_the_configured_origin() {
    let app = TestApp::spawn(|c| c.stream_url = Some("https://live.example.org:8889/melee/".into())).await;
    // /v1/stream == {"url": "https://live.example.org:8889/melee/"};
    // /stream CSP contains "frame-src https://live.example.org:8889" and not "/melee"; its body still has no "http";
    // every other page's CSP contains neither "frame-src" nor "live.example.org"
}
```
`pages_have_security_headers`'s `!csp.contains("http")` assertion stays as is (it runs on the default app).

- [ ] **Step 6: Run to verify failure**

Run: `cargo test -p autobracket-server --test view stream`
Expected: FAIL.

- [ ] **Step 7: Implement.** `StreamInfo` in proto; `results::stream(State) -> Json<StreamInfo>`; route `/v1/stream`; `stream.html`: `<p class="notice" id="no-stream" hidden></p><iframe id="player" class="player" title="Live stream" allow="autoplay; fullscreen" referrerpolicy="no-referrer" hidden></iframe><p id="open" hidden><a id="open-link" rel="noopener" target="_blank">Open the stream in a new tab</a></p><h2>Now playing</h2><div id="now"></div>`; `stream.js`: fetch `/v1/stream` once (url → `player.src = url`, `open-link.href = url`, unhide; else notice "No stream is configured on this server."), then `AB.poll` every 10 s: `/v1/events`, each `running` event's view, its `Ready` sets as rows "A vs B · <round> · a–b" linking to the bracket, or "Nothing is being played right now."; `view::stream(State)` serves the page with `format!("{}; frame-src {}", page.csp, origin)` when `stream_origin()` is `Some`.

- [ ] **Step 8: Run the view tests**

Run: `cargo test -p autobracket-server --test view`
Expected: PASS.

- [ ] **Step 9: Document.** `docs/api.md`: rows `GET /stream` and `GET /v1/stream` (`200 StreamInfo`), `### StreamInfo` with a checked example `{"url": "https://live.example.org/melee/"}`; `api_doc.rs` arm and list entry. README deployment: a "Stream" paragraph (the MediaMTX viewer URL goes in `stream_url`; the page frames that origin only; everything else stays first-party).

- [ ] **Step 10: Run the whole suite and commit**

```bash
cargo test --workspace
git add crates deploy docs README.md
git commit -m "stream: stream_url config, /v1/stream and the stream page with frame-src"
```

---

### Task 9: Landing page and documentation

**Files:**
- Modify: `assets/pages/home.html`, `home.js` (replace the Task 4 stub), `README.md`, `docs/api.md` (public route list wording), `assets/site.css` (podium if needed)
- Test: `crates/server/tests/view.rs`

**Interfaces:**
- Consumes: `/v1/events`, `/v1/events/{id}`, `/v1/history`, `/v1/stream`; `AB.howToEnter`, `AB.fmtWhen`, `AB.sideRounds`/`roundName`, `AB.poll`, `AB.ordinal`.

- [ ] **Step 1: Extend the test** `home_redirects_old_hash_bookmarks` into `home_page_has_its_sections`: body contains `location.replace("/events/" +`, "/v1/history", "/v1/stream", "Live now", "Next up", "Latest results", "No events yet", "autobracket-client register", "Watch the stream". Run: FAIL on the missing strings.

- [ ] **Step 2: Write the landing.** `home.html`: `<section id="live"></section><section id="next"></section><section id="latest"></section><p class="links"><a href="/schedule">Schedule</a> · <a href="/history">History</a> · <a href="/players">Players</a></p>`. `home.js`: the hash redirect first; then `AB.poll` every 10 s: fetch `/v1/events`, `/v1/history`, `/v1/stream`; *Live now*: for each `running` event fetch its view and list `Ready` sets (as the stream page); add "Watch the stream" (`/stream`) when `stream.url`; hide the section when nothing runs; *Next up*: the `signup` event by `(starts_at ?? Infinity, id)` as a card with `fmtWhen`, description, entrants and `AB.howToEnter(id)`; *Latest results*: `history[0]` as a `.podium` of the placings with `place <= 3` (`ordinal` + tag link) and a link to the bracket; with no events at all show one notice "No events yet. This page updates when one is created." and hide the sections; `document.title = "Bracket"`.

- [ ] **Step 3: Run the view tests**

Run: `cargo test -p autobracket-server --test view`
Expected: PASS.

- [ ] **Step 4: README.** A new "## Pages" section after "Organiser quick start": the seven pages in a table (path, what it shows), that `/#N` bookmarks still land on the event, that everything is public and read-only, that times show in the visitor's time zone, and `stream_url`. Update the intro's "a read-only bracket page" to "public pages (landing, schedule, history, players, stream) and the bracket viewer".

- [ ] **Step 5: Manual check** (no test can run the scripts): `cargo run -p autobracket-server -- serve --config <a temp config with allow_no_organiser>` is not needed; instead run the server from a `TestApp`-like config or the deploy steps on a dev box, create one event with `--starts-at` and `--description`, sign two players up, start it, and open `/`, `/schedule`, `/history`, `/players`, `/stream`, `/events/1` at 400 px width and with reduced motion on. Note anything found as follow-up edits in this task.

- [ ] **Step 6: Run the whole suite and commit**

```bash
cargo test --workspace
git add crates/server README.md docs/api.md
git commit -m "pages: landing page; README pages section"
```

---

## Self-review notes

- Spec coverage: pages (Tasks 4-9), vertical bracket layout (5), data model and admin route (1), placings (2), results API (3), stream config (8), docs (1, 3-9), retired test (4), bookmarks (4, 9). The `round_names_match_the_doc` test moves to `site.js` in Task 4.
- Types: `EventSummary.created_at` is `i64` everywhere; `sets`/`games` are `[u32; 2]` as `[won, lost]` in `PlayerSummary`, `PlayerEvent`, `HeadToHead`; `Placing.place` is `u32` as `Bracket::placings` returns.
- Review Focus items 1-6 are pinned by tests in Tasks 1, 3, 4, 7, 8 and 5 respectively.
