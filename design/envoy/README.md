# Envoy UI kit (reference for the bracket page)

Gathered 2026-11-15 from the GD's Melee workspace so bracket.gsd.sh can match the game. Read-only
extraction; nothing here is shipped by the game.

## What it is

Envoy is **not a wasm mod UI or a design-system package**. It is a written design spec plus generated
art for a co-op roguelite mode, drawn at runtime by the game's own UI layer (flat quads, 9-slices, a font
atlas) from Lua scripts. There is no web or CSS source; `tokens.css` here is a transcription.

| piece | where |
|---|---|
| Envoy UI spec (wins over step sheets) | workspace repo `GurekamDhillon/gd-melee-workspace`, `menu/out_envoy/UI-SPEC.md` (local checkout branch `integration/2026-10-01/roguelite-100`) |
| Envoy art generator (SVG/HTML) | `menu/out_envoy/build_final.py`; reference screens `menu/out_envoy/screens/*.png` |
| Envoy mod (Lua) | game repo `GurekamDhillon/melee`, `pc/scripts/examples/envoy/` |
| Engine kit ("Atlas": menus, LAB, Hub) | workspace `menu/` (`menu/README.md`, `menu/pipeline/kit.py`, `menu/out_kit/kit.json`, `menu/atlas/tokens.json`, `menu/concepts/reunification-2026-10-06/c-atlas/tokens.css`); LAB API in `melee/docs/geno.md` s14 |

There are three looks in the workspace. For a page that matches the game's Envoy look, use **Party pack**
(chosen by the owner, Nov 10).

