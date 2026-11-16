# autobracket

A self-hostable HTTP service that owns a Super Smash Bros. Melee tournament bracket and
advances it from verified match results. A tournament organiser drops one binary and one
SQLite file on a cheap VPS, points a Melee client at it, and runs nightly brackets without a
human updating a bracket by hand.

What sets it apart is that results are proven, not reported. Both players upload the replay
of every game; the server compares the two, checks them against the event's ruleset, derives
the winner from the replay itself, and re-simulates it with
`GurekamDhillon/melee`, a native Rust port of
Melee that replays a `.slp` deterministically and headless, with no GPU. On a small VPS it
re-simulates at about 19-22x realtime, roughly 6 seconds for a typical full game. A bracket that cannot be lied to is also the honest argument for self-hosting: an
organiser running their own instance gets result integrity, not just control. Replays that
fail to re-simulate exactly are kept, with their divergence point and engine build, as a
corpus for fixing the port.

Phase 1 (this repository) is the API, a read-only bracket page, and a reference command-line
client, `autobracket-client`, that drives the existing engine. There are no accounts: a
player is an Ed25519 key their client generates, plus a display tag.

## Quick start

Build both binaries (`autobracket` and `autobracket-client`):

```sh
cargo build --release
```

### Organiser

```sh
# 1. Make your organiser key. The secret goes to to.key; the public key is printed.
autobracket keygen --out to.key

# 2. Write a config. Put the printed public key in `to_keys`, and point `db_path` and
#    `replay_dir` somewhere writable (the example uses /var/lib/autobracket).
cp deploy/autobracket.example.toml autobracket.toml
$EDITOR autobracket.toml

# 3. Run the server.
autobracket serve --config autobracket.toml

# 4. Create an event. The response is the new event as JSON, including its id.
autobracket admin --server http://localhost:8080 --key-file to.key \
  event create --name "Nightly" --format double --best-of 3

# 5. Once players have signed up, start it. Seeds follow sign-up order.
autobracket admin --server http://localhost:8080 --key-file to.key event start 1
```

If `require_invite = true`, hand each player a code from `autobracket admin ... invite new`.
Spectators open `http://localhost:8080/` for the live bracket.

### Players (each on their own machine)

Each player needs the `melee` engine binary and their own NTSC 1.02 disc image.

```sh
autobracket-client init --melee /path/to/melee --iso /path/to/melee.iso
autobracket-client register --server http://bracket.example.org:8080 --tag Mango
autobracket-client signup 1
# After the organiser starts the event:
autobracket-client play 1
```

`play` waits for your next set, starts the engine as host or guest (the host's room code is
passed to the opponent through the server), uploads the replay after every game, and keeps
going until you are eliminated or win the event.

The engine command lines are templates in the client's config (`host_args`, `join_args`).
Two changes are needed today:

- **Pick fighters and stage with `--match`.** An engine bug in the netplay character select
  (its name-entry screen indexes past the end of a name and panics) crashes both peers at
  once, because the menus run in lockstep. Until the fix lands upstream, bracket games start
  with `--match` instead of `--np-css --quit-after-matches 1`: the host's template gets
  `"--match", "<fighter>,<stage>"` (e.g. `"--match", "falcon,32"`) and the guest's
  `"--match", "<fighter>"`.
- **No display, no window.** On a machine without a display (a server, a container, a
  remote shell) the engine exits at once unless both templates also get
  `"--headless", "--no-audio"`. Players at a desktop do not need them.

If your opponent never shows up, report it so the organiser sees it straight away:
`POST /v1/sets/{event}/{set}/no-show` with an optional `{"detail": "..."}` note. The set
goes to the organiser queue (reason `no-show`) and both players are told to wait.

### When a set needs a human

Games that cannot be counted automatically land in the organiser's queue, and so do sets
that are not being played: a player can report a no-show (reason `no-show`, with the
reporting slot and their note in `detail`), and a `ready` set with no activity (no room code,
upload or counted game) for `set_stall_secs` (default 1800; 0 turns the check off) is sent
there as `stalled`, checked about every 30 seconds. Both players then see the set as waiting
for the organiser.

```sh
autobracket admin --server URL --key-file to.key queue list
autobracket admin --server URL --key-file to.key queue resolve 7 award --slot 0
autobracket admin --server URL --key-file to.key queue resolve 7 replay
autobracket admin --server URL --key-file to.key queue resolve 7 dq --slot 1
```

