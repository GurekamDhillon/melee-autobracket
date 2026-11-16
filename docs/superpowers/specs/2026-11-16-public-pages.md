# Public pages (phase 2a): design

Date: 2026-11-16
Status: scope approved by owner 2026-11-15 (`.superpowers/sdd/2026-11-15-verified-autobracket/phase2-scope-v2.md`, item 5(a)); this document makes the rulings below.

## Intent

The server today serves one page, the bracket viewer at `GET /`, and a JSON API. Phase 2a adds
the public face a small community needs between events: what is live, what is next and how to
enter, what happened, who plays here, and where the stream is. Everything is public and
read-only. No login, no accounts, no personal data beyond what the API already shows (a tag).

Out of scope, and must not be blocked by this design: accounts and key linking (2b), VODs and
the render queue (2c), clips (2d). Players will later link to Zitadel accounts; the pages here
key everything by tag and event id, and all JSON carries tags, so a later player id or account
link is an addition, not a change.

## Rulings made here

| Question | Ruling | Why |
|---|---|---|
| Shared assets vs one page with client routing | **Several pages, assembled on the server at startup** from shared files (`site.css`, `site.js`) and per-page fragments, each served as one self-contained HTML document with one inline `<style>` and one inline `<script>`. No new asset routes, no client-side router. | Keeps the existing hash-pinned CSP and its test unchanged in shape (one hash per block per page); every page is a plain URL that works on reload and in bookmarks; the test for "no external URLs" and "security headers" simply iterates the page list. Cost: the shared CSS/JS travels with every page (tens of KB, uncached between pages), acceptable for this traffic. |
| Old `/#N` bookmarks | The landing page redirects `#N` (digits only) to `/events/N` with `location.replace` before drawing anything. No `?event=` form. | The hash is the only form the old page produced (its picker wrote `location.hash`). |
| Event start time | New nullable `events.starts_at` (unix seconds) and `events.description` (text), set at create and by a new signed route. Displayed in the viewer's local time zone by the browser. | Events have no start time today; the schedule page needs one. |
| Completion time | Not stored. History orders by event id (newest first) and shows `starts_at`, else `created_at`, as the event's date. | Avoids touching the lifecycle code; the ordering is right for events created in order. |
| Placings | Computed from the bracket (pure function in the `bracket` crate), ties share a place (two 3rds, four 5ths). | The bracket is the source of truth for every result; nothing new is stored. |
| Player identity on pages | `/players/{tag}`, matched in any case, showing the canonical tag. Rekey (`/v1/admin/players/{old_key}/rekey`) keeps the tag, so URLs survive it. There is no rename today; when 2b adds accounts, a rename would change the URL and old links would 404 unless a stable player id or a redirect is added then. | Tags are the only public identity the API has. |
| Stream embed | `stream_url` in the server config (optional). `/stream` embeds it in an `<iframe>` and its CSP gains `frame-src <origin of stream_url>`; no other page gets that directive. Unconfigured: the page says no stream is configured. | The only external origin the site ever touches, and only on that one page, only when the organiser asked for it. |
| Admin update route | `POST /v1/admin/events/{id}/schedule` with `EventScheduleReq`: it **replaces** both schedule fields; an omitted or null field clears the stored value. Any event state. | A two-field PUT is simpler to specify, sign and test than a PATCH with "unset" semantics. |
| Times in the admin CLI | Unix seconds, with the hint `date -d '2026-11-20 20:00' +%s` in the help text. | No date-parsing dependency. |
| Players index membership | Every registered player, including those who never entered an event (zeros). | A registration is already public through sign-up lists. |

## Pages

All pages share the Envoy "Party pack" look of the current bracket page (tokens copied from
`design/envoy/tokens.css`; stickers with a 3 px ink edge and hard offset shadow; Bricolage
Grotesque 800 for display text and the self-hosted "Autobracket Text" 700 for text; dark violet
scene with the derived light variant; reduced-motion kills every animation and transition;
responsive down to 400 px). A sticker-strip navigation (Home, Schedule, History, Players,
Stream) sits under the title on every page; the current page carries `aria-current="page"`.
Every data value reaches the DOM through `textContent` (the `el()` helper); `innerHTML` is never
used with data; links are built with `encodeURIComponent` on the tag.

