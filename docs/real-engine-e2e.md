# Real-engine end-to-end run

Date: 2026-11-15. Machine: the dev server (8 cores, 22 GB RAM, no GPU, no display: neither
`DISPLAY` nor `WAYLAND_DISPLAY` is set).

| | |
|---|---|
| autobracket | branch `lane-e2e`, base commit `925fe83` (merge: lane-client); binaries from `cargo build --release -p autobracket-server -p autobracket-client` |
| engine | `/home/gd/melee-engine` at `0cd085d` (branch port/candidate12), binaries `build/cargo/fast/melee` and `melee_relay`; `melee version` = `melee (rust) 0.1.0 development build` |
| verifier build recorded | `melee (rust) 0.1.0 development build+339fd9aabad8` |
| disc | your own NTSC 1.02 image (here `/home/gd/Super Smash Bros. Melee (USA) (En,Ja) (v1.02).iso`) |

## Result

**The happy path works with the real engine, unchanged code, and configuration only.** One real
netplay game (Captain Falcon host vs Fox guest, Final Destination, 60 s, 4 stocks, scripted pads,
both engines headless) went: room code from the host's engine to the server, guest joined with
it, game played, both engines exported a `.slp`, both clients uploaded, the in-process verifier
ran the real `melee slpcheck`, the game was judged `Verified` / resim `Match`, the set and the
event completed, and the clients ended `Champion` / `Eliminated`. Reproduced six times with the
`--match` start (runs `run1`, `run-restart`, `run-final`, `run-verify` and the scenario runs that
reached judging) and once with the client's default `--np-css` templates (`run-css`).

The three failure scenarios asked for also behaved as designed: one upload -> `Single` -> queue
-> `award` -> event complete; an illegal stage -> `Illegal("stage")` -> queue; a server restart
in mid-game -> the event still completes, `Verified`.

No source file was changed. The defects found are listed below; none blocks the happy path.

## How to run it

```sh
. ~/.cargo/env && cargo build --release -p autobracket-server -p autobracket-client
E2E_DIR=/some/scratch/dir scripts/e2e-real-engine.sh          # --match start (default)
ENGINE_MODE=css E2E_DIR=/some/scratch/dir scripts/e2e-real-engine.sh   # client default templates
```

`scripts/e2e-real-engine.sh` takes the engine, relay, pad-script directory, disc image, scratch
directory and ports from environment variables (defaults documented at the top of the script),
starts relay + server + two clients, cleans up every process it started with an EXIT trap, and
exits 0 only if a game was counted and the event is `complete`. It wipes `E2E_DIR` only if the
directory is empty or was made by the script (marker file). Ports: HTTP 18311, relay UDP 51711,
host netplay UDP 51712, all on 127.0.0.1.

What the script does, in the order the brief asked for:

1. `melee_relay --port 51711 --bind 127.0.0.1 --bind6 off --quiet`
2. `autobracket keygen --out to.key`, then this `server.toml`, then `autobracket serve --config server.toml`:

   ```toml
   bind = "127.0.0.1:18311"
   db_path = "<E2E_DIR>/autobracket.sqlite"
   replay_dir = "<E2E_DIR>/replays"
   relay = "127.0.0.1:51711"
   to_keys = ["<printed public key>"]
   upload_timeout_secs = 120

   [verifier]
   mode = "in_process"
   engine = "/home/gd/melee-engine/build/cargo/fast/melee"
   iso = "<disc image>"
   timeout_secs = 300
   jobs = 2

   [ruleset]
   stocks = 4
   timer_secs = 60
   best_of = 1
   best_of_finals = 1      # validation requires both odd; 1 is accepted
   legal_stages = [2, 3, 8, 28, 31, 32]
   ```

