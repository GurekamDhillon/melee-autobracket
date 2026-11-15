# Verified Auto-Bracket (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A self-hosted HTTP service that runs Melee brackets and advances them only from replay-verified results, plus a command-line reference client that drives the existing game binary.

**Architecture:** A Cargo workspace of five crates: a pure bracket engine, a minimal `.slp` reader, shared protocol types and request signing, the `autobracket` server (axum + SQLite, with an in-process or external verifier that shells out to `melee slpcheck`), and `autobracket-client`. The relay (`melee_relay`) and the game are untouched; the server only passes room codes between players.

**Tech Stack:** Rust stable, axum 0.8, tokio, sqlx (SQLite, bundled), ed25519-dalek 2, serde/serde_json, toml, clap 4, reqwest (client and tests), sha2, hex, tower_governor, tempfile (tests).

**Spec:** `docs/superpowers/specs/2026-11-15-verified-autobracket-design.md`

## Global Constraints

- Phase 1 only: this repo, **no changes to the melee repo or `melee_relay`**. The in-game menu is phase 2 and out of scope.
- No email, passwords, or other personal data. Identity is an Ed25519 public key (hex, 64 chars) plus a display tag.
- A set never advances on a single upload or on any client-stated outcome. The server derives the winner from the replay.
- Peer agreement compares the `.slp` **`raw` element**, never whole-file bytes (`startAt` differs per peer).
- `slpcheck` exit code 0 covers both `match` and `diverged`; verdicts come from the CSV `status` column, never the exit code.
- The verifier sets `SLPCHECK_VANILLA_SPAWN=1` and runs one `slpcheck` process per file with a timeout.
- Netplay port convention: port 1 = host, port 2 = guest. The server names bracket slot 0 the host.
- `melee play` has no stage striking (that lives in the lobby, which `melee play` rejects). In phase 1 players pick characters and stage on the in-game screens (`--np-css`), one engine process and one room code **per game**, and the server enforces stage legality after the fact.
- The engine exports `.slp` files only when its process exits; the parent's exit means the exports are complete.
- Storage is one SQLite file plus a replay directory. No Postgres, no external services.
- Must run on a GPU-less Linux VPS. The verifier is optional (`mode = "off"` runs on peer agreement + ruleset).
- The disc image is never committed, copied into the repo, or distributed.
- Rendering, deep stats, spectating, rankings, arbitrary-client mode: out of scope.
- Commit after every task; commit trailer per the session's attribution rule.

## Review Focus

1. **Same replay uploaded twice, or an upload for a game already counted** — expected: idempotent success for an identical re-upload, `409` otherwise, and the game is never counted twice. (Task 7)
2. **Server restarts mid-event** — expected: games caught mid-judgement are judged again after restart, and the single-upload timeout still fires from the stored first-upload time. (Task 10)
3. **Garbage, truncated, or oversized upload** — expected: `400`/`413`, no panic, nothing stored, and the other player's upload still proceeds to the timeout path. (Tasks 3, 7)
4. **Tiny and odd-sized events** — 1 entrant refuses to start with a clear error; 2, 3, 5 entrants run to completion with byes never requiring play; duplicate sign-up is rejected. (Tasks 1, 2, 6)
5. **A registered player acting on a set they are not in** — posting a room code or replay for someone else's set is `403` and changes nothing. (Task 7)

---

## File Structure

```
Cargo.toml                         workspace, shared dependency versions
crates/bracket/                    pure bracket engine, no IO
  src/lib.rs                       types, build(), Bracket::report()
  src/seed.rs                      seed positions
  src/single.rs  src/double.rs     structure builders
  tests/invariants.rs
crates/replay/                     minimal .slp reader
  src/lib.rs                       ReplayFacts, parse(), winner_port()
  src/raw.rs                       raw_element()
  src/events.rs                    event walking and field offsets
  src/testkit.rs                   SlpBuilder (feature "testkit")
crates/proto/                      shared by server and client
  src/lib.rs  src/sign.rs  src/types.rs  src/ruleset.rs
crates/server/                     lib `autobracket_server` + bin `autobracket`
  migrations/0001_init.sql
  assets/bracket.html
  src/main.rs                      clap: serve | admin | keygen | verify-worker
  src/lib.rs                       build_app()
  src/config.rs  src/db.rs  src/auth.rs  src/state.rs
  src/routes/{mod,players,events,sets,admin,quarantine,verify_jobs,view}.rs
  src/lifecycle.rs                 set/game state transitions, bracket advance
  src/judge.rs                     pure verdict logic
  src/resim.rs                     Resim trait, Slpcheck, ExternalResim, NoResim
  src/worker.rs                    judging loop
  src/admin_cli.rs
  src/testkit.rs                   TestApp, TestPlayer, FakeResim (feature "testkit")
  tests/*.rs
crates/client/                     bin `autobracket-client`
  src/main.rs  src/config.rs  src/api.rs  src/engine.rs  src/play.rs
deploy/autobracket.service  deploy/autobracket.example.toml
README.md
```

---

### Task 1: Workspace, seeding, single elimination

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `crates/bracket/{Cargo.toml,src/lib.rs,src/seed.rs,src/single.rs}`, `crates/bracket/tests/invariants.rs`

**Interfaces:**
- Produces (`bracket` crate):

```rust
pub type EntrantId = u32;          // index into the seed-ordered entrant list, 0 = top seed
pub type SetId = u32;
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format { SingleElim, DoubleElim }
pub enum Side { Winners, Losers, GrandFinal }
pub enum Slot { Empty, Bye, Entrant(EntrantId) }
pub enum SetState { Waiting, Ready, Done { winner: u8 }, Skipped }
pub struct Set { pub id: SetId, pub side: Side, pub round: u8, pub slots: [Slot; 2],
                 pub winner_to: Option<(SetId, u8)>, pub loser_to: Option<(SetId, u8)>,
                 pub state: SetState }
pub struct Bracket { pub format: Format, pub entrants: u32, pub sets: Vec<Set> }   // serde both ways
pub enum BracketError { TooFewEntrants, UnknownSet, NotReady, BadSlot }
pub fn build(format: Format, entrants: u32) -> Result<Bracket, BracketError>;
pub fn seed_positions(size: u32) -> Vec<u32>;   // size is a power of two
impl Bracket {
    pub fn report(&mut self, set: SetId, winner_slot: u8) -> Result<Vec<SetId>, BracketError>; // newly Ready sets
    pub fn ready_sets(&self) -> Vec<SetId>;
    pub fn is_complete(&self) -> bool;
    pub fn champion(&self) -> Option<EntrantId>;
    pub fn was_played(&self, set: SetId) -> bool;   // Done with two Entrant slots
}
```

Rules the tests pin: bracket size is the next power of two; missing entrants are `Bye`s given to the top seeds; a set with a `Bye` resolves itself (at build, or when its other slot fills) without `report`; `build` with fewer than 2 entrants is `TooFewEntrants`; slot 0 of a first-round set is the better seed.

- [ ] **Step 1: Set up the toolchain and workspace**

