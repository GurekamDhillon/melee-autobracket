# Phase 2b: identity, admin pages, in-game client API, live relay receive

Status: spec, 2026-11-16. Branch `phase1`. Plan: `docs/superpowers/plans/2026-11-16-identity-and-ingame.md`.

Inputs, all read for this spec:
- Identity design and runbook (branch `lane-zitadel`, 60c6b9a): `docs/design/identity-zitadel.md`,
  `deploy/zitadel/README.md` (Zitadel, login, Postgres and oauth2-proxy deployed 2026-11-16 from
  `/srv/autobracket-auth`; `bracket.gsd.sh.conf` not edited yet), `deploy/zitadel/nginx-auth.conf.example`.
- Owner decisions: `.superpowers/sdd/2026-11-15-verified-autobracket/phase2-scope-v2.md` and
  `ingame-flow-requirements.md` (owner-approved 2026-11-16).
- Live relay wire v1: `origin/desktop/live-relay:docs/design/live-relay.md`.
- Today's API (`docs/api.md`), pages (`crates/server/assets`, `routes/view.rs`), client (`crates/client`).

## 1. Goals

1. A player signs in with Discord (via Zitadel + oauth2-proxy), picks a tag, links a game client with a
   one-time code typed in the game, and joins events from the web or from the in-game Brackets screen.
2. Organisers run events from `/admin` in the browser (role `autobracket-admins` **and** a second factor),
   with every action written to an audit log with its actor.
3. The engine's native `bracket` host service gets every route the in-game Brackets screen needs: home
   and badge, ready/unready, server-held stage strikes/bans/picks, between-games and set-end state.
4. The host's engine streams a live game to the server over a signed WebSocket; the server stores it in
   memory and the stream page shows live stocks and percent. Display only.
5. Deleting a user in Zitadel anonymises them here.

Non-goals: render queue, clips, stats, analysis (only the `share_analysis` toggle is stored), a public
SSE feed, start.gg, TO mods, tag renames by players, an OAuth flow or session store in the app.

## 2. Trust model for identity

Binding inputs: the security review `lane-zitadel-review.md` (C1, I1, I3, I4, I5, I7). Hardening already
applied on the deploy (masterkey from env, OIDC/login-only public paths on auth.gsd.sh, Zitadel in the
nightly backup, atomic `.env`) is not repeated here.

- nginx (Mailu front) runs `auth_request` against oauth2-proxy only on `/account/` and `/admin/`. Every
  `proxy_set_header` lives at **server level** (review C1, fix A): the identity headers are set from
  variables that default to `""` and are overwritten only by `auth_request_set` inside the two
  protected locations; every other identity name (`Authorization`, `X-Auth-Request-Email`,
  `-Access-Token`, `X-Forwarded-User/-Groups/-Email/-Preferred-Username/-Access-Token`) is stripped;
  `Host`, `X-Real-IP`, `X-Forwarded-For` (`$remote_addr`, not appended) and `X-Forwarded-Proto` are set;
  and `X-Autobracket-Proxy: <proxy_secret>` (from a root-only include) is set on every request. No
  location sets its own `proxy_set_header`, except the internal `/oauth2/auth*` locations, which are
  self-contained and repeat the whole list.
- The app accepts identity **only** when all hold: the route is under `/account/` or `/admin/` (the
  extractors exist only there); the TCP peer is inside `trusted_proxies`; `X-Autobracket-Proxy` equals
  `identity.proxy_secret` (constant-time); `X-Id-Token: Bearer <jwt>` **verifies** (below); and
  `X-Auth-Request-User` equals the token's `sub`. Otherwise the request is anonymous: 401
  `{"error":"not signed in"}`. Bad header contents are never a 500.
