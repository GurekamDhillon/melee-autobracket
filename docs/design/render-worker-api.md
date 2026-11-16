# Render worker API (design)

Status: **draft, revision 2 after the desktop's review** (2026-11-16; review in
`.superpowers/sdd/2026-11-15-verified-autobracket/worker-api-desktop-review.md`, plus the
owner's host-agnostic requirement of the same day). No code yet.
This document extends
`docs/api.md`; everything there (signing, conventions, error bodies, limits, the rate limit)
applies to every route below unless this document says otherwise. Once agreed, the routes and
types move into `docs/api.md` and the `proto` crate with `checked` examples like the rest.

The problem: the server holds every replay of every counted game. A machine with a GPU and a
copy of the engine (today: the owner's Windows desktop; tomorrow perhaps another machine, another
GPU vendor, Linux) turns them into video: a VOD of each set, and short clips of notable moments.
The desktop's facts (phase2-desktop-answers.md and the review of 2026-11-16):

- On the current desktop the worker is a Windows Task Scheduler job. It runs at below-normal
  priority, **pauses while the owner plays**, and may therefore sit on a job for hours. Any
  other host runs it as a service the same way (section 11).
- **The engine makes no mp4 and does no muxing.** `melee slpcheck --slp F --video-out X` writes
  raw RGBA at 60 fps to stdout (`-`), a file or a named pipe; `--audio-out Y` writes raw s16le
  stereo at 32000 Hz (pre-roll skipped; sample 0 is the first output frame). The only engine
  video knob is `--video-size WxH` (both even, W 64..7680, H 64..4320, default 1280x720; wider
  than 4:3 renders 16:9). `--video-realtime` is not used. Run with `MELEE_WINDOW_HIDDEN=1`.
- **ffmpeg is a hard worker dependency**: it encodes (H.264 + AAC) and muxes (mp4, faststart).
  The proven pipeline on the current desktop (Windows, NVIDIA):
  `melee slpcheck --slp F --video-out - --audio-out \\.\pipe\NAME | ffmpeg -f rawvideo -pix_fmt rgba -s WxH -r 60 -i - -f s16le -ar 32000 -ac 2 -i \\.\pipe\NAME -c:v h264_nvenc -preset p4 -tune ll -c:a aac -movflags +faststart -f mp4 out.mp4`.
  The Linux equivalent uses a FIFO: `mkfifo "$T/audio.pcm"`, then the same command with
  `--audio-out "$T/audio.pcm"` / `-i "$T/audio.pcm"` and whichever H.264 encoder the host has.
  ~155 fps is the whole pipeline on that desktop (~2.5x realtime): a 4-minute set in ~1.6
  minutes.
- **The GPU host may change** (owner heads-up, 2026-11-16): not necessarily this desktop, not
  necessarily NVIDIA, possibly Linux. So nothing in the API or a job assumes Windows, NVIDIA or
  one worker host. The encoder is worker configuration, probed from `ffmpeg -encoders`
  (`h264_nvenc`, `h264_amf`, `h264_qsv`, `h264_vaapi`, `h264_videotoolbox`, else `libx264`) and
  reported after the fact; paths, the pause predicate and the scheduler are worker-side
  (section 11). Several renderer keys may be registered and claim concurrently.
- `--from-frame N --to-frame M` (branch `port/video-range`, a3778b5cf): N and M are `.slp` frame
  indices (the replay starts at -123); the simulation runs from frame 0 and the output covers
  `N..=M` inclusive. `N <= 0` means from the start. `M` may equal the replay's last frame (the
  final frame is output; no off-by-one); `M` past it, or `N > M`, exits 2, so the **server clamps
  every range before queueing**. The expected frame count is `M - max(N, -123) + 1`. Seek runs
  ~560 fps headless, so a clip near the end of a 6-minute game costs ~40 s before encoding.
- `--progress` (bc0e66284) prints `progress <frame> <last>` on stderr every 60 replay frames,
  during the seek too (`frame` may be negative); `--verify-disc` checks the disc header (GALE01
  rev 2, plain GCM) and exits 4 on a bad image. A `--csv FILE` `status` column is the real
  result signal; exit codes are listed in section 2.
- Output depends only on replay, engine build and disc, **but the mp4 bytes are not
  reproducible** across ffmpeg, NVENC and driver versions: a rerun yields a different sha256.
  The report records the ffmpeg version.