Rust is not installed on this server. Install per-user: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal` then `source ~/.cargo/env`. `git init -b main`. Workspace `Cargo.toml` with `members = ["crates/*"]`, `resolver = "2"`, and `[workspace.dependencies]` for the stack above. `.gitignore`: `target/`, `*.sqlite*`, `replays/`, `*.iso`.

- [ ] **Step 2: Write the failing tests** (`crates/bracket/tests/invariants.rs`)

```rust
#[test] fn seed_positions_8() { assert_eq!(seed_positions(8), vec![0, 7, 3, 4, 1, 6, 2, 5]); }
#[test] fn too_few() { assert!(matches!(build(Format::SingleElim, 1), Err(BracketError::TooFewEntrants))); }
#[test] fn single_8_first_round_pairs() {
    let b = build(Format::SingleElim, 8).unwrap();
    let r1: Vec<_> = b.sets.iter().filter(|s| s.round == 1).map(|s| s.slots).collect();
    assert_eq!(r1, [[E(0),E(7)],[E(3),E(4)],[E(1),E(6)],[E(2),E(5)]]);   // E = Slot::Entrant
    assert_eq!(b.ready_sets().len(), 4);
}
#[test] fn single_5_byes_go_to_top_seeds() {
    let b = build(Format::SingleElim, 5).unwrap();
    assert_eq!(b.ready_sets().len(), 1);                 // only 3v4 needs playing in round 1
    assert!(!b.sets.iter().any(|s| s.round == 1 && b.was_played(s.id)));
}
#[test] fn single_plays_n_minus_1_and_top_seed_wins_chalk() {
    for n in 2..=33 { let mut b = build(Format::SingleElim, n).unwrap();
        let played = play_out(&mut b, |_set| 0);        // helper: always report slot 0 until complete
        assert_eq!(played, n - 1); assert_eq!(b.champion(), Some(0)); }
}
#[test] fn report_not_ready() { /* reporting the final first => Err(NotReady); slot 2 => Err(BadSlot) */ }
#[test] fn serde_round_trip() { /* to_string then from_str yields an equal Bracket mid-event */ }
```

- [ ] **Step 3: Run** `cargo test -p bracket` — expected: FAIL, unresolved `build`.
- [ ] **Step 4: Implement** `seed.rs`, `single.rs`, and `Bracket` methods in `lib.rs`. `build(Format::DoubleElim, _)` may `todo!()` until Task 2.
- [ ] **Step 5: Run** `cargo test -p bracket` — expected: PASS.
- [ ] **Step 6: Commit** (include `docs/`): `feat(bracket): workspace, seeding, single elimination`

### Task 2: Double elimination

**Files:**
- Create: `crates/bracket/src/double.rs`
- Modify: `crates/bracket/src/lib.rs`, `crates/bracket/tests/invariants.rs`

**Interfaces:**
- Consumes: Task 1 types.
- Produces: `build(Format::DoubleElim, n)`. Structure: winners bracket as in single elim with `loser_to` into the losers bracket; losers bracket; one `Side::GrandFinal` set `round: 1` whose slot 0 is the winners-bracket champion; one `Side::GrandFinal` set `round: 2` (the reset). If slot 0 wins grand final 1 the reset becomes `Skipped` and the bracket is complete; otherwise the reset becomes `Ready` with the same two entrants. Any standard losers-bracket drop order is acceptable provided the invariants below hold.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn double_4_structure() {
    let b = build(Format::DoubleElim, 4).unwrap();
    assert_eq!(b.sets.len(), 7);                         // 3 winners, 2 losers, GF, reset
}
#[test] fn chalk_skips_reset() {
    let mut b = build(Format::DoubleElim, 8).unwrap();
    assert_eq!(play_out(&mut b, |_| 0), 14);             // 2n-2
    assert_eq!(b.champion(), Some(0));
    assert!(matches!(reset(&b).state, SetState::Skipped));
}
#[test] fn losers_side_winning_gf1_forces_reset() {
    // play chalk until GF1, report slot 1 there, assert reset is Ready with the same entrants,
    // report slot 1 again => champion is the losers-side entrant; played == 2n-1
}
#[test] fn loss_count_invariant() {
    for n in 2..=33 { for seed in 0..20u64 {
        let mut b = build(Format::DoubleElim, n).unwrap();
        let losses = play_out_random(&mut b, seed);      // returns Vec<u32> losses per entrant in played sets
        let champ = b.champion().unwrap() as usize;
        assert!(losses[champ] <= 1);
        for (e, l) in losses.iter().enumerate() { if e != champ { assert_eq!(*l, 2, "n={n} e={e}"); } }
        assert!(b.is_complete());
    }}
}
#[test] fn byes_never_need_play() { /* n in {3,5,6,7}: no set with a Bye slot is ever in ready_sets() */ }
```

- [ ] **Step 2: Run** `cargo test -p bracket` — expected: FAIL (`todo!()` panic).
- [ ] **Step 3: Implement** `double.rs` and the grand-final/reset handling in `report`.
- [ ] **Step 4: Run** `cargo test -p bracket` — expected: PASS.
- [ ] **Step 5: Commit** `feat(bracket): double elimination with reset`

### Task 3: Minimal `.slp` reader

**Files:**
- Create: `crates/replay/{Cargo.toml,src/lib.rs,src/raw.rs,src/events.rs,src/testkit.rs}`, `crates/replay/tests/parse.rs`

**Interfaces:**
- Produces (`replay` crate; `testkit` module behind feature `testkit`):

```rust
pub enum PlayerKind { Human, Cpu, Demo, Empty }
pub struct PlayerStart { pub port: u8 /*1-based*/, pub character: u8 /*external id*/, pub kind: PlayerKind, pub start_stocks: u8 }
pub struct PlayerFinal { pub port: u8, pub stocks: u8, pub percent: f32 }
pub struct GameEnd { pub method: u8, pub lras_initiator: Option<u8> /*port, 1-based*/ }
pub struct ReplayFacts { pub slippi_version: [u8; 4], pub stage: u16, pub timer_secs: u32, pub is_teams: bool,
    pub players: Vec<PlayerStart> /*non-Empty only*/, pub has_gecko: bool, pub last_frame: i32,
    pub end: Option<GameEnd>, pub finals: Vec<PlayerFinal> /*from the last frame, leaders only*/ }
pub enum ReplayError { NotSlp, Truncated, BadEventSizes, MissingGameStart }
pub fn raw_element(file: &[u8]) -> Result<&[u8], ReplayError>;
pub fn parse(file: &[u8]) -> Result<ReplayFacts, ReplayError>;
pub fn winner_port(f: &ReplayFacts) -> Option<u8>;

// testkit
pub struct SlpBuilder;  // new(), stage(u16), timer_secs(u32), stocks(u8), teams(bool), gecko(bool),
                        // player(port, character, PlayerKind), frame(frame, port, stocks, percent),
                        // end(method, lras: Option<u8>), start_at(&str), build() -> Vec<u8>
pub fn simple_game(winner_port: u8, start_at: &str) -> Vec<u8>;  // legal 4-stock FD game, ports 1 and 2
```