- **ID-token verification** (review I1): signature (RS256 or EdDSA, by the JWK's `kty`/`alg`) against the
  JWKS at `identity.jwks_url` (default `<issuer>/oauth/v2/keys`), cached 1 h, re-fetched on an unknown
  `kid` at most once a minute; `iss == identity.issuer`; `aud` contains `identity.client_id`; `exp` in
  the future (60 s leeway); `sub` 1-64 visible ASCII. JWKS unreachable and no cached key: 503
  `{"error":"sign-in service unavailable"}`. **`sub`, `groups`, `amr` and `auth_time` come from the
  verified token only.** `X-Auth-Request-Groups`, `-Preferred-Username` and `-Email` are never read.
- Account rows: the extractor creates the `accounts` row for an unknown `sub` (no Discord id: that comes
  only from the hooks, §4) and refuses a `sub` whose SHA-256 is in `account_tombstones` (401
  `{"error":"this account was deleted"}`) or whose account has `disabled_at` set (403
  `{"error":"this account is disabled"}`) — never re-creating a removed user (review I7).
- Admin (`/admin/*`): the token's `groups` array contains `identity.admin_group` (exact string), **and**
  `amr` contains `"mfa"`, **and** `now - auth_time ≤ identity.admin_max_auth_age_secs` (default 43200).
  Not in the group: 403 `{"error":"not an organiser"}`. No `mfa` or `auth_time` too old / absent: 403
  `{"error":"admin pages need a recent sign-in with a second factor","mfa_setup_url":…,"sign_in_url":
  "/oauth2/sign_in?rd=/admin/"}`.
- CSRF: every non-GET/HEAD request under `/account/` and `/admin/` must carry `x-ab-csrf: 1` and an
  `Origin` exactly equal to `identity.public_origin`; otherwise 403 `{"error":"cross-site request refused"}`.
- Responses under `/account/` and `/admin/` carry `Cache-Control: no-store`, `X-Frame-Options: DENY`
  and (pages) a CSP with `frame-ancestors 'none'` (review I4: the app does not rely on nginx for these).
- `/account` and `/admin` without the slash answer 308 to the slashed path without reading headers.
- With no `[identity]` table, `/account/*`, `/admin/*` and `/hooks/zitadel/*` are 404 and `/v1/site`
  says `"identity": false` (self-hosters without Zitadel lose nothing).
- Session revocation (review I7): oauth2-proxy gets `offline_access` and `COOKIE_REFRESH=10m`, so a
  removed role, a deactivated or a deleted user stops working within 10 minutes; the app's tombstone and
  `disabled_at` (§4) act at once.

Config (TOML, in `Config`):

```toml
[identity]
public_origin = "https://bracket.gsd.sh"     # https origin, no path
issuer = "https://auth.gsd.sh"
client_id = "<bracket-web client id>"        # the ID token's aud
jwks_url = "https://auth.gsd.sh/oauth/v2/keys"   # optional; default issuer + /oauth/v2/keys
project_id = "<autobracket project id>"      # groups come only from grants of this project
org_id = "<autobracket org id>"
discord_idp_id = "<Discord IdP id>"
proxy_secret = "<at least 32 characters>"
admin_group = "autobracket-admins"            # default
admin_max_auth_age_secs = 43200               # default
sign_out_url = "/oauth2/sign_out?rd=…"        # the design doc's logout link, verbatim
mfa_setup_url = "https://auth.gsd.sh/ui/v2/login/mfa/set"   # shown on the admin 403
hook_peers = ["10.203.48.0/28"]               # optional: peers allowed to call /hooks/zitadel/*
[identity.hook_keys]                          # Zitadel Actions v2 target signingKeys
idp_intent = "…"
token = "…"
user_events = "…"
[site]
operator = "…"     # optional, shown on privacy/terms
contact = "…"      # optional, shown on privacy/terms
```

`validate` with `[identity]`: `public_origin` and `issuer` are `https://host[:port]` origins; `jwks_url`
an https URL; `proxy_secret` ≥ 32 characters; `trusted_proxies` non-empty; `client_id`, `project_id`,
`org_id`, `discord_idp_id`, `admin_group` and the three hook keys non-empty; `hook_peers` parse as CIDRs.

## 3. Data model (migration `0006_identity.sql`)

| Table / column | Purpose |
|---|---|
| `accounts(id INTEGER PK, sub TEXT UNIQUE NOT NULL, discord_id TEXT NULL UNIQUE, connect_code TEXT NULL, notify INTEGER NOT NULL DEFAULT 0, share_analysis INTEGER NOT NULL DEFAULT 0, created_at, last_seen_at, disabled_at INTEGER NULL)` | One per Zitadel user, created on first sight (extractor or token hook). `discord_id` is set once and never re-bound. |
| `account_tombstones(sub_sha256 TEXT PK, removed_at)` | Removed Zitadel users; their `sub` is refused forever. |
| `discord_profiles(discord_id TEXT PK, username TEXT NOT NULL, global_name TEXT NULL, avatar TEXT NULL, updated_at)` | Refreshed on every login by the idp-intent hook from Discord's `/users/@me`. |
| `players.account_id INTEGER NULL` + unique index where not null | The player (tag, results) an account owns; at most one each way. |
| `player_keys(key TEXT PK, player_key TEXT NOT NULL, label TEXT NULL, linked_at)` | Additional client keys that sign as that player. |
| `link_codes(code_sha256 TEXT PK, account_id INTEGER UNIQUE NOT NULL, expires_at)` | At most one live code per account. |
| `audit_log(id INTEGER PK, at, actor_kind TEXT, actor_id TEXT, actor_name TEXT NULL, action TEXT, target TEXT NULL, detail_json TEXT NULL)` | Append-only. |
| `sets.ready0_at`, `sets.ready1_at` INTEGER NULL | Ready check-in per slot. |
| `stage_moves(event_id, set_id, game_no, seq, slot, kind TEXT CHECK (kind IN ('strike','ban','pick')), stage, at, PRIMARY KEY (event_id, set_id, game_no, seq))` | Server-held stage selection. |

**Player identity.** `players.key` stays the canonical id that `entrants.player_key` refers to. A signer
key resolves to a player through `players.key` or `player_keys.key`; handlers keep using the canonical
key, so every existing route works for linked keys unchanged. A player created from the web gets a
**placeholder** canonical key: 32 random bytes as hex, whose secret nobody holds. `POST /v1/players`
refuses (409) a key present in either table.

## 4. Zitadel Actions v2 hooks (`/hooks/zitadel/*`)

Reached by Zitadel directly at `http://172.17.0.1:8790/hooks/zitadel/…` (review I5.5: no dependency on
public DNS or TLS); the public nginx answers 404 for `/hooks/`. When `identity.hook_peers` is set, other
peers get 404. Rules for all three (review I5):

- **Signature first, on raw bytes, before any JSON parsing.** Header `zitadel-signature: t=<unix>,v1=<hex>[,v1=<hex>…]`
  (parts in any order, several `v1` allowed for key rotation); HMAC-SHA256 with that hook's own key over
  `"<t>.<raw body>"`, constant-time; `|now − t| ≤ 300` in both directions; a `(t, v1)` pair seen in the
  last 5 minutes is refused (in-memory replay cache). Any failure: 400 with body `{"error":"bad request"}`
  and no detail. Body ≤ 256 KiB.
- **Never log hook bodies** (the intent response carries the Discord access token). Logs carry the hook
  name, outcome and at most key names.
- Discord-sourced strings are untrusted: control characters removed, `username` cut to 32 characters,
  `global_name` to 64; pages show them with `textContent` only.

1. `POST /hooks/zitadel/idp-intent` (`restCall`, interruptOnError). Body `{…, "response": {…}}`.
   400 unless `response.idpInformation.idpId == identity.discord_idp_id`. The Discord user is
   `response.idpInformation.rawInformation` if it has a string `id`, else the first object value inside
   it that has one (nesting varies by version; the ops task confirms). `id` must be 1-20 ASCII digits,
   else 400. Upsert `discord_profiles` (username, global name, avatar hash — profile fields only).
   - If `response.addHumanUser` is an object, set in it **only**: `username = "discord-<id>"`;
     `profile = {givenName: G, familyName: "(Discord)", displayName: G, nickName: username}` with
     G = `global_name` or else `username`; `email = {email: "<id>@users.invalid", isVerified: true}`
     (verified on purpose: oauth2-proxy rejects unverified emails, review M9); `metadata = [{key, value:
     base64(utf8)}]` for `discord_id`, `discord_username`, `discord_avatar_url`.
   - If `response.updateHumanUser` (or `updateUser`) is an object, set only `profile` (never `username`).
   - Never touch `idpLinks`, `userId`, `organization`, any password field or `passwordChangeRequired`.
   - Return 200 with the (possibly modified) `response` object as the body.
2. `POST /hooks/zitadel/token` (`restCall`, `preuserinfo`; `preaccesstoken` is harmless and kept).
   `groups` = the unique `roles` of `user_grants` entries whose `projectId == identity.project_id` and
   `orgId == identity.org_id`, first-seen order; never from metadata, profile or names. Always return
   `{"append_claims":[{"key":"groups","value":[…]}]}` (`[]` when none). Side effect, errors logged and
   never failing the hook: if `user.id` has no account and is not tombstoned, create it with
   `discord_id` from `user_metadata` (`discord_id`, value base64); if the account exists without a
   Discord id, set it; if it exists with a **different** Discord id, change nothing and log a warning.
   Never re-bind a `sub` to another Discord id or a Discord id to another `sub`.
3. `POST /hooks/zitadel/user-event` (`restWebhook`, executions `user.removed`, `user.deactivated`,
   `user.locked`, `user.reactivated`, `user.unlocked`). User id = `aggregateID` (fallback `userID`), type
   = `event_type`. Idempotent; unknown users: 200.
   - `user.deactivated` / `user.locked`: `accounts.disabled_at = now` (the extractor refuses at once);
     `user.reactivated` / `user.unlocked`: cleared. Audit `account.disabled` / `account.enabled`.
   - `user.removed`: anonymise in one transaction: insert the tombstone `sha256(sub)`; delete the
     `accounts` row's personal fields (`sub` replaced by `"removed:<account id>"`, `discord_id`,
     `connect_code` NULL, toggles 0, `disabled_at` set); delete its `discord_profiles` row and
     `link_codes`; its player's tag becomes `anon_<6 base32>` (unique), `player_keys` deleted, canonical
     key replaced by a fresh placeholder (`rekey_player`) so no device can sign as them; results stay
     under the anonymous tag; audit `account.removed` by actor `zitadel`.

