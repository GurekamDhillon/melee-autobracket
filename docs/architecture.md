# Architecture map

How the pieces of autobracket plug together. Status as of 2026-11-16:
`[live]` running, `[wip]` being built, `[plan]` approved but not started, `[proto]` prototype.

```
 A = player side        B = VPS (192.99.42.92)        C = desktop (GD-PC, RTX 5070)

 ┌─ A ─────────────────────────┐
 │ A1 melee engine  [live]     │  rollback netplay ─────────► A1 opponent (direct connect)
 │    (native Rust port)       │
 │      │ .slp replay          │
 │      ▼                      │
 │ A2 autobracket-client [live]│  signed Ed25519 requests: signup / next set / upload
 │    (CLI; later in-game menu)│─────────────┐
 │ A3 browser          [live]  │──────────┐  │
 │ A4 Discord   [plan]         │◄──┐      │  │
 └─────────────────────────────┘   │      │  │ HTTPS 443
                                   │      ▼  ▼
 ┌─ B ─────────────────────────────┼─────────────────────────────────────────┐
 │ B1 Mailu nginx front [live]  TLS for bracket. / live. / auth.gsd.sh       │
 │    strips identity headers; forward-auth only on /account, /admin [plan]  │
 │      │                │                    │                              │
 │      ▼                ▼                    ▼                              │
 │ B2 autobracket server [live]      B7 sign-in stack [plan]                 │
 │    axum :8790                        Zitadel + Postgres                   │
 │    brackets, sets, uploads,          + oauth2-proxy                       │
 │    API, public pages [wip]           Discord-only login ◄──── A4          │
 │    │   │   │   │                     admin pages need 2FA                 │
 │    │   │   │   └► A4 webhook "your set is up"  [plan]                     │
 │    │   │   ▼                                                              │
 │    │   │  B3 verifier [live]    both uploads → re-sim with slpcheck       │
 │    │   │     Gecko allow-list    → Verified / Conflicted / ...            │
 │    │   ▼                                                                  │
 │    │  B4 SQLite + replays [live]  (store replays as .slpz [plan], ~8x)    │
 │    │      │                                                               │
 │    │      ▼                                                               │
 │    │  B5 nightly backup [live]  /var/backups, keep 7, NO off-site copy    │
 │    ▼                                                                      │
 │ B8 slippi-js sidecar [proto]  verified .slp → hype clips + player stats   │
 │                                                                           │
 │ B6 MediaMTX [live]  SRT in :8890 → WebRTC out at live.gsd.sh/melee/       │
 │      ▲          └────────────► embedded on /stream page [wip]             │
 └──────┼───────────────────────────────────┬────────────────────────────────┘
        │ SRT stream                        │ render job queue [plan]
        │                                   │ signed pull / heartbeat / chunked upload
 ┌─ C ──┼───────────────────────────────────┼─────────────────────────┐
 │ C2 SRT publisher [live]          C1 render worker [plan]           │
 │    renders replays → stream          slpcheck --video-out (raw)    │
 │                                      → ffmpeg NVENC + AAC → mp4    │
 │                                      → VOD + clip mp4 → back to B2  │
 │                                   C3 analyse [plan, after VODs]     │
 │                                      "missed punish" cards,         │
 │                                      private to the 2 players + TOs │
 └────────────────────────────────────────────────────────────────────┘
```

## Broadcast (proposed, not yet approved)

```
 C1 render (slpcheck raw → ffmpeg)  ──► local SRT/NDI feed
                                          │
 B10 overlay pages on B2 [plan]           ▼
   /overlay/scoreboard, /overlay/bracket ──► C4 OBS [plan]  scenes: game, between sets, bracket, starting soon
   (browser sources, live data)               ▲   │
                                              │   │ one SRT upload
 C5 producer [plan] ── obs-websocket v5 ──────┘   ▼
   (local on desktop; watches B2 for           B6 MediaMTX ──► WebRTC on live.gsd.sh
    "set live / set done", switches scenes)        └──► restream RTMP → Twitch / YouTube
```

- OBS runs on the desktop (GPU compositing). obs-websocket stays on localhost and is never exposed to the internet. Its password lives in a local secret file, not git.
- C5 pulls state from B2's public API, so no inbound connection to the desktop is needed.
- OBS uploads once, to MediaMTX. MediaMTX fans out to Twitch and YouTube, so the desktop's upstream carries one stream. The stream keys live in MediaMTX's root-only env file.

## Build order

| Step | Status | Pieces |
|---|---|---|
| (a) public pages | wip | B2: landing, schedule, history, players, stream, vertical bracket |
| (b) sign-in | plan | B7 + A4 (Zitadel, Discord-only, oauth2-proxy) |
| (c) render queue | plan | C1 + VODs (design: docs/design/render-worker-api.md on lane-worker-api) |
| (d) clips + stats | plan | B8 → C1 |
| (e) analyse | plan | C3 |
| later | parked | arena / casual queue, start.gg integration |