Field offsets come from the Slippi replay spec (`project-slippi/slippi-wiki`, `SPEC.md`, v3.18 layout): events Event Payloads `0x35`, Game Start `0x36`, Post-Frame Update `0x38`, Game End `0x39`, Gecko List `0x3D`. Unknown event codes are skipped using the Event Payloads size table. `has_gecko` is true when a Gecko List event has a non-empty payload. `last_frame` is the highest Post-Frame frame number.

`winner_port`: requires exactly two `Human` players and a `GameEnd`, else `None`. Method `7` (no contest): the `lras_initiator` loses; `None` if absent. Otherwise more final stocks wins; equal stocks, lower percent wins; still equal, `None`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn raw_ignores_metadata() {
    let a = simple_game(1, "2026-11-15T20:00:00Z"); let b = simple_game(1, "2026-11-15T20:00:07Z");
    assert_ne!(a, b); assert_eq!(raw_element(&a).unwrap(), raw_element(&b).unwrap());
}
#[test] fn facts() { let f = parse(&simple_game(2, "t")).unwrap();
    assert_eq!((f.stage, f.timer_secs, f.is_teams, f.has_gecko), (32, 480, false, false));
    assert_eq!(f.players.len(), 2); assert_eq!(f.players[0].start_stocks, 4); assert_eq!(winner_port(&f), Some(2)); }
