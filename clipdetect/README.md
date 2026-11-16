# clipdetect — hype-moment detector for Slippi replays (prototype)

Phase (d) of the autobracket auto-clip work, early prototype. Reads one `.slp`
replay, reuses the combos / conversions / stocks that `@slippi/slippi-js`
computes, applies the desktop thresholds as tunable filters, and prints the
frame ranges worth clipping as JSON.

## Usage

```sh
cd clipdetect
npm ci                      # Node 20+, pins @slippi/slippi-js 9.1.3
node cli.js FILE.slp [--config thresholds.json] [--pretty]
node cli.js --print-defaults
```

Exit codes: `0` ok (possibly with zero clips), `1` the replay could not be read
or parsed (message on stderr, nothing on stdout), `2` bad usage or bad config.
Stdout carries exactly one JSON document; stderr is quiet on success.

### Output

```json
{
  "game": {
    "file": "x.slp", "slp_version": "3.17.0", "stage_id": 2, "stage": "Fountain of Dreams",
    "is_teams": false,
    "players": [{ "port": 1, "player_index": 0, "character_id": 19, "character": "Sheik",
                  "type": "human", "team_id": null, "start_stocks": 4,
                  "netplay_name": null, "connect_code": null, "display_name": "..." }],
    "first_frame": -123, "last_frame": 8709, "frame_count": 8833, "playable_frames": 8748,
    "duration_s": 145.8, "game_complete": true,
    "game_end": { "method": 2, "method_name": "game", "lras_initiator_port": null },
    "winner_ports": [3], "started_at": "2026-07-11T05:06:25.636Z", "played_on": "nintendont"
  },
  "clips": [
    { "kind": "combo|low_kill|edgeguard|comeback|zero_to_death",
      "player_port": 1, "victim_port": 3, "score": 0.586,
      "start_frame": 8219, "end_frame": 8709,
      "details": { "core_start_frame": 8339, "core_end_frame": 8626, "...": "per kind" } }
  ],
  "ranges": [ { "start_frame": 8219, "end_frame": 8709, "kinds": ["combo"], "score": 0.586 } ],
  "config": { "...": "the thresholds that were applied" }
}
```

* `start_frame` / `end_frame` are **padded** (2 s before, 1.5 s after the
  event, at 60 fps) and clamped to the replay's frame range (first frame is
  normally -123). The unpadded event is in `details.core_start_frame` /
  `details.core_end_frame`.
* `player_port` is the player who did the thing (attacker / winner),
  `victim_port` the one it was done to. Ports are 1-based (`player_index + 1`).
* `clips` keeps one entry per detected moment, so the same kill can appear as
  `low_kill`, `edgeguard` and `zero_to_death`. `ranges` is the merged list
  (overlapping or touching padded ranges joined) for cutting video.
* `score` is a 0..1 ranking hint within a kind, not a probability.
* Output is deterministic: clips are sorted by `start_frame`, then kind
  (combo, low_kill, edgeguard, comeback, zero_to_death), then ports.

### Thresholds

All keys of `--print-defaults` may be overridden by the `--config` JSON file
(unknown keys and wrong types are rejected, exit 2). `thresholds.example.json`
holds the defaults.

