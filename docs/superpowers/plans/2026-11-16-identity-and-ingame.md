# Phase 2b: Identity, Admin, In-game API, Live Relay Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Discord sign-in (Zitadel + oauth2-proxy) with accounts, linked game clients and web join; an MFA-gated `/admin` with an audit log; the signed client routes the in-game Brackets screen needs (home, badge, ready, server-held stage strikes, set flow); and the live relay receive side feeding the stream page.

**Architecture:** nginx authenticates; on `/account/*` and `/admin/*` only, the app accepts a request from a trusted peer carrying the proxy secret and a Zitadel ID token it verifies itself (JWKS, iss, aud, exp, sub); groups, amr and auth_time come from that token only. Three HMAC-signed Zitadel Actions v2 hooks map Discord profiles, add a flat `groups` claim and anonymise deleted users. Client keys become aliases of a canonical player key, so every existing signed route works for linked clients unchanged. Stage selection is a pure engine in the `bracket` crate; the server stores moves and enforces the chosen stage at judging. The live relay is an in-memory WebSocket receiver parsed with the `replay` crate.

**Tech Stack:** Rust 1.94, axum 0.8 (+ `ws`), sqlx 0.9 SQLite, `hmac` 0.12, `base64` 0.22, `jsonwebtoken`; plain HTML/CSS/JS pages with hash-pinned CSP.

**Spec:** `docs/superpowers/specs/2026-11-16-identity-and-ingame.md` (read it first; section numbers below refer to it). Binding security input: `.superpowers/sdd/2026-11-15-verified-autobracket/lane-zitadel-review.md` (C1, I1, I3, I4, I5, I7) — Tasks 2, 4 and 12 must be reviewed against it.

## Global Constraints

- Licence AGPL-3.0-or-later. New dependencies only: `hmac = "0.12"`, `base64 = "0.22"`, `jsonwebtoken` (newest release at least two weeks old, pure-Rust crypto backend if the release makes you choose; RS256 and EdDSA) (Task 2), axum feature `ws` and dev-dependency `tokio-tungstenite` at the version axum's `ws` already locks in `Cargo.lock` (Task 9). Nothing else.
- Identity is read only through `identity::WebUser` / `identity::AdminUser`; no other code reads `x-auth-request-*`, `x-id-token` or `x-autobracket-proxy`. `X-Auth-Request-Groups`, `-Preferred-Username` and `-Email` are never read at all: groups, amr and auth_time come only from the verified ID token.
- Hook bodies are never logged (they carry Discord access tokens); hook signatures are checked on the raw bytes before any parsing.
- Every error body is `{"error": …}` (`ApiError`); bad header or hook contents are 4xx, never 500.
- Every new route gets a row in the route table of **its task's own `docs/api.md` §8.x subsection** (created by Task 1; the `documented_routes_exist` test reads every route table); every new proto type has a `<!-- checked: Type -->` example (the `api_doc` test).
- Pages: one `<script>`, one `<style>`; data reaches the DOM via `textContent` (`AB.el`); no page HTML contains `http://` or `https://`; only `/account/` and `/admin/` add `img-src https://cdn.discordapp.com`.
- Signer perspective in client types: pairs are `[me, opponent]`.
- Migration numbering is fixed: `0006_identity.sql` (Task 2) is the only migration in waves 1–2; `0007_notify.sql` belongs to Task 13. No other task adds a migration.
- Audit action names exactly as spec §5.2.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass at every commit.
- Commit after every task; message ends with the session's attribution trailer.

## Parallel execution and shared files

| Wave | Tasks (run in parallel worktrees) | Merge order |
|---|---|---|
| 1 | 1 Contracts · 2 Identity core · 3 Stage engine | any; all three before wave 2 |
| 2 | 4 Hooks · 5 Account · 6 Admin web · 7 Home/badge · 8 Set flow · 9 Live relay · 10 Site pages · 11 Client | any |
| 3 | 12 Ops (controller, not an implementer) | after wave 2 deployed |
| 4 | 13 Discord pings (optional) | after 12 |

Shared-file ownership (a file not listed is touched by one task only):

| File | Owner(s) | How conflicts are avoided |
|---|---|---|
| `routes/mod.rs` | 1 only | Task 1 merges one sub-router per module (`account`, `admin_web`, `hooks`, `me`, `setflow`, `live`, `site`); wave-2 tasks register routes in their own module's `router()` |
| `routes/view.rs` | 1 (wave 1); 10 extends only its `tests` module (wave 2) | Task 1 adds `account`, `admin`, `privacy`, `terms` with stub assets; wave 2 edits only the asset files |
| `docs/api.md` | 1 (types, §8.x skeleton); 4–10 (own §8.x subsection only; Task 10 also the §2 public-route sentence) | separate subsections |
| `crates/proto/tests/api_doc.rs` | 1 only | all new types dispatched in Task 1 |
| `migrations/` | 2 (0006), 13 (0007) | fixed numbers |
| `config.rs`, `auth.rs`, `testkit.rs` | 2 (13 adds nothing to config) | — |
| `state.rs`, `lib.rs`, server `Cargo.toml` | 2 (wave 1, also declares the `anonymise`, `link`, `live` stub modules), 9 (wave 2), 13 (wave 4) | different waves |
| `routes/sets.rs` | 1 (refactor only) | wave 2 calls the `pub(crate)` helpers |
| `routes/admin.rs` | 6 (13 in wave 4) | — |
| `routes/events.rs` | 5 | — |
| `review.rs` | 6 (resolve signature), 8 (`Replay` arm body only) | disjoint hunks |
| `judge.rs`, `worker.rs` | 8 | — |
| `assets/site.js` | 5 | — |
| `assets/shell.html` | 10 | — |
| `assets/pages/stream.js` | 9 | — |
| `crates/proto/src/ruleset.rs` | 1 | — |

## Review Focus

1. **Forged identity: a peer outside `trusted_proxies`, a wrong/missing `X-Autobracket-Proxy`, an unsigned (`alg: none`) or wrongly signed token, wrong `iss`/`aud`, an expired token, a header `sub` differing from the token's, and `X-Auth-Request-Groups: autobracket-admins` with a token lacking the group** — 401 `not signed in` (or 403 `not an organiser` for the last), the account table unchanged, never 500. (Task 2 `forged_identity_is_refused`)
2. **Group near-misses in the token (`autobracket-admins-old`, `AUTOBRACKET-ADMINS`), `amr` without `mfa`, `auth_time` older than 12 h or absent** — only the exact string counts; MFA/age problems are 403 with `mfa_setup_url`, never 500; a removed user's still-valid token is 401 and never re-creates the account. (Task 2 `admin_requires_exact_group_recent_mfa`, `tombstoned_sub_is_refused`)
3. **A link code used twice, used after 10 minutes, or redeemed by a key that already belongs to another player or account** — 404 / 409, and no player, alias or account changes hands. (Task 5 `link_never_moves_a_player`)
4. **Live relay: reconnect that resends old cursors, a cursor jump, and late messages on the replaced socket** — stored bytes equal the uploaded `.slp` raw element minus the skipped range exactly; `gaps` counts the jump; the old socket's messages are dropped. (Task 9 `resume_gap_and_replacement`)
5. **Stage moves out of turn, twice in one slot, on a struck stage, or for a game that is not current** — 409 and the stored moves unchanged; a replayed game starts selection afresh. (Task 8 `stage_moves_are_turn_checked`)

Also pinned (lower likelihood): a `user.removed` event for an unknown or already-removed id is 200 and changes nothing (Task 4); an anonymised player's old device key gets 403 on `/v1/me/next` (Task 4); the token hook never re-binds a `sub` to another Discord id (Task 4); a replayed hook `(t, v1)` is refused (Task 4).

---

## File Structure