#[test] fn winner_rules() {
    // timeout, stocks 2 v 1 => port with 2; stocks equal, percent 40.0 v 90.5 => lower percent;
    // equal stocks and percent => None; method 7 with lras Some(1) => Some(2); method 7, None => None;
    // no Game End => None; a third Human player => None
}
#[test] fn skips_unknown_events() { /* builder inserts event 0x10 with a declared size; parse still succeeds */ }
#[test] fn rejects_garbage() {
    assert!(matches!(parse(b"hello"), Err(ReplayError::NotSlp)));
    let g = simple_game(1, "t");
    for cut in [10, 40, g.len() / 2] { assert!(parse(&g[..cut]).is_err()); }   // never panics
    assert!(parse(&[0u8; 4096]).is_err());
}
```

- [ ] **Step 2: Run** `cargo test -p replay --features testkit` — expected: FAIL.
- [ ] **Step 3: Implement** `raw.rs` (UBJSON `{U\x03raw[$U#l` + big-endian length), `events.rs`, `lib.rs`, `testkit.rs`. All slicing is bounds-checked; no `unwrap` on input-derived data.
- [ ] **Step 4: Run** the tests — expected: PASS.
- [ ] **Step 5: Commit** `feat(replay): minimal slp reader and test builder`

### Task 4: Protocol types and request signing

**Files:**
- Create: `crates/proto/{Cargo.toml,src/lib.rs,src/sign.rs,src/types.rs,src/ruleset.rs}`, `crates/proto/tests/sign.rs`

**Interfaces:**
- Produces (`proto` crate; everything in `types.rs`/`ruleset.rs` derives `Serialize, Deserialize, Clone, Debug, PartialEq`):

```rust
// sign.rs — headers: x-ab-key, x-ab-time, x-ab-nonce, x-ab-sig (all lowercase hex / decimal)
pub const MAX_SKEW_SECS: i64 = 120;
pub struct SignedHeaders { pub key: String, pub time: String, pub nonce: String, pub sig: String }
pub fn canonical(method: &str, path_and_query: &str, time: &str, nonce: &str, body: &[u8]) -> String;
    // "{METHOD}\n{path_and_query}\n{time}\n{nonce}\n{sha256_hex(body)}"
pub fn sign_request(sk: &SigningKey, method: &str, path_and_query: &str, now_unix: i64, body: &[u8]) -> SignedHeaders;
pub enum SignError { Malformed, BadSignature, Skew }
pub fn verify_request(h: &SignedHeaders, method: &str, path_and_query: &str, now_unix: i64, body: &[u8])
    -> Result<String /*key hex*/, SignError>;

// ruleset.rs
pub struct Ruleset { pub stocks: u8, pub timer_secs: u32, pub best_of: u8, pub best_of_finals: u8,
                     pub legal_stages: Vec<u16>, pub banned_characters: Vec<u8> }
impl Default for Ruleset  // 4, 480, 3, 5, [2, 3, 8, 28, 31, 32], []

// types.rs
pub enum Role { Host, Guest }
pub enum Next { NotEntered, Waiting, Review { set_id: u32 }, Eliminated, Champion,
    Play { set_id: u32, opponent_tag: String, role: Role, relay: String, room_code: Option<String>,
           game_no: u8, wins: [u8; 2] /*me, opponent*/, best_of: u8, stocks: u8, timer_secs: u32,
           awaiting_verdict: bool /*my replay for game_no is uploaded and not yet judged*/ } }
pub enum ResimOutcome { Match, Diverged { frame: i32, port: u8, field: String, engine_value: String, replay_value: String },
    EarlyEnd { engine_last: i32, replay_last: i32 }, Crashed(String), Unavailable(String), Skipped }
pub enum Verdict { Verified, VerifiedInexact, PeerOnly, Conflicted, Single, Illegal(String), Undecided, Inexact }
impl Verdict { pub fn counts(&self) -> bool }   // Verified | VerifiedInexact | PeerOnly
pub struct RegisterReq { pub tag: String, pub invite: Option<String> }
pub struct SignupReq { pub engine_build: String }
pub struct RoomReq { pub code: String }
pub struct EventCreateReq { pub name: String, pub format: bracket::Format, pub ruleset: Option<Ruleset> }
pub struct EventSummary { pub id: i64, pub name: String, pub state: String, pub entrants: u32 }
pub struct EventView { pub summary: EventSummary, pub ruleset: Ruleset, pub tags: Vec<String> /*seed order*/,
                       pub bracket: Option<bracket::Bracket>, pub scores: Vec<(u32, [u8; 2])> /*set_id, wins*/ }
pub enum Resolve { Award { slot: u8 }, Replay, Dq { slot: u8 } }
pub struct QueueItem { pub id: i64, pub event_id: i64, pub set_id: u32, pub game_id: Option<i64>,
                       pub reason: String, pub verdict: Option<Verdict>, pub resim: Option<ResimOutcome> }
pub struct VerifyJob { pub id: i64, pub replay_last_frame: i32 }
pub struct VerifyResult { pub outcome: ResimOutcome, pub build: String }
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn round_trip() { /* sign then verify at the same time returns the signer's key hex */ }
#[test] fn tamper() { /* changed body, path, or method each => Err(BadSignature) */ }
#[test] fn skew() { /* verify at now+121 => Err(Skew); now+120 => Ok */ }
#[test] fn malformed() { /* non-hex key or short sig => Err(Malformed) */ }
#[test] fn ruleset_default() { assert_eq!(Ruleset::default().legal_stages, vec![2, 3, 8, 28, 31, 32]); }
#[test] fn verdict_counts() { /* true for the three counting verdicts, false for the other five */ }
```

- [ ] **Step 2: Run** `cargo test -p proto` — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p proto` — expected: PASS.
- [ ] **Step 5: Commit** `feat(proto): signed requests and shared types`

### Task 5: Server skeleton — config, store, authentication, registration

**Files:**
- Create: `crates/server/{Cargo.toml,migrations/0001_init.sql,src/{lib.rs,main.rs,config.rs,db.rs,auth.rs,state.rs,resim.rs}}`, `crates/server/src/routes/{mod.rs,players.rs}`, `crates/server/src/testkit.rs` (feature `testkit`, enabled for the crate's own tests via a self dev-dependency), `crates/server/tests/auth.rs`

**Interfaces:**
- Consumes: `proto` (Task 4).
- Produces:

```rust
// config.rs — TOML; every field has a serde default except to_keys
pub struct Config { pub bind: String /*"0.0.0.0:8080"*/, pub db_path: PathBuf, pub replay_dir: PathBuf,
    pub relay: String /*"netplay.gsd.sh:51600"*/, pub require_invite: bool, pub to_keys: Vec<String>,
    pub verifier_keys: Vec<String>, pub accepted_builds: Vec<String> /*empty = any*/,
    pub upload_timeout_secs: u64 /*120*/, pub strict_resim: bool, pub max_replay_bytes: usize /*16 MiB*/,
    pub verifier: VerifierConfig, pub ruleset: Ruleset }
pub struct VerifierConfig { pub mode: VerifierMode /*InProcess | External | Off; default Off*/,
    pub engine: PathBuf, pub iso: PathBuf, pub timeout_secs: u64 /*120*/, pub jobs: usize /*2*/ }
// resim.rs
#[async_trait] pub trait Resim: Send + Sync {
    async fn check(&self, slp: &Path, replay_last_frame: i32) -> ResimOutcome;
    fn build(&self) -> String; }
pub struct NoResim;                 // check() => Skipped, build() => "none"
// state.rs
pub struct AppState { pub cfg: Arc<Config>, pub db: SqlitePool, pub resim: Arc<dyn Resim>, pub wake: Arc<Notify> }
// auth.rs — axum extractors; both buffer the body (limit cfg.max_replay_bytes) and verify the signature
pub struct Player { pub key: String, pub tag: String, pub body: Bytes }   // 401 unsigned/invalid, 403 unregistered
pub struct To { pub key: String, pub body: Bytes }                         // 403 unless key in cfg.to_keys
pub struct AnyKey { pub key: String, pub body: Bytes }                     // signature only (used by registration)
// lib.rs
pub async fn build_app(cfg: Config, resim: Arc<dyn Resim>) -> anyhow::Result<(Router, AppState)>;
// testkit.rs
pub struct TestApp { pub url: String, pub to: SigningKey, pub state: AppState, pub dir: TempDir }
impl TestApp { pub async fn spawn(edit: impl FnOnce(&mut Config)) -> Self;
               pub async fn spawn_with(edit: impl FnOnce(&mut Config), resim: Arc<dyn Resim>) -> Self;
               pub async fn player(&self, tag: &str) -> TestPlayer;       // generates a key and registers
               pub fn as_to(&self) -> TestPlayer; }
pub struct TestPlayer { pub sk: SigningKey, pub tag: String, /* url */ }
impl TestPlayer { pub async fn get(&self, path: &str) -> reqwest::Response;
                  pub async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> reqwest::Response;
                  pub async fn post_bytes(&self, path: &str, body: Vec<u8>, headers: &[(&str, &str)]) -> reqwest::Response; }
```

Schema (`0001_init.sql`), all timestamps unix seconds:

| Table | Columns |
|---|---|
| `players` | `key` PK, `tag` UNIQUE, `created_at` |
| `invites` | `code` PK, `used_by` NULL |
| `nonces` | `key`, `nonce`, `seen_at`; PK(`key`,`nonce`) |
| `events` | `id` PK, `name`, `format`, `ruleset_json`, `state` (`signup`/`running`/`complete`), `bracket_json` NULL, `created_at` |
| `entrants` | `event_id`, `player_key`, `seed`, `engine_build`, `dq` DEFAULT 0; PK(`event_id`,`player_key`) |
| `sets` | `event_id`, `set_id`, `state` (`ready`/`review`/`done`), `room_code` NULL, `wins0`, `wins1`; PK(`event_id`,`set_id`) |
| `games` | `id` PK, `event_id`, `set_id`, `game_no`, `state` (`uploading`/`judging`/`counted`/`void`/`review`), `verdict_json` NULL, `winner_slot` NULL, `first_upload_at` NULL, `resim_json` NULL, `verifier_build` NULL |
| `uploads` | `game_id`, `slot`, `path`, `sha256`, `client_build`, `uploaded_at`; PK(`game_id`,`slot`) |
| `queue` | `id` PK, `event_id`, `set_id`, `game_id` NULL, `reason`, `state` (`open`/`resolved`), `resolution_json` NULL, `created_at` |
| `quarantine` | `id` PK, `game_id`, `kind`, `created_at` |
| `resim_jobs` | `id` PK, `game_id`, `path`, `replay_last_frame`, `state` (`queued`/`claimed`/`done`), `result_json` NULL, `claimed_at` NULL |

Route: `POST /v1/players` (`AnyKey`, body `RegisterReq`) → `201`; tag must match `^[A-Za-z0-9_]{2,16}$` (`400`), unique (`409`), re-registering the same key is `409`; with `require_invite` a missing, unknown, or used code is `403` and a valid one is consumed. Nonces: reject a repeated (`key`,`nonce`) with `401`; delete rows older than 300 s on insert. Rate limit with `tower_governor`: 20 requests/s per IP, burst 40.

- [ ] **Step 1: Write the failing tests** (`tests/auth.rs`)

```rust
#[tokio::test] async fn register_ok() { /* 201; players row exists */ }
#[tokio::test] async fn unsigned_is_401() { /* plain reqwest POST /v1/players */ }
#[tokio::test] async fn replayed_nonce_is_401() { /* send the identical signed request twice: 201 then 401 */ }
#[tokio::test] async fn stale_time_is_401() { /* signed with now-121 */ }
#[tokio::test] async fn bad_tag_400_dup_tag_409() {}
#[tokio::test] async fn invite_required() { /* require_invite=true: none 403, unknown 403, valid 201, reused 403 */ }
#[tokio::test] async fn oversized_body_is_413() { /* max_replay_bytes=1024; 2 KiB body */ }
#[tokio::test] async fn rate_limited() { /* 200 rapid GET /v1/events from one client: at least one 429 */ }
```

- [ ] **Step 2: Run** `cargo test -p autobracket-server` — expected: FAIL to compile.
- [ ] **Step 3: Implement.** `main.rs`: `autobracket serve --config PATH` and `autobracket keygen --out FILE` (writes the secret key hex with mode 0600, prints the public key hex). `GET /v1/events` may return `[]` for now.
- [ ] **Step 4: Run** the tests — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): config, store, signed auth, registration`

### Task 6: Events, sign-up, start

**Files:**
- Create: `crates/server/src/routes/{events.rs,admin.rs}`, `crates/server/src/lifecycle.rs`, `crates/server/tests/events.rs`
- Modify: `crates/server/src/routes/mod.rs`

**Interfaces:**
- Consumes: `bracket::build`, Task 5 extractors.
- Produces routes:
  - `POST /v1/admin/events` (`To`, `EventCreateReq`) → `201 EventSummary`; ruleset defaults to `cfg.ruleset`.
  - `POST /v1/admin/invites` (`To`) → `201 {"code": "..."}` (12 chars, base32).
  - `POST /v1/events/{id}/entrants` (`Player`, `SignupReq`) → `201`; `409` if already entered or event not in `signup`; `403` with body `{"error":"engine build not accepted","accepted":[...]}` when `accepted_builds` is non-empty and does not contain the build.
  - `POST /v1/admin/events/{id}/start` (`To`) → `200 EventView`; `409` with `{"error":"need at least 2 entrants"}` otherwise. Seeds are sign-up order.
  - `GET /v1/events` → `Vec<EventSummary>`; `GET /v1/events/{id}` → `EventView` (both public, unsigned).
- Produces (`lifecycle.rs`):

```rust
pub async fn start_event(st: &AppState, event_id: i64) -> Result<(), LifecycleError>;
    // builds the bracket, stores bracket_json, inserts a `sets` row (state ready) per Ready set
pub async fn load_bracket(db: &SqlitePool, event_id: i64) -> Result<Bracket, LifecycleError>;
pub async fn store_bracket(db: &SqlitePool, event_id: i64, b: &Bracket) -> Result<(), LifecycleError>;
```

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn create_requires_to() { /* a player key => 403 */ }
#[tokio::test] async fn signup_and_start_4() { /* 4 players; start => EventView.bracket has 7 sets, 2 Ready; 2 sets rows */ }
#[tokio::test] async fn start_with_one_entrant_is_409() {}
#[tokio::test] async fn duplicate_signup_is_409() {}
#[tokio::test] async fn signup_after_start_is_409() {}
#[tokio::test] async fn unaccepted_build_is_403() { /* accepted_builds=["b1"]; "b2" => 403 listing ["b1"]; "b1" => 201 */ }
#[tokio::test] async fn three_entrants_start() { /* double elim, 3 entrants: start ok, exactly 1 sets row */ }
```

- [ ] **Step 2: Run** — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p autobracket-server` — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): events, sign-up, start`

### Task 7: Playing a set — next, room code, replay upload

**Files:**
- Create: `crates/server/src/routes/sets.rs`, `crates/server/tests/sets.rs`
- Modify: `crates/server/src/lifecycle.rs`, `crates/server/src/routes/mod.rs`

**Interfaces:**
- Consumes: `replay::parse`, Task 6.
- Produces routes (all `Player`):
  - `GET /v1/me/next?event={id}` → `Next`. Slot 0 of the set is `Role::Host`. `game_no` = counted games + 1. `Review` while the set is in `review`. `Eliminated`/`Champion` from the bracket; `Waiting` when entered with no ready set.
  - `POST /v1/sets/{event}/{set}/room` (`RoomReq`) → `204`. Host only (`403` otherwise); code must match `^[A-Z0-9]{4}$` or `^[A-Za-z0-9.\-]+:[0-9]{1,5}$` (`400`); posting again replaces the code while the set is `ready`. The code is per game: the server clears it when the first replay of a game arrives.
  - `POST /v1/sets/{event}/{set}/games/{n}/replay` (raw bytes; header `x-ab-build`) → `202`. Checks, in order: caller is in the set (`403`); set is `ready` (`409`); `n` equals the current `game_no` (`409`); `replay::parse` succeeds (`400`, nothing stored). Stores the file at `{replay_dir}/{event}/{set}/{game_id}/{slot}.slp`, inserts `uploads`, sets `first_upload_at` and clears `sets.room_code` on the first one, and calls `state.wake.notify_one()`. Re-upload from the same slot: identical sha256 → `200`, different → `409`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn next_roles() { /* 2-player event: seed 0 gets Play{role: Host, room_code: None, game_no: 1, wins: [0,0], best_of: 3, stocks: 4, timer_secs: 480, awaiting_verdict: false}; seed 1 Guest */ }
#[tokio::test] async fn next_states() { /* not entered => NotEntered; entered before start => Waiting */ }
#[tokio::test] async fn room_code_flow() { /* guest posting => 403; host "KQ7X" => 204; guest's next shows Some("KQ7X"); host re-posts "AB12" => guest sees "AB12"; "bad code!" => 400 */ }
#[tokio::test] async fn upload_stores_and_is_idempotent() { /* 202; same bytes again 200; different bytes 409; one uploads row; file on disk */ }
#[tokio::test] async fn upload_wrong_game_no_is_409() {}
#[tokio::test] async fn first_upload_clears_code_and_sets_awaiting() { /* after host uploads: both players see room_code None; host awaiting_verdict true, guest false */ }
#[tokio::test] async fn upload_garbage_is_400_and_stores_nothing() { /* b"nope" and a truncated simple_game */ }
#[tokio::test] async fn outsider_is_403() { /* a third registered player posts a room code and a replay to the set: both 403, no rows */ }
```

