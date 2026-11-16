# Render worker API (design)

Status: **draft for desktop review**, 2026-11-15. No code yet. This document extends
`docs/api.md`; everything there (signing, conventions, error bodies, limits, the rate limit)
applies to every route below unless this document says otherwise. Once agreed, the routes and
types move into `docs/api.md` and the `proto` crate with `checked` examples like the rest.

The problem: the server holds every replay of every counted game. A machine with a GPU and a
copy of the engine (today: the owner's Windows desktop) turns them into video: a VOD of each
set, and short clips of notable moments. The desktop's facts (phase2-desktop-answers.md):

- The worker is a Windows Task Scheduler job. It runs at below-normal priority, **pauses while
  the owner plays**, and may therefore sit on a job for hours.
- It renders with `melee slpcheck --video-out … --audio-out …`. `--from-frame N --to-frame M`
  has landed (branch `port/video-range`, a3778b5cf): N and M are `.slp` frame indices (the
  replay starts at -123); the simulation runs from frame 0 and the output covers `N..=M`
  inclusive, exit 0. `N <= 0` means from the start. `M` past the last frame, or `N > M`, exits 2,
  so the **server clamps every range to the game's frame count before queueing**. Seek runs
  ~560 fps headless, so a clip near the end of a 6-minute game costs ~40 s before encoding
  starts.
- Output is mp4, H.264 (NVENC) + AAC, faststart. VOD 1080p60 at ~6 Mbps (~45 MB/min), clips
  720p60 at ~4 Mbps. Full render ~155 fps (~2.5x realtime): a 4-minute set in ~1.6 minutes.

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

**Everything the output depends on is in the job.** Engine build, disc image hash, replay hashes
and the render settings are part of the job and are recorded with the output, so any output can
be reproduced (or rejected as stale when the engine changes).

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
`slpcheck` accepts (`from <= to`, `to <= last_frame`) and never has to know the game's length.
A `render_from <= 0` means "from the start" to `slpcheck` and includes the pre-game frames; the
server lets that stand rather than special-casing it. A job whose range cannot be clamped into
the game (a moment entirely past `last_frame`: a detector bug) is refused at creation (400) rather
than queued. The unclamped moment is kept (`moment_from`/`moment_to`) for the page's "at 2:14"
caption.

**Clip cost.** The engine simulates from frame 0 at ~560 fps to reach `render_from`, then encodes
at ~155 fps: a 10 s clip at the end of a 6-minute game is ~40 s of seek plus ~4 s of encode. A
clip job is therefore never "instant", and `max_running_secs` for clips (1800) allows for a
worker at below-normal priority seeking through a timeout game several times over.

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
every two seconds; the claim filter (section 5) is the real fix.

The terminal failure reasons the worker posts are free text (200 printable characters) plus a
short `code` from a fixed list so the server can count them: `engine-crash`, `engine-timeout`,
`replay-unreadable`, `encoder-unavailable` (no NVENC session, say), `disk-full`,
`output-too-large`, `build-mismatch` (the worker's engine build does not match the job and it
chose to fail rather than release), `bad-range` (`slpcheck` exited 2 on the job's
`render_from`/`render_to`: the server's clamp was wrong, so not retryable, and the organiser
sees the reason), `tool-missing` (an `analyse` job on a worker without the
analysis tool; usually a `release` with `no-build` is better), `other`.

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

A heartbeat may carry `progress` (which output, frames done, frames total) purely for the
admin listing; it is never required and never validated beyond its shape.

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
| POST | `/v1/admin/render/jobs` | organiser | `RenderJobCreateReq` | 201 `RenderJob`; 200 `RenderJob` an identical job exists | 400; 404 no such set or game; 409 set not decided, game not counted |
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
  job row, never from the request. The body is the stored upload of the slot the job names
  (`inputs[n].slot`, slot 0 unless the organiser chose otherwise); for a `Verified` game both
  slots' files are byte-identical anyway. `x-ab-sha256` repeats `inputs[n].sha256` so the worker
  can check the download without re-reading the job.
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
   same hash, if it trusts its file; the server cannot tell which side is wrong).