```
crates/proto/src/ingame.rs                 new: every new wire type (Task 1)
crates/proto/src/ruleset.rs                + starter_stages, stage_bans (Task 1)
crates/bracket/src/stages.rs               new: strike/ban/pick engine (Task 3)
crates/server/migrations/0006_identity.sql new (Task 2)
crates/server/src/identity.rs              new: WebUser, AdminUser, CSRF, hook signature (Task 2)
crates/server/src/jwks.rs                  new: JWKS cache and ID-token verification (Task 2)
crates/server/src/audit.rs                 new: Actor, record, list (Task 2)
crates/server/src/accounts.rs              new: account store, placeholder keys, rekey_player (Task 2)
crates/server/src/anonymise.rs             stub (Task 2), filled by Task 4
crates/server/src/link.rs                  stub (Task 2), link-code issue/redeem (Task 5)
crates/server/src/live.rs                  stub (Task 2), LiveHub (Task 9)
crates/server/src/routes/{account,admin_web,hooks,me,setflow,live,site}.rs   stubs (Task 1), filled by 5,6,4,7,8,9,10
crates/server/assets/pages/{account,admin,privacy,terms}.{html,js}           stubs (Task 1)
crates/server/tests/{identity,hooks,account,admin_web,home,setflow,live,site,notify}.rs
crates/client/src/{api.rs,main.rs}         + link, me, home, ready, stage, flow (Task 11)
deploy/nginx/bracket.gsd.sh.conf.example   new (Task 12, controller)
```

---

### Task 1: Contracts (wave 1)

**Files:**
- Create: `crates/proto/src/ingame.rs`; `crates/server/src/routes/{account,admin_web,hooks,me,setflow,live,site}.rs`; `crates/server/assets/pages/{account,admin,privacy,terms}.{html,js}`
- Modify: `crates/proto/src/lib.rs` (re-export), `crates/proto/src/ruleset.rs`, `crates/proto/tests/api_doc.rs`, `docs/api.md`, `crates/server/src/routes/mod.rs`, `crates/server/src/routes/view.rs`, `crates/server/src/routes/sets.rs`, `crates/server/tests/view.rs` (`documented_routes_exist`)
- Test: `crates/proto/tests/api_doc.rs`, `crates/proto/tests/ruleset.rs` (create if absent), `crates/server/tests/view.rs`

**Interfaces:**
- Produces (proto, all `Serialize + Deserialize + Debug + Clone + PartialEq`, enums serde-default encoding like `Next`):
  - `SiteInfo { identity: bool, operator: Option<String>, contact: Option<String> }`
  - `MeStatus { tag: String, linked: bool, display_name: Option<String> }`
  - `LinkReq { code: String }`, `LinkResult { tag: String }`
  - `Home { badge: bool, action: HomeAction, events: Vec<MyEvent> }`; `MyEvent { event: EventSummary, next: Next }`
  - `enum HomeAction { None, SetUp { event_id: i64, set_id: u32, side: Side, round: u8, opponent_tag: String, game_no: u32, ready: [bool; 2] }, Review { event_id: i64, set_id: u32 }, Upcoming { event_id: i64, set_id: u32, side, round: u8, opponent_tag: Option<String>, eta_secs: Option<u32> } }` — `side` is `proto::Side` (new enum `Winners|Losers|GrandFinal`, same strings as the bracket JSON)
  - `Badge { set_up: bool, event_id: Option<i64>, set_id: Option<u32> }`
  - `ReadyReq { engine_build: String }`
  - `#[serde(rename_all = "lowercase")] enum StageKind { Strike, Ban, Pick }`; `#[serde(rename_all = "lowercase")] enum Who { Me, Opponent }`
  - `StageMoveReq { kind: StageKind, stage: u16 }`
  - `StageState { mode: StageMode /* "strike"|"counterpick" lowercase */, pool: Vec<u16>, moves: Vec<StageMove>, turn: Option<StageTurn>, chosen: Option<u16> }`; `StageMove { who: Who, kind: StageKind, stage: u16 }`; `StageTurn { who: Who, kind: StageKind, count: u8 }`
  - `#[serde(rename_all = "snake_case")] enum SetPhase { ReadyCheck, StageSelect, Playing, Judging, Review, Done }`
  - `GameResult { game_no: u32, stage: Option<u16>, winner: Option<Who>, counted: bool, verdict: Option<Verdict> }`
  - `SetFlow { set_id: u32, side: Side, round: u8, opponent_tag: String, role: Role, best_of: u8, wins: [u32; 2], game_no: u32, ready: [bool; 2], phase: SetPhase, stage: Option<StageState>, games: Vec<GameResult>, verified: bool, after: Option<Next> }`
  - `LiveGame { event_id: i64, set_id: u32, game_no: u32, tags: [String; 2], stage: Option<u16>, ports: Vec<LivePort>, frame: Option<i32>, ended: bool, gaps: u32, updated_at: i64 }`; `LivePort { port: u8, character: u8, stocks: u8, percent: f32 }`
  - `Ruleset.starter_stages: Vec<u16>` (`#[serde(default)]`), `Ruleset.stage_bans: u8` (default 2); `Ruleset::starters(&self) -> &[u16]` (empty → `legal_stages`)