- [ ] **Step 2: Run** — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p autobracket-server` — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): next set, room code, replay upload`

### Task 8: Verdict logic

**Files:**
- Create: `crates/server/src/judge.rs`, `crates/server/tests/judge.rs`

**Interfaces:**
- Consumes: `replay`, `proto::{Ruleset, Verdict, ResimOutcome}`.
- Produces:

```rust
pub enum Pre { Final(Verdict) /*Conflicted | Single | Illegal(_) | Undecided*/, NeedsResim { winner_port: u8, last_frame: i32 } }
pub fn legality(f: &ReplayFacts, rules: &Ruleset) -> Result<(), &'static str>;
pub fn pre_judge(uploads: [Option<&[u8]>; 2], rules: &Ruleset) -> Pre;
pub fn finish(resim: &ResimOutcome, strict_resim: bool) -> Verdict;
pub fn winner_slot(winner_port: u8) -> u8;      // port 1 => slot 0 (host), port 2 => slot 1
```

`legality` checks, first failure wins, returning that word: `"players"` (exactly two `Human` players, on ports 1 and 2), `"teams"`, `"stage"`, `"stocks"` (both start with `rules.stocks`), `"timer"`, `"character"`, `"gecko"`.
`pre_judge`: one upload → `Single`; two uploads with differing `raw_element` → `Conflicted`; then `legality` → `Illegal(word)`; then `winner_port` `None` → `Undecided`; else `NeedsResim`.
`finish`: `Match` → `Verified`; `Diverged`/`EarlyEnd`/`Crashed` → `Inexact` if strict else `VerifiedInexact`; `Unavailable`/`Skipped` → `PeerOnly`.

- [ ] **Step 1: Write the failing tests** — one per rule above, built with `SlpBuilder`: `agreeing_pair_needs_resim` (differing `start_at`, `winner_port: 2`), `differing_raw_is_conflicted` (one file has an extra frame), `single_upload`, `illegal_stage` (stage 4), `illegal_stocks`, `illegal_timer`, `illegal_gecko`, `illegal_banned_character`, `cpu_player_is_illegal_players`, `no_game_end_is_undecided`, `finish_table` (all six outcomes × strict on/off), `winner_slot_mapping`.
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test judge` — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): verdict logic`

### Task 9: `slpcheck` re-simulation

**Files:**
- Modify: `crates/server/src/resim.rs`
- Create: `crates/server/tests/resim.rs`, `crates/server/tests/fixtures/fake_slpcheck.sh`