Blast radius, stated for the design doc (review I5.5): whoever controls the app process can mint
`groups` for anyone, so the app process is as trusted as Zitadel's admin.

## 5. Web routes

### 5.1 Account (`/account/`, any signed-in user)

| Method | Path | Body | Answer |
|---|---|---|---|
| GET | `/account/` | — | the account page |
| GET | `/account/api/me` | — | 200 `AccountView` |
| POST | `/account/api/player` | `{"tag": "Mango"}` | 201 `AccountView`; 400 tag rule; 409 already has a player, tag taken |
| POST | `/account/api/settings` | `{"connect_code": str\|null, "notify": bool, "share_analysis": bool}` | 200 `AccountView`; 400 connect code not `^[A-Z]{1,7}#[0-9]{1,4}$` |
| POST | `/account/api/link-codes` | `{}` | 201 `{"code": "ABCD2345", "expires_at": unix}`; replaces any live code; 409 no player yet |
| DELETE | `/account/api/clients/{key}` | — | 204; removing the canonical key when it is a real device replaces it with a placeholder; 404 not this account's |
| POST | `/account/api/events/{id}/join` | `{}` | 201; 409 no player yet / not in sign-up / already entered / full; 404 |

`AccountView`: `display_name`, `discord_username`, `avatar_url` (Discord CDN URL or the default avatar
by `(id >> 22) % 6`), `tag` (or null), `connect_code`, `notify`, `share_analysis`, `is_admin` (the verified token's
groups contain `admin_group`; MFA not checked here), `clients: [{key_prefix (16 hex), label, linked_at, placeholder: bool}]`,
`entered: [event ids in sign-up or running]`, `sign_out_url`.