- Produces (server): each stub module exposes `pub fn router() -> Router<AppState>` returning `Router::new()`; `routes::router()` ends with `.merge(account::router()).merge(admin_web::router()).merge(hooks::router()).merge(me::router()).merge(setflow::router()).merge(live::router()).merge(site::router())` before the fallbacks.
- Produces (view.rs): `pub(crate) fn page(name: &'static str) -> Response`; `pub(crate) fn page_with_img_src(name: &'static str, extra: &str) -> Response` (CSP's `img-src data:` becomes `img-src data: {extra}`, no second `img-src` directive).
- Produces (sets.rs, behaviour unchanged): `pub(crate) async fn next_for(conn: &mut SqliteConnection, cfg: &Config, event: i64, player_key: &str) -> Result<Next, ApiError>` (the body of `next`), `pub(crate) async fn current_game(...)`, `pub(crate) async fn entrant_id(...)`, `pub(crate) fn bracket_set(...)`, `pub(crate) fn slot_of(...)` (existing signatures, visibility only).

- [ ] **Step 1: Write the failing tests**
  - `api_doc.rs`: add every type above to the dispatch match.
  - `ruleset.rs`: `starter_stages_default_to_legal` (`Ruleset::default().starters() == legal_stages`); `validate_rejects_starter_outside_legal` (`starter_stages = [99]` → `Err` containing `"starter_stages"`); `validate_rejects_too_many_bans` (`stage_bans = legal_stages.len()` → `Err` containing `"stage_bans"`); `old_ruleset_json_still_decodes` (the existing `docs/api.md` Ruleset example without the new fields decodes with `stage_bans == 2`).
  - `crates/server/tests/view.rs`: `new_pages_serve_with_csp` — none routable yet, so assert in the unit test in `view.rs`: `PAGES` has `account`, `admin`, `privacy`, `terms`, and `page_with_img_src("account", "https://cdn.discordapp.com")`'s CSP contains `img-src data: https://cdn.discordapp.com` exactly once and no other `img-src`.
- [ ] **Step 2: Run** `cargo test -p proto --test api_doc --test ruleset && cargo test -p autobracket-server view` — Expected: FAIL (types and fields missing).
- [ ] **Step 3: Implement** the types in `ingame.rs`, the ruleset fields and `validate` rules (spec §6.3), `docs/api.md`: new §8 "Accounts and in-game client" with one `checked` example per type (perspective rule stated once), Ruleset table and example gain the two fields, and empty subsections §8.1 Zitadel hooks, §8.2 Account, §8.3 Admin web, §8.4 Home and badge, §8.5 Set flow, §8.6 Live relay, §8.7 Site, each with an empty route table of the §4 columns and a sentence saying it continues §4. Make the `documented_routes_exist` test read every route table in the file (rows whose first cell is an HTTP method), not only §4's. Add the stub modules and assets (`<main id="main"></main>` and `(function () {})();`), the `SOURCES` entries (titles "Account", "Admin", "Privacy", "Terms"), the helpers, and the `sets.rs` refactor.
- [ ] **Step 4: Run** `cargo test --workspace` — Expected: PASS (all existing tests unchanged).
- [ ] **Step 5: Commit** `git commit -m "contracts: phase 2b wire types, route modules, page stubs"`

### Task 2: Identity core (wave 1)

Review against `lane-zitadel-review.md` I1 and I7 before merging.

**Files:**
- Create: `crates/server/migrations/0006_identity.sql`, `crates/server/src/identity.rs`, `crates/server/src/jwks.rs`, `crates/server/src/audit.rs`, `crates/server/src/accounts.rs`, `crates/server/tests/identity.rs`; empty modules (one doc-comment line each) `crates/server/src/{anonymise,link,live}.rs` declared in `lib.rs`, so Tasks 4, 5 and 9 never add `mod` lines
- Modify: `Cargo.toml` (workspace deps `hmac`, `base64`, `jsonwebtoken`), `crates/server/Cargo.toml`, `crates/server/src/{config.rs,state.rs,lib.rs,auth.rs,testkit.rs}`, `crates/server/src/routes/players.rs`, `crates/server/tests/migrations.rs`

**Interfaces:**
- Produces config (spec §2): `Config.identity: Option<IdentityConfig>`, `Config.site: SiteConfig` (`#[serde(default)]`); `IdentityConfig { public_origin, issuer, client_id, jwks_url: Option<String>, project_id, org_id, discord_idp_id, proxy_secret, admin_group /* default "autobracket-admins" */, admin_max_auth_age_secs: u64 /* default 43200 */, sign_out_url, mfa_setup_url, hook_peers: Vec<String>, hook_keys: HookKeys { idp_intent, token, user_events } }` (all `String` unless shown); `IdentityConfig::jwks_url(&self) -> String`; `SiteConfig { operator: Option<String>, contact: Option<String> }`. Validation per spec §2.
- Produces state: `AppState.trusted: Arc<[net::Cidr]>`, `AppState.jwks: Arc<jwks::Jwks>`.
- Produces `jwks`: `pub struct Jwks` (`reqwest` client with a 5 s timeout, cache 1 h, unknown-`kid` re-fetch at most once per 60 s); `pub async fn verify(&self, cfg: &IdentityConfig, jwt: &str, now: i64) -> Result<IdClaims, TokenError>`; `IdClaims { sub: String, groups: Vec<String>, amr: Vec<String>, auth_time: Option<i64> }` (missing `groups`/`amr` → empty); `TokenError { Invalid, Unavailable }` (Unavailable only when no key could ever be fetched).
- Produces `identity`:
  - `pub struct WebUser { pub account_id: i64, pub sub: String, pub groups: Vec<String>, pub amr: Vec<String>, pub auth_time: Option<i64>, pub display_name: Option<String> }` — `FromRequestParts<AppState>`. Order: identity off → 404; peer not trusted, secret wrong/missing, no/invalid token, header sub ≠ token sub → 401 `not signed in`; JWKS unavailable → 503; tombstoned → 401 `this account was deleted`; `disabled_at` → 403 `this account is disabled`; non-GET/HEAD without CSRF → 403 `cross-site request refused`. Creates the account row for a new `sub` (no Discord id), updates `last_seen_at`.
  - `pub struct AdminUser(pub WebUser)` — group / mfa / auth_time rules and 403 bodies exactly as spec §2.
  - `pub fn is_admin(groups: &[String], cfg: &IdentityConfig) -> bool` (exact string match)
  - `pub fn verify_hook(sig_header: Option<&str>, body: &[u8], key: &str, now: i64) -> Option<(i64, String)>` (spec §4: returns the matching `(t, v1)` for the replay cache; any `v1` may match; window both ways)
  - `pub fn probe_router() -> Router<AppState>`: `GET`/`POST /account/api/ping` (`WebUser` → 204) and `GET /admin/api/ping` (`AdminUser` → 204), merged in `lib.rs::build_app` after `routes::router()` so `routes/mod.rs` stays Task 1's.
  - `pub fn sha256_hex(s: &str) -> String` (tombstones)
- Produces `audit`: `pub enum Actor { Key(String), Account { id: i64, name: Option<String> }, Zitadel, System }`; `impl From<&WebUser> for Actor`; `pub async fn record(conn: &mut SqliteConnection, actor: &Actor, action: &str, target: Option<&str>, detail: Option<serde_json::Value>) -> Result<(), sqlx::Error>`; `pub async fn list(db: &SqlitePool, before: Option<i64>, limit: u32) -> Result<Vec<AuditEntry>, sqlx::Error>`; `#[derive(Serialize)] pub struct AuditEntry { id: i64, at: i64, actor_kind: String, actor_id: String, actor_name: Option<String>, action: String, target: Option<String>, detail: Option<Value> }`.
- Produces `accounts`: `pub fn placeholder_key() -> String`; `pub async fn account_player(conn, account_id: i64) -> Result<Option<(String /*key*/, String /*tag*/)>, sqlx::Error>`; `pub async fn create_player(conn, account_id: i64, tag: &str) -> Result<String, CreatePlayerError /* HasPlayer | BadTag | TagTaken | Db */>`; `pub async fn rekey_player(conn, old: &str, new: &str) -> Result<(), RekeyError /* NoSuchPlayer | KeyInUse | Db */>` (moves `players`, `entrants`, `invites.used_by`, `player_keys.player_key`); `pub async fn resolve_key(conn, signer: &str) -> Result<Option<(String, String)>, sqlx::Error>` (canonical key, tag via `players.key` or `player_keys.key`); `pub async fn bind_discord(conn, sub: &str, discord_id: &str) -> Result<BindOutcome /* Created | Bound | Unchanged | Conflict */, sqlx::Error>` (the only writer of `accounts.discord_id`; never re-binds; refuses tombstoned subs); `pub async fn view(conn, account_id: i64, cfg: &IdentityConfig, is_admin: bool) -> Result<AccountView, sqlx::Error>` with `#[derive(Serialize)] AccountView` exactly per spec §5.1 and `ClientView { key_prefix, label, linked_at, placeholder }`.
- Produces `auth`: `Player { key /* canonical */, signer: String, tag, body }`; `PlayerHead { key, signer, tag }` implementing `FromRequestParts<AppState>` (all checks of `authenticate` with an empty body; used by Task 9's upgrade). Nonces are recorded under `signer`.
- Produces `testkit`: `TestApp::spawn_identity(edit)` starts a JWKS server on a random local port serving one Ed25519 JWK (`kid "t1"`) and sets identity: `public_origin = "https://bracket.test"`, `issuer = "https://auth.test"`, `client_id = "client-test"`, `jwks_url = <that server>/oauth/v2/keys`, `project_id = "p1"`, `org_id = "o1"`, `discord_idp_id = "idp1"`, `proxy_secret = "test-proxy-secret-test-proxy-secret-00"`, `trusted_proxies = ["127.0.0.1/32"]`, hook keys `"k-idp"`, `"k-token"`, `"k-events"`, `mfa_setup_url = "https://auth.test/mfa"`, `sign_out_url = "/oauth2/sign_out"`. `TestApp::rotate_jwks(&self)` adds a second key `t2` (for the unknown-kid test). `TestWeb { sub, groups: Vec<String>, amr: Vec<String>, auth_time_ago: i64, token: TokenMode /* Valid | Unsigned | WrongKey | WrongIss | WrongAud | Expired | Kid2 | Absent */, header_sub: Option<String>, proxy_secret: Option<String> /* None = correct */ }` with `TestWeb::player(sub)`, `TestWeb::admin(sub)` (groups `["autobracket-admins"]`, amr `["user","mfa"]`, `auth_time_ago 60`); `TestApp::web(&self, who: &TestWeb) -> WebClient` with `get`, `post_json`, `delete` setting `X-Auth-Request-User`, `X-Id-Token: Bearer <jwt>`, `X-Autobracket-Proxy`, `Origin: https://bracket.test`, `x-ab-csrf: 1`.

- [ ] **Step 1: Write the failing tests** in `tests/identity.rs` (all through `/account/api/ping` and `/admin/api/ping`):
  - `forged_identity_is_refused` (Review Focus 1): untrusted peer (`trusted_proxies = ["10.0.0.1/32"]`) → 401; `proxy_secret: Some("wrong")` and no secret → 401; each `TokenMode` of `Unsigned`, `WrongKey`, `WrongIss`, `WrongAud`, `Expired`, `Absent` → 401; `header_sub` differing → 401; `TestWeb::player` plus a raw `X-Auth-Request-Groups: autobracket-admins` header → `/admin/api/ping` 403 `not an organiser`; `SELECT COUNT(*) FROM accounts` is 0 after all the 401s; no response is 500.
  - `identity_off_is_404`: plain `TestApp::spawn` → `/account/api/ping` 404.
  - `admin_requires_exact_group_recent_mfa` (Review Focus 2): token groups `["autobracket-admins-old"]` and `["AUTOBRACKET-ADMINS"]` → 403 `not an organiser`; admin with amr `["user"]`, with `auth_time_ago = 43201`, with no `auth_time` → 403 whose JSON has `mfa_setup_url == "https://auth.test/mfa"` and `sign_in_url == "/oauth2/sign_in?rd=/admin/"`; correct → 204.
  - `unknown_kid_refetches`: `rotate_jwks`, token signed with `Kid2` → 204; a second unknown kid within 60 s → 401 without a second fetch (count fetches on the JWKS server).
  - `jwks_down_is_503`: JWKS server stopped before the first request → 503 `sign-in service unavailable`.
  - `tombstoned_sub_is_refused`: insert `account_tombstones(sha256("u9"))` → valid token for `u9` is 401 `this account was deleted` and no account row appears; `disabled_at` set → 403 `this account is disabled`.
  - `csrf_is_required_on_writes`: `POST /account/api/ping` without `x-ab-csrf` → 403; with `Origin: https://evil.test` → 403; correct → 204.
  - `alias_key_signs_as_player`: register player A with key K1; insert `player_keys(K2 → K1)`; K2 signs `GET /v1/me/next?event=<id>` → 200 `NotEntered`; K2 signs `POST /v1/players` → 409; the nonce row is under K2.
  - Unit (`accounts.rs`): `rekey_player_moves_everything` (entrants, invite, aliases follow; new key in use → `KeyInUse`); `bind_discord_never_rebinds` (Created → Unchanged on repeat; different id → Conflict and the row unchanged; the same Discord id under another sub → Conflict; tombstoned sub → no row).
  - Unit (`identity.rs`): `verify_hook_*` — body `{"a":1}`, key `"k"`: valid at t = now; t − 301 and t + 301 fail; `v1` of another key fails; two `v1` entries where the second matches pass; parts reordered (`v1=…,t=…`) and with spaces pass; missing or malformed header → `None`.
  - Unit (`config.rs`): identity with a 31-char secret, empty `trusted_proxies`, `public_origin = "http://x"` or with a path, `issuer` with a path, an empty `client_id`/`project_id`/`org_id`/`discord_idp_id`/hook key, a bad `hook_peers` entry → `Err` naming the field.
  - `migrations.rs`: 0006 applies on a database with phase-1 data and the old rows are intact.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test identity` — Expected: FAIL (modules missing).
- [ ] **Step 3: Implement** in the files listed. Hook HMAC via `hmac::Mac::verify_slice` (constant time); the proxy-secret comparison is constant time. Token verification with `jsonwebtoken` (`DecodingKey::from_jwk`, `Validation` with the issuer, audience, `exp`, 60 s leeway, algorithm from the JWK).
- [ ] **Step 4: Run** `cargo test --workspace` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "identity: verified ID tokens, accounts, aliases, audit log"`

### Task 3: Stage engine (wave 1)

**Files:**
- Create: `crates/bracket/src/stages.rs`, `crates/bracket/tests/stages.rs`
- Modify: `crates/bracket/src/lib.rs` (`pub mod stages;`)

**Interfaces:**
- Produces:
  ```rust
  pub enum StageKind { Strike, Ban, Pick }
  pub struct Move { pub slot: u8, pub kind: StageKind, pub stage: u16 }
  pub struct Turn { pub slot: u8, pub kind: StageKind, pub count: u8 } // moves left in this turn
  pub enum GameCtx<'a> { First { starters: &'a [u16] }, Later { legal: &'a [u16], bans: u8, prev_winner: u8 } }
  pub struct Selection { pub pool: Vec<u16>, pub removed: Vec<u16>, pub turn: Option<Turn>, pub chosen: Option<u16> }
  pub enum StageError { NotYourTurn, WrongKind, NotInPool, AlreadyRemoved, Complete, BadSlot }
  pub fn replay(ctx: GameCtx, moves: &[Move]) -> Result<Selection, StageError>
  pub fn apply(ctx: GameCtx, moves: &[Move], next: Move) -> Result<Selection, StageError>
  ```
  `replay` validates a stored history move by move; `apply` = `replay` of `moves + [next]`. Strike turn sizes per spec §6.3 (turn k: 1 if k = 0, else `min(2, remaining − 1)`; slot 0 first, alternating). Later games: `bans` consecutive `Ban` moves by `prev_winner`, then one `Pick` by the other slot from the rest of `legal`. Duplicate stage ids in the input lists count once.

- [ ] **Step 1: Write the failing tests** (`tests/stages.rs`):
  - `five_starters_strike_1_2_1`: starters `[31,32,28,3,8]`; turns observed: (0,Strike,1), (1,Strike,2), (1,Strike,1), (0,Strike,1); after 4 strikes `chosen == Some(remaining)`, `turn == None`.
  - `three_starters_1_1`, `seven_starters_1_2_2_1`, `one_starter_is_chosen_at_once`.
  - `later_game_winner_bans_two_loser_picks`: legal 6 stages, bans 2, prev_winner 1: two `Ban` by slot 1 then `Pick` by slot 0 → `chosen == Some(pick)`.
  - `errors`: move by wrong slot → `NotYourTurn`; `Pick` during strikes → `WrongKind`; stage not in pool → `NotInPool`; struck stage again → `AlreadyRemoved`; any move after `chosen` → `Complete`; slot 2 → `BadSlot`; loser picking a banned stage → `AlreadyRemoved`.
- [ ] **Step 2: Run** `cargo test -p bracket --test stages` — Expected: FAIL.
- [ ] **Step 3: Implement** `stages.rs`.
- [ ] **Step 4: Run** `cargo test -p bracket` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "bracket: stage strike/ban/pick engine"`

### Task 4: Zitadel hooks, account events and anonymise (wave 2)

Review against `lane-zitadel-review.md` I5 and I7 before merging.

**Files:**
- Create: `crates/server/tests/hooks.rs`
- Modify: `crates/server/src/anonymise.rs` (stub from Task 2), `crates/server/src/routes/hooks.rs`, `docs/api.md` (§8.1)

**Interfaces:**
- Consumes: `identity::{verify_hook, sha256_hex}`, `IdentityConfig.{hook_keys, hook_peers, project_id, org_id, discord_idp_id}`, `AppState.trusted`-style CIDR parsing (`net::parse_cidrs`), `accounts::{bind_discord, rekey_player, placeholder_key}`, `audit::{record, Actor::Zitadel}`.
- Produces: routes `POST /hooks/zitadel/idp-intent`, `/token`, `/user-event` (404 when identity is off or the peer is outside a non-empty `hook_peers`; body limit 256 KiB → 413); the `(t, v1)` replay cache is a `static LazyLock<Mutex<HashMap<(i64, String), i64>>>` in `hooks.rs`, pruned on insert; `pub async fn anonymise::anonymise(db: &SqlitePool, sub: &str) -> Result<bool, sqlx::Error>`; `pub async fn anonymise::set_disabled(db, sub: &str, disabled: bool) -> Result<bool, sqlx::Error>`.

- [ ] **Step 1: Write the failing tests** (`tests/hooks.rs`, `TestApp::spawn_identity`; a local `sign(key, body, t)` builds `zitadel-signature`):
  - `hooks_reject_bad_signatures`: each hook with no header, with another hook's key, with `t` 301 s old and 301 s ahead, and with a body changed after signing (one byte of whitespace) → 400 `{"error":"bad request"}`; the same valid request sent twice → second 400; nothing written by any refused call.
  - `hooks_never_log_bodies`: with a tracing capture (the existing `logging` test helper), an idp-intent body containing `"accessToken":"SECRET-TOKEN"` → no log line contains `SECRET-TOKEN` or the Discord username.
  - `idp_intent_fills_add_human_user`: body `{"response":{"idpInformation":{"idpId":"idp1","rawInformation":{"id":"80351110224678912","username":"nelly","global_name":"Nelly","avatar":"8342729096ea3675442027381ff50dfe"},"oauth":{"accessToken":"SECRET-TOKEN"}},"addHumanUser":{"idpLinks":[{"idpId":"idp1","userId":"80351110224678912","userName":"nelly"}],"password":{"password":"x"},"organization":{"orgId":"o1"}}}}` → 200; `addHumanUser.username == "discord-80351110224678912"`, `profile.givenName == "Nelly"`, `profile.familyName == "(Discord)"`, `email == {"email":"80351110224678912@users.invalid","isVerified":true}`, metadata keys `discord_id`, `discord_username`, `discord_avatar_url` with base64 values decoding to the id, `nelly`, `https://cdn.discordapp.com/avatars/80351110224678912/8342729096ea3675442027381ff50dfe.png`; `idpLinks`, `password` and `organization` byte-equal to the input; `discord_profiles` row present.
  - `idp_intent_update_keeps_username`: `updateHumanUser: {"username":"discord-1","profile":{…}}` → `profile` rewritten, `username` unchanged.
  - `idp_intent_shapes`: same user under `rawInformation.User` → same result; `global_name: null` → givenName `nelly`; avatar `a_…` → `.gif`; a 200-character `global_name` with a control character → stored cut to 64, control character removed.
  - `idp_intent_rejects`: `idpId "other"` → 400; `"id": "12a"` → 400; no `addHumanUser` → `response` returned unchanged.
  - `token_hook_groups_claim`: `{"user":{"id":"u1"},"user_metadata":[{"key":"discord_id","value":"<b64 '42'>"}],"user_grants":[{"projectId":"p1","orgId":"o1","roles":["autobracket-admins"]},{"projectId":"p1","orgId":"o1","roles":["autobracket-admins","x"]},{"projectId":"p2","orgId":"o1","roles":["evil"]},{"projectId":"p1","orgId":"o9","roles":["evil2"]}]}` → `{"append_claims":[{"key":"groups","value":["autobracket-admins","x"]}]}`; account `u1` with `discord_id 42`. No grants → `"value": []`. A metadata `discord_username` of `"autobracket-admins"` adds nothing to groups.
  - `token_hook_never_rebinds`: account `u1` bound to `42`; hook with metadata `discord_id 43` → still `42`, groups still returned; account `u2` with metadata `42` → no binding for `u2`.
  - `user_events_disable_and_enable`: `user.deactivated` → `/account/api/ping` for that sub 403; `user.reactivated` → 204; `user.locked`/`user.unlocked` likewise; audit rows `account.disabled`/`account.enabled`.
  - `user_removed_anonymises`: account with player `Mango` (key K, alias K2), an entered event with a done set, a link code, a profile: `{"aggregateID":"<sub>","event_type":"user.removed"}` → 200; tag matches `^anon_[A-Z2-7]{6}$`; its `discord_profiles`, `link_codes`, `player_keys` rows gone; K signing `GET /v1/me/next?event=1` → 403; `/v1/events/1` lists the anonymised tag; a still-valid token for the sub → 401 `this account was deleted` and no new account row; audit `account.removed` actor `zitadel`; the same event again → 200, no new audit row; unknown sub → 200.
  - `hook_peers_enforced`: `hook_peers = ["10.9.9.0/24"]` → any hook from 127.0.0.1 is 404.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test hooks` — Expected: FAIL.
- [ ] **Step 3: Implement** per spec §4: verify the signature on the raw bytes first, then parse; modify only the listed fields of the `response` JSON value. Log only the hook name, outcome and (idp-intent, at debug) the key names of `rawInformation`.
- [ ] **Step 4: Run** `cargo test -p autobracket-server` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "hooks: Zitadel idp mapping, project-scoped groups, account events, anonymise"`

### Task 5: Account page, link codes, web join, header sign-in (wave 2)

**Files:**
- Create: `crates/server/tests/account.rs`
- Modify: `crates/server/src/link.rs` (stub from Task 2), `crates/server/src/routes/account.rs`, `crates/server/assets/pages/account.{html,js}`, `crates/server/assets/site.js`, `crates/server/assets/pages/event.js`, `docs/api.md` (§8.2)

**Interfaces:**
- Consumes: `WebUser`, `accounts::*`, `audit::record`, `view::page_with_img_src`, the existing sign-up insert (call the same SQL path as `events::signup` — move its insert into `pub(crate) async fn enter(conn, event, player_key, engine_build) -> Result<(), ApiError>` inside `routes/events.rs`; Task 5 is the only task touching `events.rs`).
- Produces: routes of spec §5.1 plus `GET /account` → 308, and `POST /v1/me/link` (signed `AnyKey`, registered in `account::router()`); `link::issue(conn, account_id) -> Result<(String, i64), LinkError>`; `link::redeem(conn, code: &str, signer: &str) -> Result<String /* tag */, LinkError>`; `LinkError { NoPlayer, UnknownCode, TakenElsewhere(String), Db }`.
- Produces JS: `AB.me()` → `Promise<AccountView|null>` (fetch `/account/api/me`, `redirect: "manual"`, `null` on anything but 200; cached per page load); `AB.site()` → `Promise<SiteInfo>`; the nav link `#nav-account` (markup from Task 10; if absent, do nothing) shows "Sign in" or "Signed in as <name>"; `AB.howToEnter(eventId)` gains the Join / pick-a-tag / sign-in line of spec §5.3 above the testers block.

- [ ] **Step 1: Write the failing tests** (`tests/account.rs`, `TestApp::spawn_identity`):
  - `account_flow`: `GET /account/api/me` → `tag == null`, `clients == []`, `Cache-Control: no-store`; `POST /account/api/player {"tag":"Mango"}` → 201 with `tag "Mango"` and one client with `placeholder: true`; again → 409; `{"tag":"x"}` → 400; a second account taking `"mango"` → 409.
  - `settings_validate_connect_code`: `"ABC#123"` ok; `"abc#123"`, `"ABCDEFGH#1"`, `"ABC#12345"` → 400.
  - `link_new_client`: issue code (8 chars of the spec alphabet, `expires_at - now` in 595..=600), fresh key signs `POST /v1/me/link` → 200 `{"tag":"Mango"}`; that key's `GET /v1/me/next?event=…` works; `/account/api/me` lists 2 clients; audit `client.link`.
  - `link_adopts_cli_player`: CLI-registered `Armada` + an account without a player → adoption; account `tag == "Armada"`.
  - `link_never_moves_a_player` (Review Focus 3): code reused → 404; expired (set `expires_at` in the past via SQL) → 404; key of another player while the account has a player → 409 and both players unchanged; key already aliased to another account → 409; account with no player and an unregistered key → 409 `pick a tag on your account page first`; a new code replaces the old (old → 404).
  - `unlink`: deleting an alias → 204, key now 403 on `/v1/me/next`; deleting the canonical real key of an adopted player → 204, the player keeps its results under a placeholder key; another account's key → 404.
  - `web_join`: event in sign-up → `POST /account/api/events/{id}/join` 201, `entrants.engine_build == ""`; again → 409; event running → 409; no player → 409; missing CSRF → 403.
  - `slashless_redirects`: `GET /account` → 308 `Location: /account/` with no identity headers.
  - `account_page_csp`: `GET /account/` → 200 HTML, CSP contains `img-src data: https://cdn.discordapp.com`.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test account` — Expected: FAIL.
- [ ] **Step 3: Implement.** Page sections: Profile (avatar `<img>` with `alt=""`, names, tag picker when no tag, connect code, the two toggles — `share_analysis` labelled "Share my game analysis publicly (only when my opponent also shares)"), Game clients (list, "Get a link code" showing the code large with the expiry countdown and "Type this code in the game's Brackets screen", Unlink buttons), Events entered, Sign out (link to `sign_out_url`). All writes send `x-ab-csrf: 1`.
- [ ] **Step 4: Run** `cargo test -p autobracket-server` — Expected: PASS. Manually: `cargo run -p autobracket-server` with an identity test config and `curl` the page; check the browser console shows no CSP violation (headless Playwright if available).
- [ ] **Step 5: Commit** `git commit -m "account: profile, link codes, linked clients, web join, header sign-in"`

### Task 6: Admin web and audited organiser actions (wave 2)

**Files:**
- Create: `crates/server/tests/admin_web.rs`
- Modify: `crates/server/src/routes/admin.rs`, `crates/server/src/routes/admin_web.rs`, `crates/server/src/review.rs` (accept an `&Actor` for the resolution's `by`), `crates/server/assets/pages/admin.{html,js}`, `docs/api.md` (§8.3; `QueueItem.reason` in §5 gains `"organiser"`)

**Interfaces:**
- Consumes: `AdminUser`, `audit::{record, list, Actor}`, `accounts::rekey_player`.
- Produces: each organiser operation as `pub(crate) async fn <op>(state: &AppState, actor: &Actor, …) -> Result<…, ApiError>` in `admin.rs` (`create_event_as`, `start_event_as`, `schedule_as`, `remove_entrant_as`, `create_invite_as`, `resolve_as`, `decide_as`, `rekey_as`), each writing its audit row in the same transaction where one exists; the signed handlers (`Actor::Key`) and web handlers (`Actor::from(&admin.0)`) both call them. Routes of spec §5.2, including signed `POST /v1/admin/sets/{event}/{set}/decide` registered in `admin_web::router()`. `admin.rs`'s private rekey body is replaced by `accounts::rekey_player`.

- [ ] **Step 1: Write the failing tests** (`tests/admin_web.rs`):
  - `admin_web_mirrors_signed_api`: as `TestWeb::admin`, create event (201, same `EventSummary` shape), schedule (204), two players sign up, start (200), invite (201 code of 12 base32 chars); `GET /admin/api/queue` equals the signed `GET /v1/admin/queue`.
  - `decide_award_and_dq`: running event, ready set 0: `POST /admin/api/sets/{e}/0/decide {"Award":{"slot":1}}` → 204, set done for slot 1, `decided_by` `award`; `"Replay"` → 400; on a decided set → 409; signed `/v1/admin/sets/{e}/{s}/decide {"Dq":{"slot":0}}` → 204.
  - `every_action_is_audited`: after the above, `GET /admin/api/audit` lists newest first `set.decide`, `event.start`, `invite.create`, `event.schedule`, `event.create` with web actor `account`/display name and signed actor `key`/16-hex prefix; `?limit=2` returns 2; `?before=<id>` pages; `limit=0` and `201` → 400.
  - `player_cannot_use_admin_api`: `TestWeb::player` → 403 on every `/admin/api/*` route; `/admin/` page 403.
  - `rekey_via_web`: 204 and the player's old key 403s; audit `player.rekey`.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test admin_web` — Expected: FAIL.
- [ ] **Step 3: Implement.** Page: Events (list from `/v1/events`, create form with name/format/best_of/starts_at/description, per-event Start, Schedule, remove entrant), Queue (each item's reason, tags, detail, evidence summary; Award slot 0/1, Replay, DQ slot 0/1 buttons with a confirm step built into the page, since `confirm()` is unavailable in some embeds), Invites, Rekey, Audit log (paged by "Older"). On the 403 with `mfa_setup_url`, show "Admin pages need a recent sign-in with a second factor" with links to `mfa_setup_url` and `sign_in_url`. Every `/admin/` response also carries `X-Frame-Options: DENY` (spec §2; test it in `player_cannot_use_admin_api`'s sibling `admin_pages_frame_deny`).
- [ ] **Step 4: Run** `cargo test -p autobracket-server` — Expected: PASS (existing `tests/admin.rs` unchanged).
- [ ] **Step 5: Commit** `git commit -m "admin: web organiser pages, set decide, audit log"`

### Task 7: Home, badge, me (wave 2)

**Files:**
- Create: `crates/server/tests/home.rs`
- Modify: `crates/server/src/routes/me.rs`, `docs/api.md` (§8.4)

**Interfaces:**
- Consumes: `sets::{next_for, entrant_id, bracket_set, slot_of}`, `lifecycle::load_bracket_on`, proto `Home`, `HomeAction`, `MyEvent`, `Badge`, `MeStatus`.
- Produces: `GET /v1/me`, `GET /v1/me/home`, `GET /v1/me/badge` (spec §6.1–6.2); `pub(crate) fn eta_secs(bracket: &Bracket, set: SetId, median_secs: u32) -> Option<u32>` (longest chain of undecided feeder sets × median).

- [ ] **Step 1: Write the failing tests** (`tests/home.rs`):
  - `me_status`: CLI player → `{"tag":"Mango","linked":false,"display_name":null}`.
  - `home_lists_open_entries_only`: entered one sign-up event, one running, one complete → `events` has the two, oldest first, each with its `next`.
  - `home_set_up_and_badge`: 4-player single elim started; seed 0's `action` is `SetUp` with `ready [false,false]`, `badge true`; set `ready0_at` via SQL → `badge false`, `ready [true,false]`.
  - `home_upcoming_with_eta`: in an 8-player bracket, the winner of R1 set 0 waiting on set 1 → `Upcoming` with `opponent_tag null`, `eta_secs == Some(600)`; after three played sets lasting 100, 200, 300 s (set `first_upload_at`/`judged_at` via SQL) → `Some(200)`.
  - `badge_etag`: two calls give the same `ETag`; with `If-None-Match` → 304 empty body; after the state changes → 200 with a new ETag.
  - `eta_secs` unit tests in `me.rs`: chain of 2 undecided feeders → 2 × median; next set Ready → 0.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test home` — Expected: FAIL.
- [ ] **Step 3: Implement.** Median: durations from `MIN(games.first_upload_at)` to `MAX(games.judged_at)` per done set with counted games in this event.
- [ ] **Step 4: Run** `cargo test -p autobracket-server` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "me: home, badge with ETag, status"`

### Task 8: Set flow, ready, stage selection, stage enforcement (wave 2)

**Files:**
- Create: `crates/server/tests/setflow.rs`
- Modify: `crates/server/src/routes/setflow.rs`, `crates/server/src/judge.rs`, `crates/server/src/worker.rs` (pass the chosen stage), `crates/server/src/review.rs` only inside the `Resolve::Replay` arm (delete that game's `stage_moves`) — coordinate: Task 6 also edits `review.rs` (signature of `resolve_queue_item`); keep this edit to the `Replay` arm's body so the merge is mechanical, `docs/api.md` (§8.5)

**Interfaces:**
- Consumes: `bracket::stages::{replay, apply, GameCtx, Move}`, `sets::{current_game, entrant_id, bracket_set, slot_of, next_for}`, `PlayerUpload`/`Player`, proto `ReadyReq`, `StageMoveReq`, `StageState`, `SetFlow`.
- Produces: `POST`/`DELETE /v1/sets/{event}/{set}/ready`, `POST /v1/sets/{event}/{set}/games/{n}/stage`, `GET /v1/sets/{event}/{set}/flow`; `pub(crate) async fn chosen_stage(conn, event, set, game_no, ruleset: &Ruleset) -> Result<Option<u16>, ApiError>`; judge's legality check gains `agreed_stage: Option<u16>` → `Illegal("stage")` on mismatch.

- [ ] **Step 1: Write the failing tests** (`tests/setflow.rs`):
  - `ready_and_unready`: player of the set → 204, `flow.ready == [true,false]` for them and `[false,true]` for the opponent; `DELETE` → 204; non-player → 403; `accepted_builds = ["b1"]` and `engine_build "b2"` → 403 with `accepted == ["b1"]`; web-joined entrant (`engine_build ""`) sends `"b1"` → stored `"b1"`.
  - `stage_moves_are_turn_checked` (Review Focus 5): game 1 with default ruleset (6 legal, starters = all 6 → turns 1,2,2): slot 1 first → 409; slot 0 strikes 31 → 200 `StageState` with `turn.who == "opponent"` for slot 0's view; slot 0 again → 409; slot 1 strikes 31 → 409; `kind: "pick"` → 409; `n = 2` → 409; finishing all strikes → `chosen` set, `phase "playing"`; any further move → 409; stored row count unchanged after each 409.
  - `counterpick_after_game_1`: winner of game 1 bans two, loser picks; `flow.stage.mode == "counterpick"`.
  - `replay_resets_selection`: complete selection, organiser `Replay` → selection empty again.
  - `stage_mismatch_is_illegal`: chosen stage 31, both upload a replay on stage 32 (fixture builder from `replay::testkit`) → verdict `{"Illegal":"stage"}`; without moves → judged as today (`Verified`/`PeerOnly`).
  - `flow_phases_and_verified`: walk `ready_check → stage_select → playing → judging → done`; `verified` false while a counted game is `PeerOnly`, true when all are `Verified` (use `FakeResim` with `Match`); `after` is the player's `Next` once done, null before; `games[i].winner` from the signer's view.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test setflow` — Expected: FAIL.
- [ ] **Step 3: Implement** per spec §6.2–6.4 (moves for a game are re-validated with `stages::replay` on every read; a stored history that no longer validates is a 500 logged as corrupt).
- [ ] **Step 4: Run** `cargo test --workspace` — Expected: PASS (`tests/judge.rs` unchanged).
- [ ] **Step 5: Commit** `git commit -m "setflow: ready, server-held stage strikes and picks, set flow"`

### Task 9: Live relay receive (wave 2)

**Files:**
- Create: `crates/server/tests/live.rs`
- Fill: `crates/server/src/live.rs` (stub from Task 2)
- Modify: `Cargo.toml`/`crates/server/Cargo.toml` (axum `ws`, dev `tokio-tungstenite`), `crates/server/src/{state.rs,lib.rs}` (`AppState.live: Arc<LiveHub>`, `.with_upgrades()` on the hyper connection, a 30 s sweep task), `crates/server/src/routes/live.rs`, `crates/replay/src/lib.rs` (+ `pub fn parse_stream(raw: &[u8]) -> Result<ReplayFacts, ReplayError>` on an event stream without the file header, a missing Game End allowed), `crates/server/assets/pages/stream.js`, `docs/api.md` (§8.6)

**Interfaces:**
- Consumes: `auth::PlayerHead`, `sets::{current_game, bracket_set}`, proto `LiveGame`, `LivePort`.
- Produces: `LiveHub { fn connect(&self, game: GameKey) -> (u64 /*generation*/, u32 /*next cursor*/); fn push(&self, game: GameKey, gen: u64, msg: Incoming) -> Push; fn snapshot(&self) -> Vec<LiveGame>; fn sweep(&self, now) }`, `GameKey { event: i64, set: u32, game_no: u32 }`; routes `GET /v1/sets/{event}/{set}/games/{n}/live` (upgrade) and `GET /v1/live`.

- [ ] **Step 1: Write the failing tests** (`tests/live.rs`, client via `tokio-tungstenite`, request signed with `proto::sign_request` over `GET` and the path):
  - `upgrade_checks`: unsigned → 401; guest key → 403; unknown set → 404; `n = 2` while game 1 is current → 409; all before `101`.
  - `stream_matches_upload`: split a fixture `.slp`'s raw element into whole events (sizes from 0x35) in chunks, send `connect_request`, `start_game`, `game_event`s, `end_game`; `connect_reply.cursor == 0` first; the hub's stored bytes equal `replay::raw_element(file)`; `GET /v1/live` shows stage, two ports with characters and final stocks, `ended true`.
  - `resume_gap_and_replacement` (Review Focus 4): after 10 messages open a second connection → `connect_reply.cursor == 10`; the first socket's further sends are ignored and it is closed; resend cursors 5..9 (duplicates ignored), then jump to 15 → `gaps == 1`, stored bytes = messages 0..9 + 15.. exactly.
  - `limits`: a 70 KiB frame → connection closed; malformed JSON → close 1003; 33rd socket → 503.
  - `nothing_reaches_results`: after a full live game, `games`/`uploads` tables are empty.
  - `parse_stream` unit test in `replay`: raw element of a fixture parses to the same `ReplayFacts` as `parse(file)`; the first half of it parses with `end == None`.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test live && cargo test -p replay parse_stream` — Expected: FAIL.