**Interfaces:**
- Produces:

```rust
pub struct Slpcheck { /* engine, iso, timeout, build */ }
impl Slpcheck { pub async fn new(engine: PathBuf, iso: PathBuf, timeout: Duration) -> anyhow::Result<Self>; }
    // build = first line of `{engine} version` + "+" + first 12 hex of the binary's sha256
impl Resim for Slpcheck   // runs: {engine} slpcheck --slp F --iso ISO --csv TMP   with env SLPCHECK_VANILLA_SPAWN=1
pub fn outcome_from_csv(csv: &str, replay_last_frame: i32) -> ResimOutcome;
pub const EARLY_END_SLACK_FRAMES: i32 = 0;
```

`outcome_from_csv` reads the header to find columns by name (`status`, `last_frame`, `first_frame`, `first_port`, `first_field`, `engine_value`, `replay_value`) and uses the last data row. `status` `match` with `last_frame + EARLY_END_SLACK_FRAMES < replay_last_frame` → `EarlyEnd`; `match` otherwise → `Match`; `diverged` → `Diverged`; any other status → `Crashed(status)`. Process handling: exit code 2 → `Unavailable(stderr tail)`; no CSV row written → `Crashed`; timeout → kill, `Crashed("timeout")`.

`fake_slpcheck.sh` is a shell script standing in for the engine: `version` prints `fake 0.0`; `slpcheck` picks its behaviour from the replay's file name (`match`, `diverged`, `early`, `panic` → exit 134 with no CSV, `nodisc` → exit 2, `hang` → `sleep 30`) and appends a header and row to the `--csv` path.

- [ ] **Step 1: Write the failing tests** — `csv_match`, `csv_diverged_fields` (frame 797, port 2, field `x`), `csv_early_end` (`last_frame` 1000 vs replay 5000), `csv_panic_status_is_crashed`, `csv_columns_found_by_name_not_position` (shuffled header), and against the fake script: `run_match`, `run_diverged_despite_exit_0`, `run_panic_is_crashed`, `run_missing_disc_is_unavailable`, `run_timeout_is_crashed` (timeout 1 s), `env_var_is_set` (script fails unless `SLPCHECK_VANILLA_SPAWN=1`), `build_string` (`"fake 0.0+"` prefix).
- [ ] **Step 2: Run** `cargo test -p autobracket-server --test resim` — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): slpcheck re-simulation`

### Task 10: Judging worker and bracket advance

**Files:**
- Create: `crates/server/src/worker.rs`, `crates/server/tests/flow.rs`
- Modify: `crates/server/src/lifecycle.rs`, `crates/server/src/lib.rs` (spawn the worker in `build_app`), `crates/server/src/testkit.rs`

**Interfaces:**
- Consumes: Tasks 7–9.
- Produces:

```rust
// worker.rs
pub async fn run(st: AppState);                       // loop: wait on st.wake or 1 s tick, then judge_due
pub async fn judge_due(st: &AppState) -> anyhow::Result<usize>;   // games judged this pass
pub async fn recover(st: &AppState) -> anyhow::Result<()>;        // games in `judging` => `uploading`; called at startup
// lifecycle.rs
pub async fn apply_verdict(st: &AppState, game_id: i64, v: &Verdict, winner_slot: Option<u8>, resim: &ResimOutcome)
    -> Result<(), LifecycleError>;
pub async fn award_set(st: &AppState, event_id: i64, set_id: u32, winner_slot: u8) -> Result<(), LifecycleError>;
// testkit.rs
pub struct FakeResim(pub Mutex<VecDeque<ResimOutcome>>);   // pops per check(); Match when empty; build() "fake"
pub async fn play_game(app: &TestApp, event: i64, set: u32, n: u8, host: &TestPlayer, guest: &TestPlayer, winner_port: u8);
    // uploads an agreeing pair (different start_at) then calls judge_due