Web join enters with `engine_build = ""`; the accepted-build check moves to `ready` for such entrants
(§6.2). Accounts need no invite: Discord sign-in is the gate (`require_invite` still applies to
`POST /v1/players`).

**Link codes**: 8 characters from `ABCDEFGHJKMNPQRSTUVWXYZ23456789` (no I, L, O, 0, 1), stored as SHA-256,
valid 10 minutes, one use. Consumed by the signed `POST /v1/me/link` (§6.1).

### 5.2 Admin (`/admin/`, organiser + MFA)

Page `/admin/` with sections Events, Queue, Invites, Players (rekey), Audit log. JSON under `/admin/api/`
mirrors the signed organiser API one to one, same bodies and statuses:
`POST events`, `POST events/{id}/start`, `POST events/{id}/schedule`, `DELETE events/{id}/entrants/{tag}`,
`POST invites`, `GET queue`, `POST queue/{id}/resolve`, `POST players/{old_key}/rekey`; plus
`POST sets/{event}/{set}/decide` (`Resolve`, `Award` or `Dq` only: opens a queue item with reason
`organiser` and resolves it, so the existing resolution path and its audit apply; also offered signed as
`POST /v1/admin/sets/{event}/{set}/decide`), `GET audit?before=<id>&limit=<1-200, default 50>`
(newest first) and `GET whoami`. Events are read from the public `/v1/events*`.

