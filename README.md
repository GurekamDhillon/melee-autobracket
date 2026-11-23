# autobracket

A self-hostable HTTP service that owns a Super Smash Bros. Melee tournament bracket and
advances it from verified match results. A tournament organiser runs one binary with one
SQLite file on a small VPS, players run a command-line client next to the game, and nightly
brackets run without a human updating the bracket by hand.

Results are proven, not reported. Both players upload the replay of every game. The server
compares the two, checks them against the event's ruleset, takes the winner from the replay
itself, and re-simulates it with `GurekamDhillon/melee`, a native Rust port of Melee that
replays a `.slp` deterministically and headless, with no GPU. Re-simulation runs at about 20x
realtime on one core: about 10 s for a 3-minute game, about 25 s for a full 8-minute game.
Replays that fail to re-simulate exactly are kept, with their divergence point and engine
build, as a corpus for fixing the port. A bracket that cannot be lied to is also the honest
argument for self-hosting: an organiser running their own instance gets result integrity, not
just control.

Phase 1 (this repository) is the API, public pages (landing, schedule, history, players,
stream) and the bracket viewer, and a reference command-line
client, `autobracket-client`, that drives the engine. There are no accounts: a player is an
Ed25519 key their client generates, plus a display tag. The HTTP API, including the request
signing, is documented in [docs/api.md](docs/api.md) for anyone writing another client.

## Install

