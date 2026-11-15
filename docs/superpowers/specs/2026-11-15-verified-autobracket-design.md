# Verified auto-bracket: design

Date: 2026-11-15
Status: approved by owner 2026-11-15

## Intent and why

A self-hostable HTTP service that owns a Super Smash Bros. Melee tournament bracket and
advances it from verified match results. A tournament organizer drops one binary and one
SQLite file on a cheap VPS, points a Melee client at it, and runs nightly brackets without a
human updating a bracket by hand.

The existing commercial option, Jungle Melee (junglemelee.com), runs nightly automated
brackets with deep per-player statistics and auto-rendered per-set VODs. Its bracket advances
from `.slp` replay files it is handed. It is closed, centrally hosted, and a player cannot run
their own instance.

The thing this project has that Jungle does not is `GurekamDhillon/melee`: a native Rust port
of Melee that re-simulates a `.slp` deterministically, headless, with no GPU, at roughly 40-55x
realtime. That makes server-side verification of a match result cheap. A ~7 minute set
re-simulates in about ten seconds on one CPU core.

So the pitch is not "open-source Jungle". It is **a bracket that cannot be lied to**: results
are proven by re-simulation, not reported. That is also the honest argument for self-hosting --
an organizer running their own instance gets result integrity, not just control.

A second, deliberate benefit: replays that fail to re-simulate exactly are not discarded. They
are quarantined with their divergence point and engine version, forming a conformance corpus
that feeds back into fixing the port's playback.

## Goals

- A TO self-hosts: one service binary, one SQLite file, one config file, no external services.
  Re-simulation additionally needs the engine binary (42 MB stripped, ~267 MB with debuginfo) and the TO's own NTSC 1.02 disc
  image (~1.4 GB); an instance without them still runs, with peer-agreement checks only.
- Brackets are created, seeded, and advanced by the service. No manual bracket editing in the
  normal path.
- Every reported game result is checked by re-simulating the submitted `.slp` with the engine.
- A player never has to open a website or message an opponent to find and connect to their
  next set: the bracket is a native main-menu entry in the game. The API is built and proven
  first with a reference client; the menu entry follows (see Client integration).
- Replays that fail re-simulation are captured as engine bugs, with enough context to fix them.
- The HTTP surface is plain and documented, so a client the project does not control can be
  adapted to it later.
- Runs on a GPU-less VPS.

## Non-goals (v1)

- Rendering video. Auto-VOD and auto-streaming need a GPU and a video encoder; the engine has
  only PNG frame dumps and no ffmpeg path. Deliberately out of scope, and not the project's
  differentiator -- Jungle already does it well.
- Deep statistics (L-cancel %, openings per kill, neutral win rate, stage control). There is
  no stats layer in the engine today. v1 records only what a bracket needs: winner, per-game
  stocks, characters, stage, duration.
- Supporting arbitrary Slippi clients. Explicitly deferred, not rejected -- see "Any client,
  later".
- Accounts with email, passwords, or recovery flows.
- Spectating, clips, skins, player profile pages, rankings.
- A polished web UI. v1 ships the API plus a read-only bracket view.

## Anti-goals

Things that would make this a failure even if it worked:

- Becoming a single hosted service everyone depends on. If the only real instance is one the
  project runs, it has reproduced what it set out to replace.
- Advancing a bracket on an unverified claim. If a player can win a set by asserting they won,
  the central premise is gone.
- Collecting personal data. A self-hosted instance run by a hobbyist TO should not hold email
  addresses or passwords it could leak.
- Requiring a fork of the engine per instance. Configuration, not recompilation.

## Constraints

From the two repos (verified against the desktop and laptop working trees; paths are relative
to the melee repo root):

- **Engine, headless, no GPU:** `melee play --headless` (`rust/melee/src/retail_flow.rs:189`,
  `:1520-1541`); `golden` / `synctest` / `slpcheck` subcommands (`rust/melee/src/main.rs:621-627`).
  `MELEE_WINDOW_HIDDEN=1` still uses the GPU and is not the headless path
  (`rust/melee_platform/src/main_loop.rs:327-332`).
- **No software-rendering fallback:** wgpu only, Vulkan by default, `force_fallback_adapter`
  false, requires `PUSH_CONSTANTS` (`rust/melee_gx/src/render.rs:1195-1220`). lavapipe is
  untested. This is why rendering is a non-goal.
- **Replay playback:** `melee golden --slp FILE` (`rust/melee/src/main.rs:998-1004`) boots and
  ticks a replay headless; `melee slpcheck` is the comparing verifier (below).