Every organiser action, signed or web, writes one `audit_log` row: `actor_kind` `key` (actor_id = the
key's first 16 hex) or `account` (actor_id = account id, actor_name = display name at the time);
action names `event.create`, `event.start`, `event.schedule`, `entrant.remove`, `invite.create`,
`queue.resolve`, `set.decide`, `player.rekey`, plus `account.removed`, `client.link`, `client.unlink`, `account.disabled`, `account.enabled`.

### 5.3 Site header, privacy, terms

- Shell nav gains an `Account` link (`/account/`, id `nav-account`) and a footer with Privacy and Terms.
  `site.js` asks `GET /v1/site`; when `identity` is false the link is hidden. It then fetches
  `/account/api/me` with `redirect: "manual"`: 200 shows the display name as text ("Signed in as X");
  anything else shows "Sign in". No avatar image on public pages (no third-party request).
- `/account/` and `/admin/` pages alone allow `img-src data: https://cdn.discordapp.com`.
- Event page, event in sign-up: signed in with a player → a "Join" button posting the join route; signed
  in without a player → "Pick a tag on your account page to join"; signed out → "Sign in to join" linking
  `/oauth2/start?rd=/events/{id}`. The "For testers" CLI block stays.
- `GET /privacy` and `GET /terms`: public static pages; `GET /v1/site` → `SiteInfo
  {identity, operator, contact}` fills operator/contact. Privacy copy states exactly: what Zitadel stores
  (design §3: Zitadel id, `discord-<id>` username, Discord global name, the synthetic
  `<id>@users.invalid` address that receives nothing, IdP link, metadata, sessions with user agent and
  IP, an event history Zitadel keeps); what this server stores (account id, Discord id, username, global
  name, avatar hash, tag, connect code, toggles, linked client public keys, entries, results, uploaded
  replays, an audit log of organiser actions, request logs); only Discord scope `identify` (no email,
  guilds or friends); deletion through Zitadel's self-service or by asking the operator anonymises
  results (tag replaced, keys removed) rather than deleting them; backups are local nightly copies kept
  7 days with **no off-site copy**. Terms: verified-results rules (results come only from replays both
  players upload and the server re-simulates), the organiser's decisions on the queue are final, no
  cheating codes beyond the event's allow-list, the service is provided as is.

## 6. Signed client API additions (the engine's `bracket` host service)

All signed as `docs/api.md` §2. "Player" includes linked keys. Perspective is the signer's: pairs are
`[me, opponent]`, as in `Next`.

### 6.1 Identity

- `GET /v1/me` (player) → `MeStatus {tag, linked: bool (player has an account), display_name}`.
- `POST /v1/me/link` (any key) `LinkReq {code}` → 200 `LinkResult {tag}`. Code unknown, used or
  expired: 404. Then, by the signer key:
  already this account's player (canonical or alias) → 200, nothing changes;
  registered to a player without an account, and the account has no player → the account adopts it;
  unregistered, account has a player → alias added;
  unregistered, account has no player → 409 "pick a tag on your account page first";
  registered to another player or another account → 409 and nothing moves.
  The code is consumed only on 200. Audit `client.link`.

### 6.2 Home, badge, ready

- `GET /v1/me/home` (player) → `Home {badge, action, events}`:
  - `events: [MyEvent {event: EventSummary, next: Next}]`, every event in sign-up or running the player
    entered, oldest first (complete events are history, not home).
  - `action`: `"None"`, or `{"SetUp": {event_id, set_id, side, round, opponent_tag, game_no, ready:
    [bool, bool]}}` for the first event whose `next` is `Play` (sets with the organiser show as
    `{"Review": {event_id, set_id}}`), or `{"Upcoming": {event_id, set_id, side, round, opponent_tag:
    str|null, eta_secs: int|null}}` for the first event where the player waits on a feeder set.
  - ETA: `eta_secs` = (number of undecided sets on the longest feeder chain into the player's next set)
    × (median duration of the event's done played sets, from the set's first game upload to its last
    judged game; 600 s while fewer than 3 such sets exist). Null when the next set cannot be found.
  - `badge`: true when `action` is `SetUp` with the player's own `ready` false.
- `GET /v1/me/badge` (player) → `Badge {set_up: bool, event_id, set_id}` with a strong `ETag`
  (`"<sha256 hex of the body, first 16>"`); `If-None-Match` equal → 304 with no body. Polled every 5 s
  costs one signed GET and one nonce row. (An SSE feed is the later B2 public feed, not this.)
- `POST /v1/sets/{event}/{set}/ready` (player of the set) `ReadyReq {engine_build}` → 204, sets
  `ready{slot}_at` and the entrant's `engine_build`. 400 bad build string; 403 `engine build not
  accepted` with `accepted`; 404; 409 set not `ready`.
- `DELETE /v1/sets/{event}/{set}/ready` → 204 clears it (409 once game 1 has an upload).
- Ready is advisory (it gates nothing in v1); the no-show rules are unchanged.

### 6.3 Stage selection (server-held)

Ruleset gains `starter_stages: [int]` (default `[]` = all `legal_stages`) and `stage_bans: int`
(default 2). `validate`: starters ⊆ legal, at least 1 starter, `stage_bans < legal_stages.len()`.

- **Game 1, strikes** over the starters, slot 0 first, turns alternating. Turn *k* (0-based) strikes
  `1` if *k* = 0, else `min(2, remaining − 1)`; ends when one stage remains, which is the stage.
  5 starters: 1-2-1. 3: 1-1. 7: 1-2-2-1. 1: no turns.
- **Game n ≥ 2**: the winner of game n−1 bans `stage_bans` (one stage per move, consecutive moves),
  then the loser picks one of the remaining `legal_stages`.
- A **replayed game** (organiser `Replay`) starts its selection afresh (moves of that game_no deleted).
- `POST /v1/sets/{event}/{set}/games/{n}/stage` (player of the set) `StageMoveReq {kind: "strike"|"ban"|
  "pick", stage}` → 200 `StageState`. 404 set; 409 set not `ready`, `n` not the current game, not your
  turn, wrong kind, stage not in the pool or already struck/banned, selection complete.
- Verification: when game n's selection is complete, the judge additionally requires the replay's
  stage to equal the chosen stage, else `Illegal("stage")`. Games without a completed selection (CLI
  clients) are judged as today.
- `StageState {mode: "strike"|"counterpick", pool: [stage], moves: [{who: "me"|"opponent", kind,
  stage}], turn: {who, kind, count}|null, chosen: stage|null}`.

### 6.4 Set flow, between games, set end

- `GET /v1/sets/{event}/{set}/flow` (player of the set) → `SetFlow`:
  `set_id, side, round, opponent_tag, role, best_of, wins [me, opp], game_no (current), ready [me, opp],
  phase: "ready_check"|"stage_select"|"playing"|"judging"|"review"|"done", stage: StageState (current
  game; null when done), games: [GameResult {game_no, stage, winner: "me"|"opponent"|null, counted,
  verdict}], verified: bool, after: Next|null`.
  - `phase`: `done` once the set is decided; `review` when the set is with the organiser; `judging`
    while the current game has an upload not judged; `playing` once selection is complete (or game 1
    had an upload); `stage_select` once both are ready; else `ready_check`.
  - `verified`: every counted game's verdict is `Verified` (re-sim matched); it flips true once the last
    re-sim passes. `PeerOnly` / `VerifiedInexact` keep it false.
  - `after` (only when `done`): the player's `Next` in this event (next set, `Waiting`, `Eliminated`,
    `Champion`); a drop to losers is a `Play`/`Waiting` whose set is on the Losers side, which the
    screen reads via `GET /v1/me/home`'s `Upcoming`.
- "Report": results come only from uploads; the existing `no-show` and replay upload routes are the
  report routes. No result-report route is added.

## 7. Live relay receive (B2)

- `GET /v1/sets/{event}/{set}/games/{n}/live`, WebSocket upgrade, signed with an empty body (401 before
  the upgrade); 403 unless the signer is the set's slot 0 player; 404 no such set; 409 unless the set is
  `ready` and `n` is its current game. Plain HTTP status before switching protocols.
- Messages per the wire doc: answer `connect_request` with `connect_reply {cursor: next needed}`; store
  `game_event` payloads (base64) whose cursor equals the expected one; a smaller cursor is a duplicate
  (ignored); a larger one is a gap (counted, accepted, expected = cursor + 1). `start_game`/`end_game`
  advance the cursor; `end_game` marks the game ended. Unknown types/fields ignored. Malformed JSON or
  base64: close 1003.
- A new connection for the same game replaces the old one: the old socket is closed and anything it
  still delivers is discarded (generation counter).
- Limits: frame ≤ 64 KiB; idle 30 s → close; stored bytes per game ≤ `max_replay_bytes` (beyond: not
  stored, counted as gap); at most 32 live sockets (more: 503 before upgrade); a game is forgotten 10 min
  after `end_game` or 2 min after its last message.
- Memory only; nothing reaches results, verdicts or the database.
- `GET /v1/live` (public) → `[LiveGame {event_id, set_id, game_no, tags [slot0, slot1], stage,
  ports: [{port, character, stocks, percent}], frame, ended, gaps, updated_at}]`, from parsing the stored
  bytes with the replay crate (cached, recomputed at most once a second per game).
- The stream page's "Now playing" polls `/v1/live` every 2 s and adds, under each live set, a line like
  "Game 2 · Battlefield · Fox 3 stocks 42% — Marth 2 stocks 87%" (character and stage names from a
  table in the page script).

## 8. nginx and end-to-end (ops, controller only)

`bracket.gsd.sh.conf` from `nginx-auth.conf.example`, rewritten per review C1 (fix A) and I4:
- All `proxy_set_header` at server level: `set $auth_user ""; set $auth_idtok "";` then
  `X-Auth-Request-User $auth_user`, `X-Id-Token $auth_idtok`; `""` for every other identity name
  (including `X-Auth-Request-Groups`, `-Preferred-Username`, `-Email`, `-Access-Token`, `Authorization`,
  all `X-Forwarded-User/Groups/Email/Preferred-Username/Access-Token`); `Host $host`, `X-Real-IP
  $remote_addr`, `X-Forwarded-For $remote_addr`, `X-Forwarded-Proto https`; and
  `include /mailu/overrides/nginx-conf.d/bracket-secret.inc;` (root-only, not `.conf`) holding
  `proxy_set_header X-Autobracket-Proxy <secret>;`. `/account/` and `/admin/` contain only
  `auth_request`, `error_page`, `auth_request_set` and `proxy_pass` (plus the cookie line below).
- Internal `/oauth2/auth` and `/oauth2/auth_admin` are self-contained: they repeat the full header list
  plus `X-Forwarded-Uri $request_uri`, `Content-Length ""`, `proxy_pass_request_body off`.
- `/oauth2/` sets no headers (inherits); no `X-Auth-Request-Redirect` (review M3); sign-in `rd` is
  URL-encoded (M1).
- Security headers (`X-Content-Type-Options`, `Referrer-Policy`, `X-Frame-Options DENY`, HSTS) in an
  include `bracket-security-headers.inc` used at server level **and** in every location with its own
  `add_header`. With cookie refresh on (I7), `/account/` and `/admin/` keep exactly
  `auth_request_set $auth_cookie $upstream_http_set_cookie; add_header Set-Cookie $auth_cookie always;`
  and the include; the split-cookie `if` block is dropped unless the test shows split cookies.
- `/hooks/` → 404. The live WebSocket regex location `^/v1/sets/[0-9]+/[0-9]+/games/[0-9]+/live$`
  sets `proxy_http_version 1.1` and passes `Upgrade`/`Connection` through **variables set at server
  level** (`map $http_upgrade $conn_upgrade`), not location-level `proxy_set_header`.
- oauth2-proxy: scope adds `offline_access`, `OAUTH2_PROXY_COOKIE_REFRESH=10m`.
- After the Actions setup, revoke the setup PAT and remove the `machinekey` mount (review I3).
- Forged-header test (review C1): with **and** without a valid session cookie, through `/account/`,
  `/admin/`, `/oauth2/` and `/`, send every identity name plus `X-Real-IP`, `X-Forwarded-For`,
  `Authorization` and `X-Autobracket-Proxy`; an echo upstream shows that `Host: bracket.gsd.sh`, the real
  client IP and none of the forged values arrive. Plus direct requests to `172.17.0.1:8790` (401).

## 9. Discord "your set is up" pings (optional, last)

Per-event Discord webhook URL (`https://discord.com/api/webhooks/<id>/<token>` only) set by an organiser
(signed and admin web), stored on the event, never shown back except as "set". A background task every
5 s posts `<@discord_id> your set vs <opponent> is up (<round name>)` once per (set, player) for players
whose account has `notify` on, `allowed_mentions` restricted to that user. Failures are logged and
retried 3 times, then given up.

## 10. Open items

- `rawInformation` exact nesting and whether Zitadel v4.20.3 offers `updateHumanUser`/`updateUser` on
  later logins: confirmed by the ops task from a real login (the hook logs key names only).
- Hook transport: Zitadel → `http://172.17.0.1:8790` needs a ufw allow from `10.203.48.0/28` to 8790
  (owner OK needed; fallback: the public URL with `/hooks/zitadel/` proxied and `hook_peers` unset).
- `allowRegister=false` vs auto-creation: README step 7.