- [ ] **Step 3: Implement** per spec §7. Stream page: poll `/v1/live` every 2 s (separate timer from the 10 s now-playing poll); add a `<p class="row-line live">` under the matching set line; stage and character name tables (external ids) live in `stream.js`.
- [ ] **Step 4: Run** `cargo test --workspace` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "live: signed WebSocket relay receive and now-playing stocks"`

### Task 10: Privacy, terms, site info, shell nav and footer (wave 2)

**Files:**
- Create: `crates/server/tests/site.rs`
- Modify: `crates/server/src/routes/site.rs`, `crates/server/assets/pages/{privacy,terms}.{html,js}`, `crates/server/assets/shell.html`, `crates/server/assets/site.css` (footer only), `docs/api.md` (§8.7; the public-route sentence in §2 gains `/privacy`, `/terms`, `/v1/site`, `/v1/live`)

**Interfaces:**
- Produces: `GET /privacy`, `GET /terms`, `GET /v1/site` → `SiteInfo`; shell nav `<a href="/account/" id="nav-account" hidden>Account</a>` (Task 5's script unhides it) and `<footer class="site-foot"><a href="/privacy">Privacy</a> · <a href="/terms">Terms</a></footer>` after `{{body}}`.

- [ ] **Step 1: Write the failing tests** (`tests/site.rs`): `site_info` (identity off → `{"identity":false,"operator":null,"contact":null}`; `[site]` set and identity on → values); `privacy_and_terms_pages` (200 HTML, CSP like every page, body contains `users.invalid`, `identify`, `no off-site copy`, `anonymis`); `every_page_has_footer_and_account_link` (the unit test in `view.rs` already iterates `PAGES` — extend it: every page contains `id="nav-account"` and `href="/privacy"`).
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test site` — Expected: FAIL.
- [ ] **Step 3: Implement** with the copy of spec §5.3 (plain words, second person). Operator and contact are filled from `/v1/site` by the page script; when absent the sentence reads "Ask the organisers of this server."
- [ ] **Step 4: Run** `cargo test -p autobracket-server` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "site: privacy and terms pages, account link, footer"`

### Task 11: Client crate API and commands (wave 2)

**Files:**
- Modify: `crates/client/src/api.rs`, `crates/client/src/main.rs`, client tests (`crates/client/tests/*` — extend the existing test using `TestApp`)

**Interfaces:**
- Consumes: proto types of Task 1; server routes of Tasks 5, 7, 8 (tests run against `TestApp` and therefore need waves 1–2 merged; write the tests now, mark them `#[ignore]` only if the routes are absent on the branch, and remove `#[ignore]` at merge — the controller checks this).
- Produces: `Api::{link(&self, code: &str) -> Result<LinkResult>, me() -> Result<MeStatus>, home() -> Result<Home>, badge(etag: Option<&str>) -> Result<Option<(Badge, String)>> /* None on 304 */, ready(event, set) -> Result<()>, unready(event, set) -> Result<()>, stage(event, set, game_no, &StageMoveReq) -> Result<StageState>, flow(event, set) -> Result<SetFlow>}`; CLI commands `link CODE`, `me`, `home`, `ready EVENT SET`, `stage EVENT SET GAME strike|ban|pick STAGE`, `flow EVENT SET` printing plain text.

- [ ] **Step 1: Write the failing tests:** `link_then_home` (account via `TestApp::spawn_identity` + `web()`, code issued, `Api::link` → tag; `Api::home` → `SetUp` after start); `badge_304` (`badge(Some(etag))` → `None`); `stage_round_trip`.
- [ ] **Step 2: Run** `cargo test -p autobracket-client` — Expected: FAIL.
- [ ] **Step 3: Implement**; `ready` sends the configured engine build.
- [ ] **Step 4: Run** `cargo test --workspace` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "client: link, home, badge, ready, stage and flow"`

