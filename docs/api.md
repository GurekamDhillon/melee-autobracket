# autobracket HTTP API

This is the complete contract between an autobracket server and the programs that talk to it:
the reference client (`autobracket-client`), the admin CLI (`autobracket admin`), the external
verifier (`autobracket verify-worker`), and any client written elsewhere. Everything a request
or response carries is listed here. The JSON shapes are those of the `proto` crate
(`crates/proto/src/types.rs`, `crates/proto/src/ruleset.rs`); every JSON example below marked
`checked` is decoded into that type and re-encoded by a test (`crates/proto/tests/api_doc.rs`),
so the examples cannot drift from the code. The worked signing example is checked by the same
test.

Contents:
1. [Conventions](#1-conventions)
2. [Signing a request](#2-signing-a-request)
3. [Limits](#3-limits)
4. [Routes](#4-routes)
5. [Types](#5-types)
6. [What a player's client does](#6-what-a-players-client-does)
7. [Round names](#7-round-names)

## 1. Conventions

- **Transport.** The server speaks plain HTTP/1.1. Run it behind a TLS reverse proxy for any
  public deployment: room codes and invite codes cross the wire, and `Next` (relay address,
  room code, stocks, timer) feeds the game's command line. Behind a proxy every request arrives
  from the proxy's address, so the server is configured to trust that one address and nothing
  wider, for example `trusted_proxies = ["10.203.47.1/32"]`, the pinned gateway of
  `deploy/docker-compose.yml` (see [Limits](#3-limits)).
- **Bodies.** Request and response bodies are JSON (UTF-8), except a replay upload's request
  body and a replay download's response body, which are the raw `.slp` bytes
  (`application/octet-stream`). The server does not look at `content-type` on requests.
- **Paths.** Ids in paths are decimal integers (`{event}`, `{set}`, `{id}`, `{n}`, `{slot}`);
  `{old_key}` is 64 lowercase hex characters; `{tag}` is a player tag, matched in any case
  (tags are unique regardless of case).
- **Set ids** are the bracket's own set ids, starting at 0. **Slots** are 0 (the host, the
  bracket's upper slot) and 1 (the guest). `wins` and `scores` are `[slot 0, slot 1]` unless
  stated otherwise.
- **Times** are unix seconds.
- **Errors.** Every error response, from any route, has a JSON body
  `{"error": "<human-readable message>"}`, sometimes with more keys (sign-up's 403 adds
  `"accepted"`). This includes signature failures, unknown routes (404), wrong methods (405),
  malformed paths and queries (400), oversized bodies (413), body timeouts (408) and the rate
  limit (429). Messages are for people; act on the status code. The exception is an error the
  HTTP layer produces before a request is parsed at all (a malformed request line, oversized
  headers, or headers that do not arrive in time): that answer is not JSON, or the connection
  is simply closed.

| Status | Meaning |
|---|---|
| 400 | The request is malformed: body, path, query, or a value out of range. Do not retry unchanged. |
| 401 | The signature headers are missing or malformed, the clock is off, the signature is wrong, or the nonce was used. |
| 403 | Signed correctly, but the key may not do this (not registered, not the organiser, not in this set, not the host). |
| 404 | No such event, set, queue item, job or replay; or no such route. |
| 405 | The route exists but not with this method. |
| 408 | The request body did not arrive in time. |
| 409 | The request is valid but the state does not allow it now (see each route). |
| 413 | The body is larger than the route accepts. |
| 429 | Rate limited. `retry-after` (and `x-ratelimit-after`) give the seconds to wait. |
| 500 | Server fault; the cause is in the server's log. Retrying later is safe. |

## 2. Signing a request

There are no accounts or passwords. A participant is an Ed25519 key pair; the public key is
their identity. Every route except the public ones (`GET /`, `GET /events/{id}`,
`GET /schedule`, `GET /history`, `GET /players`, `GET /players/{tag}`, `GET /assets/fonts/{file}`, `GET /v1/events`, `GET /v1/events/{id}`, `GET /v1/history`,
`GET /v1/players`, `GET /v1/players/{tag}`, `GET /live/{event}/{set}`, `GET /v1/live`, `GET /v1/live/{event}/{set}`) must be signed with four headers:

| Header | Value |
|---|---|
| `x-ab-key` | The signer's 32-byte Ed25519 public key, 64 hex characters (lowercase recommended). |
| `x-ab-time` | The signer's clock, unix seconds, decimal. |
| `x-ab-nonce` | 16 random bytes, 32 hex characters, fresh for every request. |
| `x-ab-sig` | The 64-byte Ed25519 signature of the canonical string, 128 hex characters. |

**The canonical string** is five lines joined by a single `\n` (byte 0x0A), with no trailing
newline:

```text
METHOD
PATH_AND_QUERY
TIME
NONCE
BODY_SHA256
```

- `METHOD` is the request method in upper case (`GET`, `POST`, `DELETE`).
- `PATH_AND_QUERY` is the request target exactly as sent in the request line: the path and,
  if present, `?` and the query string, byte for byte as the client sends them (no scheme, no
  host, no normalisation). `/v1/me/next?event=1` signed and `/v1/me/next?event=2` sent is a 401.
- `TIME` and `NONCE` are the header values, exactly as sent.
- `BODY_SHA256` is the lowercase hex SHA-256 of the request body bytes; for an empty body it is
  `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.

The signature is plain Ed25519 (RFC 8032, no pre-hashing, no context) over the canonical
string's bytes. The server verifies it strictly (`verify_strict`: no small-order keys, no
non-canonical signatures).

**Clock skew.** `x-ab-time` must be within 120 seconds of the server's clock, in either
direction. Otherwise the answer is 401
`{"error":"clock skew: your clock differs from the server's by more than 120 seconds"}`.

**Nonces.** The server remembers each `(key, nonce)` it accepted for 300 seconds, which is
longer than any signed time stays acceptable (240 seconds), so a captured request cannot be
replayed: the same nonce again is 401 `{"error":"nonce already used"}`. A nonce is recorded
only once the signature has verified. Use a fresh random nonce for every request, including
retries (a retry is a new request).

**Order of checks.** The server checks, in this order, and answers with the first failure:

1. All four headers are present as visible ASCII (else 401 `missing signature headers`; a
   header value with any other byte counts as missing) and well formed:
   hex of the right lengths, the key a valid Ed25519 point, the time an integer (else 401
   `malformed signature headers`).
2. The time is within the skew window (else 401 `clock skew: …`).
3. The key holds the role the route needs (else 403): a registered player for player routes
   (`key is not registered`), a key in the server's `to_keys` for organiser routes
   (`not a tournament organiser`), a key in `verifier_keys` or `to_keys` for verifier routes
   (`not a verifier`). Registration (`POST /v1/players`) accepts any key.
4. The body: a declared `content-length` over the route's limit is 413 at once; otherwise the
   body is read, up to the limit (else 413 `request body too large`) and within the time
   limit (else 408 `request body not received in time`).
5. The signature, over the full body (else 401 `invalid signature`).
6. The nonce (else 401 `nonce already used`).

Steps 1-3 happen before any of the body is read, so a request from a key that may not use the
route costs the server nothing but its headers.

**Worked example.** The secret key below is the RFC 8032 test key 1; never use it for
anything real. Signing this registration request:

<!-- signing-example -->
```text
secret key   9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
method       POST
path         /v1/players
time         1763200000
nonce        000102030405060708090a0b0c0d0e0f
body         {"tag":"Mango","invite":null}
body sha256  75b3ccf624edbf047501905d78082bcf831e3d40a78c50112916d6d64db6a91c
x-ab-key     d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
x-ab-sig     d5e64034507c78a9938b1fe15b48130f5e3716eadc4eb23f5562f9ea2bafa5e4dd0f4e7ab07296bc552409902e7f4c7debafee185cad08e016c1acb0c1ff5b02
```

The canonical string signed (five lines, no trailing newline):

<!-- signing-canonical -->
```text
POST
/v1/players
1763200000
000102030405060708090a0b0c0d0e0f
75b3ccf624edbf047501905d78082bcf831e3d40a78c50112916d6d64db6a91c
```

The request on the wire (the body is the 29 bytes shown, no trailing newline):

```http
POST /v1/players HTTP/1.1
host: bracket.example.org
content-type: application/json
content-length: 29
x-ab-key: d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
x-ab-time: 1763200000
x-ab-nonce: 000102030405060708090a0b0c0d0e0f
x-ab-sig: d5e64034507c78a9938b1fe15b48130f5e3716eadc4eb23f5562f9ea2bafa5e4dd0f4e7ab07296bc552409902e7f4c7debafee185cad08e016c1acb0c1ff5b02

{"tag":"Mango","invite":null}
```

(A real server would refuse this exact request today as clock skew: its time is in 2025.)

## 3. Limits

| What | Limit |
|---|---|
| Request body, every signed route except replay upload | 64 KiB (65536 bytes) |
| Request body, replay upload | `max_replay_bytes` (default 16 MiB, 16777216 bytes) |
| Time for the request body to arrive | 30 s; 120 s for a replay upload (else 408) |
| Time for the request line and headers to arrive | 30 s, after which the connection is closed |
| Idle keep-alive connection | closed after 30 s without a new request |
| Connections served at once | `max_connections` (default 512); more wait to be accepted |
| Requests per client | `rate_per_sec` sustained (default 20), `rate_burst` in a burst (default 40), on every route including the public ones; over it, 429 |
| Clock skew | 120 s either way |
| Nonce memory | 300 s |

**Who counts as one client for the rate limit.** The TCP peer's address, except when the peer
is inside one of the server's `trusted_proxies` CIDRs: then the client is the right-most
address in `X-Forwarded-For` that is not itself a trusted proxy (each proxy appends the address
it received the request from, so anything further left is the client's own claim and is
ignored). Without `trusted_proxies`, `X-Forwarded-For` is ignored entirely, so a client cannot
escape its bucket by sending one. IPv6 clients are counted per /64 (one subscriber usually
holds a whole /64). Behind a reverse proxy, set `trusted_proxies` to the single address the
proxy's connections arrive from, otherwise every client shares the proxy's single bucket: for
example `trusted_proxies = ["10.203.47.1/32"]` (the pinned gateway of
`deploy/docker-compose.yml`, with the proxy on the host) and in nginx
`proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;`. Never put a range in
`trusted_proxies` that contains any client's address: every trusted entry is skipped in the
walk from the right, so such a client gets the entry to its left believed, which is one it
wrote itself. The README's "TLS and reverse proxies" section gives the reasoning and the
address to trust in each setup.

A client polling `GET /v1/me/next` every 2 seconds uses 0.5 requests per second.

**Text fields.**

| Field | Rule |
|---|---|
| tag | 2-16 characters of `A-Z a-z 0-9 _`; unique regardless of case (`Mango` and `MANGO` are one tag) |
| `engine_build`, `x-ab-build` | at most 64 printable characters |
| event name | at most 80 printable characters |
| event `description` | at most 280 printable characters; empty or whitespace is stored as null |
| event `starts_at` | unix seconds, 0 to 4102444800 |
| no-show `detail` | at most 200 printable characters |
| room code | 4 characters of `A-Z 0-9` (a relay room); or, only where the server sets `allow_direct_connect`, `HOST:PORT` (host 1-253 characters of `A-Z a-z 0-9 . -`, port 1-65535) |

"Printable" excludes control characters and the Unicode line and paragraph separators.

## 4. Routes

Who may call: **public** (no signature), **any key** (any valid signature), **player** (a
registered key), **organiser** (a key in `to_keys`), **verifier** (a key in `verifier_keys` or
`to_keys`). Every signed route can also answer 401 (signature), 403 (role), 408, 413 and 429 as
described above; the tables list the other statuses.

| Method | Path | Who | Request body | Success | Other statuses |
|---|---|---|---|---|---|
| GET | `/` | public | — | 200 HTML landing page (live now, next up, latest results) | — |
| GET | `/events/{id}` | public | — | 200 HTML bracket page; `{id}` is decimal digits as written | 400 (a sign, an escape or anything else), 404 |
| GET | `/live/{event}/{set}` | public | — | 200 HTML live view of the set's current game (display only) | 400 |
| GET | `/stream` | public | — | 200 HTML stream page | — |
| GET | `/schedule` | public | — | 200 HTML schedule page | — |
| GET | `/history` | public | — | 200 HTML history page | — |
| GET | `/players` | public | — | 200 HTML players index | — |
| GET | `/players/{tag}` | public | — | 200 HTML player profile; `{tag}` matches in any case | 404 no such player |
| GET | `/assets/fonts/{file}` | public | — | 200 `font/woff2`, one of the page's fonts | 404 |
| GET | `/v1/stream` | public | — | 200 `StreamInfo` | — |
| GET | `/v1/events` | public | — | 200 `[EventSummary]` | — |
| GET | `/v1/events/{id}` | public | — | 200 `EventView` | 400, 404 |
| GET | `/v1/history` | public | — | 200 `[EventResult]`, complete events, newest first; `?limit=N` (1–100) keeps the newest N, default every one | 400 `limit` outside 1–100 |
| GET | `/v1/players` | public | — | 200 `[PlayerSummary]`, every registered player, by tag ignoring case | — |
| GET | `/v1/players/{tag}` | public | — | 200 `PlayerProfile`; `{tag}` matches in any case | 404 no such player |
| POST | `/v1/players` | any key | `RegisterReq` | 201, empty | 400 tag or body; 403 invite missing, unknown or used; 409 key already registered, tag taken |
| POST | `/v1/players/slippi` | player | `SlippiCodeReq` | 204, empty | 400 code not shaped like `ABCD#123`; 409 another player has that code |
| POST | `/v1/events/{id}/entrants` | player | `SignupReq` | 201, empty | 400; 403 `{"error":"engine build not accepted","accepted":[…]}`; 404; 409 not in sign-up, already entered, event full (1024) |
| GET | `/v1/me/next?event={id}` | player | — | 200 `Next` | 400; 404 no such event |
| POST | `/v1/sets/{event}/{set}/room` | player, host (slot 0) of the set | `RoomReq` | 204, empty | 400 bad code, or a direct-connect code where not allowed; 403 not in the set, or not the host; 404 no such set; 409 set not `ready`, or a game of the set is uploading (with an upload) or being judged |
| POST | `/v1/sets/{event}/{set}/no-show` | player of the set | `NoShowReq`, or empty (a `no-show` with no note) | 204, empty | 400; 403; 404; 409 set not `ready` (or already in review), a game is uploading or being judged, or (kind `no-show` only) the room code was posted less than `no_show_grace_secs` ago |
| POST | `/v1/sets/{event}/{set}/games/{n}/replay` | player of the set | the `.slp` file's bytes; header `x-ab-build` optional | 202 stored; 200 this exact file was already stored for this slot | 400 not a readable replay, bad `x-ab-build`; 403; 404; 409 `n` is not the current game, a different file is stored for this slot, the game no longer takes uploads, the set is not in play; 413 |
| GET | `/v1/sets/{event}/{set}/games/{n}/live` | player, host (slot 0) of the set | — (a WebSocket upgrade, signed with an empty body) | 101, the live relay ingest | 403 not the host; 404 no such set, or `live_relay` off; 409 set not `ready`, or `n` not the current game |
| GET | `/v1/live` | public | — (`?event={id}` optional) | 200 `[LiveInfo]` | — |
| GET | `/v1/live/{event}/{set}` | public | — (a WebSocket upgrade) | 101, the set's live messages | 404 nothing streamed for the set, or `live_relay` off; 503 too many viewers (64 per set) |
| POST | `/v1/admin/events` | organiser | `EventCreateReq` | 201 `EventSummary` | 400 |
| POST | `/v1/admin/events/{id}/start` | organiser | ignored (send `{}`) | 200 `EventView` | 404; 409 already started, fewer than 2 entrants |
| POST | `/v1/admin/events/{id}/schedule` | organiser | `EventScheduleReq` | 204, empty; replaces both fields, an omitted or null field clears it | 400; 404 |
| DELETE | `/v1/admin/events/{id}/entrants/{tag}` | organiser | — | 204, empty; only while the event is in sign-up; later seeds shift down; `{tag}` matches in any case | 400 bad path; 404 no such event, or no such entrant; 409 event not in sign-up |
| POST | `/v1/admin/invites` | organiser | ignored (send `{}`) | 201 `{"code":"<12 characters A-Z 2-7>"}` | — |
| GET | `/v1/admin/queue` | organiser | — | 200 `[QueueItem]`, open items, oldest first | — |
| POST | `/v1/admin/queue/{id}/resolve` | organiser | `Resolve` | 204, empty | 400 bad slot; 404; 409 already resolved, set already decided, no entrant in that slot |
| POST | `/v1/admin/players/{old_key}/rekey` | organiser | `RekeyReq` | 204, empty | 400; 404 no such player; 409 new key already registered |
| GET | `/v1/quarantine` | organiser | — | 200 `[QuarantineItem]`, oldest first | — |
| GET | `/v1/quarantine/{id}/replay/{slot}` | organiser | — | 200 the `.slp` bytes | 400; 404 no such replay, file missing |
| POST | `/v1/verify/claim` | verifier | ignored | 200 `VerifyJob`; 204 none waiting | — |
| GET | `/v1/verify/jobs/{id}/replay` | verifier | — | 200 the `.slp` bytes | 400; 404 |
| POST | `/v1/verify/jobs/{id}/result` | verifier | `VerifyResult` | 204, empty | 400; 404; 409 job not claimed |

Notes on individual routes:

- **`GET /v1/me/next`** answers only about the signer, and gives the room code only to the two
  players of the set. See [Next](#next) and [section 6](#6-what-a-players-client-does).
- **Room code.** Posting a code replaces any earlier one; the first upload of a game clears it
  (so a guest never joins a stale room), as does the set going to review.
- **Replay upload.** The body is the whole `.slp` file. `x-ab-build` is the uploader's engine
  build string, kept with the upload for the organiser; it is not signed and not trusted. An
  identical re-upload answers 200, so retrying an upload whose answer was lost is always safe.
  A game is judged once both players' files are in, or `upload_timeout_secs` (default 120)
  after the first one. Uploads are accepted while the set is `ready`, and also while it is in
  review for no other reason than `no-show`, `stalled` or `client-failure`: the game was perhaps
  played after all, so its replays are taken and judged as evidence for the organiser, but they
  never count (the server stores the verdict and voids the game). One such game per review;
  once it has been judged, further files for it are 409.
- **No-show.** `kind` is `"no-show"` (the opponent never joined; the default) or
  `"client-failure"` (the reporter's own client could not complete a game). The set goes to
  the organiser with that reason and to review. A `no-show` is refused for
  `no_show_grace_secs` (default 90) after a room code is posted, because the guest may still be
  joining; `client-failure` has no grace period.
- **Resolve.** `Award` gives the set to `slot`; `Replay` reopens the set at the same game number
  (wins unchanged); `Dq` disqualifies the entrant in `slot` from the event (every set they are
  or will be in is awarded to the opponent).
- **Verifier routes** are for `autobracket verify-worker` (external verifier mode). A claim
  not answered within 300 s is offered again.
- **Admin CLI commands** that use these routes include `autobracket admin quarantine get ID
  SLOT --out FILE` (it downloads `GET /v1/quarantine/{id}/replay/{slot}`).

- <a id="live-relay"></a>**Live relay** (display only; `docs/design/live-relay.md` is the wire
  format). During a game the host's engine opens the ingest WebSocket at the `live_path` from
  `Next` and streams the game's Slippi event bytes as finalised. The server checks the signature
  and the four refusals above before switching protocols, answers `connect_request` with
  `connect_reply` (the cursor after the last message it stored), records skipped cursors as gaps,
  and closes an older connection when a newer one for the set arrives. Viewers (the bracket
  page's `/live/{event}/{set}`) dial `GET /v1/live/{event}/{set}` and receive the current game's
  head (everything before its first frame), its last 600 messages, then every message as it
  arrives: the ingest's `start_game`, `game_event` (`payload`, base64 event bytes) and `end_game`
  objects, each with `cursor` and `game_no`. Nothing in the live stream is ever used for a
  verdict, a result or a dispute; it is held in memory only. `LiveInfo`: `event_id`, `set_id`,
  `game_no`, `ended`, `started_at`, `messages`, `event_bytes`, `gaps`, `viewers`.

## 5. Types

Enums are encoded the serde way: a variant without data is a JSON string (`"Waiting"`); a
variant with data is an object with one key, the variant name (`{"Review":{"set_id":3}}`,
`{"Illegal":"banned stage"}`). Struct fields are always present in responses; `null` means
absent. In requests, a field marked optional may be left out.

### RegisterReq

| Field | Type | |
|---|---|---|
| `tag` | string | see [Limits](#3-limits) |
| `invite` | string or null | required when the server sets `require_invite` |

<!-- checked: RegisterReq -->
```json
{"tag": "Mango", "invite": "ABCDEFGHJKLM"}
```

### SlippiCodeReq

Sets (or changes) the Slippi connect code of the signing key's player. The server stores it
upper case with an ASCII `#`; 1 to 8 letters or digits, `#`, 1 to 6 digits. A code belongs to
one player on the whole server, so it is unique within every event too. In an event with
`any_client` on, the judge identifies the two players in a replay by this code instead of by
port (README, "Any client mode"). Without one, that event falls back to the port rule.

| Field | Type | |
|---|---|---|
| `code` | string | `ABCD#123` |

<!-- checked: SlippiCodeReq -->
```json
{"code": "MANG#0"}
```

### SignupReq

| Field | Type | |
|---|---|---|
| `engine_build` | string | the player's engine build; refused (403) unless in the server's `accepted_builds`, when that list is not empty |

<!-- checked: SignupReq -->
```json
{"engine_build": "melee (rust) 0.1.0 development build+339fd9aabad8"}
```

### RoomReq

<!-- checked: RoomReq -->
```json
{"code": "KQ7X"}
```

### NoShowReq

| Field | Type | |
|---|---|---|
| `detail` | string or null, optional | a note for the organiser |
| `kind` | `"no-show"` or `"client-failure"`, optional | default `"no-show"` |

<!-- checked: NoShowReq -->
```json
{"detail": "nobody joined room KQ7X", "kind": "no-show"}
```

<!-- checked: NoShowReq -->
```json
{"detail": null, "kind": "client-failure"}
```

### EventCreateReq

| Field | Type | |
|---|---|---|
| `name` | string | at most 80 printable characters |
| `format` | `"SingleElim"` or `"DoubleElim"` | |
| `ruleset` | `Ruleset` or null | null: the server's configured ruleset |
| `best_of` | integer or null, optional | overrides the ruleset's `best_of`; odd |
| `starts_at` | integer or null, optional | when the event starts, unix seconds (see [Limits](#3-limits)) |
| `description` | string or null, optional | what the event is about, at most 280 printable characters; empty or whitespace counts as null |

<!-- checked: EventCreateReq -->
```json
{"name": "Open", "format": "SingleElim", "ruleset": {"stocks": 4, "timer_secs": 480, "best_of": 3, "best_of_finals": 5, "legal_stages": [31], "banned_characters": [], "allowed_gecko": [], "any_client": true}, "best_of": null, "starts_at": null, "description": null}
```

<!-- checked: EventCreateReq -->
```json
{
  "name": "Weekly #12",
  "format": "DoubleElim",
  "ruleset": null,
  "best_of": 3,
  "starts_at": 1795000000,
  "description": "Bo3, Friday 8pm"
}
```

### EventScheduleReq

Sets when an event starts and what it is about, in any state. Both fields are replaced by the
request: an omitted or null field clears it, as does an empty or all-whitespace description.

| Field | Type | |
|---|---|---|
| `starts_at` | integer or null, optional | unix seconds, 0 to 4102444800 |
| `description` | string or null, optional | at most 280 printable characters |

<!-- checked: EventScheduleReq -->
```json
{"starts_at": 1795000000, "description": "Bo3, Friday 8pm"}
```

### Ruleset

| Field | Type | |
|---|---|---|
| `stocks` | integer | stocks per player |
| `timer_secs` | integer | game timer |
| `best_of` | odd integer | games per set |
| `best_of_finals` | odd integer | games per grand-final set (and reset) |
| `legal_stages` | [integer] | stage ids as Game Start records them |
| `banned_characters` | [integer] | external character ids |
| `allowed_gecko` | [string] | permitted Gecko code lists, lowercase hex SHA-256 |
| `any_client` | boolean | any-client mode (default false; README, "Any client mode"): uploads agree on their derived outcome instead of identical `raw` bytes, and a diverging re-simulation is `UnverifiedForeign`; left out of responses when false |

In a request every field is optional (missing ones take the defaults shown here).

<!-- checked: Ruleset -->
```json
{
  "stocks": 4,
  "timer_secs": 480,
  "best_of": 3,
  "best_of_finals": 5,
  "legal_stages": [2, 3, 8, 28, 31, 32],
  "banned_characters": [],
  "allowed_gecko": []
}
```

### EventSummary

`state` is `"signup"`, `"running"` or `"complete"`. `starts_at` is when the event starts
(unix seconds) and `description` what it is about, both as the organiser set them or null;
`created_at` is when the event was created (unix seconds).

<!-- checked: EventSummary -->
```json
{
  "id": 1,
  "name": "Weekly #12",
  "state": "running",
  "entrants": 2,
  "starts_at": 1795000000,
  "description": "Bo3, Friday 8pm",
  "created_at": 1763200000
}
```

### StreamInfo

The organiser's `stream_url` as configured, or `null` when no stream is configured. The
`/stream` page embeds it; that page's CSP allows framing the URL's origin and nothing else,
and no page's HTML carries the URL (it reaches the page through this route).

<!-- checked: StreamInfo -->
```json
{"url": "https://live.example.org/melee/"}
```

### EventView

| Field | Type | |
|---|---|---|
| `summary` | `EventSummary` | |
| `ruleset` | `Ruleset` | the event's rules |
| `tags` | [string] | entrants' tags in seed order; an entrant id is an index into this list |
| `bracket` | `Bracket` or null | null until the event starts |
| `scores` | [[set id, [wins slot 0, wins slot 1]]] | one entry per set that has become ready (is being played or is decided) |
| `decided_by` | [[set id, how]] | sets decided other than by played games: `"award"`, `"dq"` or `"double-dq"`; their scores count only the games played before |
| `unverified_games` | integer | counted games of the event whose re-simulation outcome is `Unavailable`: a verifier is configured but was unavailable, so they counted on peer agreement alone (`PeerOnly`) |

`Bracket`: `format`; `entrants` (count); `sets`, each with `id`, `side` (`"Winners"`,
`"Losers"` or `"GrandFinal"`), `round`, `slots` (two of `"Empty"`, `"Bye"` or
`{"Entrant": id}`), `winner_to` and `loser_to` (`[set id, slot]` or null), and `state`
(`"Waiting"`, `"Ready"`, `{"Done":{"winner": slot}}` or `"Skipped"`).

<!-- checked: EventView -->
```json
{
  "summary": {
    "id": 1,
    "name": "Weekly #12",
    "state": "running",
    "entrants": 2,
    "starts_at": 1795000000,
    "description": "Bo3, Friday 8pm",
    "created_at": 1763200000
  },
  "ruleset": {
    "stocks": 4,
    "timer_secs": 480,
    "best_of": 3,
    "best_of_finals": 5,
    "legal_stages": [2, 3, 8, 28, 31, 32],
    "banned_characters": [],
    "allowed_gecko": []
  },
  "tags": ["Mango", "Armada"],
  "bracket": {
    "format": "SingleElim",
    "entrants": 2,
    "sets": [
      {
        "id": 0,
        "side": "Winners",
        "round": 1,
        "slots": [{"Entrant": 0}, {"Entrant": 1}],
        "winner_to": null,
        "loser_to": null,
        "state": "Ready"
      }
    ]
  },
  "scores": [[0, [1, 0]]],
  "decided_by": [],
  "unverified_games": 0
}
```

### Next

What the signing player should do next in the event (`GET /v1/me/next`):

| Variant | Meaning |
|---|---|
| `"NotEntered"` | the player is not an entrant |
| `"Waiting"` | sign-up is still open, or the player's next set waits for another set to finish |
| `{"Review": {"set_id": N}}` | the player's set is with the organiser; wait (see [section 6](#6-what-a-players-client-does)) |
| `"Eliminated"` | out of the event |
| `"Champion"` | won the event |
| `{"Play": {...}}` | play game `game_no` of set `set_id` |

`Play` fields: `set_id`; `opponent_tag`; `role` (`"Host"` for slot 0, `"Guest"` for slot 1);
`relay` (the netplay relay address, `host:port`); `room_code` (null until the host posts one
for this game); `game_no` (1-based); `wins` (**`[me, opponent]`**); `best_of`; `stocks`;
`timer_secs`; `awaiting_verdict` (my replay of `game_no` is in and not yet judged);
`live_path` (for the host only, when the server has `live_relay` on: the path of this game's
live relay ingest, see [Live relay](#live-relay); null otherwise; older servers leave it out).

<!-- checked: Next -->
```json
{
  "Play": {
    "set_id": 0,
    "opponent_tag": "Armada",
    "role": "Host",
    "relay": "relay.example.org:51600",
    "room_code": "KQ7X",
    "game_no": 2,
    "wins": [1, 0],
    "best_of": 3,
    "stocks": 4,
    "timer_secs": 480,
    "awaiting_verdict": false,
    "live_path": "/v1/sets/1/0/games/2/live"
  }
}
```

<!-- checked: Next -->
```json
{"Review": {"set_id": 3}}
```

<!-- checked: Next -->
```json
"Waiting"
```

### Verdict

How a judged game was decided. Only `Verified`, `VerifiedInexact` and `PeerOnly` count.

| Variant | Meaning |
|---|---|
| `"Verified"` | both uploads agree, the ruleset is met, and the re-simulation matched |
| `"VerifiedInexact"` | both agree and are legal, but the re-simulation diverged (an engine fault; the replay is quarantined); counts unless `strict_resim` |
| `"PeerOnly"` | both agree and are legal; no re-simulation (verifier off, or unavailable) |
| `"Conflicted"` | the two uploads disagree |
| `"Single"` | only one upload arrived in time |
| `{"Illegal": "word"}` | a ruleset violation; the word is one of `"players"` (not two human players), `"teams"`, `"stage"`, `"stocks"`, `"timer"`, `"character"` (a banned character), `"gecko"` (a Gecko code list not in `allowed_gecko`), `"codes"` (any-client events only: both players have registered connect codes and the replay carries codes on both ports, but they are not those two players' codes) |
| `"Undecided"` | the replays determine no winner (a game that ended by a method other than TIME!, GAME! or an LRAS quit, no Game End in the file, no frames, a tie); the organiser decides. An LRAS quit (No Contest with a known initiator) is judged a win for the other player. The game's evidence says why in `undecided_reason` |
| `"Inexact"` | `strict_resim` and the re-simulation diverged |
| `"VerifierUnavailable"` | `strict_resim`, a verifier is configured, and it was unavailable; does not count |
| `"UnverifiedForeign"` | the event has `any_client` on: both uploads agree on the outcome and are legal, but the re-simulation diverged, ended early or crashed; counts, and is quarantined for the organiser |

<!-- checked: Verdict -->
```json
"Verified"
```

<!-- checked: Verdict -->
```json
{"Illegal": "stage"}
```

<!-- checked: Verdict -->
```json
"VerifierUnavailable"
```

<!-- checked: Verdict -->
```json
"UnverifiedForeign"
```

### ResimOutcome

The re-simulation's result on a game.

<!-- checked: ResimOutcome -->
```json
"Match"
```

<!-- checked: ResimOutcome -->
```json
{"Diverged": {"frame": 1234, "port": 1, "field": "pos_x", "engine_value": "1.5", "replay_value": "1.25"}}
```

<!-- checked: ResimOutcome -->
```json
{"EarlyEnd": {"engine_last": 3000, "replay_last": 3156}}
```

<!-- checked: ResimOutcome -->
```json
{"Crashed": "engine exited with signal 6"}
```

<!-- checked: ResimOutcome -->
```json
{"Unavailable": "no verifier online"}
```

<!-- checked: ResimOutcome -->
```json
"Skipped"
```

### Resolve

<!-- checked: Resolve -->
```json
{"Award": {"slot": 1}}
```

<!-- checked: Resolve -->
```json
"Replay"
```

<!-- checked: Resolve -->
```json
{"Dq": {"slot": 0}}
```

### QueueItem

An open organiser queue item.

| Field | Type | |
|---|---|---|
| `id` | integer | |
| `event_id`, `set_id` | integer | |
| `game_id` | integer or null | the game in question, for verdict items |
| `reason` | string | a verdict name (`"Conflicted"`, `"Single"`, `"Illegal"`, `"Undecided"`, `"Inexact"`, `"VerifierUnavailable"`), or `"no-show"`, `"client-failure"`, `"stalled"`, `"double-dq"`, `"judging-failed"` (a game whose judging kept failing) |
| `verdict` | `Verdict` or null | |
| `resim` | `ResimOutcome` or null | |
| `detail` | string or null | for a no-show, who reported it and their note; for a stalled set, how long it was idle |
| `evidence` | JSON or null | per uploaded file, what the replay shows (slot, tag, file hash prefix, parse result, stage, timer, per-port character, stocks and percent, end method, derived winner; in an any-client event also the replay's connect codes, `mapping` (how ports were matched to the set's players: by code, or the port rule and why not) and, for a game with no result, `undecided_reason`), and for a lone upload the re-simulation run as evidence; for a set-level item, that of every game of the set |
| `reporter_tag` | string or null | the tag of the player who reported the set (`no-show`, `client-failure`), else null |
| `tags` | [slot 0 tag, slot 1 tag] or null | the tags of the set's two players, or null when they are not known |

<!-- checked: QueueItem -->
```json
{
  "id": 7,
  "event_id": 1,
  "set_id": 0,
  "game_id": null,
  "reason": "no-show",
  "verdict": null,
  "resim": null,
  "detail": "reported by slot 1 (Armada): nobody joined room KQ7X",
  "evidence": null,
  "reporter_tag": "Armada",
  "tags": ["Mango", "Armada"]
}
```

### QuarantineItem

A game kept for study (a re-simulation divergence, a conflict), with its evidence.

<!-- checked: QuarantineItem -->
```json
{
  "id": 3,
  "game_id": 12,
  "kind": "VerifiedInexact",
  "verdict": "VerifiedInexact",
  "resim": {"EarlyEnd": {"engine_last": 3000, "replay_last": 3156}},
  "verifier_build": "melee (rust) 0.1.0 development build+339fd9aabad8",
  "client_builds": ["b1", "b1"],
  "created_at": 1763200000,
  "judged_at": 1763200004
}
```

### EventResult

A complete event and its final placings (`GET /v1/history`, newest first; `?limit=N` with N in
1–100 keeps the newest N, as the landing page's `?limit=1` does). Placings are computed from the
bracket: the champion is 1st; every other entrant is ranked by the highest `(side, round)` set
they lost (Winners < Losers < GrandFinal) and placed 1 + the number of entrants ranked
strictly higher, so equal ranks share a place (single elimination gives 1, 2, 3, 3, 5, 5, 5,
5; double elimination 1, 2, 3, 4, 5, 5, 7, 7). Ordered by place, then seed.

| Field | Type | |
|---|---|---|
| `summary` | `EventSummary` | `state` is always `"complete"` |
| `placings` | [`Placing`] | one per entrant: `place` (1-based) and `tag` |

<!-- checked: EventResult -->
```json
{
  "summary": {
    "id": 1,
    "name": "Weekly #12",
    "state": "complete",
    "entrants": 4,
    "starts_at": 1795000000,
    "description": "Bo3, Friday 8pm",
    "created_at": 1763200000
  },
  "placings": [
    {"place": 1, "tag": "Mango"},
    {"place": 2, "tag": "Armada"},
    {"place": 3, "tag": "Zain"},
    {"place": 3, "tag": "Cody"}
  ]
}
```

### PlayerSummary

One row of the players index (`GET /v1/players`): every registered player, including those
who never entered an event (all zeros), ordered by tag ignoring case.

Counting rules, shared with [PlayerProfile](#playerprofile):

- A **set** counts for both players once its bracket set is `Done` with an entrant in both
  slots, whatever decided it (played games, an award, a DQ). Sets of running events count as
  they finish; a set resolved by a bye is not a set.
- **Games** are the set's stored wins (`scores` in `EventView`): the games played, including
  those before an award or DQ. A set awarded before any game was played counts as a set with
  no games.
- **Events** counts entries in any state. **Titles** counts complete events the player won.

| Field | Type | |
|---|---|---|
| `tag` | string | the canonical spelling |
| `events` | integer | events entered, in any state |
| `titles` | integer | complete events won |
| `sets` | [won, lost] | |

<!-- checked: PlayerSummary -->
```json
{"tag": "Mango", "events": 1, "titles": 1, "sets": [2, 0]}
```

### PlayerProfile

A player's record (`GET /v1/players/{tag}`; the tag matches in any case and `tag` in the
answer is the canonical spelling). Counting rules as under
[PlayerSummary](#playersummary); `place` comes from the event's placings and is null unless
the event is complete; `seed` is the entrant's index in the event's `tags` (0 = top seed).

| Field | Type | |
|---|---|---|
| `tag` | string | the canonical spelling |
| `since` | integer | when the player registered, unix seconds |
| `events` | [`PlayerEvent`] | newest first: `event` (`EventSummary`), `seed`, `place` (or null), `sets` and `games` (each [won, lost]) in that event |
| `head_to_head` | [`HeadToHead`] | one per opponent ever played, by sets played descending then tag: `tag`, `sets` and `games` (each [won, lost]) |

<!-- checked: PlayerProfile -->
```json
{
  "tag": "Mango",
  "since": 1763100000,
  "events": [
    {
      "event": {
        "id": 1,
        "name": "Weekly #12",
        "state": "complete",
        "entrants": 4,
        "starts_at": 1795000000,
        "description": "Bo3, Friday 8pm",
        "created_at": 1763200000
      },
      "seed": 0,
      "place": 1,
      "sets": [2, 0],
      "games": [4, 1]
    }
  ],
  "head_to_head": [
    {"tag": "Armada", "sets": [1, 0], "games": [2, 1]},
    {"tag": "Cody", "sets": [1, 0], "games": [2, 0]}
  ]
}
```

### RekeyReq

<!-- checked: RekeyReq -->
```json
{"new_key": "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"}
```

### VerifyJob and VerifyResult

<!-- checked: VerifyJob -->
```json
{"id": 41, "replay_last_frame": 3156}
```

<!-- checked: VerifyResult -->
```json
{"outcome": "Match", "build": "melee (rust) 0.1.0 development build+339fd9aabad8"}
```

## 6. What a player's client does

1. **Key.** Generate an Ed25519 key pair and keep the secret key private. Register it once
   with `POST /v1/players` (`RegisterReq`; with an invite if the server requires one).
2. **Sign up** for an event in sign-up: `POST /v1/events/{id}/entrants` with the engine build
   the player will play on.
3. **Poll** `GET /v1/me/next?event={id}` every couple of seconds and act on the answer. Keep
   polling throughout; every step below ends by going back to polling.
4. **`Play`, a game to play** (`game_no` of `set_id`):
   - **Host** (`role` `"Host"`): start the game hosting a room on `relay`, and post the room
     code with `POST /v1/sets/{event}/{set}/room`. One room per game: the next game needs a new
     room and a new code. A code cannot be posted while a replay of the current game is in
     and the game not yet judged (409).
   - **Guest**: wait until `room_code` is not null, then join that room on `relay`.
   - Play with the event's `stocks` and `timer_secs`; stage and characters are picked in the
     game and checked afterwards against the ruleset (see `GET /v1/events/{id}` for
     `legal_stages` and `banned_characters`).
5. **Upload** the game's replay when it ends: `POST /v1/sets/{event}/{set}/games/{game_no}/replay`
   with the file's bytes. Retry on transport errors and 5xx/429/408 (an identical re-upload is 200);
   do not retry a 400 or 409 unchanged. Both players upload; while `awaiting_verdict` is true,
   wait.
6. **Verdict.** The game is judged once both files are in (or `upload_timeout_secs` after the
   first). If it counts, `next` shows the next `game_no` and the new `wins`, or, when the set
   is won, the player's next set, `Waiting`, `Eliminated` or `Champion`. If it does not count,
   the set goes to the organiser (`Review`).
7. **No-show.** If the opponent never joins, report it: `POST /v1/sets/{event}/{set}/no-show`
   with kind `"no-show"`, not sooner than `no_show_grace_secs` after the room code was posted.
   A guest whose host never posts a code reports the same way. If the player's own client
   cannot complete a game (the engine keeps failing), report kind `"client-failure"` instead,
   with a neutral note. Either sends the set to the organiser.
8. **`Review`.** The set is with the organiser. Do not stop a game that is running: let it
   finish and upload its replay as in step 5 (while the review is for `no-show`, `stalled` or
   `client-failure`, the server takes it as evidence; it never counts). Keep polling: the
   organiser's decision shows up as `Play` again (a replay of the game, same `game_no`), or as
   the set being decided. Stop a running game only when `next` offers a different set or the
   player is out of the event.

### Any client: uploading without the reference client

In an event whose ruleset has `any_client` on, a player on another client (Slippi Dolphin)
takes part with nothing but their key and the requests above: `POST /v1/players`, `POST
/v1/events/{id}/entrants`, `GET /v1/me/next` (to learn `set_id` and `game_no`) and `POST
/v1/sets/{event}/{set}/games/{n}/replay` with the `.slp` file's bytes. Each is signed as in
[section 2](#2-signing-a-request); nothing else is needed. `autobracket-upload` is a minimal
client that does exactly that (a key file of 64 hex digits, or an `autobracket-client`
config), for example:

```sh
autobracket-upload --server https://bracket.example.org --key player.key   upload --event 7 --set 3 --game 1 Game_20261122T200000.slp
```

Agreement in such an event compares each upload's derived outcome (stage; each port's
character and final stocks; end method and LRAS initiator; winning port; last frame), not
bytes; each upload must pass the ruleset (its Gecko code list on `allowed_gecko`). Port 1 is
still slot 0 (the host).

## 7. Round names

What a round is called on the bracket page and in the in-game bracket menu. The names are
presentation only: the API carries no round names, and each one is derived from a set's `side`
and `round` and from the highest `round` of that side in the bracket.

On the winners and losers sides the last rounds are named counting back from the side's last
round; every earlier round is numbered by its `round`, which starts at 1.

| Side | Round | Name |
|---|---|---|
| Winners | last | Winners final |
| Winners | last − 1 | Winners semis |
| Winners | last − 2 | Winners quarters |
| Winners | earlier | Winners round N |
| Losers | last | Losers final |
| Losers | last − 1 | Losers semis |
| Losers | earlier | Losers round N |
| Grand final | 1 | Grand final |
| Grand final | 2 | Grand final reset |
| Single elimination | last | Final |
| Single elimination | last − 1 | Semis |
| Single elimination | last − 2 | Quarters |
| Single elimination | earlier | Round N |

A single-elimination bracket has only winners-side sets and its names carry no side. The grand
final reset is shown only when it is played, that is when its set is not `Skipped`. A round whose
sets all hold a `Bye` or are `Skipped` is not shown, and the numbering of the other rounds does
not change: in a bracket whose losers round 1 is all byes, the first losers round shown is
"Losers round 2".
