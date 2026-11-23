# Live relay wire format, v1

Status: draft for the server side (desktop, 2026-11-15). The engine side is built on the engine branch
`port/live-spectate` (`rust/melee/src/live_out.rs`, flags `--relay-url`, `--relay-key`).

## 1. What it is for

During a set, the host's engine streams the game's Slippi event bytes, frame by frame, outbound to
the bracket server, so the bracket page can show the match live. It is **display only**: results
still come only from the `.slp` upload and the server's re-simulation. Nothing in the live stream
is trusted for a result, a verdict or a dispute.

The engine sends only **finalised** frames (confirmed by rollback; never a predicted frame). The
bytes are exactly the event stream of the `.slp` the same match uploads: the concatenated
`game_event` payloads of one game equal the bytes of the uploaded file's `raw` element, byte for byte
(engine netplay exports are byte-identical between host and guest).

No inbound port is needed anywhere: the host dials out.

## 2. Transport and the upgrade

A WebSocket (RFC 6455) the host opens: `wss://` in production, `ws://` only for local tests.

    GET /v1/sets/{event}/{set}/games/{n}/live HTTP/1.1

The client takes the URL from `--relay-url` as given; the route above is the proposal for the server.
The client sends nothing else in the URL (no query), so the signed path is the URL's path.

The upgrade request is **signed like any API request** (`docs/api.md` section 2): the four headers
`x-ab-key`, `x-ab-time`, `x-ab-nonce`, `x-ab-sig`, Ed25519 over the canonical string

    GET
    {PATH_AND_QUERY exactly as in the request line}
    {x-ab-time}
    {x-ab-nonce}
    e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

(the body of an upgrade is empty). The key is the player's existing client key (the client's
`secret_key_hex`; the engine reads it from `--relay-key FILE`, either that TOML or a file of 64 hex
digits). Every connection attempt, including a reconnect, has a fresh nonce and time.

Proposed server checks: the usual signature checks (401 on failure, before the upgrade), then 403
unless the key is a player of the set (the host, slot 0, in v1), 404 for an unknown set, 409 unless
`n` is the set's current game and it is in play. Refuse with a plain HTTP status before switching
protocols; the client logs it and retries with backoff (500 ms doubling to 8 s), so a 4xx that will
not change should be rare (the client gives up only when its game is over, section 5).

## 3. Messages

Every message, both directions, is one **text frame** holding one JSON object. The engine-to-server
messages are the same objects the Slippi "Dolphin connection" protocol carries (what Slippi Dolphin
serves on 51441 and slippi-js `DolphinConnection` reads), so one parser serves both.

Client to server:

| `type` | fields | when |
|---|---|---|
| `connect_request` | `cursor` (the next cursor the client would send), `version` (1) | first message on every connection |
| `start_game` | `cursor`, `next_cursor` | the game began |
| `game_event` | `cursor`, `next_cursor`, `payload` | event bytes, base64 (RFC 4648, padded) |
| `end_game` | `cursor`, `next_cursor` | after the Game End event |
| `keepalive` | — | after 2 s with nothing else to send |

Server to client:

| `type` | fields | when |
|---|---|---|
| `connect_reply` | `cursor`: the next cursor the server needs | answer to `connect_request`; nothing else is sent before it |

- **Cursors** count messages from 0 for the connection's game; `next_cursor` is always `cursor + 1`.
  `keepalive` has no cursor.
- **Order within a game:** `start_game`, then `game_event`s, then `end_game`. The first
  `game_event` starts with the Event Payloads event (0x35) and the Game Start (0x36); the last one
  holds the Game End (0x39).
- **Whole events:** a `game_event` payload always starts and ends on an event boundary (sizes from the
  0x35 event). In v1 the engine sends each finalised frame (Frame Start, pre-frames, post-frames,
  Frame Bookend) as one message, or several when it is over 768 bytes; the server must not rely on
  one frame per message.
- The server may ignore unknown `type`s and unknown fields (later versions may add them).

## 4. Resume after a reconnect

On a dropped connection the client reconnects (new signature) and sends `connect_request`. The server
answers `connect_reply` with the cursor it needs next, which is the cursor after the last message it
**stored**, not what the client thinks it sent (messages written to a socket that dropped can be
lost). The client resends from that cursor; it keeps the whole game in memory, so any cursor of this
game can be served. A `connect_reply` cursor larger than what the client has is clamped to the
client's end.

A new connection for a game **replaces** any older one: the server closes the older connection and
discards anything still arriving on it (after a stall the old socket can deliver buffered messages
late, interleaved with the new connection's).

## 5. Back-pressure: never stall the game

The game thread only appends to a queue; the socket lives on its own thread (in fact in a separate
low-priority process). A slow link therefore never slows the game. A write that blocks for 5 s
drops the connection and the client reconnects (section 4). If the sender falls more than
`MELEE_LIVE_MAX_BEHIND` messages behind (default 3600, about a minute of frames), it **skips to the
newest message** and logs the gap. The server then sees a cursor jump (`cursor` larger than the one
it expected): it must accept it, record the gap, and carry on from the new cursor (the live view is
display only; a viewer joining later only needs recent frames anyway). A server that wants the
skipped frames can take them from the upload.

When the game ends the client sends the rest and `end_game`, then closes the WebSocket normally
(close code 1000). If it cannot deliver within 8 s after the game's end it gives up.

## 6. Sizes and rates

About 60 `game_event`s a second, typically 330 raw bytes each for two fighters (about 450 bytes of
base64, 520 of JSON): roughly 30 KB/s per live game, about 2 MB a minute. The Game Start message is
about 1 KB. Suggested server limits: 64 KiB per frame, idle timeout 30 s (keepalives arrive every
2 s when nothing else does).

## 7. Server side (to build)

- The route and its checks (section 2), then the handshake (`connect_reply`).
- Store per game: messages by cursor, gaps, closed or not. Fan out to page viewers (a read route of
  the server's choosing; it can serve the same JSON objects, so slippi-js style parsers work).
- Never feed any of it into results.

## 8. Test fixture

The engine branch tests against a local receiver that checks the signature exactly as above, answers
`connect_reply`, drops the first connection after N messages (resume) and stalls reading for a while
(back-pressure). Pass criteria: the concatenated payloads equal the final `.slp`'s `raw` bytes, and
the drop test resumes with no gap.

Measured on the engine branch (2026-11-15, loopback, a 60-second netplay pair, 3182 messages,
1,036,490 event bytes): plain run byte-identical to the host's `.slp` `raw`; drop after 500 messages
resumed at the server's cursor 500 (the client had written 501) with no gap and identical bytes;
a 15 s read stall with the threshold lowered to 120 timed out the write, reconnected and skipped
300..1042 as designed, the game unaffected.