Open uploads expire `upload_ttl_secs` (default 86400, a day) after their last chunk; the lease
rules still apply, so in practice an upload dies with its lease unless the same worker re-claims
the job and resumes it, which it can: `UploadState` is keyed by `(job, name)`, not by attempt, so
a re-claim after a reboot continues where the chunks stopped, as long as the worker's rendered
file is the same bytes (same hash). Partial uploads live in `media_dir/incoming/{job}-{name}.part`
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

The render settings; part of the job key. Field names mirror what the worker will pass to
`slpcheck` (open question 2).

| Field | Type | |
|---|---|---|
| `preset` | `"vod"` or `"clip"` | which server preset filled the rest; informational once filled in |
| `width`, `height` | integer | 1920×1080 (vod), 1280×720 (clip) |
| `fps` | integer | 60 |
| `video_codec` | `"h264"` | the only value today |
| `video_kbps` | integer | 6000 (vod), 4000 (clip) |
| `audio_codec` | `"aac"` | |
| `audio_kbps` | integer | 160 |
| `container` | `"mp4-faststart"` | |
| `hud` | `"game"` or `"none"` | render the in-game HUD (open question 9) |

```json
{
  "preset": "vod",
  "width": 1920, "height": 1080, "fps": 60,
  "video_codec": "h264", "video_kbps": 6000,
  "audio_codec": "aac", "audio_kbps": 160,
  "container": "mp4-faststart",
  "hud": "game"
}
```

### RenderSource

What the output depends on besides the replays; part of the job key.

| Field | Type | |
|---|---|---|
| `engine_build` | string | exact build string, as `SignupReq.engine_build` (at most 64 printable characters); the worker must run this build |
| `iso_sha256` | string or null | lowercase hex SHA-256 of the disc image the server was told about (`render.iso_sha256` in config); null when unknown; the worker compares with its own (open question 3) |

### RenderInput

One replay the job reads.

| Field | Type | |
|---|---|---|
| `n` | integer | index for `GET …/replays/{n}` |
| `game_no` | integer | 1-based game number in the set |
| `game_id` | integer | the server's game id |
| `slot` | 0 or 1 | whose upload is served |
| `sha256` | string | the file's hash (from `uploads.sha256`) |
| `bytes` | integer | |
| `first_frame`, `last_frame` | integer | from the replay's metadata |

### RenderOutput