```

A game is due when it is `uploading` and has two uploads, or one upload and `first_upload_at + upload_timeout_secs <= now`. Judging marks it `judging`, runs `pre_judge`, and for `NeedsResim` calls `st.resim.check` on the host's file. `apply_verdict`:
- counting verdict → game `counted` with `winner_slot`, bump `wins0`/`wins1`; when a player reaches `best_of / 2 + 1` (`best_of_finals` for `Side::GrandFinal` sets) call `award_set`.
- non-counting → game `review`, set `review`, insert a `queue` row whose `reason` is the verdict name.
- `VerifiedInexact`, `Inexact`, `Conflicted`, `Single` → also insert `quarantine` (`kind` = verdict name).
- always store `verdict_json`, `resim_json`, `verifier_build`.

`award_set`: set `done`, `Bracket::report`, store the bracket, insert a `ready` `sets` row for each newly ready set, mark the event `complete` when the bracket is. If a newly ready set contains a `dq` entrant, award it to the other entrant immediately.

- [ ] **Step 1: Write the failing tests** (`tests/flow.rs`)

```rust
#[tokio::test] async fn four_player_double_elim_runs_to_champion() {
    // best_of 3; seed 0 wins everything 2-0; drive every Ready set via play_game until the event is `complete`.
    // Assert: EventView.bracket champion is seed 0; 6 sets played; every game verdict Verified; queue empty.
}
#[tokio::test] async fn inexact_counts_and_is_quarantined() { /* FakeResim yields Diverged: game counted, 1 quarantine row, queue empty */ }
#[tokio::test] async fn strict_resim_sends_inexact_to_queue() { /* strict_resim=true: game review, set review, Next::Review for both players */ }
#[tokio::test] async fn conflicting_uploads_go_to_queue() { /* host uploads winner 1, guest winner 2: Conflicted, wins unchanged */ }
#[tokio::test] async fn single_upload_times_out_to_queue() { /* upload_timeout_secs=0, one upload, judge_due: Single, never counted */ }
#[tokio::test] async fn single_upload_waits_before_timeout() { /* upload_timeout_secs=3600: judge_due returns 0 */ }
#[tokio::test] async fn illegal_stage_goes_to_queue() {}
#[tokio::test] async fn verifier_off_is_peer_only() { /* NoResim: verdict PeerOnly, game counted */ }
#[tokio::test] async fn game_never_counted_twice() { /* after a counted game, re-posting the same replay for n=1 is 409 and wins stay 1 */ }
#[tokio::test] async fn restart_recovers_judging_games() {
    // set a game to `judging` directly in the DB, call recover() then judge_due(): the game is counted.
    // Also: a game with one upload and first_upload_at = now-10, timeout 5, is judged Single after recover().
}
#[tokio::test] async fn grand_final_uses_best_of_finals() { /* 2 entrants double elim, best_of 3, best_of_finals 5: GF needs 3 wins */ }
```

- [ ] **Step 2: Run** `cargo test -p autobracket-server --test flow` — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace` — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): judging worker and bracket advance`

### Task 11: TO queue, quarantine corpus, admin CLI

**Files:**
- Create: `crates/server/src/routes/quarantine.rs`, `crates/server/src/admin_cli.rs`, `crates/server/tests/admin.rs`
- Modify: `crates/server/src/routes/admin.rs`, `crates/server/src/lifecycle.rs`, `crates/server/src/main.rs`

**Interfaces:**
- Consumes: Task 10.
- Produces routes (all `To`):
  - `GET /v1/admin/queue` → `Vec<QueueItem>` (open items).
  - `POST /v1/admin/queue/{id}/resolve` (`Resolve`) → `204`; `409` if already resolved. `Award{slot}` → `award_set`. `Replay` → the game becomes `void`, the set returns to `ready` with its room code cleared (same `game_no` is played again). `Dq{slot}` → mark the entrant `dq`, `award_set` to the other slot. Every resolution stores `resolution_json` and the TO's key.
  - `POST /v1/admin/players/{old_key}/rekey` (`{"new_key": "..."}`) → `204`; moves the player and their entrant rows; `409` if `new_key` is registered.
  - `GET /v1/quarantine` → list of `{id, game_id, kind, verdict, resim, verifier_build, client_builds, created_at}`; `GET /v1/quarantine/{id}/replay/{slot}` → the `.slp` bytes.
- Produces CLI: `autobracket admin --server URL --key-file FILE <cmd>` with `event create --name N --format single|double [--best-of N]`, `event start ID`, `invite new`, `queue list`, `queue resolve ID award --slot S | replay | dq --slot S`, `player rekey OLD NEW`, `quarantine list`. Each is a signed call to the route above and prints the JSON response.

- [ ] **Step 1: Write the failing tests** — `queue_lists_conflict_with_evidence` (verdict and resim present), `award_advances_bracket`, `replay_voids_game_and_reopens_set` (afterwards `Next::Play{game_no: 1, room_code: None}` and a fresh pair counts), `dq_awards_current_and_future_sets` (double elim: the DQ'd player's losers-bracket set is auto-awarded), `resolve_twice_is_409`, `queue_requires_to`, `rekey_moves_identity` (old key `403`, new key gets the same `Next`), `quarantine_serves_replay_bytes`, and one CLI test running the built binary: `admin event create` then `GET /v1/events` shows it.
- [ ] **Step 2: Run** — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace` — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): TO queue, quarantine corpus, admin CLI`

### Task 12: External verifier mode

**Files:**
- Create: `crates/server/src/routes/verify_jobs.rs`, `crates/server/tests/external.rs`
- Modify: `crates/server/src/resim.rs`, `crates/server/src/main.rs`, `crates/server/src/lib.rs`

**Interfaces:**
- Consumes: `Slpcheck` (Task 9), `resim_jobs` table.
- Produces:
  - `pub struct ExternalResim { /* db, wake */ }` implementing `Resim`: `check` inserts a `resim_jobs` row and waits for its result; after 600 s returns `Unavailable("no verifier")`. `build()` returns `"external"`; the stored `verifier_build` comes from the worker's `VerifyResult.build`.
  - Routes, authenticated by a key in `cfg.verifier_keys` or `cfg.to_keys`: `POST /v1/verify/claim` → `200 VerifyJob` or `204`; a job claimed more than 300 s ago is offered again. `GET /v1/verify/jobs/{id}/replay` → bytes. `POST /v1/verify/jobs/{id}/result` (`VerifyResult`) → `204`.
  - CLI: `autobracket verify-worker --server URL --key-file FILE --engine PATH --iso PATH [--timeout-secs 120]`: loop claim → download to a temp file → `Slpcheck::check` → post result; sleep 2 s on `204`.
  - `main.rs` picks the `Resim` from `verifier.mode`: `InProcess` → `Slpcheck`, `External` → `ExternalResim`, `Off` → `NoResim`.

- [ ] **Step 1: Write the failing tests** — `job_round_trip` (app in external mode; a game is uploaded; a test task claims, fetches the replay, posts `Match`; the game ends `Verified` with `verifier_build` equal to the posted build), `claim_requires_verifier_key` (`403` for a player), `no_jobs_is_204`, `stale_claim_is_reoffered`, and `worker_binary_end_to_end` (run `autobracket verify-worker` with `fake_slpcheck.sh` as the engine against a `TestApp`; the game ends `Verified`).
- [ ] **Step 2: Run** — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace` — expected: PASS.
- [ ] **Step 5: Commit** `feat(server): external verifier mode`

### Task 13: Reference client

**Files:**
- Create: `crates/client/{Cargo.toml,src/{main.rs,config.rs,api.rs,engine.rs,play.rs}}`, `crates/client/tests/{play.rs,fixtures/fake_melee.sh}`

**Interfaces:**
- Consumes: `proto`; server routes from Tasks 5–7.
- Produces:

```rust
// config.rs — ~/.config/autobracket/client.toml (override with --config)
pub struct ClientConfig { pub server: String, pub secret_key_hex: String, pub engine_build: String,
    pub melee: PathBuf, pub iso: PathBuf, pub work_dir: PathBuf,
    pub host_args: Vec<String>, pub join_args: Vec<String>, pub code_regex: String }
// api.rs
pub struct Api { /* base url, key */ }   // one signed method per route: register, events, signup, next, post_room, upload_replay
// engine.rs
pub struct GameParams<'a> { pub relay: &'a str, pub stocks: u8, pub timer_secs: u32 }
pub trait Engine {
    fn host(&mut self, p: &GameParams) -> anyhow::Result<()>;
    fn join(&mut self, p: &GameParams, code: &str) -> anyhow::Result<()>;
    fn room_code(&mut self, wait: Duration) -> anyhow::Result<Option<String>>;   // host only
    fn running(&mut self) -> bool;
    fn take_replays(&mut self) -> anyhow::Result<Vec<PathBuf>>;   // after exit: this run's .slp files, oldest first, once
    fn stop(&mut self); }
pub struct CommandEngine;
// play.rs
pub async fn play_event(api: &Api, engine: &mut dyn Engine, event: i64, poll: Duration) -> anyhow::Result<Next>;
    // returns the terminal state: Eliminated, Champion, or NotEntered
```

Engine facts (candidate11b; `rust/melee/src/play_net.rs:1157-1215`, `slp_export.rs:150-163`):

- Default `host_args`: `play --iso {iso} --netplay host --np-server {relay} --record {mrpl} --slp auto --np-css --quit-after-matches 1 --stocks {stocks} --time-secs {timer_secs}`. Default `join_args`: the same with `--netplay join {code}`. `--record` is mandatory whenever `--slp` is set. Both peers must pass the same rules flags.
- The room code appears on **stderr** as `play: netplay host: MATCH CODE K7QX (give it to the other player)`. Default `code_regex`: `MATCH CODE ([A-Z0-9]+)`.
- `--slp auto` writes `$MELEE_SLP_DIR/Game_<UTC timestamp>.slp`. `CommandEngine` gives every run a fresh directory `{work_dir}/run-{n}/`, sets `MELEE_SLP_DIR` to it, and uses `{work_dir}/run-{n}/game.mrpl` for `{mrpl}`.
- Exports are written when the process exits by itself (also after a failed run), so `take_replays` returns nothing while `running()`. A killed process writes nothing, so `stop()` is only for abandoning a game.
- The process exits by itself after the results screen, or with a failure code if the peer leaves.