`award` gives the set to a slot (slot 0 is the set's host), `replay` voids the game so it is
played again (or, for a no-show, gives the players another try), and `dq` disqualifies the
player in that slot from the event. A set decided by `award` or a DQ is listed in the event's
`decided_by` and shown on the bracket page as "awarded" or "DQ" instead of a score.

## Verdicts

Every game gets one verdict. The server never takes a player's word for the winner.

| Verdict | When | Counts? |
|---|---|---|
| Verified | Both uploads agree, the ruleset is satisfied, and re-simulation matches. | Yes |
| VerifiedInexact | Both uploads agree and are legal, but re-simulation diverged, ended early, or crashed. Both machines recorded the same game, so this is treated as an engine bug and the replay is quarantined. | Yes |
| PeerOnly | Both uploads agree and are legal; re-simulation was not available (verifier off, or no engine/disc). | Yes |
| Conflicted | The two uploads differ. | No: TO queue, quarantined |
| Single | Only one upload arrived before `upload_timeout_secs`. A lone file cannot be proven to be the bracket game. | No: TO queue, quarantined |
| Illegal | The replay breaks the ruleset (players, teams, stage, stocks, timer, character, or Gecko codes). | No: TO queue |
| Undecided | The replay has no clear winner. | No: TO queue |
| Inexact | Only with `strict_resim = true`: a VerifiedInexact game is sent to a human instead of counting. | No: TO queue, quarantined |

Uploads "agree" when the `raw` element of the two `.slp` files is identical. In every real run
so far the two peers' files were byte-identical as a whole, but only the `raw` element (the
game's event stream) is compared: the metadata around it, such as the start time, is written
by each peer's exporter and may differ between versions or clocks without the game being
any different. Port 1 is always the set's host and port 2 the guest, because a netplay replay
carries no identity.

Games decided before any re-simulation (Single, Conflicted, Illegal, Undecided) record no
verifier build; every verdict records the time it was applied (`judged_at`).

Two players colluding can still produce any result they like, exactly as they could by
throwing the match.

## Verifier modes

Set in the `[verifier]` table of the config:

- `off` (default): no re-simulation. Games count on peer agreement and the ruleset alone
  (verdict PeerOnly).
- `in_process`: the server runs `melee slpcheck` itself, `jobs` at a time, each limited to
  `timeout_secs`. Needs `engine` and `iso`: the server refuses to start in this mode unless
  the engine runs (`melee version`) and the disc image is a readable file of at least 1 GiB.
- `external`: the server queues re-simulations and an `autobracket verify-worker` running
  elsewhere does them:

  ```sh
  autobracket verify-worker --server URL --key-file verifier.key \
    --engine /path/to/melee --iso /path/to/melee.iso
  ```

  The worker's public key goes in `verifier_keys` (an organiser key also works).

**Bring your own disc.** Re-simulation needs an NTSC 1.02 disc image, and it must be your
own dump. The project never distributes one, and no disc image belongs in this repository.
If you do not want the image on a public VPS, use `external` mode and run the worker on a
machine at home against the same server.

## Deployment

`deploy/autobracket.service` is a hardened systemd unit (`DynamicUser`, `ProtectSystem=strict`,
`ProtectHome`, `NoNewPrivileges`) with its state in `/var/lib/autobracket`:

```sh
sudo install -m 755 target/release/autobracket /usr/local/bin/
sudo install -d /etc/autobracket
sudo install -m 644 deploy/autobracket.example.toml /etc/autobracket/autobracket.toml
sudo $EDITOR /etc/autobracket/autobracket.toml     # add your to_keys
sudo install -m 644 deploy/autobracket.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now autobracket
```

### Logs

The server logs to stderr (the journal, under systemd), one plain-text line per event with
`key=value` fields: start-up settings, registrations, events created, started and completed,
sign-ups, room codes posted (never the code itself), uploads (size, SHA-256 prefix, client
build), verdicts (with judging and re-simulation times in ms), sets decided and how, queue
items opened and resolved, no-shows, stalled sets and verifier jobs. Keys appear only as their
first 8 characters; secret keys, room codes and request bodies are never logged. Requests
refused for a bad signature are counted and summarised at most once a minute. Set the level
with `AUTOBRACKET_LOG` (default `info`; e.g. `AUTOBRACKET_LOG=debug`, or any `tracing`
filter such as `autobracket_server=debug,info`).

Back up a tournament by copying `/var/lib/autobracket`. Because the unit uses `DynamicUser`
and `ProtectHome`, an `in_process` verifier's `engine` and `iso` must live somewhere outside
home directories that the service can read, such as `/opt/melee`.

## Phase 2

The product players should actually use is inside the game: a native main-menu entry in the
melee engine that does what `autobracket-client` does (pick an instance, sign up, see your next
set, get placed into the match, watch the bracket), built on the engine's existing online
host/join code and lobby, so proper stage striking comes with it. The engine's replay exporter
will also stamp a server-issued nonce and its build id into the replay, which binds a lone
upload to its bracket game. That work lives in the melee repository; this repository defines
the API it talks to.