3. Per player: `autobracket-client --config pN/client.toml init --melee MELEE --iso ISO --work-dir pN/work`,
   edit `host_args` / `join_args` (below), `register --server http://127.0.0.1:18311 --tag HostFalcon|GuestFox`.
   Organiser: `autobracket admin --server URL --key-file to.key event create --name ... --format single --best-of 1`;
   players `signup 1`; `admin ... event start 1`.
4. `MELEE_PAD_SCRIPT=np_fight.txt autobracket-client --config p1/client.toml play 1 --poll-secs 1` and
   `MELEE_PAD_SCRIPT=np_fight2.txt autobracket-client --config p2/client.toml play 1 --poll-secs 1`, both
   with `MELEE_VOLUME=0 MELEE_PAD_IGNORE_ADAPTER=1 MELEE_NETPLAY_BIND=127.0.0.1` in the environment
   (the engine inherits the client's environment; the client itself sets `MELEE_SLP_DIR` to the run
   directory, overriding any inherited value).
5. A watcher polls the SQLite file every 100 ms and logs each state change with a timestamp
   (`watch.log`), because the server itself logs nothing (see defect 3). A sampler records each
   game process's CPU ticks (`load.log`).
6. Afterwards: `GET /v1/events/1`, `GET /`, `admin queue list`, `admin quarantine list`, and the
   `games` / `uploads` rows.

## Changes needed versus the defaults

Server (`deploy/autobracket.example.toml` defaults): `bind`, `db_path`, `replay_dir`, `relay`,
`to_keys` as above; `[verifier] mode = "in_process"` with `engine` and `iso`; `timeout_secs = 300`
(only head-room; slpcheck takes ~2.4 s); `[ruleset] timer_secs = 60, best_of = 1, best_of_finals = 1`
to match the scripted game. Nothing else.

Client `host_args` / `join_args` (defaults in `crates/client/src/config.rs:55-85`):

| default flag | --match run (default mode) | why |
|---|---|---|
| (none) | `--headless --no-audio` added | Required on this machine. Without it the engine exits at once: `play: event loop: os error at .../winit-0.30.13/src/platform_impl/linux/mod.rs:765: neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set.` (experiment D1). Players with a desktop do not need it. The hidden-window variables are not needed and not used. |
| `--np-css` | removed | The scripted fight pads (`np_fight*.txt`) do not drive a character select. With them, the lockstep menus wander CSS -> main menu -> name entry and **both engines panic** (defect 1, experiment D2). |
| `--quit-after-matches 1` | removed | Only meaningful with `--np-css`; a `--match` netplay run is one match per connection and quits on its own. |
| (none) | `--match falcon,32` (host) / `--match fox` (guest) | Picks fighters and stage without menus. |
| `--stocks {stocks}` | dropped (kept would also work) | Experiment D3: `--stocks 4 --time-secs 60` with `--match` plays the same 4-stock / 60 s game (`np_rules=s4,t60`), replay judged identically. Without `--stocks` the scene is still `match=stock;stocks=4`. |
| `--time-secs {timer_secs}` | kept | The guest's value is replaced by the host's anyway (`the host's rules: ... time 60 s`). |
| (none) | `--np-lan 127.0.0.1 --np-bind 127.0.0.1` | Keeps all traffic on loopback as required here. Without them (D2, D3) the host advertised the machine's public address (`dialling 192.99.42.92:...`) and it still connected on this machine. |
| (none) | `--np-port 51712`, `--np-seed 4242`, `--max-frames 9000` | Recipe fidelity and a hard stop; not required. |

Pad scripts differ per role, so they cannot go in the shared templates: each client process gets its
own `MELEE_PAD_SCRIPT` in its environment.

`ENGINE_MODE=css` keeps the client's default templates **verbatim plus only `--headless --no-audio`**,
with CSS-navigating pad scripts generated by `tools/rust-port/netplay/gen_css_scripts.py` (Mario v
Fox on Battlefield) and `MELEE_NP_SCRIPT_NO_START=1`. That passed too (`run-css`: `Verified`,
`Match`, stage 31). So the client's default flags work headless with scripted pads, provided the
scripts drive the character select. `--np-css` with `--headless` had not been tested by the engine
authors before; it works (also standalone, experiment D4: `quit after 1 matches`, both `.slp`
byte-identical).