`play_event` loop: poll `next`. `Waiting`/`Review` → print once, keep polling. On `Play`:
- engine running → keep polling.
- engine just exited → upload every path from `take_replays()` as game `game_no` (a `409` is logged and skipped, not fatal; zero replays is logged as "no replay produced").
- engine idle and `awaiting_verdict` → keep polling.
- engine idle, host → `engine.host`, wait up to 60 s for `room_code`, `post_room`.
- engine idle, guest, `room_code` present → `engine.join`.

Leaving `Play` or a changed `set_id` → `engine.stop()`.

CLI: `autobracket-client init --melee PATH --iso PATH` (writes a config with a new key and the defaults above; `engine_build` is the first line of `{melee} version`; prints the public key), `register --server URL --tag T [--invite C]`, `events`, `signup EVENT`, `status EVENT`, `play EVENT`.

- [ ] **Step 1: Write the failing tests** (`tests/play.rs`, against a real `TestApp` from `autobracket_server::testkit`, a dev-dependency with feature `testkit`)

```rust
struct ScriptedEngine { /* records host()/join() calls; yields a preset code; each run "exits" at once and yields one preset replay */ }
#[tokio::test] async fn host_posts_code_and_guest_joins_with_it() {}
#[tokio::test] async fn two_clients_play_a_bo3_to_champion() {
    // two play_event tasks with ScriptedEngines feeding agreeing simple_game pairs; a third task loops judge_due.
    // Assert one returns Champion and the other Eliminated, and the host's engine recorded exactly 2 host() calls (one per game).
}
#[tokio::test] async fn does_not_relaunch_while_awaiting_verdict() { /* no judge task: after one game each engine has exactly one host()/join() call across 20 polls */ }
#[tokio::test] async fn upload_conflict_is_not_fatal() {}
#[test] fn command_engine_reads_code_and_collects_replays() {
    // fake_melee.sh: prints the MATCH CODE line on stderr, sleeps 1 s, writes $MELEE_SLP_DIR/Game_20261115T200000.slp, exits 0.
    // Assert room_code == Some("K7QX"); take_replays empty while running, one path after exit, empty on a second call.
}
#[test] fn command_engine_substitutes_arguments() { /* fake_melee.sh echoes its argv to a file: {relay} {code} {stocks} {timer_secs} {iso} {mrpl} all replaced */ }
#[test] fn init_writes_0600_config() {}
```

- [ ] **Step 2: Run** `cargo test -p autobracket-client` — expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace` — expected: PASS.
- [ ] **Step 5: Commit** `feat(client): reference client`

### Task 14: Bracket page, deployment, README

**Files:**
- Create: `crates/server/assets/bracket.html`, `crates/server/src/routes/view.rs`, `deploy/autobracket.service`, `deploy/autobracket.example.toml`, `README.md`, `crates/server/tests/view.rs`
- Modify: `crates/server/src/routes/mod.rs`

**Interfaces:**
- Produces: `GET /` serves `bracket.html` (embedded with `include_str!`): one self-contained page, no build step and no external requests, that fetches `/v1/events` and `/v1/events/{id}` and draws the bracket with tags and set scores, refreshing every 10 s. Light and dark via `prefers-color-scheme`; usable at 400 px width.
- `deploy/autobracket.service`: copy the hardening from `~/gdmelee-server/gdmelee.service` (`DynamicUser`, `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`), add `StateDirectory=autobracket` and `ExecStart=/usr/local/bin/autobracket serve --config /etc/autobracket/autobracket.toml`.
- `deploy/autobracket.example.toml`: every `Config` field with its default and a one-line comment; `db_path` and `replay_dir` under `/var/lib/autobracket`.
- `README.md`: what it is (two paragraphs from the spec's intent), quick start (keygen, config, serve, create and start an event, two clients), the verdict table, verifier modes and the bring-your-own-disc rule, what phase 2 is.

- [ ] **Step 1: Write the failing tests** — `root_serves_html` (`200`, `content-type: text/html`, body contains `<title>` and `/v1/events`), `page_has_no_external_urls` (body contains no `http://` or `https://`), `example_config_parses` (`deploy/autobracket.example.toml` deserialises into `Config`).
- [ ] **Step 2: Run** — expected: FAIL.
- [ ] **Step 3: Implement.** Then start the server locally with two fake events and load `/` in a browser at desktop and 400 px width to confirm the bracket is readable.
- [ ] **Step 4: Run** `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` — expected: PASS, no warnings.
- [ ] **Step 5: Commit** `feat: bracket page, deployment files, README`

### Task 15: Real-engine check

*Needs `sudo` once for build dependencies — ask the owner before running it.*

**Files:**
- Create: `crates/server/tests/real_engine.rs` (all tests `#[ignore]`), `docs/real-engine.md`

- [ ] **Step 1:** Build the engine outside this repo: clone `github.com/GurekamDhillon/melee` branch `port/candidate12` (commit `0cd085d0b`; `candidate11b` at `97289408e` is the fallback) into `~/melee-engine`, run `tools/rust-port/linux/deps.sh`, then `cargo build --profile fast -p melee -p melee_relay` in `rust/`.
- [ ] **Step 2:** Fetch the fixture pair (both peers' exports of one netplay game) from branch `port/slp-fixture-netplay`, path `golden/slp/netplay/`, of the same repository into `~/autobracket-fixtures/` — not into this repo. If the branch is not there yet, ask the desktop engine session for it.
- [ ] **Step 3: Write the tests**, reading `AB_ENGINE`, `AB_ISO`, `AB_FIXTURE_A`, `AB_FIXTURE_B` from the environment: `fixture_pair_raw_elements_equal`; `fixture_parses_with_expected_facts` (two humans on ports 1 and 2, `winner_port` is `Some`); `slpcheck_says_match_on_own_build_export`; `pre_judge_then_finish_is_verified`.
- [ ] **Step 4: Run** `AB_ENGINE=... AB_ISO="$HOME/Super Smash Bros. Melee (USA) (En,Ja) (v1.02).iso" ... cargo test -p autobracket-server --test real_engine -- --ignored`. Expected: PASS. If `slpcheck_says_match` reports `EarlyEnd`, measure the gap between the CSV `last_frame` and the replay's last frame, set `EARLY_END_SLACK_FRAMES` to it, and record the measurement in `docs/real-engine.md`. If the replay reader disagrees with a real file, fix the offsets in `crates/replay/src/events.rs` and add the failing case to its tests using `SlpBuilder`.
- [ ] **Step 5:** Run one real game through the whole loop on this machine: `autobracket serve` with `verifier.mode = "inprocess"`, two `autobracket-client play` instances with separate configs and `host_args`/`join_args` extended with `--headless --no-audio --np-lan 127.0.0.1 --np-bind 127.0.0.1`, a local `melee_relay`, a 2-entrant event. Expected: the game is judged `Verified` or `VerifiedInexact` and the bracket shows a score. Two idle headless peers may never finish a game; if so, record that and leave the two-machine run to the owner.
- [ ] **Step 6:** Write `docs/real-engine.md`: the engine commit, the commands run, the results, and the wall-clock time of one `slpcheck` run on this VPS.
- [ ] **Step 7: Commit** `test: real-engine verification check`