- **Speed, render off:** ~2500 f/s on slpcheck replays, ~3300 f/s with `golden --slp`
  (`docs/rust-port/notes/bench-unlocked.md:36-47`, `:75-85`) -- roughly 40-55x realtime. The
  desktop tree reports ~0.20 ms/tick, roughly 80x. Treat 40x as the planning number.
- **Frame-exactness on foreign replays is partial:** 86 of 125 local tournament files
  (`notes/slpcheck.md:226`), 392 of 428 on a larger set, on the laptop's `candidate11b` tree;
  the desktop's `candidate12` tree reports 102 of 125 and 3162 of 3479. The number is moving
  and tree-dependent. The design must not assume 100%.
- **Replay recording:** Slippi 3.18 `.slp` via `play --slp FILE|auto`
  (`rust/melee/src/slp_export.rs:1-12`). Netplay exports by replaying its own `.mrpl`.
- **`.slp` parsing exists:** `rust/melee_data/src/slp.rs` (~564 lines, from the public spec) --
  Game Start, Post-Frame, Gecko. Game Start to match setup in `rust/melee/src/slpcheck.rs`.
  No stats layer.
- **Netplay, own protocol:** room codes over a UDP relay, `rust/melee_relay` (std-only Rust).
  Packets `GDMR`/`GDMD` with `REG`/`JOIN`/`PEER`, a `RAND` queue, `RELAY`, and `SPEC`
  spectating. Client: `melee play --netplay host|join CODE --np-server S`
  (`rust/melee/src/play_net.rs:1195-1265`, `notes/net.md:244-255`). Configured server is
  `netplay.gsd.sh:51600`.
- **Direct connect, no rendezvous:** `--netplay host [--np-port 51500]` /
  `--netplay join ADDR[:PORT]` (`play_net.rs:1238-1255`).
- **Slippi protocol also present:** ENet plus Slippi messages (`rust/melee_net/src/slippi.rs`),
  WebSocket matchmaking client (`slippi_mm.rs`; loopback unless `--mm-real`, wss needs the
  `real-net` feature), subcommand `melee slippi-netplay` (`rust/melee/src/slippi_np.rs`).
  Tested against real Slippi Dolphin on mm.slippi.gg with 0 desyncs
  (`notes/slippi-online.md:563-640`). The notes flag Slippi's terms of service as unresolved
  (`:28-29`) -- relevant to "any client, later", not to v1.
- **One binary does both netplay paths**, as separate subcommands (`main.rs:629-631`).
- **Existing in-game UI to extend:** the browser mod does online host/join/spectate by code or
  `HOST:PORT` (`rust/melee/src/browser.rs:11-13`, `:190`, `:402-413`). The netplay lobby
  already implements 1-2-2 striking, ban/pick and counterpicks (`notes/net.md:36-40`).
- **Retail Tournament Mode is ported but unreachable** from the menu
  (`rust/melee_modes/src/tu/gmtou_0.rs`, `gmtou_1.rs`; `rust/melee/src/retail/mainmenu.rs:734`;
  `notes/parity.md:41` lists it MISSING). Not used by this design; noted so it is not
  mistaken for an entry point.
- **`slpcheck` is the verifier, not `golden`.** `melee slpcheck --slp F [--iso D] [--full]
  [--csv OUT.csv]` (`rust/melee/src/slpcheck.rs:368-379`) runs the engine on the replay's
  pre-frame inputs and compares fighter state against the recorded Post-Frame rows every frame
  (position, facing, percent, action, state frame, shield, stocks, hitlag, ...; floats
  bit-equal). `golden --slp` only writes the engine's own state hashes and never compares.
- **`slpcheck` output contract:** one stdout line,
  `SLPCHECK <file> match|diverged|refused|panic@F|boot-error|parse-error ...`, plus
  `first divergence: frame F port P field X engine=.. replay=..`. **Exit code 0 covers both
  `match` and `diverged`**; 1 is refused/parse-error/panic/boot-error; 2 is usage, missing
  disc, or unreadable file. A panic aborts the process (exit 134 on Linux). Machine-readable
  output is `--csv` only (append mode; columns include `status`, `first_frame`, `first_port`,
  `first_field`, `engine_value`, `replay_value`, `last_frame`). No JSON. It does **not** print a
  winner, stock totals, or a build id.
- **Runs headless on Linux:** builds with `cargo build --release -p melee`; deps in
  `tools/rust-port/linux/deps.sh` (`docs/rust-port/notes/linux.md:3-19`). Results match Windows
  on the 125-file set.
- **Runtime needs the disc image:** the user's own NTSC 1.02 dump (~1.4 GB), read in place, via
  `--iso` or `GW_ISO_VANILLA` (`slpcheck.rs:375`, `main.rs:786-787`). Akaneia/m-ex replays need
  a second disc; the service refuses those.