Three ways to a running server, shortest first. Each ends at the same place: the server
running on this machine with a config that names your organiser key, and the key itself in
`organiser.key`. Then do the three things in [After installing](#after-installing).

A release holds `autobracket` (the server, with the admin CLI, `init`, `keygen` and
`verify-worker`) and the player's `autobracket-client`, for Linux x86_64. It does **not**
include the game engine or a disc image (see [Requirements](#requirements)), so a new server
runs with verification off: games count on peer agreement and the ruleset alone (verdict
`PeerOnly`), which means both players' replays must agree and be legal but nothing
re-simulates them. Turning verification on is optional; see
[Verifier modes](#verifier-modes).

### A. Release archive

The repository is private and has no published release yet: `<release URL>` below stands for
the address of a release's files once there is one. Until then, build the same archive
yourself with `scripts/build-release.sh` (path C). The binaries are built on Ubuntu 24.04
(the release workflow's runner, `ubuntu-24.04`), which is the baseline: they need glibc 2.39
or newer (Ubuntu 24.04, Debian 13 or later).

```sh
curl -LO <release URL>/autobracket-0.1.0-x86_64-unknown-linux-gnu.tar.gz
curl -LO <release URL>/SHA256SUMS
sha256sum -c SHA256SUMS
tar xzf autobracket-0.1.0-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 755 autobracket-0.1.0-x86_64-unknown-linux-gnu/autobracket* /usr/local/bin/

autobracket init --dir ~/autobracket       # key, config, replays/; prints the next steps
autobracket serve --config ~/autobracket/autobracket.toml
```

`init` writes `organiser.key` (mode 0600), `autobracket.toml` (verifier off, listening on
`127.0.0.1:8080`; every other setting shown commented out at its default) and `replays/`, then
prints the commands to create and start your first event with that key. `--bind ADDR`,
`--proxy-cidr CIDR` (repeatable: your reverse proxy, for `trusted_proxies`) and
`--public-url URL` (used in the printed commands) adjust it; it refuses to replace an existing
key or config without `--force`. To run the server as a service, see [systemd](#systemd).

### B. Docker Compose

From a checkout of the repository (`github.com/GurekamDhillon/melee-autobracket`, private for
now); no prebuilt image is published. The image is Debian 13 slim with only the `autobracket`
binary, runs as uid 10001, and keeps everything in the volume mounted on `/data`.

```sh
docker compose -f deploy/docker-compose.yml build
docker compose -f deploy/docker-compose.yml run --rm autobracket init --dir /data \
  --bind 0.0.0.0:8080 --proxy-cidr 10.203.47.1/32
docker compose -f deploy/docker-compose.yml up -d
```

Skip step 1 of what `init` prints (`autobracket serve ...`): `docker compose up -d` runs the
server. It is then published on the host's `127.0.0.1:8080` only, with a read-only root file
system, no capabilities and no privilege escalation. The Compose network is pinned to
`10.203.47.0/28`, so a proxy on the host reaches the server from the gateway `10.203.47.1`,
the one address `--proxy-cidr` trusts. The comments in `deploy/docker-compose.yml` show how to
run the admin CLI with `docker compose exec`, how to edit the config, and how to mount your
own engine and disc image read-only for the `in_process` verifier.

### C. Build from source

You need:

- **Rust 1.94 or later** (sqlx 0.9 needs it; it is the workspace's `rust-version`).
- **A C compiler** (`cc`, e.g. gcc or clang): `aws-lc-sys` (TLS) and the bundled SQLite are
  built from C source. Nothing else: no OpenSSL, no system SQLite.

```sh
cargo build --release
# target/release/autobracket          the server, admin CLI, init, keygen and verify-worker
# target/release/autobracket-client   the player's client
target/release/autobracket init --dir ~/autobracket
target/release/autobracket serve --config ~/autobracket/autobracket.toml
```

`scripts/build-release.sh` builds the release archive of path A into `dist/`, with its
`SHA256SUMS`.

### After installing

1. **Put TLS in front** before players connect over the internet, and list the proxy in
   `trusted_proxies`: [TLS and reverse proxies](#tls-and-reverse-proxies), with ready-made
   `deploy/Caddyfile.example` (automatic certificates) and `deploy/nginx.conf.example`.
2. **Turn on verification** when you have an engine and a disc image:
   [Verifier modes](#verifier-modes). Until then results are `PeerOnly`.
3. **Back up** the database and `organiser.key`: [Backups](#backups).

## Requirements

- **Linux** for the client: it uses process groups, Unix signals and `/proc` (`play` refuses
  to start on a system without `/proc`).
  It does not run on Windows. The server is tested on Linux (Debian 13) under systemd.
- **The game engine** for players, and for an organiser who re-simulates: build it from
  `github.com/GurekamDhillon/melee`. The client's defaults were checked against branch
  `port/candidate12`, commit `0cd085d`; in its `rust/` directory, after
  `tools/rust-port/linux/deps.sh`, run `cargo build --profile fast -p melee -p melee_relay`,
  which gives `build/cargo/fast/melee` and `build/cargo/fast/melee_relay`.
- **Your own NTSC 1.02 disc image** (a plain `.iso`). The project never distributes one, and no
  disc image belongs in this repository.

## Organiser quick start

```sh
# 1. Make your organiser key. `autobracket init --dir DIR` (see Install) makes the key
#    (DIR/organiser.key) and the config in one go: then go on at step 3. By hand: the secret
#    goes to to.key (kept out of git by .gitignore); the public key is printed.
autobracket keygen --out to.key

# 2. Write a config. Put the printed public key in `to_keys`, delete the
#    `allow_no_organiser` line, point `db_path` and `replay_dir` somewhere writable, and set
#    `relay` (see "The relay" below).
cp deploy/autobracket.example.toml autobracket.toml
$EDITOR autobracket.toml

# 3. Run the server.
autobracket serve --config autobracket.toml

# 4. Create an event. The response is the new event as JSON, including its id. The start
#    time (unix seconds; `date -d '2026-11-20 20:00' +%s` gives one) and the description are
#    optional and show on the public schedule; `event schedule ID --starts-at ... --description
#    ...` changes them later (an omitted flag clears that field).
autobracket admin --server http://localhost:8080 --key-file to.key \
  event create --name "Nightly" --format double --best-of 3 \
  --starts-at "$(date -d '2026-11-20 20:00' +%s)" --description "Bo3, Friday 8pm"

# 5. Once players have signed up, start it. Seeds follow sign-up order.
autobracket admin --server http://localhost:8080 --key-file to.key event start 1
```

The server refuses to start without an organiser key in `to_keys`, unless you set
`allow_no_organiser = true` (then nobody can administer it). Unknown keys and bad values in
the config, `[ruleset]` included, are start-up errors.

Spectators open `http://YOUR-SERVER/` for the landing page and the bracket page at
`/events/{id}` for one event (see [Pages](#pages)). For a public instance set
`require_invite = true` and hand each player a code from `autobracket admin ... invite new`;
otherwise anyone can register keys and fill your events. Tags are unique regardless of case.

Before an event starts, `autobracket admin ... entrant remove EVENT TAG` takes an entrant out
(the later seeds move up).

### The relay

Players do not connect to each other by address. The host's engine registers a room with a
`melee_relay` server and gets a four-character match code; the client hands the code to the
opponent through autobracket, and the opponent's engine joins it. The relay introduces the two
games (they then connect directly where they can) and relays the traffic when they cannot.
The server tells every client which relay to use: the `relay` key.

The example config's `relay` is a public relay run by the project's author. Do not depend on
it for your own events: run your own, from the same engine checkout, on any machine with a
public UDP port:

```sh
melee_relay --port 51600            # also: --bind ADDR, --bind6 ADDR|off, --quiet
```

and set `relay = "relay.your-domain.example:51600"`. Rooms live only in the relay's memory.

`allow_direct_connect = true` lets a host hand over `HOST:PORT` instead of a match code (LAN
events). It is off by default, and a player's client also refuses such a code unless its own
config sets `allow_direct_connect = true`.

## Pages

The server serves these pages. Everything on them is public and read-only: no login, no
accounts, nothing beyond what the JSON API already shows. Times show in the visitor's own time
zone (the API carries unix seconds).

| Path | What it shows |
|---|---|
| `/` | Landing: *Live now* (every running event and its sets in play, "Watch the stream" when a stream is configured), *Next up* (the soonest sign-up event, its time and description, and the two client commands that enter it), *Latest results* (the newest complete event's top three). Refreshes every 10 seconds. |
| `/events/{id}` | The bracket viewer for one event: the sign-up list before the start, the bracket with live and decided sets after it. Refreshes every 10 seconds. |
| `/schedule` | Events happening now and coming up, with their start times and how to enter. |
| `/history` | Complete events, newest first, with placings. |
| `/players` | Every registered player: events entered, titles, set record, with a search box. |
| `/players/{tag}` | One player: totals, events entered with seed and place, head-to-head records. |
| `/stream` | The organiser's stream, embedded, with the sets being played under it. |

Old bookmarks of the form `/#N` still land on that event: the landing page sends them to
`/events/N`.

The stream page embeds the URL in the config's `stream_url` (optional; see Deployment). Its
origin is the only third party a visitor's browser ever contacts, and only from `/stream`;
without `stream_url` the page says no stream is configured.

## Players

Each player needs the `melee` engine binary and their own NTSC 1.02 disc image.

```sh
autobracket-client init --melee /path/to/melee --iso /path/to/melee.iso --match fox
autobracket-client register --server https://bracket.example.org --tag Mango
autobracket-client signup 1
# After the organiser starts the event:
autobracket-client play 1
```

`play` waits for your next set and prints its rules (stocks, timer, best-of, the legal stages
by name and id, banned characters). It starts the game as host or guest (the host's room code
reaches the opponent through the server), uploads the replay after every game, and keeps going
until you are eliminated or win the event.

All commands (`autobracket-client COMMAND --help` for details):

| command | what it does |
|---|---|
| `init --melee PATH --iso PATH [--work-dir DIR] [--match FIGHTER[,STAGE]] [--headless] [--force]` | Writes `~/.config/autobracket/client.toml` (mode 0600) with a new key, after checking the disc image (a readable file of at least 1 GiB) and that `melee version` runs. Prints your engine build and public key. |
| `register --server URL --tag TAG [--invite CODE]` | Registers your key under a tag and remembers the server. |
| `events` | Lists the server's events. |
| `signup EVENT` | Enters an event, sending your engine build. |
| `status EVENT` | Shows the event and what you should do next. |
| `play EVENT [--poll-secs N]` | Plays your sets (below). |
| `no-show EVENT [--note TEXT]` | Tells the organiser your opponent has not turned up for your current set. |
| `upload EVENT SET GAME FILE` | Uploads one replay by hand, e.g. one `play` could not upload. |

Every command takes `--config FILE`. Your engine build is worked out afresh at each `signup`,
`play` and `upload`, the same way the server's verifier does it (the first line of
`melee version`, `+`, and the first 12 hex digits of the binary's SHA-256), so it follows an
engine update. If the organiser only accepts some builds, a refused sign-up lists them.

If the server's address is plain `http://` to another machine, the client warns you: room
codes and the server's answers can then be read and changed on the way. If your computer's
clock is off by more than two minutes, the server refuses every request and the client says
so; fix the clock (turn on automatic time).

### Engine arguments

The engine command lines are templates in the client config (`host_args`, `join_args`); the
file's opening comments explain each option.

- **Character select or `--match`.** The default templates start each game on the netplay
  character select (`--np-css`). In the engine build named above, a player who reaches the
  name-entry screen from there crashes **both** games (the menus run in lockstep). Engine
  branch `port/np-nameentry` fixes it, and adds `--np-no-name-entry` (a host option that turns
  name entry off on both sides). Until your build has the fix, create your config with
  `init --match FIGHTER[,STAGE]` (e.g. `--match falcon,32` or `--match fox`): the game then
  starts with a preset fighter, and when you host, the preset stage (default Final
  Destination). A preset is the same fighter and stage for every game of every set, so pick a
  legal stage. `init` warns when the templates use the character select.
- **No display.** On a machine without a display the engine exits at once unless both
  templates also have `--headless --no-audio` (`init --headless` adds them).
- **Disc check.** The defaults include `--verify-disc`: the engine checks the image is NTSC
  1.02 Melee before it starts, and `play` stops with a clear message instead of retrying.
  Without that flag the engine has no such check (a wrong image just fails to run).
- Engines ignore flags they do not know; they do not refuse them.

### When things go wrong

- **Your opponent never joins.** The host's engine gives up after its `--np-timeout` (60 s)
  and prints `play: netplay host: no connection after 60 s`. After two such runs in a row the
  client reports a **no-show** to the organiser and waits for their decision. Engine builds
  from `port/np-nameentry` on also accept `--np-join-wait SECS` in `host_args`; they then
  print `play: netplay: no opponent joined` (exit 5), which the client recognises too. The
  server refuses a no-show within `no_show_grace_secs` (default 90) of a room code being
  posted; the client then tries again shortly, without hosting again.
- **Your host never sends a room code.** As guest, the client reports a no-show after
  `no_code_wait_secs` (client config, default 600; 0 turns it off). You can report at once
  with `autobracket-client no-show EVENT`.
- **A room code that no longer works** (the host's room is gone) is not joined again; the
  client waits for the next one, and it does not count as your failure.
- **Your own setup is broken.** `play` stops at once, reporting nothing, when the engine's
  disc check fails (exit 4), when it reports a usage error (exit 2), or when it does not know
  the `--match` fighter. A game that keeps ending without a replay for other reasons (4 runs
  in a row) is reported to the organiser as a **client failure** of this player, and `play`
  stops with a message about your own `melee`, `iso` and relay.
- **A refusal is not the end.** A room code or a replay the server refuses is reported on the
  screen and `play` carries on; a refused replay is kept in its run directory, with the
  `upload` command to send it by hand.
- **The server goes away.** Calls are retried with backoff until it is back. Requests time
  out (10 s to connect, 30 s per request, 120 s per replay upload), so a dropped connection is
  noticed.

### Stopping and restarting `play`

Each game runs in its own `{work_dir}/run-N/` directory, in its own process group, with its
output in `engine.log`. Ctrl-C (or SIGTERM) stops `play` but **not** a game in progress.

Run `play` again: it reads `{work_dir}/session.json`, takes over the game if it is still
running (it never starts a second one) and uploads its replay when it ends, or uploads the
replay of a game that ended in the meantime, before doing anything else. A game is taken over
only if it is provably the same process (its pid, its start time in `/proc`, the boot, and
its run directory), never a recycled pid. Only one `play` can use a work directory at a time;
a second one is refused with a message.

A game is never stopped because its set went to the organiser: it finishes and its replay is
uploaded as evidence. The client stops a game only when the server offers a different set or
you are out of the event.

## When a set needs a human

Games that cannot be counted automatically land in the organiser's queue, and so do sets that
are not being played:

- a **no-show** report (queue reason `no-show`, with the reporting slot and their note);
- a **client failure** report (reason `client-failure`): the reporting player's own client
  could not get a game going;
- a `ready` set with no activity (no room code, upload or counted game) for `set_stall_secs`
  (default 1800; 0 turns the check off), sent as `stalled` and checked about every 30 s.
  Inactivity counts from the server's own start at the earliest, so a restart after downtime
  does not flag every set at once;
- a game the server could not judge after 5 attempts (reason `judging-failed`, with the last
  error).

While a set is in review only because of a `no-show`, `stalled` or `client-failure` item, the
server still accepts the replays of its current game and judges them as **evidence**: nothing
counts until the organiser decides, but the organiser sees what was played.

```sh
autobracket admin --server URL --key-file to.key queue list
autobracket admin --server URL --key-file to.key quarantine get 12 0 --out game.slp
autobracket admin --server URL --key-file to.key queue resolve 7 award --slot 0
autobracket admin --server URL --key-file to.key queue resolve 7 replay
autobracket admin --server URL --key-file to.key queue resolve 7 dq --slot 1
```

`queue list` prints each item's players (slot 0 and slot 1 tags), who reported it and a
one-line summary of the evidence, then the full JSON. The evidence of each judged game holds,
per uploaded replay: the slot and tag, a SHA-256 prefix, whether it parsed, and if so the
stage, timer, each player's port, character, final stocks and percent, how the game ended and
the winner the replay shows. A lone legal replay (`Single`) is also re-simulated as evidence;
that never counts it. `quarantine get ID SLOT --out FILE` downloads a quarantined replay.

`award` gives the set to a slot (slot 0 is the set's host), `replay` voids the game so it is
played again (or, for a no-show, gives the players another try), and `dq` disqualifies the
player in that slot from the event. A set decided by `award` or a DQ is listed in the event's
`decided_by` and stamped "Awarded" or "DQ" on the bracket page, next to the games played before
the decision (or in place of a 0-0 score).

## Verdicts

Every game gets one verdict. The server never takes a player's word for the winner.

| Verdict | When | Counts? |
|---|---|---|
| Verified | Both uploads agree, the ruleset is satisfied, and re-simulation matches. | Yes |
| VerifiedInexact | Both uploads agree and are legal, but re-simulation diverged, ended early, or crashed. Both machines recorded the same game, so this is treated as an engine bug and the replay is quarantined. | Yes |
| PeerOnly | Both uploads agree and are legal, and nothing re-simulated them: the verifier is `off`, or it was unavailable (no verifier online, disc or engine gone) while `strict_resim = false`. | Yes |
| VerifierUnavailable | Only with `strict_resim = true`: a game that needed re-simulation while the verifier was unavailable. | No: TO queue |
| Conflicted | The two uploads differ. | No: TO queue, quarantined |
| Single | Only one upload arrived before `upload_timeout_secs`. A lone file cannot be proven to be the bracket game. | No: TO queue, quarantined |
| Illegal | The replay breaks the ruleset (players, teams, stage, stocks, timer, character, or Gecko codes). | No: TO queue |
| Undecided | The replay has no clear winner. | No: TO queue |
| Inexact | Only with `strict_resim = true`: a VerifiedInexact game is sent to a human instead of counting. | No: TO queue, quarantined |
| UnverifiedForeign | Only in an event with `any_client` on (see "Any client mode"): both uploads agree on the outcome and are legal, but re-simulation diverged, ended early or crashed (the replay may come from another client). | Yes, quarantined for the TO |

With a verifier configured (`mode` not `off`), games that counted as PeerOnly because it was
unavailable are visible: the event view's `unverified_games` counts them, each is logged as a
warning, and the first one after start-up is logged as an error. An internal error (a busy
database, a full scratch directory, an engine that would not start) is not "unavailable": the
game is judged again.

Uploads "agree" when the `raw` element (the game's event stream) of the two `.slp` files is
identical. In every real run so far both peers' files were byte-identical as a whole; only
`raw` is compared because the metadata around it is written by each peer's exporter. Port 1
is always the set's host and port 2 the guest, because a netplay replay carries no identity.

A game records a verifier build only when the engine actually re-simulated it. Every verdict
records when it was applied (`judged_at`). Two players colluding can still produce any result
they like, exactly as they could by throwing the match.

## Any client mode

**Off by default, per event. Slippi's terms of service are unresolved for this use** (the
engine notes flag it); settle that before turning it on for a real event.

The design's deferred mode: players on different clients (Slippi Dolphin, the reference
client) in one bracket. It is the ruleset's `any_client` flag, so an organiser sets it per
event when creating it:

```sh
autobracket admin event create --name "Open" --format single --ruleset rules.json
# rules.json: {"any_client": true, "allowed_gecko": ["<sha256 of the code list players use>"]}
```

What changes in such an event:

- **Agreement compares the outcome, not bytes.** Two clients need not write identical event
  streams, so the two uploads agree when both parse and derive the same outcome: stage, each
  port's character and final stocks, how the game ended (method and LRAS initiator), the
  winning port and the last frame. Each upload is checked against the ruleset. A pair that
  disagrees on any of these is `Conflicted`, as before.
- **Re-simulation of foreign replays.** With the verifier on, a replay the engine re-simulates
  exactly is `Verified`. One it cannot (it diverged, ended early, crashed) is
  `UnverifiedForeign`: it counts on peer agreement and the ruleset, whatever `strict_resim`
  says, and is quarantined, so the organiser sees each one (`autobracket admin quarantine
  list`). It is never rejected for that alone. A verifier that is unavailable behaves as in any
  other event.
- **Uploading without the reference client.** `autobracket-upload` (built with the rest,
  `target/release/autobracket-upload`; it has no Unix-only parts, so it builds on Windows too)
  signs each request with the player's Ed25519 key exactly as `docs/api.md` section 2 says:

  ```sh
  autobracket-upload --key player.key keygen                       # once; prints the public key
  autobracket-upload --server https://bracket.example.org --key player.key register --tag Mango
  autobracket-upload --server https://bracket.example.org --key player.key signup 7 --build slippi
  autobracket-upload --server https://bracket.example.org --key player.key next 7
  autobracket-upload --server https://bracket.example.org --key player.key     upload --event 7 --set 3 --game 1 Game_20261122T200000.slp
  ```

  `--key` also takes an `autobracket-client` config (its `secret_key_hex`). `--build` is what
  `signup` sends as `engine_build` (so the organiser's `accepted_builds` must list it, or be
  empty) and what `upload` sends as `x-ab-build`.

Limits that stay: port 1 is still the set's host and port 2 the guest (a replay carries no
identity), so the players must take those ports; the room-code hand-off is for the reference
client, and players on other clients arrange their connection themselves; a Gecko code list
must be on `allowed_gecko` (Slippi Dolphin writes one into every replay).

Tested (`crates/server/tests/any_client_real.rs`, `#[ignore]`d, `AB_SLP_DIR=...`) on real
Slippi Dolphin tournament replays kept outside the repository: never commit a `.slp`.

## Verifier modes

Set in the `[verifier]` table of the config:

- `off` (default): no re-simulation. Games count on peer agreement and the ruleset alone
  (verdict PeerOnly). A config that sets `engine` or `iso` with `mode = "off"` (or
  `"external"`, where the worker has its own) is refused, so verification is never off by
  accident.
- `in_process`: the server runs `melee slpcheck` itself, up to `jobs` re-simulations at the
  same time, each limited to `timeout_secs`. Needs `engine` and `iso`: the server refuses to
  start unless the engine runs (`melee version`) and the disc image is a readable file of at
  least 1 GiB.
- `external`: the server queues re-simulations and an `autobracket verify-worker` running
  elsewhere (say, at home, next to your disc image) does them:

  ```sh
  autobracket verify-worker --server URL --key-file verifier.key \
    --engine /path/to/melee --iso /path/to/melee.iso
  ```

  The worker's public key goes in `verifier_keys` (an organiser key also works). If no worker
  has been heard from in the last minute, a game waits about 15 s and is then treated as
  "verifier unavailable" (see Verdicts) instead of waiting 10 minutes.

`accepted_builds`, when not empty, lists the engine builds players may sign up with, and binds
the verifier too: an entry matches a build string that equals it or whose part before the
first `+` equals it. An in-process verifier on another build refuses to start; an external
result from another build counts as "verifier unavailable".

## Deployment

### systemd

`deploy/autobracket.service` is a hardened unit (`DynamicUser`, `ProtectSystem=strict`,
`ProtectHome`, `NoNewPrivileges`) that runs in, and keeps its state in, `/var/lib/autobracket`:

```sh
sudo install -m 755 target/release/autobracket /usr/local/bin/
sudo install -d /etc/autobracket
sudo install -m 644 deploy/autobracket.example.toml /etc/autobracket/autobracket.toml
sudo $EDITOR /etc/autobracket/autobracket.toml     # to_keys, relay, ...
sudo install -m 644 deploy/autobracket.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now autobracket
```

With `init` instead of the example config (the server never reads the organiser key, so move
it to wherever you run admin commands; the config holds no secret). The project has not run
this exact sequence (`init` as root into `/var/lib/autobracket`, then starting the
`DynamicUser` unit, which relies on systemd re-owning that directory for the dynamic user);
the unit itself has run in production with a config installed by hand as above, in-process
verification included.

```sh
sudo autobracket init --dir /var/lib/autobracket --proxy-cidr 127.0.0.1/32
sudo install -d /etc/autobracket
sudo install -m 644 /var/lib/autobracket/autobracket.toml /etc/autobracket/
sudo rm /var/lib/autobracket/autobracket.toml
sudo mv /var/lib/autobracket/organiser.key ~/organiser.key && sudo chown "$USER" ~/organiser.key
```

Because of `DynamicUser` and `ProtectHome`, an `in_process` verifier's `engine` and `iso` must
live outside home directories and be readable by the service's dynamic user, e.g.
world-readable files under `/opt/melee`.

### TLS and reverse proxies

The server speaks plain HTTP. On the public internet put a TLS reverse proxy in front of it
and give players the `https://` address; plain `http://` exposes room codes and invite codes,
and lets anyone on the path change what the server tells a client. For example with nginx
(any proxy works the same way):

```nginx
server {
    listen 443 ssl;
    server_name bracket.example.org;
    ssl_certificate     /etc/ssl/bracket.example.org/fullchain.pem;
    ssl_certificate_key /etc/ssl/bracket.example.org/privkey.pem;
    client_max_body_size 17m;              # at least max_replay_bytes
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header Host $host;
    }
}
```

The server rate-limits per client address (`rate_per_sec`, `rate_burst`). Behind a proxy every
request comes from the proxy's address, so list the proxy in `trusted_proxies` (CIDRs, empty by
default): for requests from a trusted address the client is the right-most `X-Forwarded-For`
entry that is not itself trusted. With the proxy on the same host that is
`trusted_proxies = ["127.0.0.1/32"]`; with the server under `deploy/docker-compose.yml` and
the proxy on the host it is the pinned network's gateway, `["10.203.47.1/32"]`; with the proxy
itself in Docker, give its container a fixed address and trust that. **Never put a range in
`trusted_proxies` that contains any client's address**, LAN players and direct visitors
included: trust only the proxy's own addresses, as narrowly as possible (a /32). Every
trusted entry is skipped when the server walks `X-Forwarded-For` from the right, so a client
whose own address is trusted gets the entry to its left believed, which is one it wrote
itself. IPv6 clients are limited per /64. Players behind one NAT (a venue) share one
address: a client polls every 2 s, so raise `rate_per_sec` and `rate_burst` for a venue
event. `max_connections` (default 512) caps connections handled at once.

The gateway address `10.203.47.1` was verified with Docker's default userland proxy enabled;
other setups (userland proxy disabled, rootless Docker, Podman) were not tested, and there the
proxy's connections may arrive from a different address. The image has no tool that shows a
connection's remote address, so read the gateway with
`docker network inspect autobracket_default --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'`:
that is the address to trust. A wrong value fails safe against spoofing but not for usability:
the header is ignored, so every player appears as the proxy's one address and shares one rate
limit, and players get HTTP 429.

### Stream

To show a live stream on the `/stream` page, put the viewer's URL in `stream_url` (for example
a MediaMTX WebRTC viewer, `stream_url = "https://live.example.org/melee/"`); unset, the page
says no stream is configured. The page embeds the URL in a frame, and its security policy lets
the browser frame that URL's origin (`https://live.example.org` here) and nothing else; every
other page, and everything else on the stream page, stays first-party, so the stream's origin
is the only third party a visitor's browser ever contacts, and only on that page. The URL must
be `http` or `https` with a host, or the server refuses to start. Under it the page lists the
sets being played right now, with links to their brackets.

### Logs

The server logs to stderr (the journal, under systemd), one plain-text line per event with
`key=value` fields: start-up settings, registrations, events created, started and completed,
sign-ups, room codes posted (never the code itself), uploads (size, SHA-256 prefix, client
build), verdicts (with judging and re-simulation times in ms), sets decided and how, queue
items opened and resolved, no-shows, stalled sets and verifier jobs. Keys appear only as their
first 8 characters; secret keys, room codes and request bodies are never logged. Text that
comes from outside is escaped so it cannot start a line of its own, and the server refuses
control characters and Unicode direction controls in engine builds, the `x-ab-build` header,
event names and notes. Requests refused for a bad signature are counted and summarised at
most once a minute. Set the level with `AUTOBRACKET_LOG` (default `info`; any `tracing`
filter, e.g. `autobracket_server=debug,info`).

### Backups

The database runs in SQLite WAL mode, so copying the `.sqlite` file (or the directory) while
the server runs can lose committed results or give a torn copy. Either take a consistent copy
with the server running:

```sh
sqlite3 /var/lib/autobracket/autobracket.sqlite ".backup /root/autobracket-$(date +%F).sqlite"
```

or stop the service (`systemctl stop autobracket`), copy `/var/lib/autobracket` (database and
`replays/`), and start it again.

`deploy/backup/` automates the first form as a nightly systemd timer: a verified
`.backup` snapshot (and, if configured, a `pg_dump` of a Zitadel Postgres container) into
root-only `/var/backups/autobracket`, keeping the newest 7. It is a **local** backup on the
same disk, with no off-site copy: it covers bad writes and botched migrations, not a lost
disk. Install, limits and restore steps: [docs/backup.md](docs/backup.md).

Keep a private copy of the organiser key (`organiser.key` from `init`) too: whoever holds it
can run your events. If it is lost or leaks, make a new one with `autobracket keygen` and
replace the old public key in `to_keys`.

With Docker Compose everything (database, replays, key, config) is in the volume, and the
image has no `sqlite3`; stop the server and archive the volume:

```sh
umask 077    # the archive holds organiser.key, the secret key
docker compose -f deploy/docker-compose.yml stop
docker compose -f deploy/docker-compose.yml run --rm -T --entrypoint tar autobracket \
  -czf - -C /data . > autobracket-$(date +%F).tar.gz
docker compose -f deploy/docker-compose.yml start
```

The archive contains the organiser's secret key: keep it as private as the key itself.

## Limits

Phase 1 is a working tournament system with honest gaps:

- **No stage striking or counterpicks.** Players pick in game; the server checks the stage
  against `legal_stages` afterwards. With `--match` presets the host's stage is fixed for the
  whole event.
- **The in-game menu is phase 2.** Players run `autobracket-client` beside the game.
- **The client is Linux-only** and drives the Rust engine named above; there is no Windows or
  macOS client.
- **Character select can crash both games** on engine builds without the `port/np-nameentry`
  fix; use `--match` there.
- **Standard Slippi replays are not accepted.** Replays recorded on Slippi Dolphin or a
  console fail the ports rule and cannot be matched by the Gecko allow-list
  (`docs/real-engine.md`), so they go to the organiser.
- **A lone upload cannot be proven to be the bracket game**, so `Single` always goes to a
  human; a re-simulation match on it does not verify a No Contest winner.
- **Signed requests are not bound to one server**: a server operator could replay a player's
  request to another instance within the two-minute signing window if the paths match. Keep
  separate keys (configs) for servers you do not trust.
- **One SQLite file, one server process.** No clustering.
- **Licence:** GNU Affero General Public License, version 3 or (at your option) any later version (`AGPL-3.0-or-later`); see `LICENSE`. If you run a modified copy as a network service, the AGPL requires you to offer its source to that service's users.

## Phase 2

The product players should actually use is inside the game: a native main-menu entry in the
melee engine that does what `autobracket-client` does (pick an instance, sign up, see your next
set, get placed into the match, watch the bracket), built on the engine's existing online
host/join code and lobby, so proper stage striking comes with it. The engine's replay exporter
will also stamp a server-issued nonce and its build id into the replay, which binds a lone
upload to its bracket game. That work lives in the melee repository; this repository defines
the API it talks to ([docs/api.md](docs/api.md)).