- VOD 1080p60 at ~6 Mbps (~45 MB/min), clips 720p60 at ~4 Mbps.
- Akaneia replays run on a **second disc** (`GW_ISO_AKANEIA`, auto-chosen by the engine from the
  replay's gecko codes and fighters).

Contents:
1. [Shape of the design](#1-shape-of-the-design)
2. [Jobs](#2-jobs)
3. [Lease, heartbeat and pause](#3-lease-heartbeat-and-pause)
4. [State machine](#4-state-machine)
5. [Routes](#5-routes)
6. [Uploads](#6-uploads)
7. [Types](#7-types)
8. [Storage and serving](#8-storage-and-serving)
9. [Limits](#9-limits)
10. [Configuration](#10-configuration)
11. [Worker loop](#11-worker-loop)
12. [Open questions for the desktop](#12-open-questions-for-the-desktop)
13. [Broadcast (later)](#13-broadcast-later)

## 1. Shape of the design

**Signed pull, like verify workers.** The worker holds an Ed25519 key pair. The organiser puts
its public key in the server's `render_keys` list (new config, sibling of `verifier_keys`); a
key in `to_keys` may also act as a render worker. The worker polls `POST /v1/render/claim`,
downloads the replays it needs, renders, uploads the outputs, posts a result. The server never
connects to the worker, so the desktop needs no open port, no dynamic DNS and no firewall rule,
and a NAT or a VPN change on the desktop costs nothing. Role name in the API tables:
**renderer** (a key in `render_keys` or `to_keys`).

**One job = one set (VOD) or one moment (clip).** A `vod` job renders every counted game of a
decided set; a `clip` job renders one frame range of one game. See [Jobs](#2-jobs) for why a VOD
is one mp4 per game and not one concatenated file.

**The server owns job identity.** Job ids are server-assigned integers, like every id in the
API. Idempotency comes from the *job key*: the tuple that describes what is rendered
(section 2). Creating a job whose key already exists returns the existing job; posting a result
to a job that already has one is a 409 the worker treats as "not needed", exactly as
`verify-worker` does today; an upload of bytes the server already has is a no-op. A worker that
crashes at any point and restarts from the top does the right thing without local state.

**Leases, not locks.** A claim gives the worker a lease that expires unless heartbeats arrive.
A heartbeat may say `paused`; a paused lease does not expire on the normal clock but on a
longer, bounded one, so a six-hour play session does not throw the job back in the queue, and
a worker that dies while paused does not hold the job forever.

**Everything the output depends on is in the job.** Engine build, disc image hashes, replay
hashes and the render settings are part of the job and are recorded with the output, so any
output can be re-rendered from the same inputs (or rejected as stale when the engine changes).
"Reproduced" means the same frames and sound, not the same bytes: the encoder is ffmpeg with
NVENC, whose output varies with ffmpeg, driver and GPU, so the server never expects a rerun to
match an earlier sha256. The hash protects the transfer, not the identity of the render.

## 2. Jobs

### Kinds

| Kind | Renders | Inputs | Outputs |
|---|---|---|---|
| `vod` | a decided set | every counted game's replay, in game order | one mp4 per game: `game-1.mp4`, `game-2.mp4`, … |
| `clip` | one moment of one game | one replay, a frame range, padding | one mp4: `clip.mp4` |
| `analyse` | post-game analysis of one game ("missed punish") | one replay, a list of frame windows (the conversion openings) | one JSON document: `analysis.json` |

`analyse` is reserved now so the queue, lease and upload machinery never have to be reshaped
for it; the tool that runs it (a future `melee analyse --slp F --frames …`) does not exist yet,
so its payload is deliberately generic: the job carries `analyse.windows` (frame ranges) and an
opaque `params` object, and the output is a versioned JSON blob (`schema` string, `data` any
JSON) that the server stores and serves as-is, size-capped, without interpreting it. It may run
for minutes per game, so it uses the same lease, heartbeat and `paused` semantics as rendering.

**Why one mp4 per game for a VOD, not one concatenated file.** The replays are separate files
and the engine renders one at a time, so per-game outputs fall out naturally; a worker that is
paused for hours or killed after game 2 of 5 loses nothing already uploaded, since each output is
uploaded as soon as it is rendered and the job resumes at the first missing output; a failure in
game 3 (an engine crash on one replay) can be retried for game 3 alone, and partial results are
still useful; a clip of game 3 and the VOD of game 3 come from the same source file with the same
settings, so the clip range maps onto the VOD timeline trivially. The bracket page plays a set as
a playlist of its games. Should a single file per set be wanted later (for a stream VOD, for
uploading elsewhere), the server can concatenate same-settings H.264 without re-encoding, or a
`concat` option can be added to the job; neither needs a protocol change. Only counted games are
rendered (`games.state = 'counted'`); voided evidence games are not.

**Clip frame range and padding.** A clip job carries `from_frame` and `to_frame` (the moment
itself, as the detector found it) and `pad_before_frames` / `pad_after_frames` (defaults 120 and
90: 2 s before, 1.5 s after, as the desktop proposed). The server clamps the padded range to the
replay's `[first_frame, last_frame]` (from the replay's own metadata, read when the upload was
judged) before handing it out, so the worker always receives a `render_from`/`render_to` that
`slpcheck` accepts (`from <= to`, `to <= last_frame`; `to == last_frame` is valid and outputs
the final frame, so there is no `- 1`) and never has to know the game's length. A
`render_from <= 0` means "from the start" to `slpcheck` and includes the pre-game frames; the
server lets that stand rather than special-casing it. A job whose range cannot be clamped into
the game (a moment entirely past `last_frame`: a detector bug) is refused at creation (400) rather
than queued. The unclamped moment is kept (`moment_from`/`moment_to`) for the page's "at 2:14"
caption.

**Expected frame count.** For every output the server computes and stores
`expected_frames = render_to - max(render_from, -123) + 1` (for a whole game,
`last_frame - max(first_frame, -123) + 1`). The worker must produce exactly that many frames and
the server checks it at `finish` (section 6). This closes the one hole the engine leaves open:
`slpcheck` exits 0 when ffmpeg or the pipe dies mid-run ("an output closed at frame N;
stopping"), so an exit code of 0 does not prove a complete render.

**Clip cost.** The engine simulates from frame 0 at ~560 fps to reach `render_from`, then the
pipeline encodes at ~155 fps: a 10 s clip at the end of a 6-minute game is ~40 s of seek plus
~4 s of encode. A clip job is therefore never "instant", and `max_running_secs` for clips (1800)
allows for a worker at below-normal priority seeking through a timeout game several times over.

**Discs.** A vanilla replay renders on the GALE01 disc; an Akaneia replay (recognised by the
engine from the replay's gecko codes and fighters) renders on the second disc the worker has as
`GW_ISO_AKANEIA`. The server marks each `RenderInput` with `disc: "vanilla" | "akaneia"` (open
question 16 is how the server tells) and the job's `source` carries one hash per disc. A set
that needs a disc the server has no hash for (`render.akaneia_iso_sha256` unset) gets no
automatic job, and an admin create for it is 409 `no akaneia disc configured`: the organiser
either adds the hash or lives without those VODs. Excluding is the default because an
unverifiable disc is exactly the thing the hash is there to catch.

### Job key (idempotency)

A job is identified by its key; creating a duplicate returns the existing job (200, not 201):

| Kind | Key |
|---|---|
| `vod` | `(kind, event_id, set_id, settings_hash)` |
| `clip` | `(kind, event_id, set_id, game_no, moment_from, moment_to, settings_hash)` |
| `analyse` | `(kind, event_id, set_id, game_no, settings_hash)` |

`settings_hash` is the SHA-256 of the canonical JSON of the job's `source` (engine build, ISO
hash) and `render` settings (section 7, `RenderSpec`); for `analyse` it hashes `source` and the
`analyse` object (windows and params) instead, so a different set of windows is a different
job. A new engine build or a change of preset
therefore makes a new job and a new output rather than silently replacing the old one; the
organiser's `rerun` is how an output is deliberately replaced. The DB enforces the key with a
unique index on `(key_hash)` where `key_hash` is the SHA-256 of the key tuple.

### Who creates jobs

- `vod`: the server, automatically, when a set becomes `done` with at least one counted game,
  if `render.auto_vod` is on (default on when `render_keys` is not empty). Sets decided by
  award, DQ or double-DQ with no counted games get no job.
- `clip`: the server, automatically, when the set is decided and the `replay` crate's moment
  detector (phase 2d) finds moments scoring above `render.clip_threshold`; and the organiser,
  by hand, with `POST /v1/admin/render/jobs` (any frame range).
- `analyse`: the organiser, by hand, with `POST /v1/admin/render/jobs` (windows supplied), and,
  once the detector that finds conversion openings exists server-side, automatically when
  `render.auto_analyse` is on (default off until then).
- Any kind, by hand or again: `POST /v1/admin/render/jobs` and
  `POST /v1/admin/render/jobs/{id}/rerun`.

### Priority and order

Clips, then analyses, then VODs (short things first; the page wants clips, and an analysis is
small to upload), then by `priority` (admin
jobs default 10, automatic 0; higher first), then oldest first. A rerun keeps its original
`created_at`, so it does not jump the queue unless given a priority.

### Attempts, retry and failure

Each claim starts an *attempt* (`attempt` counts from 1). An attempt ends in one of:

| End | Next state | Counts as an attempt |
|---|---|---|
| `result` with `ok` | `done` (terminal) | yes |
| `result` with `failed`, `retryable: true` | `queued`, with `not_before = now + backoff` | yes |
| `result` with `failed`, `retryable: false` | `failed` (terminal), `reason` recorded | yes |
| lease expired (no heartbeat), or paused too long | `queued`, `not_before = now` | yes |
| worker `release`d it (shutting down, or lacks the build) | `queued`, `not_before = now` | **no** |

Backoff after a retryable failure is `min(60 * 4^(attempt-1), 3600)` seconds: 60 s, 240 s,
960 s, then an hour. After `max_attempts` (default 4) attempts the job is `failed` with reason
`"attempts exhausted: <last reason>"`. A `failed` job keeps every output already uploaded
(partial VODs are served, marked incomplete) and keeps the last `reason` and the worker key that
posted it, so the organiser can read it in the admin listing. A release because the worker lacks
the job's engine build sets `not_before` to `now + 300` so the same worker does not re-claim it
on every poll; the claim filter (section 5) is the real fix.

The failure the worker posts is free text (`reason`, 200 printable characters) plus a `code`
from a fixed list, so the server can count them and the organiser can read them. The engine is
deterministic, so anything the engine decides about the replay is **not** retryable: another
attempt gives the same answer. What is retryable is the machinery around it: ffmpeg, NVENC, the
pipe, the disk, time, and a native fault once. The engine's exit code is only a hint; the real
signal is the `status` column of its `--csv FILE` output, and stderr for the detail.

| `code` | Engine signal | Retryable | Meaning for the organiser |
|---|---|---|---|
| `replay-unreadable` | exit 1, status `parse-error` | no | the stored file is not a readable replay |
| `engine-panic` | exit 1, status `panic@frame` | no | an engine bug on this replay; quarantine-worthy |
| `replay-diverged` | exit 1, status `diverged` or `match-ended-early` | no | the replay does not re-simulate on this build (the verifier should have said so too) |
| `bad-range` | exit 2 and stderr mentions `--from-frame`/`--to-frame` | no | the server's clamp was wrong; a server bug |
| `unsupported` | exit 3 | no | a replay feature the engine does not do |
| `bad-disc` | exit 4, or `--verify-disc` failed | no; the worker should `release` with `no-build` instead of failing | the worker's disc is wrong; fix the worker |
| `boot-error` | exit 1, status `boot-error` | once | the engine could not boot; usually environmental |
| `engine-crash` | native fault (a Windows exception code as the exit status; a signal on Linux) | once | a GPU or engine crash; if the job's previous `failure.code` is already `engine-crash` on the same output, the worker posts it non-retryable |
| `encoder-failed` | ffmpeg exited non-zero, or no hardware encoder session could be opened and no fallback was allowed | yes | ffmpeg/driver trouble |
| `frame-count-mismatch` | exit 0 but ffmpeg's frame count or stderr `video-out: N frames` differs from `expected_frames`, or the server rejected `finish` for it | yes | the pipe or ffmpeg died mid-run |
| `io-error` | pipe, scratch or disk failure (disk full included) | yes | worker-side |
| `engine-timeout` | the worker's own per-output timeout, or the server's `max_running_secs` | yes | too slow; a paused machine, a huge game |
| `output-too-large` | the file exceeds the kind's cap | no | lower the bitrate or raise the cap |
| `build-mismatch` | the worker's build is not the job's and it chose to fail rather than release | no | the claim filter should have prevented this |
| `tool-missing` | an `analyse` job on a worker without the tool or schema | no; prefer `release` with `no-build` | |
| `other` | anything else, with `reason` | as the worker says | |

Exit 2 is also "usage, missing disc, unreadable file"; the worker maps it to `bad-range` only
when stderr names the range flags, otherwise `other` with the stderr tail, not retryable.

## 3. Lease, heartbeat and pause

A successful claim leases the job to the claiming key until `lease_until` (now +
`render.lease_secs`, default 600). The worker must heartbeat before that; every heartbeat
returns a fresh `lease_until`. Recommended heartbeat interval: `lease_secs / 4` (150 s), so one
lost request costs nothing. The heartbeat is a separate task in the worker, not something the
engine's progress drives: during the silent ~40 s seek before a clip's first encoded frame, or a
long game's render, heartbeats keep flowing with `progress` unchanged. Nothing about the lease
depends on how fast the engine is; only `max_running_secs` does.

A heartbeat carries a `state`:

| `state` | Meaning | Lease extended to |
|---|---|---|
| `running` | rendering or uploading | `now + lease_secs` |
| `paused` | the worker is alive but has stopped work (owner is playing) | `now + lease_secs`, **and** the pause clock runs |

The pause clock: the first `paused` heartbeat of a run records `paused_since`; each later one
leaves it; a `running` heartbeat clears it. While `paused_since` is set the server also requires
`now - paused_since <= render.max_pause_secs` (default 28800, eight hours); past that the lease
is revoked, the job returns to `queued`, and the worker's next heartbeat, upload or result is
409 `lease lost`. A paused worker still heartbeats every `lease_secs / 4`; it is the heartbeat,
not the pause, that keeps the lease. A worker that is killed while paused (reboot, Task
Scheduler stopping it) therefore loses the lease after `lease_secs`, not after eight hours.

Why bound the pause at all: a worker that is alive but never resumes (a bug, a stuck game) must
not hold a set's VOD hostage; the organiser sees it as a job that keeps being re-queued, and
another worker, if any, can take it.

Running time is bounded too: the sum of `running` intervals in one attempt may not exceed
`render.max_running_secs` (default 7200 for `vod`, 1800 for `clip` and `analyse`); past it the server revokes
the lease and the attempt counts as failed (`engine-timeout` from the server's side).

**Pause granularity.** The engine cannot be paused mid-render, so the worker pauses only
*between outputs*: it checks its pause predicate before starting each output, and once a render
is running it finishes it (a game is ~1.6 minutes at full speed; longer at below-normal priority
beside a running game). The `paused` heartbeat therefore means "between outputs and waiting".
The predicate is a worker setting (section 11), not part of the protocol; the server only sees
`running` and `paused`.

A heartbeat may carry `progress` purely for the admin listing; it is never required and never
validated beyond its shape. The worker fills it from `slpcheck --progress`, which prints
`progress <frame> <last>` on stderr every 60 replay frames, during the seek too (`frame` may be
negative): `{"output", "phase": "seek" | "encode" | "upload", "frame", "first", "last"}` and the
listing shows `(frame - first) / (last - first)`. During an upload `frame` is null and the
listing uses `UploadState.received` instead.

**Who may act on a leased job.** Every job route with `{id}` after a claim (`replays`,
`heartbeat`, `outputs/*`, `result`, `release`) checks that the job is leased to the signing key
and that the lease has not expired; otherwise 409 `lease lost` (or 409 `not leased` for a
`queued` job). A key in `to_keys` is not exempt: the organiser's own worker plays by the same
rules. The replay download is the exception: any renderer may fetch any job's replays at any
time (they are not secret among renderers, and a worker that wants to pre-fetch may).

## 4. State machine

```text
                       create (auto or admin)            ┌──────────────────────────────┐
  ──────────────────────────────────────────────────────▶│  queued                      │
                                                         │  not_before, attempt = n     │◀──┐
                                                         └──────────────┬───────────────┘   │
                                     claim (renderer; now >= not_before;│                   │
                                     worker has the build)              ▼                   │
      ┌─────────────────────────────────────────────────────────────────────────────┐       │
      │  leased (attempt n+1, lease_until, worker key)                              │       │
      │                                                                             │       │
      │   ┌──────────────┐  heartbeat paused   ┌──────────────┐                     │       │
      │   │  running     │────────────────────▶│  paused      │                     │       │
      │   │              │◀────────────────────│  paused_since│                     │       │
      │   └──────────────┘  heartbeat running  └──────────────┘                     │       │
      │     uploads land while running (outputs: uploading → stored, each verified) │       │
      └───┬───────────────┬───────────────────────────────┬──────────────────┬──────┘       │
          │               │                               │                  │              │
   result ok       result failed                 lease expired,       release              │
          │        retryable                     pause > max,         (worker gives it      │
          │        (attempt < max)               running > max        back; attempt not     │
          │               │                     (attempt counted)     counted)              │
          ▼               └───────────▶ backoff ─────────────┴──────────────────┴───────────┘
  ┌──────────────┐
  │  done        │◀──────────────── admin rerun ───────────────┐
  │  (terminal)  │─────────────────────────────────────────────┼─▶ queued (attempt = 0,
  └──────────────┘                                             │    outputs kept until the
  ┌──────────────┐  result failed (not retryable), or          │    rerun's result replaces
  │  failed      │  attempts exhausted                         │    them)
  │  (terminal,  │◀──────────────────────────────── ───────────┤
  │   reason)    │─────────────────────────────────────────────┤
  └──────────────┘                                             │
  ┌──────────────┐  admin cancel (from queued or leased;       │
  │  cancelled   │  a leased worker learns of it by 409)       │
  │  (terminal)  │◀─────────────────────────────── ────────────┘
  └──────────────┘
```

Rows: `render_jobs` (one per job key) and `render_attempts` (one per claim, with its end and
reason), `render_outputs` (one per output name per job, with the upload's progress, sha256,
bytes, final path and the worker's `RenderReport`), `render_uploads` (open chunked uploads).

Job `state` values on the wire: `queued`, `leased`, `done`, `failed`, `cancelled`. `leased`
carries `lease_until`, `worker_key`, `paused_since` (null when running) and `progress`.

## 5. Routes

Who may call: **renderer** (a key in `render_keys` or `to_keys`), **organiser**, **public**.
Every signed route can also answer 401, 403, 408, 413 and 429 as in `docs/api.md`; the table
lists the other statuses.

| Method | Path | Who | Request body | Success | Other statuses |
|---|---|---|---|---|---|
| POST | `/v1/render/claim` | renderer | `ClaimReq`, or empty | 200 `RenderJob` (leased to the caller); 204 nothing to do | 400; 409 this key already holds `max_jobs_per_worker` leases |
| GET | `/v1/render/jobs/{id}` | renderer | — | 200 `RenderJob` | 400; 404 |
| GET | `/v1/render/jobs/{id}/replays/{n}` | renderer | — | 200 the `.slp` bytes, `x-ab-sha256` header | 400; 404 no such job or input |
| POST | `/v1/render/jobs/{id}/heartbeat` | renderer | `Heartbeat` | 200 `Lease` | 400; 404; 409 lease lost / not leased / job cancelled |
| POST | `/v1/render/jobs/{id}/outputs/{name}` | renderer | `UploadBegin` | 201 `UploadState` (new); 200 `UploadState` (same size and sha256 already begun or stored) | 400 bad name, size over the cap; 404; 409 lease lost; a different sha256 for a stored output |
| GET | `/v1/render/jobs/{id}/outputs/{name}` | renderer | — | 200 `UploadState` | 400; 404 no such job, name or upload |
| PUT | `/v1/render/jobs/{id}/outputs/{name}/chunks/{offset}` | renderer | raw bytes (one chunk) | 200 `UploadState` | 400 offset not a chunk boundary, chunk too big, last chunk wrong size; 404; 409 lease lost, upload not begun, already stored; 413 |
| POST | `/v1/render/jobs/{id}/outputs/{name}/finish` | renderer | `RenderReport` | 200 `UploadState` (`stored`) | 400; 404; 409 lease lost, bytes missing (body lists the missing ranges), **sha256 mismatch** (upload discarded; begin again), already stored |
| POST | `/v1/render/jobs/{id}/result` | renderer | `RenderResult` | 204, empty | 400; 404; 409 lease lost, an `ok` result while an output is missing or not `stored` |
| POST | `/v1/render/jobs/{id}/release` | renderer | `ReleaseReq`, or empty | 204, empty | 400; 404; 409 not leased to this key |
| POST | `/v1/admin/render/jobs` | organiser | `RenderJobCreateReq` | 201 `RenderJob`; 200 `RenderJob` an identical job exists | 400; 404 no such set or game; 409 set not decided, game not counted, no akaneia disc configured |
| GET | `/v1/admin/render/jobs?event={id}&state={state}` | organiser | — | 200 `[RenderJob]`, newest first, at most 200 | 400 |
| GET | `/v1/admin/render/jobs/{id}` | organiser | — | 200 `RenderJob` | 400; 404 |
| POST | `/v1/admin/render/jobs/{id}/rerun` | organiser | `RerunReq`, or empty | 200 `RenderJob` | 400; 404; 409 job is `queued` or `leased` (cancel first) |
| POST | `/v1/admin/render/jobs/{id}/cancel` | organiser | ignored (send `{}`) | 200 `RenderJob` | 400; 404; 409 already terminal |
| GET | `/v1/events/{id}/media` | public | — | 200 `EventMedia` | 400; 404 |
| GET | `/media/{event}/{set}/{file}` | public | — | 200 `video/mp4`, `accept-ranges: bytes`, 206 for `range` | 404; 416 |
| GET | `/v1/sets/{event}/{set}/analyses/{job}` | player of the set, or organiser | — | 200 the full analysis document (`application/json`) | 400; **404** for anything else: no such job, not an analysis, not stored, or the key is not one of the set's two players or an organiser (never 403; see section 8) |
| GET | `/v1/sets/{event}/{set}/analyses/{job}/public` | public | — | 200 the public view of the document (section 8); `cache-control: private, no-store` | 400; **404** when nothing of it is public |
| GET | `/v1/me/analyses?event={id}` | player | — | 200 `[AnalysisRef]`, the signer's analyses in the event | 400; 404 no such event |
| POST | `/v1/admin/events/{id}/analysis-private` | organiser | `{"private": true}` | 200 `EventSummary` | 400; 404 |

Notes on individual routes:

- **Claim.** Takes the best job (section 2, priority order) that is `queued`, whose
  `not_before` has passed, and whose `source.engine_build` is in the request's `builds` (when
  given; an empty body means "anything"). Also takes a `leased` job whose lease has expired (the
  expiry is applied lazily, as `verify/claim` does today with `CLAIM_TTL_SECS`). A worker asking
  for work marks itself seen; the admin listing shows when each renderer key was last seen. A
  claim is a single transaction, so two workers cannot lease one job.
- **Replays.** `{n}` is the index into the job's `inputs` (0-based). The file comes from the
  job row, never from the request. The body is the stored upload the server chose for
  `inputs[n]`: **the copy the verifier re-simulated** when there was one (the file the server
  already knows plays back on this build), else slot 0's; `inputs[n].slot` says which. Nothing in
  the render depends on which player recorded the file. `x-ab-sha256` repeats `inputs[n].sha256`
  so the worker can check the download without re-reading the job.
- **Heartbeat** is the only route that may say `paused`. A heartbeat on a job whose lease has
  expired is 409 even if nobody else has claimed it: the worker must discard its work on it and
  claim again (it will usually get the same job back, with the outputs already stored still
  stored, so little is lost: the worker checks `outputs[*].state` in the `RenderJob` and skips
  stored ones).
- **Outputs** are named by the job (`RenderJob.outputs[*].name`); any other name is 400. An
  `analyse` job's single output `analysis.json` goes through the same begin/chunks/finish (one
  chunk, in practice); at `finish` the server additionally checks that the bytes parse as a JSON
  object with a string `schema` and a `data` member whose `halves` is an array of two (else 400
  `analysis is not valid JSON` and the upload is discarded) and does nothing else with the
  content. An output is `missing` (nothing begun), `uploading` (begun; `received` lists the byte ranges in),
  or `stored` (finished, hash verified, on disk). Uploads are described in section 6.
- **Result `ok`** requires every output `stored`; the server marks the job `done`, writes the
  set's manifest, and the page starts showing the media. **Result `failed`** may come with some
  outputs stored (a VOD whose game 3 crashed); they stay and are served, and the job is retried
  or fails as section 2 says. A result on a job that is already terminal is 409, which the
  worker logs at info level and forgets, as `verify-worker` does.
- **Release** hands the job back without an attempt counted: for shutdown (Task Scheduler
  stopping the worker), or `reason: "no-build"` when the worker finds it cannot render this
  job's build after all (it should have filtered in the claim). Outputs already stored stay.
- **Admin create** needs `kind` and the set (and game and frames for a clip; game and
  `windows` for an analysis). The server fills
  `source` from its own configuration (the engine build and ISO hash it was told, section 10)
  unless the request overrides `engine_build`, and `render` from the preset unless overridden.
- **Rerun** is for a `done`, `failed` or `cancelled` job: it resets `attempt` to 0, clears
  `not_before`, optionally raises `priority`, and goes to `queued`. Existing outputs stay served
  until the rerun's result replaces them (each `finish` of the rerun swaps that output atomically:
  rename over the old file). `RerunReq.engine_build` lets the organiser rerun under a new build:
  that is a *new job* (different key) created with the same inputs, and the answer's `id` differs
  from the path's; the old job is left as is and the manifest points at whichever job of that
  set/clip finished most recently.
- **Media listing and files** are public, like the bracket page, and unsigned; the rate limit
  applies. `/media/...` is served by the app with range support (section 8). **Analyses are
  not media**: they are never under `/media/`, and are served only by the gated routes above
  (section 8, "Analysis visibility").

## 6. Uploads

A VOD game at 6 Mbps is ~45 MB a minute, so an 8-minute game is ~360 MB and a long set over a
gigabyte; a Task Scheduler job can be stopped at any moment; home upstream is often 10-40 Mbps.
So uploads are chunked and resumable, and the server, not the worker, is the record of what has
arrived:

1. **Begin.** `POST …/outputs/{name}` with `{"bytes": N, "sha256": "<hex>"}`. The server checks
   `bytes <= max_output_bytes` for the kind and answers `UploadState` with `chunk_bytes` (fixed
   by the server, default 8 MiB) and `received: []`. Beginning again with the same size and hash
   is 200 with the current `received` (this is how a restarted worker resumes). Beginning with a
   different size or hash while an upload is open discards the open upload and starts over (the
   worker re-rendered, perhaps). Beginning with a different hash for an output already `stored`
   is 409 unless the job is a rerun attempt (then it replaces on finish).
2. **Chunks.** `PUT …/chunks/{offset}` with exactly `chunk_bytes` bytes, except the last chunk,
   which is `bytes - offset`. `offset` must be a multiple of `chunk_bytes` and less than `bytes`.
   The request is signed like any other: `BODY_SHA256` in the canonical string is the chunk's
   hash, so each chunk is integrity-checked on arrival and a corrupted chunk is a 401, retried.
   Chunks may arrive in any order and in parallel (up to `max_parallel_chunks`, default 2); a
   chunk already received is accepted again and ignored (200). The answer is the current
   `UploadState`, so the worker never has to track what landed. Body limit per request:
   `chunk_bytes`; body timeout 120 s (the replay-upload timeout).
3. **Finish.** `POST …/outputs/{name}/finish` with the `RenderReport` (what was actually used:
   build, encoder, duration, frames, ffmpeg version). The server checks every range is in,
   streams the assembled file through SHA-256 and compares with the hash from `begin`; a match
   moves the file into place (`media_dir`, section 8) and marks the output `stored`; a mismatch
   deletes the upload and answers 409 `sha256 mismatch` so the worker begins again (with the
   same hash, if it trusts its file; the server cannot tell which side is wrong). For a media
   output the server also requires `report.frames == expected_frames` (section 2) and
   `report.duration_ms` within one frame of `frames / 60 * 1000`; otherwise 409
   `frame count mismatch`, the upload is discarded, and the worker treats it as a
   `frame-count-mismatch` failure (retryable) rather than re-uploading the same truncated file.
   The worker should have caught this itself from ffmpeg's exit status and the engine's
   `video-out: N frames` line; the server check is the backstop.

Open uploads expire `upload_ttl_secs` (default 86400, a day) after their last chunk; the lease
rules still apply, so in practice an upload dies with its lease unless the same worker re-claims
the job and resumes it, which it can: `UploadState` is keyed by `(job, name)`, not by attempt, so
a re-claim after a reboot continues where the chunks stopped, as long as the worker's rendered
file is the same bytes (same hash): the worker keeps a finished mp4 in scratch until its
`finish` succeeds, precisely so a restart resumes the upload instead of re-rendering (a
re-render would produce different bytes and a new hash). Partial uploads live in `media_dir/incoming/{job}-{name}.part`
as a sparse file written at `offset`, plus a small `.ranges` sidecar; on start the server deletes
`.part` files with no `render_uploads` row.

Why not a single `PUT` with `content-range`: the signing scheme hashes the whole body, a 400 MB
body would need a 400 MB signed request that cannot be resumed, and the existing body limits
and timeouts are built for 16 MiB. Why not multipart or tus: this is one worker talking to one
server over an API that already has a signing convention; fixed-size signed chunks are the
smallest thing that is resumable.

## 7. Types

Enums follow the serde convention of `docs/api.md`. Times are unix seconds. Frames are the
replay's frame indices as Slippi records them and as `slpcheck --from-frame/--to-frame` takes
them: the first frame of a replay is -123, game start is 0, ranges are inclusive.

### RenderSpec

The render settings; part of the job key. The spec describes the *result* (size, codec,
bitrates, container, overlays), not the machine: only `width`/`height` reach the engine
(`--video-size WxH`); everything else is an ffmpeg argument the worker derives, and the encoder
implementation (NVENC, AMF, QSV, VAAPI, VideoToolbox, libx264) is the worker's choice, reported
afterwards in `RenderReport.encoder`. Two workers with different GPUs produce the same spec with
different bytes, and that is fine (section 1).

| Field | Type | |
|---|---|---|
| `preset` | `"vod"` or `"clip"` | which server preset filled the rest; informational once filled in |
| `width`, `height` | integer | even; 64..7680 × 64..4320 (the engine's bounds); 1920×1080 (vod), 1280×720 (clip); wider than 4:3 renders 16:9 |
| `fps` | `60` | fixed by the engine; present so a reader of the manifest need not know that |
| `video_codec` | `"h264"` | the codec, not the encoder |
| `video_kbps` | integer | target bitrate: 6000 (vod), 4000 (clip) |
| `audio_codec` | `"aac"` | |
| `audio_kbps` | integer | 160 |
| `container` | `"mp4-faststart"` | |
| `overlay` | object or null | ffmpeg `drawtext` overlays the worker draws (the engine has none): `tags` (bool: the two tags), `score` (bool: set score before this game), `round` (bool: round name, section 7 of api.md), `clip_title` (bool: the clip's `label` as a title card for the first second), `fade` (bool: a short fade in/out on clips). The text comes from the job (`tags`, `score`, `round_name`, `clip.label`); fonts are the worker's. Default: vod `{"tags": true, "score": true, "round": true}`, clip `{"tags": true, "clip_title": true, "fade": true}` |

```json
{
  "preset": "vod",
  "width": 1920, "height": 1080, "fps": 60,
  "video_codec": "h264", "video_kbps": 6000,
  "audio_codec": "aac", "audio_kbps": 160,
  "container": "mp4-faststart",
  "overlay": {"tags": true, "score": true, "round": true, "clip_title": false, "fade": false}
}
```

### RenderSource

What the output depends on besides the replays; part of the job key.

| Field | Type | |
|---|---|---|
| `engine_build` | string | exact build string (at most 64 printable characters): first line of the engine's `version.txt`, `+`, first 12 hex of the executable's SHA-256, as `crates/client` `config::engine_build` computes it and as players report it in `SignupReq.engine_build`; the worker must run this build |
| `discs` | object | lowercase hex SHA-256 per disc the job may need: `{"vanilla": "<hex>", "akaneia": "<hex>" or null}` from `render.iso_sha256` / `render.akaneia_iso_sha256`; a job never references a disc whose hash is null (section 2). The worker compares with its own one-time hashes (cached by size and mtime; `--verify-disc` is the cheap header check, not a substitute) and `release`s with `no-build` on a mismatch, never fails |

### RenderInput

One replay the job reads.

| Field | Type | |
|---|---|---|
| `n` | integer | index for `GET …/replays/{n}` |
| `game_no` | integer | 1-based game number in the set |
| `game_id` | integer | the server's game id |
| `slot` | 0 or 1 | whose upload is served (the verifier's re-simulated copy when there was one, else slot 0) |
| `disc` | `"vanilla"` or `"akaneia"` | which disc the replay needs; `source.discs[disc]` is its hash |
| `sha256` | string | the file's hash (from `uploads.sha256`) |
| `bytes` | integer | |
| `first_frame`, `last_frame` | integer | from the replay's metadata; `last_frame` is accepted as `--to-frame` and outputs the final frame |
| `ports` | [integer, integer] | the two human players' controller ports in the replay, in bracket slot order (`ports[0]` is slot 0's port); for overlays and for mapping analysis halves |

### RenderOutput

| Field | Type | |
|---|---|---|
| `name` | string | `game-{n}.mp4` for a VOD, `clip.mp4` for a clip, `analysis.json` for an analysis |
| `input` | integer | which `RenderInput.n` it comes from |
| `render_from`, `render_to` | integer or null | the frame range to encode, inclusive, already clamped to `[first_frame, last_frame]` so `slpcheck` accepts it; passed straight to `--from-frame`/`--to-frame`; null means the whole game (VOD: no range flags) |
| `expected_frames` | integer or null | `render_to - max(render_from, -123) + 1` (whole game: from `first_frame`); the frame count the worker must deliver and `finish` checks; null for `analysis.json` |
| `score` | [integer, integer] or null | the set score before this game, `[slot 0, slot 1]`, for the overlay |
| `state` | `"missing"`, `"uploading"` or `"stored"` | |
| `bytes`, `sha256` | integer, string, or null | once begun |
| `report` | `RenderReport` or null | once stored; for `analysis.json` the media fields are 0 and `schema` is filled |
| `url` | string or null | once stored: `/media/{event}/{set}/{file}` for media; for `analysis.json` the gated path `/v1/sets/{event}/{set}/analyses/{job}` (section 8) |
| `content_type` | `"video/mp4"` or `"application/json"` | |

### RenderJob

| Field | Type | |
|---|---|---|
| `id` | integer | |
| `kind` | `"vod"`, `"clip"` or `"analyse"` | |
| `event_id`, `set_id` | integer | |
| `state` | `"queued"`, `"leased"`, `"done"`, `"failed"`, `"cancelled"` | |
| `priority` | integer | |
| `attempt` | integer | attempts started so far (the current one included while leased) |
| `max_attempts` | integer | |
| `not_before` | integer or null | while queued after a retryable failure |
| `lease` | `Lease` or null | while leased |
| `source` | `RenderSource` | |
| `render` | `RenderSpec` or null | null for `analyse` |
| `inputs` | [`RenderInput`] | |
| `outputs` | [`RenderOutput`] | |
| `clip` | object or null | for a clip: `game_no`, `moment_from`, `moment_to`, `pad_before_frames`, `pad_after_frames`, `label` (string or null, the detector's name for it: `"combo"`, `"edgeguard-kill"`, `"zero-to-death"`, `"comeback"`, or the organiser's text) |
| `analyse` | `AnalyseSpec` or null | for an analysis |
| `failure` | object or null | for a failed attempt or job: `code`, `reason`, `at`, `worker_key` |
| `tags` | [slot 0 tag, slot 1 tag] | for overlays, the worker's logs and the page |
| `round_name` | string | the set's round name (api.md section 7), for overlays |
| `created_at`, `updated_at` | integer | |

A claim answer (a VOD):

```json
{
  "id": 17,
  "kind": "vod",
  "event_id": 1, "set_id": 3,
  "state": "leased",
  "priority": 0, "attempt": 1, "max_attempts": 4, "not_before": null,
  "lease": {"lease_until": 1763201200, "worker_key": "d75a…511a", "paused_since": null, "progress": null},
  "source": {"engine_build": "melee (rust) 0.1.0 development build+0cd085d1234", "discs": {"vanilla": "9b1c…e2f0", "akaneia": null}},
  "render": {"preset": "vod", "width": 1920, "height": 1080, "fps": 60, "video_codec": "h264",
             "video_kbps": 6000, "audio_codec": "aac", "audio_kbps": 160,
             "container": "mp4-faststart",
             "overlay": {"tags": true, "score": true, "round": true, "clip_title": false, "fade": false}},
  "inputs": [
    {"n": 0, "game_no": 1, "game_id": 12, "slot": 0, "disc": "vanilla", "sha256": "75b3…6a91c", "bytes": 1848211, "first_frame": -123, "last_frame": 9876, "ports": [1, 2]},
    {"n": 1, "game_no": 2, "game_id": 13, "slot": 1, "disc": "vanilla", "sha256": "0c2a…41d7", "bytes": 2010034, "first_frame": -123, "last_frame": 11240, "ports": [1, 2]}
  ],
  "outputs": [
    {"name": "game-1.mp4", "input": 0, "render_from": null, "render_to": null, "expected_frames": 10000, "score": [0, 0], "state": "stored",
     "bytes": 74211998, "sha256": "ab12…",
     "report": {"engine_build": "melee (rust) 0.1.0 development build+0cd085d1234", "encoder": "h264_nvenc", "muxer": "ffmpeg version 7.1-full_build", "frames": 10000, "duration_ms": 166667, "render_ms": 64400, "schema": null},
     "url": "/media/1/3/game-1.mp4", "content_type": "video/mp4"},
    {"name": "game-2.mp4", "input": 1, "render_from": null, "render_to": null, "expected_frames": 11364, "score": [1, 0], "state": "missing",
     "bytes": null, "sha256": null, "report": null, "url": null, "content_type": "video/mp4"}
  ],
  "clip": null,
  "analyse": null,
  "failure": null,
  "tags": ["Mango", "Armada"],
  "round_name": "Winners semis",
  "created_at": 1763200000, "updated_at": 1763200600
}
```

A clip's `clip` and `outputs`:

```json
{
  "clip": {"game_no": 2, "moment_from": 4410, "moment_to": 4702, "pad_before_frames": 120, "pad_after_frames": 90, "label": "zero-to-death"},
  "outputs": [{"name": "clip.mp4", "input": 0, "render_from": 4290, "render_to": 4792, "expected_frames": 503, "score": [1, 0], "state": "missing",
               "bytes": null, "sha256": null, "report": null, "url": null, "content_type": "video/mp4"}]
}
```

### AnalyseSpec and the analysis output

The payload of an `analyse` job. It is deliberately generic: the server validates only the
shape below and hands the rest to the tool.

| Field | Type | |
|---|---|---|
| `tool` | string | the tool the worker should run; `"melee-analyse"` is the only value today, and the worker filters on it in the claim (`ClaimReq.tools`) |
| `game_no` | integer | 1-based game in the set; `inputs[0]` is its replay |
| `windows` | [[from_frame, to_frame]] | the frame windows to analyse (the conversion openings), inclusive, within the game, sorted; at most 200 |
| `params` | JSON object | opaque to the server; at most 16 KiB canonical; part of the job key |
| `schema` | string | the output schema the organiser expects, e.g. `"missed-punish/1"`; the worker fails with `tool-missing` if its tool cannot produce it |

```json
{"tool": "melee-analyse", "game_no": 2, "windows": [[1800, 2140], [4410, 4702]], "params": {}, "schema": "missed-punish/1"}
```

An `analyse` job's single output is `analysis.json`, `content_type` `application/json`,
whose bytes are a JSON object:

| Field | Type | |
|---|---|---|
| `schema` | string | the schema version actually produced (`"missed-punish/1"`); must start with the job's `schema` up to the `/` |
| `tool_build` | string | the analysis tool's build string |
| `game_no` | integer | |
| `windows` | [[from, to]] | the windows analysed, echoed |
| `ports` | [integer, integer] | the two human players' controller ports **in ascending port order**; `data.halves[i]` is about the player on `ports[i]`. The tool knows ports, not bracket slots, so the split is defined in the game's own terms and the server maps port to slot with `RenderInput.ports` when serving. Defining halves by `RenderInput.slot` would swap players whenever slot 1's file was served |
| `data` | object | the tool's result, split so the server can apply per-player visibility (section 8): `data.common` (any JSON: facts about the game that belong to nobody, such as the windows and stage) and `data.halves`, an array of exactly two members in `ports` order (any JSON each, null allowed). The server never looks inside `common` or a half |

```json
{"schema": "missed-punish/1", "tool_build": "melee-analyse 0.1.0+abc1234", "game_no": 2,
 "windows": [[1800, 2140], [4410, 4702]], "ports": [1, 2],
 "data": {"common": {"stage": 31}, "halves": [{"missed": [{"window": 0, "frame": 1861, "note": "…"}]}, {"missed": []}]}}
```

The server checks `schema`, `tool_build`, `ports` and `data` are present, that `ports` equals
the input's two ports as a set, that `data.halves` is an array of two (400 at `finish`
otherwise), and that the whole file is at most
`render.max_analysis_output_bytes` (default 4 MiB). The page (or a later organiser tool)
interprets the halves by `schema`. The `common`/`halves` split is the one piece of structure the
server imposes, and it exists only so that one player's half can be shown without the other's.

### ClaimReq

| Field | Type | |
|---|---|---|
| `builds` | [string], optional | engine builds this worker can run; the server offers only jobs whose `source.engine_build` is listed. Empty or missing: any |
| `kinds` | [`"vod"`/`"clip"`/`"analyse"`], optional | default all |
| `tools` | [string], optional | analysis tools this worker has (`"melee-analyse"`); `analyse` jobs are offered only when their `tool` is listed. Empty or missing: no `analyse` jobs |
| `discs` | [`"vanilla"`/`"akaneia"`], optional | discs this worker has; a job is offered only if every input's `disc` is listed. Missing: `["vanilla"]` |
| `encoders` | [string], optional | H.264 encoders found by `ffmpeg -encoders`, best first (e.g. `["h264_nvenc", "libx264"]`); informational, shown in the admin listing so the organiser can see what a given worker will produce with. The server never filters on it |
| `worker` | string or null, optional | a label for the admin listing (at most 64 printable characters), e.g. the machine name |

```json
{"builds": ["melee (rust) 0.1.0 development build+0cd085d1234"], "kinds": ["vod", "clip", "analyse"],
 "tools": ["melee-analyse"], "discs": ["vanilla"], "encoders": ["h264_nvenc", "libx264"], "worker": "GD-PC"}
```

### Heartbeat and Lease

```json
{"state": "running", "progress": {"output": "game-2.mp4", "phase": "encode", "frame": 3200, "first": -123, "last": 11240}}
```

```json
{"state": "paused", "progress": null}
```

`Lease`: `{"lease_until": 1763201800, "worker_key": "d75a…511a", "paused_since": 1763201300, "progress": {…}}`.

### UploadBegin and UploadState

```json
{"bytes": 74211998, "sha256": "ab12cd34…"}
```

| `UploadState` field | Type | |
|---|---|---|
| `name` | string | |
| `state` | `"missing"`, `"uploading"`, `"stored"` | |
| `bytes`, `sha256` | integer, string | as begun |
| `chunk_bytes` | integer | the server's chunk size; every `PUT` but the last is exactly this long |
| `received` | [[start, end]] | byte ranges received, inclusive-exclusive, sorted, merged; `[[0, 74211998]]` when complete |
| `missing` | [[start, end]] | the complement, for convenience |
| `expires_at` | integer or null | when the open upload is discarded if nothing more arrives |

### RenderReport

What the worker actually used, recorded with the output.

| Field | Type | |
|---|---|---|
| `engine_build` | string | must equal the job's `source.engine_build`, else 400 (the output would be mislabelled; release the job or fail it with `build-mismatch`) |
| `encoder` | string | the ffmpeg encoder actually used: `"h264_nvenc"`, `"h264_amf"`, `"h264_qsv"`, `"h264_vaapi"`, `"h264_videotoolbox"`, `"libx264"` |
| `muxer` | string or null | ffmpeg's version banner's first line, e.g. `"ffmpeg version 7.1-full_build"`; since output bytes vary by ffmpeg, encoder and driver, this is the only record of what produced them |
| `frames` | integer | frames encoded; must equal `expected_frames` (else 409 at `finish`); 0 for an analysis |
| `engine_frames` | integer or null | the count from the engine's stderr `video-out: N frames` line, for the worker's own check; informational |
| `duration_ms` | integer | media duration (0 for an analysis) |
| `render_ms` | integer | wall time spent rendering or analysing (running, not paused) |
| `schema` | string or null | for `analysis.json`, the schema produced; null for media |

### RenderResult

```json
{"ok": {}}
```

```json
{"failed": {"code": "engine-crash", "reason": "slpcheck exited with 0xC0000005 at frame 7730 of game-3", "retryable": true}}
```

### ReleaseReq

```json
{"reason": "shutdown"}
```

`reason`: `"shutdown"` (default) or `"no-build"`.

### RenderJobCreateReq

| Field | Type | |
|---|---|---|
| `kind` | `"vod"`, `"clip"` or `"analyse"` | |
| `event_id`, `set_id` | integer | the set must be `done` (409 otherwise) |
| `game_no` | integer | clip and analyse; must be a counted game |
| `from_frame`, `to_frame` | integer | clip only; the moment, before padding, inclusive, within the game |
| `analyse` | `AnalyseSpec` without `game_no`, | analyse only: `tool`, `windows`, `params`, `schema` (`tool` defaults to `"melee-analyse"`, `params` to `{}`) |
| `pad_before_frames`, `pad_after_frames` | integer, optional | default 120 / 90; at most 600 each |
| `label` | string or null, optional | at most 40 printable characters |
| `slot` | 0 or 1, optional | whose upload to render; default 0 |
| `engine_build` | string or null, optional | default: the server's configured render build |
| `render` | partial `RenderSpec`, optional | overrides the preset field by field |
| `priority` | integer, optional | default 10 |

```json
{"kind": "clip", "event_id": 1, "set_id": 3, "game_no": 2, "from_frame": 4410, "to_frame": 4702, "label": "that edgeguard"}
```

```json
{"kind": "analyse", "event_id": 1, "set_id": 3, "game_no": 2,
 "analyse": {"windows": [[1800, 2140], [4410, 4702]], "schema": "missed-punish/1"}}
```

### RerunReq

```json
{"priority": 20, "engine_build": null}
```

### EventMedia (public)

One entry per set that has any stored output, with per-game VODs and clips, in bracket order,
so the page needs one request. Clips carry their `label`, `game_no`, and the moment's position
in the game's VOD in milliseconds (`at_ms`, computed from `moment_from - first_frame` at 60 fps).

```json
{
  "event_id": 1,
  "sets": [
    {"set_id": 3, "tags": ["Mango", "Armada"], "complete": false,
     "games": [{"game_no": 1, "url": "/media/1/3/game-1.mp4", "bytes": 74211998, "duration_ms": 166667}],
     "clips": [{"url": "/media/1/3/clip-19-zero-to-death.mp4", "label": "zero-to-death", "game_no": 2, "at_ms": 75550, "duration_ms": 8383}],
     "analyses": [{"job": 23, "url": "/v1/sets/1/3/analyses/23/public", "game_no": 2, "schema": "missed-punish/1", "public": "partial", "halves": [true, false]}]}
  ]
}
```

`complete` is true when every counted game of the set has a stored VOD. `analyses` lists only
the analysis documents of which **something is public** (section 8): `public` is `"full"` (both
players opted in) or `"partial"`, and `halves` says which player's half the public view carries.
A private analysis is absent from `EventMedia` entirely; its players find it with
`GET /v1/me/analyses?event={id}`, whose `AnalysisRef` entries are
`{"job", "event_id", "set_id", "game_no", "schema", "url": "/v1/sets/{event}/{set}/analyses/{job}", "stored_at"}`,
and the organiser with the admin job listing.

## 8. Storage and serving

**Layout** under `media_dir` (new config, default `media`, sibling of `replay_dir`):

```text
media/
  incoming/                       open uploads: {job}-{name}.part (sparse) + .ranges
  {event}/{set}/
    game-{n}.mp4                  the set's VOD, one file per counted game
    clip-{job}-{label}.mp4        clips; the job id keeps names unique and stable
    manifest.json                 what EventMedia says about this set, rewritten on every change
    analyses/
      analysis-{job}.json         analysis results; NOT served by /media/, see "Analysis visibility"
```

Final paths are recorded in `render_outputs.path`; `finish` writes to `incoming/` and renames
into place (same filesystem, atomic), overwriting an older output of the same name on a rerun.
A `cancel` or a job's deletion never deletes stored media; the organiser removes files by hand or
with a future `autobracket admin media prune`. Disk is the organiser's concern: a 32-player
double-elimination event is ~60 sets; at ~4 minutes each that is ~11 GB of VOD at 6 Mbps. The
server refuses to begin an upload when `media_dir` has less than `media_min_free_bytes` (default
2 GiB) free after it, answering 507 `insufficient storage` (the one status code new to the API;
the worker treats it like 5xx and retries later).

**Serving: the app, not nginx static.** The media is served by the autobracket binary at
`/media/{event}/{set}/{file}` with `accept-ranges`, 206 partial content, `etag` (the sha256),
`cache-control: public, max-age=31536000, immutable` (file names are stable and a rerun changes
the bytes but the page's manifest changes the URL's query string with the new sha prefix), and
`content-type: video/mp4`. Reasons:

- The production service runs with `DynamicUser` under `/var/lib/private/autobracket`, and the
  nginx front is the Mailu container, which cannot see that directory; a shared volume would
  weaken the unit's isolation for no gain at this scale (one tournament's viewers).
- The file set is known only to the app (which outputs are `stored`, which were replaced), so
  `404` vs `200` and the etags stay right without a second source of truth.
- `tower-http`'s `ServeDir`/`ServeFile` already does ranges, etags and conditional requests;
  there is nothing to write but the path check (`{event}` and `{set}` are integers and `{file}`
  matches `^(game-\d+|clip-\d+-[a-z0-9-]{1,40})\.mp4$`; nothing else is looked up, so the
  `analyses/` subdirectory is unreachable through `/media/`).
- The nginx front needs only `proxy_buffering off` for `/media/` and a longer
  `proxy_read_timeout`; `client_max_body_size` for `/v1/render/` rises to the chunk size plus
  headers (`9m`). If serving load ever matters, `proxy_cache` on `/media/` in the front gives
  nginx-static speed without changing anything in the app, since the responses are immutable.

The media routes are public and unsigned like the bracket page; the general rate limit applies
per client, and a video element fetching ranges makes few requests. Nothing in a URL is secret.

### Analysis visibility

Owner decision (2026-11-15): an analysis describes a player's mistakes, so **by default it is
visible only to the set's two players and to organisers, never to the public.** Two toggles
change that:

| Toggle | Where | Default | Effect |
|---|---|---|---|
| `share_analysis` | per player (`players.share_analysis`) | `false` | "share my analysis publicly". The player's **own half** of every analysis of their games becomes public. A game's analysis becomes **fully** public (both halves and `common`) only when **both** players of the set have opted in |
| `analysis_private` | per event (`events.analysis_private`), organiser override | `false` | when `true`, nothing of any analysis of the event is public, whatever the players chose; players and organisers still see everything |

`share_analysis` needs accounts (phase 2b) to be set by the player, so for now it is a column
with a default and no route; the account work adds `PUT /v1/me/settings`. `analysis_private` is
set at event creation (`EventCreateReq.analysis_private`, optional, default `false`) or later
with `POST /v1/admin/events/{id}/analysis-private`; changing it affects what is served from the
next request on, and `EventMedia` is recomputed from the rules on every request, not cached.

What each route serves:

| Route | Caller | Serves |
|---|---|---|
| `GET /v1/sets/{event}/{set}/analyses/{job}` | a player of the set (either slot), or an organiser | the whole stored document, unchanged |
| same | anyone else, or unsigned | **404** |
| `GET …/analyses/{job}/public` | anyone, unsigned | the document with `data.halves[i]` replaced by `null` for each player who has not opted in; `data.common` is included only when both opted in (it may describe both); when the event is `analysis_private`, or neither player opted in, **404** |
| `GET /v1/me/analyses?event={id}` | a player | refs of the analyses of the signer's own sets |
| `GET /v1/admin/render/jobs/{id}` | organiser | the job, with the output's URL |

**404, not 403.** A gated analysis route answers 404 to a correctly signed request from a key that
may not read it, and to an unsigned request, so that the existence of an analysis of a given set
and game cannot be probed; a 403 would confirm it. The only non-404 refusals are the signing
layer's own (401 for a missing or bad signature, 429), which say nothing about the resource. For
the same reason the public view of a fully private document is 404 rather than an empty object,
and `EventMedia` omits private analyses instead of listing them without a URL. Internally this is
a role check that *follows* the lookup: the handler loads the job, confirms it is a stored
`analyse` output of that set, then checks the signer against the set's two player keys and
`to_keys`, and any failure on that path is the same 404. This differs from every other route in
`docs/api.md`, where a role failure is 403 before the body is read; the difference is deliberate
and confined to these routes. When accounts arrive, "a player of the set" also means a session
belonging to one of those players, with the same answers.

The analysis file itself is written once at `finish` and never rewritten for visibility; the
filtering happens when serving. Caching: the gated route sends `cache-control: private, no-store`
and no `etag`, since the same URL yields different bodies for different callers and the public
view changes when a player toggles sharing.

## 9. Limits

| What | Limit |
|---|---|
| Request body, render routes other than chunks | 64 KiB (the general limit) |
| Chunk body (`PUT …/chunks/{offset}`) | `render.chunk_bytes`, default 8 MiB; fixed per server, announced in `UploadState` |
| Time for a chunk body to arrive | 120 s (as a replay upload) |
| Output size, `vod` | `render.max_vod_output_bytes`, default 1 GiB per output (an 8-minute game at 6 Mbps is ~360 MB; 1 GiB is a 22-minute timeout game) |
| Output size, `clip` | `render.max_clip_output_bytes`, default 64 MiB (a 30 s clip at 4 Mbps is ~15 MB) |
| Output size, `analyse` | `render.max_analysis_output_bytes`, default 4 MiB; must parse as JSON at `finish` |
| Outputs per job | counted games of the set (at most `best_of_finals`, 5 or 7); 1 for a clip or an analysis |
| Clip length | `to_frame - from_frame + padding` at most 3600 frames (60 s); longer is 400 |
| Analysis windows | at most 200 per job; `params` at most 16 KiB |
| Padding | at most 600 frames each |
| Leases per worker key | `render.max_jobs_per_worker`, default 1 (one hardware encoder session per host; settled with the desktop). Several worker keys may each hold one |
| Worker scratch | worker-side: the current desktop caps it at 2 GB; a long 1080p set plus its replays fits; the worker refuses to start an output that would exceed the cap and fails the job `io-error` (retryable) |
| Idle poll | worker-side: 60 s on the current desktop (jobs arrive once per set); the server does not care |
| Lease | `render.lease_secs` 600; heartbeat every ≤ 150 s recommended |
| Pause | `render.max_pause_secs` 28800 (8 h) per attempt |
| Running time per attempt | `render.max_running_secs` 7200 (vod), 1800 (clip, analyse) |
| Attempts | `render.max_attempts` 4; backoff 60 s, 240 s, 960 s, 3600 s |
| Open upload | discarded `render.upload_ttl_secs` (86400) after its last chunk |
| Parallel chunk uploads per output | 2 recommended; the server accepts any order |
| Admin job listing | 200 newest jobs per request |
| Requests | the general `rate_per_sec`/`rate_burst` (20/40). A worker uploading 8 MiB chunks two at a time on a 40 Mbps link makes ~0.6 requests/s; heartbeats add 0.007/s; the limit is not a factor. A worker that gets 429 backs off `retry-after` like any client |
| Free space | an upload is refused (507) when it would leave less than `render.media_min_free_bytes` (2 GiB) |

## 10. Configuration

New `[render]` table in `autobracket.toml`; every key has a default, and the whole feature is
off until `render_keys` (top level, next to `verifier_keys`) is non-empty.

```toml
render_keys = ["<worker public key hex>"]

[render]
media_dir = "media"
engine_build = "melee (rust) 0.1.0 development build+0cd085d1234"   # what new jobs ask for
iso_sha256 = "9b1c…e2f0"              # the GALE01 disc; required for any job
akaneia_iso_sha256 = ""               # optional; unset = Akaneia sets get no jobs
auto_vod = true
auto_clips = true
auto_analyse = false        # until a server-side opening detector exists
clip_threshold = 0.0        # the detector's score cut-off; 0 = every detected moment
lease_secs = 600
max_pause_secs = 28800
max_running_secs_vod = 7200
max_running_secs_clip = 1800
max_running_secs_analyse = 1800
max_analysis_output_bytes = 4194304
max_attempts = 4
max_jobs_per_worker = 1
chunk_bytes = 8388608
max_vod_output_bytes = 1073741824
max_clip_output_bytes = 67108864
upload_ttl_secs = 86400
media_min_free_bytes = 2147483648
```

`render.engine_build` defaults to the in-process verifier's build when there is one (it is the
build the server already trusts to re-simulate; the same binary renders), so the production
server needs only `render_keys` and `media_dir`. The server logs a warning at start when
`render.engine_build` is not in `accepted_builds` (when that list is set): the VOD would be
rendered on a build the players did not play on, which is allowed but worth knowing.

## 11. Worker loop

`autobracket render-worker --config render-worker.toml`, in the style of `verify_worker.rs`.
Everything about the host lives in the worker's config; the protocol sees none of it.

### Worker configuration

| Key | Meaning | Current desktop (Windows) | A Linux host |
|---|---|---|---|
| `server`, `key_file` | the server and the worker's secret key (`autobracket keygen`) | `https://bracket.gsd.sh`, `%APPDATA%\autobracket\render-worker.key` | same URL, `~/.config/autobracket/render-worker.key` |
| `engine` | the engine executable; its build string is computed exactly as `crates/client` `config::engine_build` does (first line of `version.txt` next to it, `+`, first 12 hex of the exe's SHA-256). That function moves to a shared crate and the worker **must** reuse it; there is no `melee --version` (it is `melee version`) and no second implementation | `E:\Projects\Melee Workspace\target\release\melee.exe` | `/opt/melee/melee` |
| `disc.vanilla`, `disc.akaneia` | disc images; hashed once (SHA-256, cached by size and mtime beside the file); `akaneia` optional, exported to the engine as `GW_ISO_AKANEIA` | `E:\Games\melee.iso`, `E:\Games\akaneia.iso` | `/opt/melee/melee.iso`, unset |
| `ffmpeg` | the ffmpeg executable; probed at start with `-encoders` | `C:\Tools\ffmpeg\bin\ffmpeg.exe` | `/usr/bin/ffmpeg` |
| `encoder` | `"auto"` (first available of `h264_nvenc`, `h264_amf`, `h264_qsv`, `h264_vaapi`, `h264_videotoolbox`, then `libx264`), or a name to force; the per-encoder quality flags (`-preset p4 -tune ll` for NVENC, `-preset veryfast` for libx264, …) are a table in the worker | `auto` → `h264_nvenc` | `auto` → `h264_vaapi` or `libx264` |
| `audio_pipe` | how raw audio reaches ffmpeg: `"named-pipe"` (`\\.\pipe\ab-render-<pid>`) or `"fifo"` (`mkfifo` in scratch) | `named-pipe` | `fifo` |
| `scratch`, `scratch_max_bytes` | working directory and cap; a finished mp4 stays until its `finish` succeeds | `E:\Projects\Melee Workspace\scratch\render-worker\`, 2 GB | `/var/tmp/autobracket-render`, 2 GB |
| `pause` | the pause predicate, pluggable: `{kind = "none"}`; `{kind = "lock-dirs", dirs = [...], file = "owner", contains = "owner-play"}`; `{kind = "command", run = [...]}` (exit 0 = pause); `{kind = "process", names = [...]}`. Checked between outputs only | `lock-dirs` over `E:\Projects\Melee Workspace\scratch\{.cargo-build.lock, .cargo-build.lock.2, .3, .4, .light-run.lock}`, pausing when a lock dir holds a file `owner` containing `owner-play`. Not bare process presence: agents run headless `melee`, and the worker's own children must be excluded | `none`, or `command` |
| `idle_poll_secs` | sleep after a 204 | 60 | 60 |
| `kinds`, `analyse_tool` | job kinds to claim; the analysis tool executable, if any | `["vod", "clip"]`, unset | |
| `priority` | OS priority for the engine and ffmpeg children | below normal | `nice 10` |
| `env` | extra environment for the engine; always includes `MELEE_WINDOW_HIDDEN=1`. `MELEE_VOLUME` is **not** set: its effect on `--audio-out` is untested and must be checked before production | | |

### Scheduling

The worker is a long-running process the host keeps alive; it needs no inbound port.

- **Windows Task Scheduler** (the current desktop): a task running as the owner's user, trigger
  "at log on", action `autobracket.exe render-worker --config ...`, "run with below normal
  priority" (the task's priority field, 7), "if the task fails, restart every 1 minute", "stop
  the task if it runs longer than" **off**, and "start only if on AC power" off. Scratch
  survives restarts, so an interrupted upload resumes (section 6).
- **Linux systemd user unit** (`~/.config/systemd/user/autobracket-render-worker.service`,
  `systemctl --user enable --now`, with `loginctl enable-linger`):

  ```ini
  [Unit]
  Description=autobracket render worker
  After=network-online.target

  [Service]
  ExecStart=/usr/local/bin/autobracket render-worker --config %h/.config/autobracket/render-worker.toml
  Nice=10
  Restart=always
  RestartSec=60
  SupplementaryGroups=video render
  KillSignal=SIGINT
  TimeoutStopSec=15

  [Install]
  WantedBy=default.target
  ```

  (`video`/`render` for VAAPI device access; on a headless NVIDIA box, NVENC needs no display.)

### The loop

1. At start: read the key; compute the engine build (shared `engine_build`); hash the discs (or
   load the cached hashes); `melee slpcheck --verify-disc` each disc (exit 4 → refuse to start);
   probe ffmpeg's encoders and pick one; warn on plain `http://` to a remote host as
   `verify-worker` does.
2. **Claim** with `builds: [own build]`, `kinds`, `tools`, `discs` (those configured),
   `encoders` (the probe's list), `worker` = host name. 204: sleep `idle_poll_secs`. Error:
   sleep 5 s, retry.
3. Start a heartbeat task at `lease_secs / 4`. It reports `running`, or `paused` while the
   worker is waiting between outputs on the pause predicate; it carries `progress` parsed from
   the engine's `progress <frame> <last>` lines. On 409 from a heartbeat, kill the render,
   discard scratch for this job, go to 2.
4. For each output whose `state` is not `stored`, in order: wait while the pause predicate holds
   (heartbeating `paused`); check `source.discs[input.disc]` against the local hash, and
   `release` with `no-build` if it differs or the disc is missing; download the input
   (`GET …/replays/{n}`, verify `sha256`; keep it for the next output of the same input); run
   the pipeline: engine (`--slp`, `--video-size`, `--from-frame`/`--to-frame` when set,
   `--video-out -`, `--audio-out <pipe>`, `--progress`, `--csv <scratch>/status.csv`) piped into
   ffmpeg (rawvideo rgba 60 fps + s16le 32000 Hz stereo from the pipe, the chosen encoder at
   `video_kbps`, AAC at `audio_kbps`, `drawtext` filters for `render.overlay`, `+faststart`);
   **check all three**: ffmpeg's exit status, the CSV `status` column, and the engine's
   `video-out: N frames` against `expected_frames` (ffmpeg's own frame count too); any mismatch
   is a failure per the table in section 2, never an upload. For an `analyse` job, run the tool
   instead and write `analysis.json`. Then `begin` the upload with the file's size and hash (200
   with ranges already received means resume); `PUT` the missing chunks (2 in parallel);
   `finish` with the `RenderReport`. On `sha256 mismatch` at finish, `begin` again once; a second
   mismatch is `failed` with `other`, not retryable. On `frame count mismatch` at finish, fail
   `frame-count-mismatch`.
5. When every output is stored, `result ok`. Otherwise `result failed` with the code and
   `retryable` from the section 2 table. On 409 from `result`, log and go to 2.
6. On a stop request (Task Scheduler stop or `CTRL_SHUTDOWN_EVENT`; `SIGINT`/`SIGTERM` from
   systemd), kill the pipeline, `release` the current job with `shutdown` if the server answers
   within 5 s, keep any finished mp4 in scratch, exit.

The worker keeps no state between runs except its scratch directory. Everything it needs to
resume is in the `RenderJob` and `UploadState` answers plus the finished files it kept.

## 12. Open questions for the desktop

Resolved by the desktop's review of 2026-11-16 (kept for the record, one line each):

1. **Frame numbering.** Settled: `.slp` indices, replay starts at -123, `N..=M` inclusive,
   `N <= 0` from the start, `M = last_frame` accepted and outputs the final frame, no off-by-one;
   expected frames `M - max(N, -123) + 1`. The `last_frame - 1` clamp is dropped.
2. **Render settings.** The engine's only knob is `--video-size WxH` (even, 64..7680 ×
   64..4320, default 1280x720; wider than 4:3 is 16:9), fixed 60 fps, no `--video-realtime`. It
   outputs raw RGBA and raw s16le 32 kHz stereo; codec, bitrate, container and preset are
   ffmpeg's. `hud` is dropped. Output bytes are not reproducible; `report.muxer` records ffmpeg.
3. **ISO hash.** Keep the worker's one-time SHA-256 (cached by size and mtime) as the strict
   check; `--verify-disc` is the cheap header check (exit 4). A mismatch is `release` with
   `no-build`, not a failure. Akaneia replays need a second disc (`GW_ISO_AKANEIA`): second hash
   in config, or those sets are excluded (section 2).
4. **Pause.** Between outputs only. The current desktop's predicate is the owner-play lock
   dirs; it is worker configuration (section 11), not protocol. Not bare process presence.
5. **Concurrency.** One hardware-encoder job per host: `max_jobs_per_worker = 1`.
6. **Progress.** `--progress` prints `progress <frame> <last>` on stderr every 60 replay frames,
   seek included, `frame` possibly negative; percent is `(frame - first) / (last - first)`.
7. **Which upload.** Nothing depends on the recorder; the server serves the verifier's
   re-simulated copy when there is one.
9. **Overlays.** None in the engine. Tags, score, round name, fades and clip title cards are
   ffmpeg `drawtext` in the worker, driven by `RenderSpec.overlay`.
10-12. **Host.** Current desktop: Task Scheduler as the owner's user, scratch
   `E:\Projects\Melee Workspace\scratch\render-worker\` capped at 2 GB, scratch survives
   restarts and keeps finished mp4s for resume, poll every 60 s, 1 GiB VOD cap is fine. All of it
   is worker configuration now that the host may change (section 11).
13. **Build string.** No `melee --version`; `engine_build` = first line of `version.txt` + `+` +
    first 12 hex of the exe's SHA-256, from `crates/client` `config::engine_build`, which the
    worker reuses from a shared crate.
14. **Failure codes.** Exit codes and the `--csv` `status` column mapped in the section 2 table;
    engine verdicts (parse-error, panic, diverged, early end, bad range, unsupported) are not
    retryable; ffmpeg/NVENC, pipe, disk and timeout are; a native crash once. New codes
    `engine-panic`, `replay-diverged`, `encoder-failed`, `frame-count-mismatch`, `io-error`,
    `boot-error`, `unsupported`, `bad-disc`. The exit-0 truncation hole is closed by the frame
    count check at `finish`.
15e. **Analysis visibility.** Owner's decision, section 8. `halves[i]` is now defined by the
    game's port order (`ports`, ascending), never by `RenderInput.slot`.

Still open:

8. **Clip detection location.** Confirm the server creates clip jobs (the replay crate's moment
   detector runs server-side at set end) and the worker never decides what to clip; the label
   vocabulary (`combo`, `kill`, `edgeguard-kill`, `comeback`, `zero-to-death`) is then the replay
   crate's, not this API's.
15. **`analyse` jobs**, after (c): (a) one game per job with a list of frame windows, or the
    whole set? (b) does the tool need the engine and disc (headless simulation) or only the
    `.slp`, so a GPU-less host could run it and `source` is irrelevant to it? (c) one JSON
    document per game, 4 MiB cap? (d) how is `--frames` spelled and how are `params` passed?
    The `common`/`halves` split in port order: confirm it is natural for the tool.
16. **Telling Akaneia from vanilla server-side.** The engine auto-chooses the disc from the
    replay's gecko codes and fighters. Which exact signal should the server's `replay` crate use
    to set `RenderInput.disc` (a gecko list hash in the ruleset's `allowed_gecko` style? a
    fighter id outside the vanilla range?), so that the server and the engine never disagree
    about which disc a replay needs? If they could disagree, the worker should also report the
    disc the engine chose (`report.disc`) and the server reject a mismatch.
17. **`--csv` status vocabulary.** The full list of `status` values (`ok`, `parse-error`,
    `boot-error`, `panic@frame`, `diverged`, `match-ended-early`, …?) and whether `panic@frame`
    carries the frame number in the same column, so the worker's mapping to failure codes is
    exact rather than string-matching stderr.
18. **`boot-error` retry.** The table treats it as retryable once (environmental). Is it ever
    deterministic for a given replay or disc, in which case it should not be retried?
19. **Audio level.** `MELEE_VOLUME`'s effect on `--audio-out` is untested: confirm the raw
    audio's level is right without it, or tell the worker what to set, before production.
20. **Upstream bandwidth.** Measure it on the current desktop; it decides whether uploading
    output N while rendering N+1 is worth implementing in the worker (the API allows it).
21. **Non-NVIDIA encoders.** When the host changes, which encoder flags table entries (AMF,
    QSV, VAAPI, VideoToolbox, libx264) has the desktop actually run the pipeline with, and at what
    fps? `encoders` in the claim is informational until someone has.
22. **Linux pipeline.** Confirm the FIFO variant (`mkfifo` + `--audio-out <fifo>`) works with the
    engine's audio writer (blocking open order: the engine opens the FIFO for writing before
    ffmpeg opens it for reading, or vice versa; one side must not time out).

## 13. Broadcast (later)

Non-binding; recorded so the render worker is not designed into a corner.

The desktop runs OBS for live broadcasts. The intended shape is: a local **producer script**
drives OBS over **obs-websocket** (scene switches, source visibility, text), reading the
tournament state from the server's **public API** (`GET /v1/events/{id}`, the bracket, the
scores, and later `GET /v1/events/{id}/media`), and OBS **browser sources** show overlay pages
(scoreboard, bracket, "up next", player cards) that the **server serves** as plain public HTML,
the way it serves the bracket page today. Nothing in that needs a new signed route: the producer
reads public state, and the organiser's actions stay in the admin CLI.

Where the render worker may meet it: the same pipeline that writes an mp4 can write an SRT or
RTMP stream (`ffmpeg … -f mpegts srt://…` to the MediaMTX relay the server already runs, or
straight to OBS as a media source), so a rendered replay of a just-finished set, or a clip, can
be an OBS source during a break. That would be a fourth job kind (`stream`, with a destination
and a start time instead of an upload) or a worker-local feature with no server involvement; it
is not decided, and the queue, lease and `RenderSpec` as designed do not prevent either. The
stream relay's address, the overlay pages' URLs and the producer's protocol are out of scope
here and will get their own design when the broadcast work starts.