1. **Envoy Party pack** (this kit's default): flat sunny/violet, white "stickers" with a thick ink edge and
   a hard offset shadow, tilted cards, playful. Owner brief: fresh look, no old GD art.
2. **Atlas** (engine custom menus, LAB): calm dark UI, chamfered corners, ember focus, jade info.
3. **Hub** (the first main-menu prototype): cobalt faces, gold selection, one 14 degree shear
   (x' = x + (240-y)*0.25), ink `#0a0e18`, bone `#f2efe4`, gold `#f0b429`, per-section tints
   (versus `#1e3a8c`, solo `#6a4308`, collection `#5a2a6e`, options `#2e3640`, data `#1c5a4a`).

## Tokens (Party pack)

Full set in `tokens.css`. Key roles:

| role | hex |
|---|---|
| Sunny (cursor, hover, ready, accent) | `#FFD84A` |
| Grape ink (all outlines, shadows, text on white) | `#2A1A6E` |
| White (sticker face) | `#FFFFFF` |
| Coral (Player 1, warning) | `#FF4D6D` |
| Lagoon (Player 2) | `#00A99D` |
| Lilac (quiet text on white) / pale lilac (on dark) | `#5A4C99` / `#C9BFF5` |
| Deep coral (error, cannot pay) | `#C2183F` |
| Faded (done/locked), cream (empty seat) | `#DAD7E8`, `#FBF6DD` |
| Scene violet base / ground | `#4C3D9E` / `#33257C` |
| Veil | `rgba(26,16,74,.86)` (.66-.70 behind dialogs) |
| Only gradient: epic foil | 135deg `#FF8FB8 #FFE066 #8CF0C8 #7FD4FF #C9A0FF #FFB27A` |

Colour is never the only cue: port colours always carry "P1"/"P2" text.

**Type.** Headlines, names, numbers: Bricolage Grotesque 800, tracking about -0.02em (SIL OFL 1.1).
Sentences and labels: Source Sans 3 700 (SIL OFL 1.1). Sizes in game px (640x480 frame): strip 13/23,
hints 12.5 (button caps Bricolage 11), list rows 14-15, names 16, sticker headline 22-28, screen
title 24, results title 30 (white, 4.5 px ink edge), percent 40, **smallest 11**. Sentence case everywhere.
Atlas uses Source Sans 3 (600/700/900), Barlow Condensed (caps labels) and Hasklug (tabular numbers), all OFL.

**Shape.** 3 px ink edge (2.5 pills, 2 name tags); radius 12 general, 10 list rows, 9-11 pills; hard ink
shadow offset (3,4), never blurred; hovered sticker: sunny face, 3 px white ring outside the edge, shadow (4,6),
rotation 0. Idle cards tilt -4..+4 degrees; list rows under 1.3 degrees, alternating sign. Faded/other items: 34-36% opacity.
Dashed seat: cream face, dashed ink edge, no shadow. Ink sticker (foe/keystone): ink face, white edge, ink ring.
Atlas instead uses chamfers (8/5/3 px) on two opposite corners, no curves, no shadows.

**Spacing.** No named scale in Envoy; layouts are absolute pixel positions on a 640x480 frame. Observed rhythm:
4/8/12/16/24; screen margin 24 (strip at x24,y403; hints at x26,y449); row 28 high (big row 46-54); strip 30 high.
Atlas defines `s1..s5 = 4, 8, 12, 16, 24`.

**Signature motifs.** Stickers on paper (flat face, edge, hard shadow); tilted cards; sunny hover that
straightens and lifts; coral/lagoon player pointers and tags; bursts (16-point star, tilted 8 degrees) and
"Cleared" stamps; the run strip (row of marks: disc fight, triangle rest stop, star boss); dashed cream seats;
zigzag paper seam between player halves; flat violet scenery (hills, tents, rays); no backing panels.

**Motion** (60 fps; ease-out unless stated; 1 frame = 16.67 ms): hover on/off 4 frames; list rows enter
2 frames apart (3 for cards); sticker enter 6+3 frames, scale .85 to 1.04 to 1 with opacity 0 to 1; press scale 1 to .94 to 1
over 2+4; burst 4+3, scale 0 to 1.2 to 1; fade of others/screen leave 6 frames; hover moves 10-12 px right (rows) or up 4-8 px (cards).
Atlas: focus 80 ms, pane 100, tab 120, modal 140, note 160. The exact easing curve is not specified beyond "ease out".

## Screens

Spec sections are in UI-SPEC.md; reference images in `refs/` (original chrome only, invented placeholder names).

- **Lists and selection** (`refs/envoy-camp.png`): plain vertical list, rows 28 high, label left and value right
  (Source Sans 12 lilac); the hovered row becomes sunny, shifts 12 px right and straightens; a 28x26 block arrow
  in the player colour sits 20 px left of it; one cursor, Player 1 holds it. A strip under the list gives one
  sentence about the hovered item. The hint line is a white button cap plus words ("Up / Down Choose",
  "A Continue run", "B Main menu"). A hint names the result, never "OK" or "Select"; B is back or the safe answer.
- **Pick of three** (`refs/envoy-pick.png`): two halves, one list per player, own cursors, a timer disc.
- **Results** (`refs/envoy-results.png`): big title, run strip, four number stickers, "Unlocked" row, one button.
- **HUD** (`refs/envoy-fight.png`): corner stickers per player (tag, percent 40 px, bar, chips).
- **Map, keystone, pause**: `refs/envoy-map.png`, `-keystone.png`, `-pause.png`. Whole sticker sheet: `refs/parts-sheet.png`.
- **Main menu**: UI-SPEC 2.1 (Envoy is a sixth row of the retail menu; right side is loose stickers). Its mock
  shows retail placeholder rows, so it is not included here.
- **Bracket / tournament screens: none.** No Envoy or kit spec exists for a bracket;
  `docs/ART-BRIEF-menus.md` lists "the tournament bracket" as out of scope. **Round naming: none** (Envoy's own
  structure is Act 1-3, fights, rest stop, boss; unrelated to brackets). The in-game bracket menu's round naming
  follows this repo's `docs/api.md` unless a later note says otherwise. For web bracket styling, derive from the
  list row, sticker, run-strip (done = ink, next = coral inside an ink ring, to-do = white) and player-tag parts.

## Likely to change

Envoy is mid-development; UI-SPEC section 9 has open items. Most likely to move: the sunny/ink hex values, family
badge colours, motion timings, and everything Atlas (still a concept direction). Reference tokens by name rather
than copying hex values.

## Local-only references

Real in-game GIFs and screenshots of the Envoy UI (css1, envoyfix) show disc portraits and icons, so they are NOT
committed. They are in `E:\Projects\Melee Workspace\scratch\envoy-ui-refs\` on the desktop.