### Task 12: nginx, Zitadel wiring and end-to-end (wave 3, controller executes; not an implementer task)

Follow `~/certs/README.md` and the global CLAUDE.md: `docker exec mailu-front-1 nginx -t` before `~/certs/reload-mailu-front.sh`; ask the owner before touching DNS, certificates, ufw or other sites on the shared front. Binding: `lane-zitadel-review.md` C1, I3, I4, I7 and minors M1, M3, M5, M10.

- [ ] **Step 1: Ask the owner** for the ufw allow `from 10.203.48.0/28 to 172.17.0.1 port 8790` (hooks reach the app without public DNS, spec §4). If refused: targets use `https://bracket.gsd.sh/hooks/zitadel/…`, nginx proxies `/hooks/zitadel/` (server-level headers only), `hook_peers` unset.
- [ ] **Step 2: Deploy the app** (`cargo build --release -p autobracket-server`, `sudo install`, restart) with `[identity]` filled (issuer `https://auth.gsd.sh`, `client_id` of `bracket-web`, `project_id`, `org_id`, `discord_idp_id` from the Zitadel setup, `hook_peers = ["10.203.48.0/28"]`) and the hook keys temporarily random; `proxy_secret` from `openssl rand -hex 32` into `/etc/autobracket/autobracket.toml` (root-only) and into `/mailu/overrides/nginx-conf.d/bracket-secret.inc` (root-only, `proxy_set_header X-Autobracket-Proxy <secret>;`). Confirm `trusted_proxies` still holds the Mailu front only and `curl https://auth.gsd.sh/oauth/v2/keys` works from the VPS.
- [ ] **Step 3: Create the three Actions v2 targets** (README step 5; endpoints `http://172.17.0.1:8790/hooks/zitadel/{idp-intent,token,user-event}`) with the `ab-admin-sa` PAT over the internal API; write the signing keys into the config; restart. No executions yet.
- [ ] **Step 4: oauth2-proxy**: add `offline_access` to the scopes and `OAUTH2_PROXY_COOKIE_REFRESH=10m` in `/srv/autobracket-auth`; recreate the container.
- [ ] **Step 5: Write `bracket.gsd.sh.conf`** per spec §8 (server-level headers via variables, self-contained `/oauth2/auth*`, `bracket-security-headers.inc` incl. HSTS in every location with `add_header`, one `Set-Cookie` forward in `/account/` and `/admin/`, `X-Forwarded-For $remote_addr`, encoded `rd`, no `X-Auth-Request-Redirect`, `/hooks/` 404, the live WebSocket location with a server-level `map` for `Connection`). Commit a copy without the secret as `deploy/nginx/bracket.gsd.sh.conf.example` plus the two `.inc` templates. Back up the old file, `nginx -t`, reload.
- [ ] **Step 6: Create the executions**: idp-intent (response), token (`preuserinfo`, `preaccesstoken`), user-event (`user.removed`, `user.deactivated`, `user.locked`, `user.reactivated`, `user.unlocked`).
- [ ] **Step 7: Revoke the setup PAT** (`ab-admin-sa`: delete the PAT or deactivate the user), shred `machinekey/admin.pat`, remove the `machinekey` mount from the compose file in `/srv/autobracket-auth` and on `lane-zitadel`, recreate the container (review I3). Further admin work uses short-lived PATs.
- [ ] **Step 8: Checks, in order:**
  (a) `/` and `/v1/events` public 200; `/hooks/zitadel/token` from outside → 404.
  (b) `/account/` → Discord → back signed in, page shows the Discord name; the journal shows the idp-intent outcome and `rawInformation` key names — if the nesting differs from spec §4, send it back to Task 4 before continuing; note whether `updateHumanUser`/`updateUser` appears on a second login; decode the stored ID token (oauth2-proxy `/oauth2/userinfo` or the app's debug log of claim names) and assert it carries `groups` (review M10).
  (c) `/admin/` 403 for a player; grant the role, enrol TOTP, sign out and in: `/admin/` works; without TOTP the 403 shows the MFA link; remove the role → refused within 10 minutes (cookie refresh).
  (d) **Forged headers, review C1:** point `/account/`, `/admin/`, `/oauth2/` and `/` temporarily at an echo upstream (`nc -l -p 18790` or `python3 -m http.server`-style echo on 172.17.0.1, root-only, removed afterwards), and with **and** without a valid session cookie send every identity name (`X-Auth-Request-User/-Groups/-Email/-Preferred-Username/-Access-Token`, `X-Id-Token`, `X-Forwarded-User/-Groups/-Email/-Preferred-Username/-Access-Token`, `Authorization`, `X-Autobracket-Proxy`) plus `X-Real-IP: 1.2.3.4` and `X-Forwarded-For: 1.2.3.4`. Assert the echo sees `Host: bracket.gsd.sh`, the real client IP, the real proxy secret, and none of the forged values (identity values only from `auth_request` on the protected paths). Restore the config. Then directly: `curl -H 'X-Auth-Request-User: 1' -H 'X-Autobracket-Proxy: guess' -H 'X-Id-Token: Bearer e30.e30.x' http://172.17.0.1:8790/admin/api/whoami` → 401.
  (e) Security headers incl. HSTS present on `/`, `/account/`, `/admin/` responses (`curl -sI`).
  (f) Link a client: code on `/account/`, `autobracket-client link CODE` on the desktop, `autobracket-client home`.
  (g) Live: desktop runs the engine with `--relay-url wss://bracket.gsd.sh/v1/sets/…/live` in a test event; `/stream` shows stocks.
  (h) Deactivate a test user in the console → `/account/` 403 at once; reactivate; delete it → tag becomes `anon_…`, audit row, its old cookie gets 401.
  (i) Sign out returns to `/` signed out. (j) `docker stats` RAM note for desktop.
- [ ] **Step 9:** Update `docs/architecture.md` statuses (B7, A4 identity live), the identity design doc (oauth2-proxy refresh, verified tokens in the app, hook transport, blast-radius note, outcomes of the open items), and commit `ops: bracket.gsd.sh identity integration live`.

### Task 13 (optional): Discord "your set is up" pings (wave 4)

**Files:**
- Create: `crates/server/migrations/0007_notify.sql` (`events.discord_webhook TEXT NULL`, `set_pings(event_id, set_id, slot, state TEXT CHECK (state IN ('sent','failed')), attempts, PRIMARY KEY (event_id, set_id, slot))`), `crates/server/src/notify.rs`, `crates/server/tests/notify.rs`
- Modify: `crates/server/src/{lib.rs,state.rs}` (spawn the task; a `Notifier` trait object in `AppState` so tests capture posts), `crates/server/src/routes/admin_web.rs` and `admin.rs` (`POST /admin/api/events/{id}/discord-webhook` and signed `/v1/admin/events/{id}/discord-webhook`, body `{"url": str|null}`, audit `event.webhook`), admin page field, `docs/api.md`

**Interfaces:**
- Produces: `#[async_trait] pub trait Notifier { async fn post(&self, url: &str, content: &str, user_id: &str) -> Result<(), String>; }`, `DiscordNotifier` (reqwest, 10 s timeout, body `{"content", "allowed_mentions": {"users": [id]}}`), `pub async fn notify_due(state: &AppState) -> anyhow::Result<usize>`.

- [ ] **Step 1: Failing tests:** `url_must_be_discord_webhook` (`https://discord.com/api/webhooks/1/abc` ok; `http://…`, `https://evil.test/api/webhooks/1/a`, `https://discord.com/x` → 400); `ping_once_per_set_and_player` (two accounts with `notify` on/off, set becomes ready → exactly one post `"<@42> your set vs Armada is up (Winners round 1)"`, `notify_due` again posts nothing); `failure_retries_three_times` (fake failing notifier → 3 attempts then `failed`); `webhook_never_shown` (`GET /v1/events/{id}` and admin views contain no webhook URL, only `"discord_webhook_set": true` on the admin event view).
- [ ] **Step 2: Run** — Expected: FAIL. **Step 3: Implement** (task every 5 s). **Step 4: Run** `cargo test --workspace` — PASS. **Step 5: Commit** `git commit -m "notify: Discord set-up pings"`