| Field | Type | |
|---|---|---|
| `name` | string | `game-{n}.mp4` for a VOD, `clip.mp4` for a clip, `analysis.json` for an analysis |
| `input` | integer | which `RenderInput.n` it comes from |
| `render_from`, `render_to` | integer or null | the frame range to encode, inclusive, already clamped to `[first_frame, last_frame]` so `slpcheck` accepts it; passed straight to `--from-frame`/`--to-frame`; null means the whole game (VOD: no range flags) |
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
| `tags` | [slot 0 tag, slot 1 tag] | for the worker's logs and the page |
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
  "source": {"engine_build": "melee (rust) 0.1.0 development build+0cd085d", "iso_sha256": "9b1c…e2f0"},
  "render": {"preset": "vod", "width": 1920, "height": 1080, "fps": 60, "video_codec": "h264",
             "video_kbps": 6000, "audio_codec": "aac", "audio_kbps": 160,
             "container": "mp4-faststart", "hud": "game"},
  "inputs": [
    {"n": 0, "game_no": 1, "game_id": 12, "slot": 0, "sha256": "75b3…6a91c", "bytes": 1848211, "first_frame": -123, "last_frame": 9876},
    {"n": 1, "game_no": 2, "game_id": 13, "slot": 0, "sha256": "0c2a…41d7", "bytes": 2010034, "first_frame": -123, "last_frame": 11240}
  ],
  "outputs": [
    {"name": "game-1.mp4", "input": 0, "render_from": null, "render_to": null, "state": "stored",
     "bytes": 74211998, "sha256": "ab12…", "report": {"engine_build": "melee (rust) 0.1.0 development build+0cd085d", "encoder": "h264_nvenc", "muxer": "ffmpeg 7.1", "frames": 10000, "duration_ms": 166667, "render_ms": 64400},
     "url": "/media/1/3/game-1.mp4"},
    {"name": "game-2.mp4", "input": 1, "render_from": null, "render_to": null, "state": "missing",
     "bytes": null, "sha256": null, "report": null, "url": null}
  ],
  "clip": null,
  "analyse": null,
  "failure": null,
  "tags": ["Mango", "Armada"],
  "created_at": 1763200000, "updated_at": 1763200600
}
```

A clip's `clip` and `outputs`:

```json
{
  "clip": {"game_no": 2, "moment_from": 4410, "moment_to": 4702, "pad_before_frames": 120, "pad_after_frames": 90, "label": "zero-to-death"},
  "outputs": [{"name": "clip.mp4", "input": 0, "render_from": 4290, "render_to": 4792, "state": "missing",
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
| `data` | object | the tool's result, split so the server can apply per-player visibility (section 8): `data.common` (any JSON: facts about the game that belong to nobody, such as the windows and stage) and `data.halves`, an array of exactly two members, `halves[0]` about the slot 0 player and `halves[1]` about the slot 1 player (any JSON each, null allowed). The server never looks inside `common` or a half |

```json
{"schema": "missed-punish/1", "tool_build": "melee-analyse 0.1.0+abc1234", "game_no": 2,
 "windows": [[1800, 2140], [4410, 4702]],
 "data": {"common": {"stage": 31}, "halves": [{"missed": [{"window": 0, "frame": 1861, "note": "…"}]}, {"missed": []}]}}
```

The server checks `schema`, `tool_build` and `data` are present, that `data.halves` is an array
of two (400 at `finish` otherwise), and that the whole file is at most
`render.max_analysis_output_bytes` (default 4 MiB). The page (or a later organiser tool)
interprets the halves by `schema`. The `common`/`halves` split is the one piece of structure the
server imposes, and it exists only so that one player's half can be shown without the other's.

### ClaimReq

| Field | Type | |
|---|---|---|
| `builds` | [string], optional | engine builds this worker can run; the server offers only jobs whose `source.engine_build` is listed. Empty or missing: any |
| `kinds` | [`"vod"`/`"clip"`/`"analyse"`], optional | default all |
| `tools` | [string], optional | analysis tools this worker has (`"melee-analyse"`); `analyse` jobs are offered only when their `tool` is listed. Empty or missing: no `analyse` jobs |
| `worker` | string or null, optional | a label for the admin listing (at most 64 printable characters), e.g. the machine name |

```json
{"builds": ["melee (rust) 0.1.0 development build+0cd085d"], "kinds": ["vod", "clip", "analyse"], "tools": ["melee-analyse"], "worker": "GD-PC"}
```

### Heartbeat and Lease

```json
{"state": "running", "progress": {"output": "game-2.mp4", "frames_done": 3200, "frames_total": 11363}}
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
| `encoder` | string | e.g. `"h264_nvenc"`, `"libx264"` |
| `muxer` | string or null | e.g. `"ffmpeg 7.1"` |
| `frames` | integer | frames encoded (0 for an analysis) |
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
| Leases per worker key | `render.max_jobs_per_worker`, default 1 (one NVENC session, one GPU; see open question 5) |
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
engine_build = "melee (rust) 0.1.0 development build+0cd085d"   # what new jobs ask for
iso_sha256 = "9b1c…e2f0"                                        # optional; null when absent
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

What `autobracket render-worker --server … --key-file … --engine … --iso … --scratch …` does,
in the style of `verify_worker.rs`:

1. At start: read the key; compute the engine build string (`melee --version`) and, once, the
   ISO's SHA-256 (cache it next to the ISO keyed by size and mtime); warn on plain `http://` to a
   remote host as `verify-worker` does.
2. **Claim** with `builds: [own build]`, `kinds` from flags, `tools` with `"melee-analyse"`
   when that binary is found on the path (`--analyse-tool`), `worker` = host name. 204: sleep
   `IDLE_SLEEP` (2 s; the desktop may prefer 10-30 s since jobs are hours apart, open
   question 11). Error: sleep `ERROR_SLEEP` (5 s), retry.
3. Start a heartbeat task at `lease_secs / 4` that reports `running`, or `paused` when the pause
   predicate holds (the owner's game is running: open question 4). On 409 from a heartbeat,
   cancel the render, discard scratch, go to 2.
4. For each output whose `state` is not `stored`, in order: download its input
   (`GET …/replays/{n}`, verify `sha256`, keep it in scratch for the next output of the same
   input); if `source.iso_sha256` is set and differs from the local hash, `release` with
   `no-build` and go to 2; run the engine for the output's frame range (for an `analyse` job:
   run `melee analyse --slp F --frames …` with the job's `windows` and `params`, and write its
   JSON output as `analysis.json`); `begin` the upload with
   the file's size and hash (200 with ranges already received means resume); `PUT` the missing
   chunks (2 in parallel); `finish` with the `RenderReport`. On a `sha256 mismatch` at finish,
   `begin` again once; a second mismatch is `failed` with `other`, not retryable.
5. When every output is stored, `result ok`. On an engine failure, `result failed` with a code
   and `retryable` (true for crashes and timeouts, false for an unreadable replay or a
   `build-mismatch`). On 409 from `result`, log and go to 2.
6. On a stop request from Task Scheduler (`CTRL_SHUTDOWN_EVENT`/service stop), `release` the
   current job with `shutdown` before exiting if the server answers within 5 s; otherwise the
   lease simply expires.

The worker keeps no state between runs except its scratch directory, which it may empty at
start. Everything it needs to resume is in the `RenderJob` and `UploadState` answers.

## 12. Open questions for the desktop

1. **Frame numbering: settled.** `--from-frame N --to-frame M` are `.slp` frame indices, replay
   starts at -123, `N..=M` inclusive, `N <= 0` is "from the start", `M > last` or `N > M` exits 2
   (a3778b5cf). The server clamps as section 2 says. Remaining detail: is `last_frame` as the
   server reads it from the replay (the last frame with a post-frame update) the same `last`
   that `slpcheck` checks `M` against, or is there an off-by-one at the game-end frame? If in
   doubt the server clamps to `last_frame - 1`.
2. **Render settings on the command line.** Does `slpcheck --video-out` take resolution, fps,
   video/audio bitrate and encoder as arguments, or presets? Does `--audio-out` write a separate
   file that the worker then muxes with ffmpeg (so ffmpeg is a worker dependency and `muxer` in
   the report is meaningful), or does the engine mux? `RenderSpec`'s fields are a guess at the
   knobs; tell me which exist and which are fixed, and anything else that changes output bytes.
3. **ISO hash.** Is it acceptable for the worker to SHA-256 the disc once (1.4 GB, a few
   seconds, cached by size and mtime), so the job's `iso_sha256` can be enforced? Or is the
   engine build alone enough to reproduce output, with the ISO only needing to be *a* valid
   image? If the latter, `iso_sha256` becomes informational.
4. **Pause granularity.** Can the worker pause mid-render (suspend the engine process / stop
   feeding frames) and resume, or does it only pause between outputs? If it aborts the current
   game on pause and re-renders it later, `max_running_secs` should be counted per output, not
   per attempt, and the desktop's render time per game is the number that matters. Also: is an
   8-hour maximum pause per attempt acceptable, and what is the pause predicate (Dolphin/engine
   process present? user session active? Task Scheduler condition)?
5. **Concurrency.** Is one job at a time right (one NVENC session at below-normal priority)? If
   two are fine, `max_jobs_per_worker = 2` and the worker runs two loops with one heartbeat
   task each.
6. **Progress.** Does `slpcheck` print frame progress on stdout/stderr in a parseable form, so
   the heartbeat's `progress` can be filled? Optional; nice for the admin listing.
7. **Which upload to render.** For a `Verified` game both slots' files are identical. For a
   `VerifiedInexact` or `PeerOnly` game they are the two players' own recordings and may differ
   in trivia (metadata, player names). Is slot 0 always fine, or should the server prefer the
   file the verifier re-simulated? Does the rendered camera/HUD depend on which "local" player
   the file was recorded by?
8. **Clip detection location.** Confirm the server creates clip jobs (the replay crate's
   detector runs server-side at set end); the worker never decides what to clip. Detection
   thresholds and the `label` vocabulary (`combo`, `kill`, `edgeguard-kill`, `comeback`,
   `zero-to-death`) are then the replay crate's, not this API's.
9. **HUD and presentation.** Should the VOD render the in-game HUD only (`hud: "game"`), or does
   the engine support overlays (tags, score, round name) that the job should carry? The job has
   `tags`; if the engine can draw them, say what it wants. Clip fades or title cards: engine,
   worker (ffmpeg), or not at all?
10. **Windows account and disk.** Under which account does the Task Scheduler job run, where do
    the key file and scratch live, and how much scratch may a job use? The design assumes up to
    the job's total output size plus the replays (a long set at 1080p60: ~1-2 GB) in scratch, and
    one job at a time.
11. **Poll interval and bandwidth.** Is a 2 s idle poll fine, or would you rather 15-30 s (jobs
    arrive once per set, minutes apart)? What is the desktop's upstream bandwidth? That sets
    whether uploading game N while rendering game N+1 is worth doing (the API allows it: the
    upload of one output and the render of the next are independent) and whether a 1 GiB VOD
    cap is sensible (at 20 Mbps up, 1 GiB is ~7 minutes).
12. **Resume across restarts.** Will the worker keep its rendered mp4 in scratch across a Task
    Scheduler restart, so an interrupted upload resumes (the server keeps the received ranges
    for a day and the hash must match)? If scratch is wiped at every start, resume costs a
    re-render, which is fine but should be known.
13. **Build string.** Is `melee --version` (or the equivalent) the exact string that the server's
    verifier and the players' `SignupReq.engine_build` use, byte for byte? The claim filter and
    `RenderReport.engine_build` compare strings exactly.
14. **Failure codes.** Do the proposed `code` values (`engine-crash`, `engine-timeout`,
    `replay-unreadable`, `encoder-unavailable`, `disk-full`, `output-too-large`,
    `build-mismatch`, `bad-range`, `tool-missing`, `other`) cover what the engine actually
    reports (exit 2 is `bad-range`; what are the other exit codes?), and which of them does the
    desktop consider retryable?
15. **`analyse` jobs.** (a) Is one game per job with a list of frame windows the right unit, or
    will `melee analyse` want the whole set (both players' context across games)? (b) Does it
    need the ISO and engine (a headless simulation, like a clip render) or only the `.slp`, so
    `source` is irrelevant and a machine without a GPU could run it? (c) Is the output a single
    JSON document per game, and what size should the cap allow (4 MiB proposed)? (d) How is
    `--frames` spelled (a list of `from-to` pairs?) and how are `params` passed? (e) **Resolved
    by the owner (2026-11-15):** private to the set's two players and organisers by default; a
    per-player "share my analysis publicly" opt-in (both must opt in for the whole game's
    analysis to be public, otherwise only each opted-in player's own half) and a per-event
    organiser override forcing private; the route is gated and answers 404, never 403, to anyone
    else. See section 8, "Analysis visibility". What this asks of the tool: the output's `data`
    must be split into `common` and two `halves` (one per slot) so the server can serve one
    player's half without the other's. Is that split natural for `melee analyse`?