| kind | rule (defaults) | config keys |
|---|---|---|
| `combo` | a slippi-js combo with ≥ 5 landed moves **or** ≥ 40% damage, no stretch > 45 frames in which the victim is continuously actionable between two hits (any new hit, grab, throw, knockdown or tech resets the count, as in slippi-js's combo reset rule; at the default 45 it only rejects what slippi-js would already have split, so it is a knob for stricter values), and no single hit carrying ≥ 80% of the damage | `comboMinHits`, `comboMinPercent`, `comboMaxHitstunGapFrames`, `comboLargeHitFraction` (null disables) |
| `low_kill` | a stock lost at ≤ 60% to an opponent's hit (deaths with no killing conversion, i.e. self-destructs, are ignored) | `lowKillMaxPercent` |
| `edgeguard` | a kill whose last hit landed with **both** the victim and the killer beyond the stage's ledge x (post-frame positions on the last-hit frame). Ledge x per stage id is in `lib/stages.js`; only the six tournament stages are known, other stages never produce edgeguards | `edgeguardEnabled` |
| `comeback` | 1v1, game ended by stocks or time, and the winner was ≥ 2 stocks behind at some point. The clip is the final kill; the deficit moment is in `details.deficit_frame` | `comebackMinStockDeficit` |
| `zero_to_death` | a slippi-js conversion with ≥ 2 landed moves that starts at ≤ 10% and kills | `zeroToDeathMaxStartPercent`, `zeroToDeathMinHits` |
| padding | 120 frames before, 90 after | `padBeforeFrames`, `padAfterFrames` |

Scores: combo `0.5·min(1,hits/10) + 0.5·min(1,damage/100) (+0.15 if it killed)`;
low_kill `0.3 + 0.7·(1 − percent/lowKillMaxPercent)`; edgeguard
`0.6 + 0.4·min(1, (|victim x| − ledge x)/40)`; comeback `0.5 + 0.25·(deficit − min)`;
zero_to_death `0.8 + 0.2·min(1,hits/10)`; all clamped to 0..1.

## What is reused and from where

* **`@slippi/slippi-js` 9.1.3** (Project Slippi, **LGPL-3.0-or-later** — note:
  not MIT; every published version checked, 6.7.0 through 9.1.3, carries this
  licence). Used unmodified as a library for parsing, `getStats()` (the
  `ComboComputer`, `ConversionComputer`, `StockComputer` results, 45-frame
  `COMBO_STRING_RESET_FRAMES` leniency), `getWinners()`, the per-frame post
  data, and the `isDamaged` / `isGrabbed` / `isCommandGrabbed` action-state
  helpers used for the hitstun-gap check. Its combo computer only runs for
  2-player games, so doubles replays produce no combo-based clips.
  The node entry (`@slippi/slippi-js/node`) pulls in `enet`, whose asm.js build
  makes V8 print one "Invalid asm.js" warning; `cli.js` filters that warning.
* **Project Clippi** (`@vinceau/slp-realtime` 5.0.0, MIT, Vince Au). Its combo
  filter *is* importable (`checkCombo`, `ComboFilter`, the `Criteria` functions
  in `src/utils/combo/criteria.ts`) and peers with slippi-js ^9, but importing
  it drags in `rxjs` for a dozen lines of logic, and none of its criteria
  express our rules (hitstun gap, edgeguard, comeback, zero-to-death). So it is
  **not** a dependency. What was taken from it: the criteria-list shape and the
  `excludesLargeSingleHit` rule (a hit carrying ≥ 80% of a combo's damage means
  it was not a combo), re-implemented here as `comboLargeHitFraction` in
  `lib/detect.js`. Clippi's own defaults (60% minimum, must kill, exclude
  chain-grabs / wobbles) are not applied.
* Ledge x values in `lib/stages.js` are the commonly quoted main-platform edge
  coordinates for the six legal stages.

## Licence

This directory is part of autobracket and is licensed **AGPL-3.0-or-later**
like the rest of the project. It links `@slippi/slippi-js` (LGPL-3.0-or-later)
as an unmodified npm dependency, which the AGPL permits; the LGPL notice travels
with the package in `node_modules/@slippi/slippi-js/LICENSE`. Nothing from
Project Clippi is copied verbatim; the one rule borrowed is credited above.

## Tests

```sh
npm test                                   # node:test, unit + fixtures
AUTOBRACKET_FIXTURES=/path/to/fixtures npm test
```

`test/detect.test.js` drives `lib/detect.js` with small synthetic models.
`test/fixtures.test.js` runs the CLI over the real replay corpus named by
`AUTOBRACKET_FIXTURES` (default `/home/gd/autobracket-fixtures/autobracket-fixtures`,
the directory holding `MANIFEST.txt`) and skips when it is absent. Replays are
never copied into the repo.

## Known gaps (prototype)

* Edgeguards that end in a self-destruct (ledgehog, no hit) are invisible:
  every kill-based kind needs a slippi-js conversion/combo with `didKill`.
* Stock "comeback" is only tracked for 1v1; teams and FFA produce none.
* slippi-js parses the truncated fixture
  (`edge/truncated_cut60pct_own_items_bf.slp`) to only 99 frames (-123..-25)
  although the file holds ~60% of the game, so it yields no clips where the
  Rust `slpcheck` sees more of the stream. Garbage-header synthetic fixtures
  parse "successfully" with a handful of frames and no clips (exit 0).
* Scores are hand-tuned ranking hints; nothing has been validated against
  human judgement yet.