## Timeline of a successful run (`run-final`, 2026-11-15)

Script output:

```
18:07:57.129 e2e: server: listening on 127.0.0.1:18311
18:07:57.280 e2e: event 1 started
18:07:57.282 e2e: clients started (pids 1739268 1739269)
18:09:02.401 e2e: client 1 exited 0
18:09:02.403 e2e: client 2 exited 0
18:09:02.454 e2e: p1: Champion
18:09:02.457 e2e: p2: Eliminated
game (1, 0, 1, 'counted', 0, '"Verified"', 'melee (rust) 0.1.0 development build+339fd9aabad8', '"Match"')
game pid 1739292: 56% of one core over 58 s
game pid 1739307: 50% of one core over 58 s
18:09:02.561 e2e: games counted: 1; event state: complete
18:09:02.563 e2e: PASS
```

Database state changes (`watch.log`, unix time; 18:07:57 = 1794791277):

```
1794791277.351 events 1 running
1794791277.351 sets 1/0 ready room=74AV 0-0        <- host's room code reached the server
1794791337.677 games 1 uploading                   <- host upload (game ended ~60 s after start)
1794791337.677 uploads 1/0 1794791337
1794791338.685 games 1 judging                     <- guest upload, worker claims the pair at once
1794791338.685 uploads 1/1 1794791338
1794791341.206 events 1 complete
1794791341.206 sets 1/0 done room=- 1-0
1794791341.206 games 1 counted "Verified"
```

Host client (`p1/play.log`, engine lines interleaved by the client):

```
Set 0 vs GuestFox — game 1 of best-of-1 (0-0) — you host
Starting game 1 as host
play: netplay host: MATCH CODE 74AV (give it to the other player)
Room code 74AV sent
play: netplay host: path direct (hole punched)
play: netplay host: match agreed after 1.01s: "mode=vs;at=match;p1=ck:0/c0/hu;p2=ck:2/c0/hu;stage=ext:32;match=stock;stocks=4;minutes=1;items=off;pause=0;np_rules=s-,t60,i-" (seed 4242, delay 2, rollback window 7)
play: netplay host: 3275 final hashes (through game frame 3273), rollbacks 0 (max depth 0, resim 0), stalls 0, waits 1, session desyncs 0, checksum desyncs 0, ping 2 ms, ...
gw: slp: 3161 frames (-123..3037) written to .../p1/work/run-1/Game_20261116T010857.slp
play: replay: 3276 ticks played, 3276 match frames hashed, 0 mismatches, final hash 0086A273 (file 0086A273)
Uploaded game 1
Game 1 uploaded; waiting for the verdict
Champion
```

Guest client (`p2/play.log`):

```
Set 0 vs HostFalcon — game 1 of best-of-1 (0-0) — you join
Waiting for HostFalcon to send the room code
Joining room 74AV for game 1
play: netplay guest: path direct (hole punched)
play: netplay guest: the host's rules: stocks default, time 60 s, items default
gw: slp: 3161 frames (-123..3037) written to .../p2/work/run-1/Game_20261116T010857.slp
play: replay: 3276 ticks played, 3276 match frames hashed, 0 mismatches, final hash 0086A273 (file 0086A273)
Uploaded game 1
Eliminated
```

Server log: only `listening on 127.0.0.1:18311` (defect 3).

### Inspection

- `games` row: `state = counted`, `winner_slot = 0`, `verdict_json = "Verified"`,
  `resim_json = "Match"`, `verifier_build = melee (rust) 0.1.0 development build+339fd9aabad8`.
- Replay facts (Game Start / Game End, read independently with a small Python reader): Slippi
  3.18.0, stage 32, timer 60 s, not teams, P1 Captain Falcon (0) human 4 stocks, P2 Fox (2) human
  4 stocks, last frame 3037, Game End method 2 (GAME!).