Pages that show live state (landing, event, stream) poll every 10 seconds and on
`visibilitychange`; schedule, history and players load once.

| Path | Page | Data |
|---|---|---|
| `GET /` | **Landing.** Redirects `#N` to `/events/N`. Sections: *Live now* (every `running` event: name linking to its bracket, its `Ready` sets as "A vs B · round · 1–0", and "Watch the stream" when `/v1/stream` has a URL); *Next up* (the `signup` event with the earliest `starts_at`, nulls last, then lowest id: name, local time and relative time, description, entrants, and *How to enter*: the two client commands with this server's origin filled in); *Latest results* (the first `/v1/history` entry: 1st, 2nd, 3rd and a link to the bracket); links to schedule, history, players. Empty server: "No events yet." | `/v1/events`, `/v1/events/{id}` for running events, `/v1/history`, `/v1/stream` |
| `GET /events/{id}` | **Bracket viewer**, the current page moved. The event picker navigates to `/events/{id}` instead of setting a hash. The header gains the start time (local) and description when present. A view that disappears (API 404 while polling) shows "This event no longer exists" and stops polling. The server answers 404 JSON for an unknown id and 400 JSON for a non-integer. Has a vertical layout for portrait screens (below). | `/v1/events`, `/v1/events/{id}` |
| `GET /schedule` | **Schedule.** *Happening now* (`running`) and *Coming up* (`signup`, ordered as the landing's "next up"); each: name linking to the bracket, local time with `<time datetime>` and a relative phrase ("in 3 days", "today at 20:00", "time to be announced", "was due 2 hours ago" for a past `starts_at` still in sign-up), description, entrants, format. *How to enter* block. Empty: "Nothing is scheduled." | `/v1/events` |
| `GET /history` | **History.** `complete` events newest first; each: name linking to the bracket, date, entrants, and placings up to 8th (ties share an ordinal: 1st, 2nd, 3rd, 3rd, 5th, 5th, 5th, 5th), then "and N more" linking to the bracket when the event had more than 8 entrants. Empty: "No completed events yet." | `/v1/history` |
| `GET /players` | **Players index.** Rows ordered by tag (case-insensitive): tag linking to the profile, events entered, titles, set record W–L. A search box filters rows client-side (substring, case-insensitive). | `/v1/players` |
| `GET /players/{tag}` | **Player profile.** Title: canonical tag; "playing since" month and year; totals (events, titles, sets W–L, games W–L); *Events* (newest first: name linking to the bracket, date, seed, place, sets, games); *Head to head* (opponents ordered by sets played, then tag: tag linking to their profile, sets W–L, games W–L). The server answers 404 JSON for an unknown tag. | `/v1/players/{tag}` |
| `GET /stream` | **Stream.** With a URL: an `<iframe title="Live stream" allow="autoplay; fullscreen" referrerpolicy="no-referrer">` of it, 16:9, plus "Open the stream in a new tab" (`rel="noopener"`). Without: "No stream is configured on this server." Under it, *Now playing*: the `Ready` sets of every `running` event (as on the landing), or "Nothing is being played right now." | `/v1/stream`, `/v1/events`, `/v1/events/{id}` |

Round names on the landing and stream pages use the same table as the bracket page
(`docs/api.md` section 7); the shared script holds it once.

### Bracket layout on portrait screens (owner addition, 2026-11-16)

The bracket has two layouts, chosen by the script and written to `#board`'s `data-layout`
(`horizontal` or `vertical`); the stylesheet keys on that attribute alone.

- **Horizontal** is today's layout: rounds side by side, the board scrolls sideways.
- **Vertical** stacks the rounds top to bottom, winners rounds first, then losers, then the
  grand final (the DOM order the page already draws), each as its round header followed by its
  sets as full-width cards. Hidden rounds and spacer cards stay hidden. Nothing on the page is
  wider than the viewport: no horizontal page scroll at 400 px.
- **Automatic choice:** `(orientation: portrait)` selects vertical; otherwise horizontal.
  Portrait is the exact condition under which a side-by-side bracket has the least room (the
  viewport is taller than it is wide), and it covers both a rotated monitor and a phone held
  upright; a width threshold would wrongly flatten a short landscape phone, where the sideways
  scroll of the horizontal board works as it does today. The page re-evaluates on an
  orientation change while no manual choice is stored.
- **Manual toggle:** two buttons, "Horizontal" and "Vertical" (`aria-pressed` marks the one in
  force), beside the event picker. A click applies the layout and stores it under
  `localStorage["bracket-layout"]`; reading and writing are each wrapped in `try`/`catch`, so a
  browser that refuses storage still gets the automatic choice. The stored choice overrides the
  orientation rule until changed.
- **Finding live sets:** a "Jump to live" button appears whenever a set is `Ready`; it scrolls
  the first live set into the middle of the view (`behavior: "auto"`, so reduced-motion users
  get no smooth scroll). Live cards keep their sunny face and "Live" flag in both layouts.

## Data model

Migration `0005_schedule.sql` (columns only, as the earlier migrations):

| Column | Type | Meaning |
|---|---|---|
| `events.starts_at` | `INTEGER NULL` | When the event is meant to start, unix seconds; NULL when not announced. |
| `events.description` | `TEXT NULL` | At most 280 printable characters (the `check_text` rule: no control characters, no line or paragraph separators). An empty or whitespace-only description is stored as NULL. |

`starts_at` must be between 0 and 4102444800 (2100-01-01) inclusive, else 400.

## API additions (all documented in `docs/api.md`; the JSON examples are `checked`)

| Method | Path | Who | Body | Success | Other |
|---|---|---|---|---|---|
| GET | `/` | public | — | 200 HTML landing page | — |
| GET | `/events/{id}` | public | — | 200 HTML bracket page | 400, 404 `no such event` |
| GET | `/schedule`, `/history`, `/players`, `/stream` | public | — | 200 HTML | — |
| GET | `/players/{tag}` | public | — | 200 HTML | 404 `no such player` |
| GET | `/v1/history` | public | — | 200 `[EventResult]`, complete events, newest first | — |
| GET | `/v1/players` | public | — | 200 `[PlayerSummary]`, by tag ignoring case | — |
| GET | `/v1/players/{tag}` | public | — | 200 `PlayerProfile` | 404 |
| GET | `/v1/stream` | public | — | 200 `StreamInfo` | — |
| POST | `/v1/admin/events/{id}/schedule` | organiser | `EventScheduleReq` | 204 | 400, 404 |

Types (in `crates/proto/src/types.rs`):

- `EventSummary` gains `starts_at: Option<i64>`, `description: Option<String>`,
  `created_at: i64` (all `#[serde(default)]` so older clients' JSON still decodes).
- `EventCreateReq` gains optional `starts_at: Option<i64>` and `description: Option<String>`.
- `EventScheduleReq { starts_at: Option<i64>, description: Option<String> }`, both optional in
  the request, both replaced by the request.
- `Placing { place: u32, tag: String }`.
- `EventResult { summary: EventSummary, placings: Vec<Placing> }`, placings ordered by place
  then seed; present only for complete events.
- `PlayerSummary { tag: String, events: u32, titles: u32, sets: [u32; 2] }` (`[won, lost]`).
- `PlayerEvent { event: EventSummary, seed: u32, place: Option<u32>, sets: [u32; 2], games: [u32; 2] }`.
- `HeadToHead { tag: String, sets: [u32; 2], games: [u32; 2] }`.
- `PlayerProfile { tag: String, since: i64, events: Vec<PlayerEvent>, head_to_head: Vec<HeadToHead> }`,
  events newest first, head-to-head by sets played descending then tag.
- `StreamInfo { url: Option<String> }`.

Counting rules (one place, `crates/server/src/results.rs`):

- A **set** counts for both players once its bracket set is `Done` with an entrant in both
  slots (`Bracket::was_played`), whatever decided it (played games, award, DQ). Sets of running
  events count as they finish.
- **Games** are the set's stored `wins` (`[slot 0, slot 1]`): the games played, including those
  before an award or DQ.
- **Events** counts entries in any state. **Titles** counts complete events the player won.
- **Place** comes from `Bracket::placings()` and exists only for complete events.
- `seed` is the entrant's index in the event's `tags` (0 = top seed); pages show seed + 1.

`Bracket::placings(&self) -> Option<Vec<u32>>` (`crates/bracket`): `None` unless the bracket is
complete; else one 1-based place per entrant id. The champion is 1st. Every other entrant's
*rank* is the highest-ranked set they lost, ranking sets by `(side, round)` with Winners <
Losers < GrandFinal; their place is 1 + the number of entrants with a strictly higher rank
(the champion counts as highest). Equal ranks share a place. This gives the usual 1, 2, 3, 3,
5, 5, 5, 5 for single elimination and 1, 2, 3, 4, 5, 5, 7, 7 for double elimination.

## Configuration

`stream_url: Option<String>` (default none). When set it must be an `http` or `https` URL with a
host, else the server refuses to start naming `stream_url`. `Config::stream_origin()` is its
`scheme://host[:port]`, the value of the stream page's `frame-src`. The example config carries
the key commented out. The README's deployment section explains it and that the embedded
origin is the only third party a visitor's browser contacts.

## Admin CLI

- `event create --name N --format F [--best-of N] [--starts-at UNIX] [--description TEXT]`
- `event schedule ID [--starts-at UNIX] [--description TEXT]` — sends both; an omitted flag
  clears that field.

## Security and quality constraints

- Every page: `X-Content-Type-Options: nosniff` and the CSP
  `default-src 'none'; script-src 'sha256-…'; style-src 'sha256-…'; connect-src 'self'; font-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`, with each hash computed from that page's one `<script>` and one `<style>` block. Only `/stream` adds `frame-src <origin>`, only when configured.
- No page's HTML contains `http://` or `https://`; the stream URL reaches the page as JSON.
- Fonts stay at `/assets/fonts/{file}`, unchanged.
- The `documented_routes_exist` test keeps every route in the `docs/api.md` table live; the
  `round_names_match_the_doc` test reads the shared script.
- Licence: AGPL-3.0-or-later, as the repository.
- Existing tests keep passing; `no_events_view_removes_the_unverified_line` is retired because
  the bracket page can no longer be shown without an event (the server 404s first). Its
  concern (a stale "unverified" line) is covered by the event page setting that notice on every
  render.

## Files

```
crates/server/migrations/0005_schedule.sql
crates/server/assets/shell.html            the document: head, title, nav, placeholders
crates/server/assets/site.css              all styles (the current page's, plus nav and new components)
crates/server/assets/site.js               shared helpers: el, getJson, notices, time formatting, round names, polling, nav
crates/server/assets/pages/{home,event,schedule,history,players,player,stream}.{html,js}
                                           (event.js also owns the layout toggle, storage and "Jump to live")
crates/server/src/routes/view.rs           page assembly, CSP per page, page handlers, fonts
crates/server/src/routes/results.rs        /v1/history, /v1/players, /v1/players/{tag}, /v1/stream
crates/server/src/results.rs               loading events once; placings, records, profiles (pure)
crates/bracket/src/lib.rs                  Bracket::placings
crates/proto/src/types.rs                  new and extended types
crates/server/src/{config,admin_cli}.rs    stream_url; CLI flags
crates/server/tests/{view,results,events,migrations}.rs
docs/api.md, README.md, deploy/autobracket.example.toml
```