- **A netplay `.slp` carries no identity.** Port 1 is the host and port 2 the guest
  (`notes/net.md:201-202`). Names are placeholders `P1`..`P4`, nametags are zero, there are no
  connect codes, no match id, no free-form field, and no engine version or git revision
  (`rust/melee/src/slp_export.rs:233-270`; Game Start version is a constant 3.18.0.0).
- **Peers' exports agree in the `raw` element, not in file bytes.** Each peer exports after the
  match from confirmed inputs only, by replaying its `.mrpl` in a headless child process;
  this needs `--record` (`play.rs:1871-1878`, `play_net.rs:518-533`, `notes/net.md:174-176`).
  Metadata `startAt` is local wall-clock time, so whole-file hashes differ.
- **Own-build exactness is measured only on small samples** (a handful of matches, all equal;
  `notes/net.md:189-228`), and needed `SLPCHECK_VANILLA_SPAWN=1` because exports are played
  without Neutral Spawn and UCF (`net.md:218`, `:226-228`). No large corpus. The design must
  not assume own-build replays always re-simulate exactly.
- **Foreign-replay exactness, latest figure:** 7705 of 7800 on a tournament corpus
  (`notes/slpcheck.md:276`), up from 86 of 125 in earlier rounds.
- **No web backend anywhere.** Neither the game repo nor the Melee Workspace repo
  (`E:\Projects\Melee Workspace`: build/release tooling in bash and Python, a Qt/C++ launcher,
  a C# reference launcher, netplay tools, research notes) has accounts, auth, or a database.
  `notes/yougame.md:75-87` lists online, ranked, friends and accounts as not implemented. This
  service is greenfield.
- "GD's Workshop" is a Discord community (`README.md:23`, `:27`), not a codebase.

## Approaches

Three were considered.

**1. One binary -- extend `melee_relay`.** Put the bracket, players, results, and verification
into the existing std-only Rust relay. Simplest possible deploy, no new transport.
*Rejected:* the relay speaks UDP with a custom packet format and the bracket wants HTTP, so
this jams two protocols into one process. Worse, the component a TO must trust with results
becomes the same component forwarding live match packets -- coupled lifetimes, and a bracket
bug can take netplay down mid-set.

**2. A separate HTTP API beside the relay.** A new Rust service owns the bracket, players, and
results. A verification worker re-simulates submitted replays with the engine. The client polls
HTTP and plays over the netplay it already has.
*Trades:* two things to deploy rather than one. Buys a clean seam -- the relay stays untouched
and already works, the API ships without risking netplay, and the verifier can move to its own
box or be disabled independently. It is also the only shape where "any client later" is a
config flag rather than a rewrite.

**3. A thin bracket oracle with no transport involvement.** The API only tells players who to
fight; results arrive as uploaded replays it trusts.
*Rejected:* this is the untrusted model, and it cannot do hard verification. It is roughly what
the deferred "any client" mode degrades to, so it is a later configuration of (2), not a
starting architecture.

**Chosen: 2**, by the owner.

## Decided design

### Shape

Three processes, two of them already existing:

```
  Melee client  --HTTP-->  autobracket API  <--jobs--  verifier worker(s)
       |                         |                           |
       |                    SQLite file                 melee slpcheck --slp
       |
       +------UDP------>  melee_relay (unchanged)
```

- **API service.** Rust, `axum`, `tokio`. Owns brackets, players, sets, games, results.
- **Store.** SQLite via `sqlx`, one file. Chosen so a TO can back up a tournament by copying a
  file, and so the service has no external dependency. Postgres is not supported in v1.
- **Verifier.** A worker that pulls queued replays, runs the engine headless as a subprocess,
  and writes back a verdict. In-process by default (a tokio task pool); runnable as a separate
  binary against the same API for TOs who want the game binary off their web host.
- **Relay.** `melee_relay`, unchanged. No new code, no new protocol.

*My call:* Rust and axum, because the engine, the relay, and the `.slp` parser are already Rust
and the verifier wants to link or shell to engine code. SQLite, for the self-host story above.

### Identity without accounts

A player's identity is an Ed25519 public key the client generates on first run and keeps in its
config. The key fingerprint is the player id. A display tag (`NAME#1234`-ish) is chosen per
instance and is cosmetic.

Every mutating request is signed by the client; the API verifies the signature and a nonce. A
TO can require an invite code to register a key on their instance.

*My call,* and it follows from the anti-goal: no email, no passwords, nothing to leak, no
recovery flow to build. The cost is that losing the key loses the identity -- acceptable for
nightly brackets, and an instance can let a TO re-point a key by hand.

### Match lifecycle

1. A TO creates an event with a ruleset and an entry window. Players sign up.
2. At start, the service seeds and builds the bracket. Single and double elimination in v1;
   the bracket engine is written against an interface so round robin and pools can follow.
3. When both players of a set are present, the service picks one as **host** and tells both.
4. The host client registers with `melee_relay` itself (the existing `REG` flow) and reports
   the room code back to the API. The API hands that code to the opponent, who joins.
   *My call:* this needs no changes to `melee_relay` at all -- the API never speaks the UDP
   protocol, it only passes a short string. Direct connect by `HOST:PORT` is accepted as an
   alternative for LAN events.
5. The players play. In phase 1 the reference client launches `melee play --np-css`, one process
   and one room code per game: characters and stage are picked on the in-game screens, and the
   server checks stage legality afterwards. The lobby's striking, ban/pick, and counterpick
   flow is not reachable from `melee play` (`play_net.rs:1168`), so proper striking arrives
   with the phase 2 native menu.
6. After each game, **both** clients upload their exported `.slp`, in a signed request that
   names the set, the game number, and the client's engine build.
7. The verifier re-simulates and issues a verdict. The set advances on verified games.
8. When a set is decided per the ruleset, the bracket advances and the next set is offered.

A set never advances on a single unverified claim. If one player disconnects or never uploads,
the set goes to a TO queue for a human decision -- with whatever evidence exists attached.

### Verification

What makes a result trustworthy is three independent checks. None relies on a player's word:
the server computes the winner itself from the replay.

1. **Peer agreement.** Both players upload. The server compares the `raw` element of the two
   files (not file bytes -- `startAt` differs). Identical means both machines recorded the same
   game. A player who uploads an edited or locally fabricated file is caught here, because the
   opponent's honest file will not match.
2. **Re-simulation.** The verifier runs `melee slpcheck --slp F --iso D --csv OUT` and reads the
   `status` column. `match` means the recorded game states follow from the recorded inputs, so
   the ending was not doctored. This is what protects a set when only one upload exists, and
   what feeds the quarantine corpus.
3. **Ruleset.** Game Start is parsed with `melee_data::slp` and checked against the event:
   legal stage and characters, stock count, timer, and the Gecko code list.

The **winner** is derived by the server from the replay's Game End event and final Post-Frame
stocks and percent. Player mapping uses the netplay convention -- port 1 is whoever the API
named host for that set, port 2 the guest -- because the file itself carries no identity.

Verdicts:

- **verified** -- peers' `raw` identical, `slpcheck` says `match`, ruleset legal. Game counts.
- **verified-inexact** -- peers' `raw` identical and ruleset legal, but `slpcheck` says
  `diverged` (or panics). Game counts; the replay is quarantined as an **engine bug**, not as
  cheating. Both machines independently recorded the same game, so a divergence here is the
  engine failing to reproduce itself.
- **conflicted** -- the two uploads differ. Game does not count; TO queue, flagged.
- **single** -- only one upload arrived before the timeout. Never auto-advances. TO queue, with
  the `slpcheck` result attached as evidence.
- **illegal** -- ruleset violation. Game does not count; TO queue.

*My call, changed after review:* an earlier draft treated any own-build divergence as a
probable cheat. Own-build exactness has only been measured on a few matches and depends on
spawn/UCF settings, so that rule would risk flagging honest players nightly. A config switch
`strict_resim = true` restores the hard rule for TOs who want it once the corpus proves the
engine out.

Limits, stated honestly:

- Two players colluding can always produce any result -- they could equally throw the match.
- A lone upload cannot be *proven* to be the bracket game, since the file has no match binding;
  that is why `single` always goes to a human. Phase 2 closes most of this by having the engine
  write a server-issued nonce and its build id into the replay metadata (a small engine change:
  `names` is already a parameter of the exporter's `install()`).
- An instance configured without the engine and disc image skips check 2 and runs on peer
  agreement and ruleset alone.

Mechanics: one `slpcheck` process per file, with a timeout; exit code alone is not a verdict.
Two traps confirmed by both engine trees: `slpcheck` also reports `match` when the engine's own
match ends early (`slpcheck.rs:578-581`), so the verifier must check the CSV `last_frame`
against the replay's last frame and Game End before accepting `match`; and own-build exports
carry no Gecko list, so the verifier must set `SLPCHECK_VANILLA_SPAWN=1` for them
(`notes/net.md:218`). The engine build is read from `melee version`.
Only one of the two identical uploads needs re-simulating. At the 40x planning figure a 7
minute game is ~10 s of one core; a 32-player double-elimination night is on the order of 150
games -- under half an hour of one core across the evening. The verifier records its own engine
build (git describe plus binary hash) with every verdict, since replays do not carry one.

### Quarantine corpus

Every `verified-inexact`, `conflicted`, and `single` replay is stored with: the file(s), the
verifier's engine build, the `slpcheck` CSV row (first diverging frame, port, field, engine
value, replay value), the Game Start block, and the uploading clients' reported builds. An
endpoint lists the corpus and serves individual cases.

This is the feedback loop into the port: a real-world, continuously growing set of replays the
engine does not yet reproduce, each with a divergence point. It is a deliberate second product
of the service, not a side effect.

### Client integration

The player-facing product is **in the game**: a native main-menu entry (the owner's "diegetic
main menu" direction), where a player picks an instance, signs up, sees their next set, gets
placed into the match, and watches the bracket. The Qt launcher is considered obsolete and is
not a target.

That menu needs a working, proven API to talk to, so the work is ordered:

**Phase 1 -- API plus a reference client (this repo, no engine changes).** A small
command-line program, `autobracket-client`, that holds a player key, talks to the API, shows
the next set, and launches the engine with flags it already has:
`melee play --netplay host|join CODE --np-server S --record F --slp auto --np-css`. It reads
the room code from the engine's stderr, reports it to the API, and uploads the exported `.slp`
when the engine process exits after each game. It is a **test harness and reference
implementation**, not the product: it lets the whole bracket-and-verify loop be built and
exercised end to end before the engine is touched, and it documents by example exactly what
the in-game menu must do. It is also the adaptor shape a client the project does not control
would later use.

**Phase 2 -- native menu entry (melee repo).** The in-game menu does natively what the
reference client does, against the same API, building on the existing online host/join code
paths (`rust/melee/src/browser.rs`) and the existing lobby. The exporter also stamps the
server nonce and build id into replay metadata. This work lives in the melee repo; this repo
defines the API it talks to.

### Deployment

`autobracket serve --config autobracket.toml`. Config covers bind address, SQLite path, replay
storage directory, relay address handed to clients, invite-code policy, TO keys, accepted
engine builds, verifier mode (in-process, external, or off), engine binary path, disc image
path, `strict_resim`, and the ruleset defaults. systemd unit shipped, modelled on the hardened
`gdmelee.service` already in `~/gdmelee-server/`.

The disc image is the TO's own dump and is never distributed with the project. A TO who does
not want it on a public VPS runs the verifier externally, on a machine at home, against the
same API.

### TO authority

TOs are Ed25519 public keys listed in the config. They use the same signed API as players,
with extra routes: create and start events, issue invite codes, resolve the queue (award,
replay, or disqualify), and re-point a player's key. An `autobracket admin ...` CLI wraps those
routes. There is no separate login system. *My call.*

### Build versions

Clients state their engine build when registering for an event and on every upload. Each
instance lists the builds it accepts; a client on another build is refused at sign-up with a
clear message rather than failing verification later. The verifier must run a build on that
list. *My call* -- it keeps a client update from turning honest players into flagged ones.

### Any client, later

The deferred mode is a config flag. It needs: peers on different clients whose `raw` elements may
not be byte-identical (so peer agreement compares derived outcome instead), reliance on
`slpcheck` for foreign replays (7705 of 7800 frame-exact on the latest corpus, with the misses
quarantined and counted on agreement), and a way for a player on another client to submit
without the reference client. It changes no architecture. Slippi's terms of service are flagged
unresolved in the engine notes and should be settled before that mode ships.

## Left to the builder

- The HTTP route table and payload shapes, including signing envelope and nonce scheme.
- The SQLite schema and migrations.
- The bracket engine's internals: seeding, bye placement, double-elimination progression, and
  the trait boundary that lets round robin follow.
- Ruleset representation and which parts are TO-configurable in v1.
- Job queue mechanics for the verifier, retry and timeout policy, and how a crashed
  re-simulation is distinguished from a diverging one.
- Replay storage layout, retention, and size limits.
- Rate limiting and request-size limits.
- Whether the verifier links `melee_data`/engine crates directly or always shells out. Shelling
  out is the safer default; linking is an optimisation.
- The read-only bracket view's implementation.
- Test strategy. The project's own replays are the obvious fixture corpus.

## Open to the owner

- Phasing (API and reference client first, native menu entry second): confirmed by the owner.
- Whether this repo (`melee-autobracket`) is the home for the API and reference client, with
  engine-side work landing in the melee repo. Assumed yes.
- Licence. Not chosen.