- Stored replays `replays/1/0/1/0.slp` and `1.slp`: same SHA-256
  (`f3d492b4...1760`), **byte-identical**, and identical to the clients' local files. So the `raw`
  elements are identical too. This held for every pair in every run (the README's remark that the
  whole files differ by start time did not show up: both peers export the same `startAt`).
- `queue`, `quarantine`, `resim_jobs`: empty; `admin queue list` and `admin quarantine list` print `[]`.
- `GET /v1/events/1`: `summary.state = "complete"`, `tags = ["HostFalcon","GuestFox"]`, the one
  set `"state": {"Done": {"winner": 0}}`, `"scores": [[0, [1, 0]]]`. There is no explicit
  champion field; the page derives it.
- `GET /` (13.7 kB, no external requests) loads `/v1/events` and `/v1/events/{id}` with `fetch`
  and computes the champion in `champion(bracket)`: every set resolved, the set with
  `winner_to === null` won by entrant 0, so it renders `Complete · 2 entrants · Best of 1, 4 stocks,
  1:00 · Champion: HostFalcon` (read from the code and the JSON; no screenshot taken).

## Measurements

| what | value |
|---|---|
| both uploads received -> game counted | **2.5 s** (`run-final`: second upload 18:08:58.685, counted 18:09:01.206; `run1`: 2.6 s; `run-restart`: 2.7 s; `run-css`: 1.6 s). The worker claims the pair the moment the second upload lands. |
| `slpcheck` alone on the same replay | 2.27-2.47 s over 3 runs (3161 frames = 52.7 s of game, about 22x realtime; the README says roughly 40x) |
| first upload -> second upload | 0-1 s (the two clients poll independently, `--poll-secs 1`) |
| whole run, script start to PASS | 66 s wall for a ~52 s game; 45.5 s user + 28.9 s system CPU for everything (engines, export children, server, clients, slpcheck) |
| CPU per headless engine during the match | 50-56 % of one core each (from `/proc/PID/stat` ticks over 58 s); 2 engines use about 1.1 of 8 cores. 1-minute load average stayed at 2.3-2.9, but another agent's test suite was running `melee slpcheck` processes on this machine at the same time, so the load average is not attributable to this run alone. |
| replay size | 1,031,527 bytes for 3161 frames (~326 B/frame, ~19.6 kB per second of game). An 8-minute game (28,923 frames) would be ~9.4 MB, under the 16 MiB `max_replay_bytes` default. `game.mrpl` (the engine's own recording) is 223 kB. |

## Scenarios

The scenario hooks live in the scratch directory, not in the repository; each is a short shell
script run through the script's `E2E_HOOK` variable (it exports `E2E_DIR URL EVENT_ID AB C1 C2
SERVER_PID`).

### (a) Only one client uploads -> `Single` -> queue -> award

`UPLOAD_TIMEOUT_SECS=20`. Hook: wait until the guest's `engine.log` shows `netplay guest:
started`, `kill` the guest's **client** only (its game keeps running in its own process group, by
design), then wait for the queue and `admin queue resolve 1 award --slot 0`.

```
1794790525.419 games 1 uploading
1794790525.419 uploads 1/0 1794790525          <- host only
1794790545.436 sets 1/0 review room=- 0-0      <- 20 s later
1794790545.437 games 1 review "Single"
1794790545.437 queue 1 open Single
1794790550.809 events 1 complete               <- after `queue resolve 1 award --slot 0`
1794790550.809 sets 1/0 done room=- 0-0
1794790550.809 games 1 void "Single"
```

The game was never counted (`void`), the replay was quarantined (`quarantine list` showed kind
`Single`), the host client printed `Set 0 is being reviewed by the organiser`, then `Champion`.

Variant (a2), a real disconnect: kill the guest's client **and** game 10 s into the match. The
host engine reported `ERROR: connection lost: peer timed out`, still exported a 620-frame replay
(Game End method 7, No Contest), exited 1, and the host client uploaded it: same `Single` ->
queue -> award path, event complete.

Variant (a3), a late manual upload: after the `Single` verdict the guest ran
`autobracket-client upload 1 0 1 <its .slp>` and got `Error: set is not in play (HTTP 409)`. The
organiser then chose `queue resolve 1 replay`: the game was voided and the set went back to
`ready` for game 1; the host client relaunched and posted new room codes, and with no guest each
run ended `the game exited with exit status: 1` / `Game 1: no replay produced` with backoff
(5 s, 15 s, ...). Nothing reached the organiser's queue (defect 2).

### (b) Ruleset mismatch -> `Illegal` -> queue

`LEGAL_STAGES="[31]"`, so Final Destination (32) is not legal for the event:

```
1794790446.761 sets 1/0 review room=- 0-0
1794790446.761 games 1 review {"Illegal":"stage"}
1794790446.761 queue 1 open Illegal
```

No re-simulation ran (`resim_json = "Skipped"`). Both clients printed `Set 0 is being reviewed by
the organiser`; after `award --slot 0` the event completed.

### (c) Server restart in mid-game

Hook: after the room code was on the server (`room=QNNE`) and the match had started, `kill` the
server (SIGTERM), wait 15 s, start it again with the same config and database.

```
17:57:01.378 stopping server 1701785
17:57:16.899 server restarted: listening on 127.0.0.1:18311
```

Clients: `Server unavailable (... Connection refused (os error 111)); retrying` then `Server is
reachable again`. The game finished while the server was back, both uploads landed, and the game
was `counted` / `Verified` / `Match`, event complete, `Champion` / `Eliminated`.

## Defects found

Severity is for a real tournament.

1. **Engine panic on the name-entry screen, reachable from the netplay character select
   (engine; medium).** Experiment D2: the client's default templates (`--np-css
   --quit-after-matches 1`) plus `--headless --no-audio`, with the fight pad scripts. The lockstep
   menus went `css` -> `menu` (tick 439) -> `css` -> `menu` (tick 1011) -> name entry, and both
   peers aborted identically (exit 134):
   `thread 'main' panicked at melee_modes/src/tu/mnnamenew.rs:1842:13: index out of bounds: the len is 8 but the index is 8`
   in `mnNameNew_MainInput` -> `WriteCharactersForNameAtIndex` (`(*nametag).namedata[idx]` with no
   bound on `idx`). Because the menus run in lockstep, one player's input crashes both games. A
   real player can reach name entry from the CSS. Crash reports:
   `crashlogs/crash-20261116-005909-1705083.log` (host) and `-1705097.log` (guest).
2. **A no-show never reaches the organiser (server; medium).** Nothing on the server times out a
   `ready` set whose game is never played. When the guest never joins, the host client relaunches
   (each run times out after the engine's 60 s `--np-timeout`), and after
   `MAX_FAILED_RUNS = 4` runs (`crates/client/src/play.rs:20`, bail at `:150`) `play` exits with an
   error, while the set stays `ready` with no queue item. Seen in (a3). The organiser has to notice
   by watching the bracket.
3. **The server logs nothing about the tournament (server; medium for operations).** Across every
   run `server.log` held only `listening on 127.0.0.1:18311`: no registrations, room codes,
   uploads, verdicts, queue items or re-simulation times. The worker prints only on errors
   (`crates/server/src/worker.rs:27`, `:140`, `:171`). To get the timeline above I had to poll SQLite.
4. **`verifier_build` is stamped on games that were never re-simulated (server; low).**
   `crates/server/src/worker.rs:164`: `Pre::Final(v) => (v, None, ResimOutcome::Skipped, st.resim.build())`.
   The `Illegal` and `Single` games carry `verifier_build = melee (rust) 0.1.0 development build+339fd9aabad8`
   with `resim_json = "Skipped"`, and `quarantine list` shows it too, which reads as if that build
   checked them.
5. **No judged/counted timestamp; upload times in whole seconds (server schema; low).**
   `crates/server/migrations/0001_init.sql` `games` has no time of verdict, and `uploads.uploaded_at`
   is in whole seconds. You cannot audit from the database how long judging took.
6. **The engine runs in the client's working directory (client; low).** `crates/client/src/engine.rs:142`
   spawns the game without `current_dir`, so anything the engine writes relative to its working
   directory (its `crashlogs/` folder) goes to wherever the player started `autobracket-client`,
   not the run directory. In D2 the crash reports landed in the cwd; `engine.log` only says
   `crash report written to crashlogs/...`.
7. **Killing `play` (SIGTERM / Ctrl+C) leaves no hint about the game it leaves running (client;
   low).** The game keeps running and writes its replay to `work/run-N/` (good, intentional), but
   `report_unfinished` (`crates/client/src/play.rs:196`) runs only when `run` returns an error,
   not on a signal, and `CommandEngine::drop` does not run on a signal either. In (a) the guest's
   `play.log` ends at `Joining room ...`. The player is not told the `upload` command, and an
   upload after `upload_timeout_secs` is refused anyway (409, (a3)).
8. **An awarded set reports a 0-0 score (server; cosmetic).** After `award --slot 0` the set is
   `done` with `wins0 = wins1 = 0`, so `scores` says `[0, [0, 0]]` next to `Done {winner: 0}`.
9. **README inaccuracies (docs; low).** The README says the whole `.slp` files differ because
   each peer stamps its own start time, but every real pair here was byte-identical. It also says
   re-simulation runs at "roughly 40x realtime", but measured here it was about 22x.

Not defects, noted for operators: the client calls the first set "Set 0" (bracket ids start at 0);
`GET /v1/events/{id}` has no champion field (the page computes one); `resolve replay` and
`resolve award` print nothing on success.

## Experiments outside autobracket (standalone engine pairs)

Same relay, same pads, engines started directly with the argv the client would build.

| | args | result |
|---|---|---|
| baseline | recipe + `--headless --no-audio` | OK in 60 s; `.slp` byte-identical on both peers; `slpcheck` match, 6114 fighter-frames |
| D1 | client defaults verbatim | both exit at once: no display (winit) |
| D2 | client defaults + `--headless --no-audio`, fight pads | both panic in name entry (defect 1) |
| D3 | `--match` + `--stocks 4 --time-secs 60`, no `--np-lan/--np-bind` | OK, `np_rules=s4,t60`, same facts, identical files |
| D4 | client defaults + `--headless --no-audio`, CSS pads from `gen_css_scripts.py` | OK, Mario v Fox on Battlefield, `quit after 1 matches`, identical files |

## Not tested

- Two machines, a real network, NAT, the hole-punch failure path and the relay fallback (every
  run was loopback; all connected `path direct (hole punched)`).
- Windowed engines (no display on this machine) and real controllers.
- Double elimination, more than two entrants, best-of-3/5 sets, more than one game per set (each
  run was a single set and a single game). `--quit-after-matches 1` with more than one game per set
  in CSS mode was not exercised.
- `verifier.mode = "external"` / `verify-worker` with the real engine, and `strict_resim = true`.
- `Conflicted` with real replays (two different real games for the same set), `Undecided`, a
  verdict of `VerifiedInexact` from a real divergence, `require_invite`, `accepted_builds`, rekey,
  `dq`.
- Feeding the fixture pairs in `/home/gd/autobracket-fixtures/autobracket-fixtures/own-netplay/`
  through `autobracket-client upload` (not needed: the happy path worked).
- A rendered screenshot of the bracket page (read from the HTML and JSON only).
- Load with several sets at once, or `jobs` > 1 re-simulations at once.
