# Real-engine check

What happens when the verifier runs against the real game engine and real replay files rather
than the fake engine script and fabricated replays. Measured on 2026-11-15 on the VPS ("server",
8 vCPU, Debian 13). The tests are in `crates/server/tests/real_engine.rs`. Every test is
`#[ignore]`, so a plain `cargo test` skips them.

## Engine and inputs

- Engine: `github.com/GurekamDhillon/melee` branch `port/candidate12`, commit
  `0cd085d0b767715df4bbc21416d00ec146206846`, checked out at `~/melee-engine`. Built there with
  `tools/rust-port/linux/deps.sh`, then `cargo build --profile fast -p melee -p melee_relay` in
  `rust/`. That gives `~/melee-engine/build/cargo/fast/melee`.
  `melee version` prints `melee (rust) 0.1.0 development build`. `Slpcheck::build()` reports
  `melee (rust) 0.1.0 development build+339fd9aabad8`.
- Disc: `Super Smash Bros. Melee (USA) (En,Ja) (v1.02).iso`.
- Corpus: `~/autobracket-fixtures/autobracket-fixtures/`, 56 files listed in its `MANIFEST.txt`.
  None of these files is in this repository.
- What the server runs: `melee slpcheck --slp FILE --iso ISO --csv OUT.csv`. Until commit
  `361152e` it always set `SLPCHECK_VANILLA_SPAWN=1`. Since then it sets the variable only for
  replays without a Gecko list and removes it for every other file (F1).

## Commands run

```sh
. ~/.cargo/env
cd /home/gd/ab-wt/real
AB_ENGINE=$HOME/melee-engine/build/cargo/fast/melee \
AB_ISO="$HOME/Super Smash Bros. Melee (USA) (En,Ja) (v1.02).iso" \
AB_FIXTURES=$HOME/autobracket-fixtures/autobracket-fixtures \
  cargo test -p autobracket-server --test real_engine -- --ignored --nocapture --test-threads=2
```

The first run (before the fixes) took 355 s in total and ended `6 passed; 2 failed`. After
commit `361152e` (F1 and F2 fixed) the same command took 328 s and ended `8 passed; 0 failed`.

These commands were run outside the tests to support the findings:

```sh
# The engine's view of each file's Gecko list (type, address, lines, hash):
melee slpcheck --slp FILE --iso ISO --gecko --frames 1
# A missing disc:
SLPCHECK_VANILLA_SPAWN=1 melee slpcheck --slp netplay_host.slp --iso /no/such.iso --csv b.csv   # exit 1
# The raw element's framing: bytes 11..15 of the file hold the declared raw length (big-endian u32).
```

## Results by test

| test | result | what it shows |
|---|---|---|
| `fixture_pairs_raw_elements_equal` | pass | Both own-netplay pairs (`netplay_*` and `falcon_fox_fd_lag40ms_loss5_*`) have the same `raw` element, and here the whole files are byte-identical too. The raw elements are 1010736 and 1070068 bytes. |
| `fixture_parses_with_expected_facts` | pass | `netplay_host.slp` reads as: Slippi 3.18.0, two Human players (p1 Falcon c0, p2 Fox c2) with 4 stocks each, stage 32, 60 s timer, no Gecko list, `last_frame` 2974, Game End method 2, `winner_port == Some(1)`. |
| `slpcheck_says_match_on_own_build_exports` | pass | All 5 own-netplay files come back `Match` (1.2-3.7 s each). |
| `pre_judge_then_finish_is_verified` | pass | For both pairs, `pre_judge` gives `NeedsResim { winner_port: 1, .. }`. The real `Slpcheck` gives `Match`, and `finish(.., false)` gives `Verified`. |
| `disconnect_host_only_is_single` | pass | One upload gives `Single`. The file itself: Game End method **7** with **no LRAS initiator**, `last_frame` 1220, both players on 3 stocks and 0 %. `winner_port` is `None`. If both peers had uploaded it, `pre_judge` would give `Undecided`. `slpcheck` gives `Match`. |
| `corpus_sweep_never_panics_and_agrees_with_manifest` | pass (failed before `361152e`) | Nothing panics, neither our reader nor the engine. All 42 files the manifest lists as `match` come back `Match` from `Slpcheck::check`. Before the fix, 31 of them came back `Diverged@-123` (finding F1). |
| `missing_disc_is_unavailable` (added) | pass (failed before `361152e`) | `Slpcheck::new` refuses a missing disc up front, naming the path. A disc that disappears after start-up (a symlink to the image, removed after `new`) gives `Unavailable("boot-error: <disc>: No such file or directory (os error 2)")`, which `finish` turns into `PeerOnly`. Before the fix it gave `Crashed(..)` and so `VerifiedInexact` (finding F2). |
| `early_end_slack_is_calibrated` | pass | The gap is 0 for every run (see "Early end" below). |

## Sweep

How to read the table. The table is from the run after commit `361152e`; the first run's
`Slpcheck::check` column, with the variable always set, is described under F1.
"Slpcheck::check" is the server's own code path, timed: the variable is set only for files
without a Gecko list. "Engine without SLPCHECK_VANILLA_SPAWN" is a direct engine run of the same
file with that variable removed. The manifest's measurements were taken without it. A file that
`replay::parse` rejects is never re-simulated by the server; for those files the third column
shows, in parentheses, what the engine says when it runs the file directly the way the server
would (without the variable).
`Diverged@F:pN:field` means the first divergence was at frame F, port N, in that field.

| file | replay::parse | Slpcheck::check (server: SLPCHECK_VANILLA_SPAWN=1 only without a Gecko list) | engine without SLPCHECK_VANILLA_SPAWN | manifest status |
|---|---|---|---|---|
| own-netplay/netplay_host.slp | Ok(2974) | Match (2.48s) | match (2.27s) | match (--full, 6196 fighter-frames) |
| own-netplay/netplay_guest.slp | Ok(2974) | Match (2.54s) | match (2.40s) | match (--full) |
| own-netplay/falcon_fox_fd_lag40ms_loss5_host.slp | Ok(3156) | Match (2.59s) | match (2.47s) | match |
| own-netplay/falcon_fox_fd_lag40ms_loss5_guest.slp | Ok(3156) | Match (2.53s) | match (2.40s) | match |
| own-netplay/falcon_fox_fd_guest_quit_25s_host.slp | Ok(1220) | Match (1.15s) | match (1.06s) | match |
| own-offline/pairs_falcon9-fox9_bf.slp | Ok(3600) | Match (2.78s) | match (2.91s) | match |
| own-offline/pairs_marth9-fox9_bf.slp | Ok(3241) | Match (2.61s) | match (2.65s) | match |
| own-offline/pairs_sheik9-marth9_bf.slp | Ok(3600) | Match (3.18s) | match (3.07s) | match |
| own-offline/levels_fox3-falco3_fd.slp | Ok(3600) | Match (3.09s) | match (2.98s) | match |
| own-offline/c_build_pad_falcon_fox_fd.slp | Err(Truncated) | (engine: diverged@-123:p1:state_frame) (0.15s) | diverged@-123:p1:state_frame (0.15s) | diverged (frame -123, state_frame) |
| dolphin/20476_1a66cc9f-d363-4ee5-a1ac-c6b77d5e2c7c.slp | Ok(10443) | Match (14.22s) | match (13.46s) | match |
| dolphin/20476_03c52089-9e8e-4425-885c-e32af62c14b2.slp | Ok(6297) | Match (7.97s) | match (8.46s) | match |
| dolphin/20476_0b318d95-f3c1-4884-8a99-67f92c0d8796.slp | Ok(14291) | Match (12.43s) | match (12.27s) | match |
| dolphin/20476_1b5afde1-cc91-4f82-b5a7-543a8cbfa25d.slp | Ok(13964) | Match (10.79s) | match (10.82s) | match |
| dolphin/20476_01c6d260-8e33-4031-ab2c-b12e46a7aa8d.slp | Ok(11169) | Match (8.15s) | match (8.09s) | match |
| dolphin/20476_18e83ec4-c553-48d5-98b5-8e75d6d01919.slp | Ok(8594) | Match (7.36s) | match (7.48s) | match |
| tournament/20476_08f9e70f-2f7e-418b-8121-699ef271aacd.slp | Ok(8709) | Match (9.75s) | match (9.86s) | match |
| tournament/20476_147832a2-24d4-4dad-bdad-d0608f83f579.slp | Ok(5843) | Match (5.11s) | match (5.01s) | match |
| tournament/20476_23b2b24b-715e-43a7-be16-f8817af0c695.slp | Ok(11153) | Match (8.78s) | match (8.82s) | match |
| tournament/20476_10909063-82b4-42b2-8ba7-08d1be1e1526.slp | Ok(14531) | Match (10.65s) | match (10.85s) | match |
| tournament/20476_12ea3f66-3b37-4e18-ac46-7142c1eee126.slp | Ok(7871) | Match (6.37s) | match (6.44s) | match |
| tournament/20476_1c57c3ef-c2ab-4548-b7ab-fb9aba553293.slp | Ok(6381) | Match (5.51s) | match (5.45s) | match |
| tournament/20476_1285c0cd-acb2-4512-b141-ffdc938d5f23.slp | Ok(10883) | Match (11.69s) | match (11.79s) | match |
| tournament/20476_28bf8e4c-5126-40bc-aca3-9b6f5fc6f695.slp | Ok(9026) | Match (8.35s) | match (8.64s) | match |
| tournament/20476_265b95dd-355b-4519-9726-1688e7e6e383.slp | Ok(5342) | Match (4.95s) | match (4.80s) | match |
| tournament/20476_1c7b2d10-8872-4f13-8cee-b8a349e14d51.slp | Ok(6272) | Match (5.18s) | match (4.97s) | match |
| tournament/20476_2619fcd0-bbb3-4f8c-8865-0a53b4f1ca5f.slp | Ok(8814) | Match (6.55s) | match (6.49s) | match |
| tournament/20476_20a1f939-1e39-4aff-ae98-1939d76999ff.slp | Ok(15186) | Match (11.80s) | match (11.56s) | match |
| tournament/20476_1a2af8a2-e21d-40d4-97ae-8e095d1a075a.slp | Ok(11895) | Match (14.91s) | match (14.72s) | match |
| tournament/20476_2df0f989-fee1-4e0d-aa1f-f6c5cffdd9a3.slp | Ok(7391) | Match (5.67s) | match (6.01s) | match |
| tournament/20476_325cee5d-46aa-475d-8f5b-fee967e2cc4a.slp | Ok(8596) | Match (7.49s) | match (8.02s) | match |
| tournament/20476_0a7cb485-47b1-4ecb-8d02-079c219272cc.slp | Ok(21515) | Match (15.79s) | match (15.55s) | match |
| tournament/20476_0ecf0448-1368-408b-ba44-951349a79c42.slp | Ok(12688) | Match (9.66s) | match (10.25s) | match |
| tournament/20476_19fd3cbb-54cc-4805-b479-f7b01d1c32e5.slp | Ok(12613) | Match (12.56s) | match (12.50s) | match |
| tournament/20476_03b84423-9e9d-43c1-b4e6-b591a7a3fbaa.slp | Ok(8421) | Match (9.46s) | match (9.69s) | match |
| tournament/20476_06200225-6243-4da9-a90d-6e40cb611d3a.slp | Ok(9947) | Match (8.11s) | match (7.75s) | match |
| edge/lras_20476_14664b32_p1_initiator.slp | Ok(7212) | Match (5.58s) | match (5.38s) | match |
| edge/lras_20689_fa028fa2_p4_initiator.slp | Ok(9069) | Match (7.64s) | match (7.47s) | match |
| edge/timeout_end_cpu_bf.slp | Ok(3600) | Match (3.09s) | match (2.91s) | match |
| edge/doubles_4p_teams_cpu_fd.slp | Ok(3600) | Match (5.24s) | match (4.74s) | match |
| edge/illegal_stage_castle_cpu.slp | Ok(3600) | Match (3.62s) | match (3.44s) | match |
| edge/illegal_stage_yoshis_story_cpu.slp | Ok(2380) | Match (2.24s) | match (2.19s) | match |
| edge/mex_check_luigi_replay.slp | Ok(98) | Match (0.32s) | match (0.31s) | match |
| edge/truncated_cut60pct_own_items_bf.slp | Err(Truncated) | (engine: diverged@-123:p1:percent) (0.09s) | diverged@-123:p1:percent (0.09s) | diverged (frame -123, percent) |
| synthetic/ok00_modern.slp | Ok(-84) | Crashed(refused: p1: team 161 is outside 0..=2) (0.01s) | refused: p1: team 161 is outside 0..=2 (0.01s) | refused: p1: team 161 is outside 0..=2 |
| synthetic/ok03_old.slp | Ok(-100) | Crashed(refused: p1: team 123 is outside 0..=2) (0.01s) | refused: p1: team 123 is outside 0..=2 (0.01s) | refused: p1: team 123 is outside 0..=2 |
| synthetic/oneframe.slp | Ok(-123) | Crashed(refused: p1: team 34 is outside 0..=2) (0.01s) | refused: p1: team 34 is outside 0..=2 (0.01s) | refused: p1: team 34 is outside 0..=2 |
| synthetic/below123.slp | Ok(-198) | Crashed(refused: p1: team 201 is outside 0..=2) (0.01s) | refused: p1: team 201 is outside 0..=2 (0.01s) | refused: p1: team 201 is outside 0..=2 |
| synthetic/trunc1.slp | Err(Truncated) | (engine: refused: the replay has no Game Start block) (0.01s) | refused: the replay has no Game Start block (0.01s) | refused: the replay has no Game Start block |
| synthetic/zerolen.slp | Err(Truncated) | (engine: refused: p1: team 231 is outside 0..=2) (0.01s) | refused: p1: team 231 is outside 0..=2 (0.01s) | refused: p1: team 231 is outside 0..=2 |
| synthetic/badtable_cmd.slp | Err(BadEventSizes) | (engine: parse-error: the first event is not Event Payloads (0x35)) (0.01s) | parse-error: the first event is not Event Payloads (0x35) (0.01s) | parse-error: the first event is not Event Payloads (0x35) |
| synthetic/nostart.slp | Err(MissingGameStart) | (engine: refused: the replay has no Game Start block) (0.01s) | refused: the replay has no Game Start block (0.01s) | refused: the replay has no Game Start block |
| synthetic/rule_frames.slp | Ok(-120) | Crashed(refused: p1: team 116 is outside 0..=2) (0.01s) | refused: p1: team 116 is outside 0..=2 (0.01s) | refused: p1: team 116 is outside 0..=2 |
| synthetic/skin.slp | Ok(-119) | Crashed(refused: p1: team 212 is outside 0..=2) (0.01s) | refused: p1: team 212 is outside 0..=2 (0.01s) | refused: p1: team 212 is outside 0..=2 |
| synthetic/flip05.slp | Ok(-112) | Crashed(refused: p1: team 87 is outside 0..=2) (0.01s) | refused: p1: team 87 is outside 0..=2 (0.01s) | refused: p1: team 87 is outside 0..=2 |
| synthetic/ctl_ok.slp | Ok(-120) | Crashed(refused: p1: team 37 is outside 0..=2) (0.01s) | refused: p1: team 37 is outside 0..=2 (0.01s) | refused: p1: team 37 is outside 0..=2 |

### Facts of the readable files

`legality` and `pre_judge` are evaluated with `Ruleset::default()`: 4 stocks, 480 s,
stages 2/3/8/28/31/32. For `pre_judge`, the file is uploaded as both halves of a pair.

| file | players | stage | timer | last_frame | gecko | Game End | winner_port | legality (default rules) | pre_judge (pair of itself, default rules) |
|---|---|---|---|---|---|---|---|---|---|
| own-netplay/netplay_host.slp | p1:c0:hu:4st p2:c2:hu:4st | 32 | 60 | 2974 | no | 2 | Some(1) | timer | Final(Illegal("timer")) |
| own-netplay/netplay_guest.slp | p1:c0:hu:4st p2:c2:hu:4st | 32 | 60 | 2974 | no | 2 | Some(1) | timer | Final(Illegal("timer")) |
| own-netplay/falcon_fox_fd_lag40ms_loss5_host.slp | p1:c0:hu:4st p2:c2:hu:4st | 32 | 60 | 3156 | no | 2 | Some(1) | timer | Final(Illegal("timer")) |
| own-netplay/falcon_fox_fd_lag40ms_loss5_guest.slp | p1:c0:hu:4st p2:c2:hu:4st | 32 | 60 | 3156 | no | 2 | Some(1) | timer | Final(Illegal("timer")) |
| own-netplay/falcon_fox_fd_guest_quit_25s_host.slp | p1:c0:hu:4st p2:c2:hu:4st | 32 | 60 | 1220 | no | 7 | None | timer | Final(Illegal("timer")) |
| own-offline/pairs_falcon9-fox9_bf.slp | p1:c0:cpu:4st p2:c2:cpu:4st | 31 | 60 | 3600 | yes | 1 | None | players | Final(Illegal("players")) |
| own-offline/pairs_marth9-fox9_bf.slp | p1:c9:cpu:4st p2:c2:cpu:4st | 31 | 60 | 3241 | yes | 2 | None | players | Final(Illegal("players")) |
| own-offline/pairs_sheik9-marth9_bf.slp | p1:c19:cpu:4st p2:c9:cpu:4st | 31 | 60 | 3600 | yes | 1 | None | players | Final(Illegal("players")) |
| own-offline/levels_fox3-falco3_fd.slp | p1:c2:cpu:4st p2:c20:cpu:4st | 32 | 60 | 3600 | yes | 1 | None | players | Final(Illegal("players")) |
| dolphin/20476_1a66cc9f-d363-4ee5-a1ac-c6b77d5e2c7c.slp | p1:c19:hu:4st p4:c1:hu:4st | 2 | 480 | 10443 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| dolphin/20476_03c52089-9e8e-4425-885c-e32af62c14b2.slp | p1:c18:hu:4st p4:c9:hu:4st | 3 | 480 | 6297 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| dolphin/20476_0b318d95-f3c1-4884-8a99-67f92c0d8796.slp | p1:c2:hu:4st p4:c16:hu:4st | 8 | 480 | 14291 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| dolphin/20476_1b5afde1-cc91-4f82-b5a7-543a8cbfa25d.slp | p1:c9:hu:4st p4:c7:hu:4st | 28 | 480 | 13964 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| dolphin/20476_01c6d260-8e33-4031-ab2c-b12e46a7aa8d.slp | p1:c20:hu:4st p2:c19:hu:4st | 31 | 480 | 11169 | yes | 2 | Some(2) | gecko | Final(Illegal("gecko")) |
| dolphin/20476_18e83ec4-c553-48d5-98b5-8e75d6d01919.slp | p1:c14:hu:4st p4:c20:hu:4st | 32 | 480 | 8594 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_08f9e70f-2f7e-418b-8121-699ef271aacd.slp | p1:c19:hu:4st p3:c9:hu:4st | 2 | 480 | 8709 | yes | 2 | Some(3) | players | Final(Illegal("players")) |
| tournament/20476_147832a2-24d4-4dad-bdad-d0608f83f579.slp | p1:c0:hu:4st p4:c9:hu:4st | 3 | 480 | 5843 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_23b2b24b-715e-43a7-be16-f8817af0c695.slp | p1:c20:hu:4st p2:c19:hu:4st | 8 | 480 | 11153 | yes | 2 | Some(2) | gecko | Final(Illegal("gecko")) |
| tournament/20476_10909063-82b4-42b2-8ba7-08d1be1e1526.slp | p1:c2:hu:4st p4:c15:hu:4st | 28 | 480 | 14531 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| tournament/20476_12ea3f66-3b37-4e18-ac46-7142c1eee126.slp | p1:c9:hu:4st p4:c20:hu:4st | 31 | 480 | 7871 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| tournament/20476_1c57c3ef-c2ab-4548-b7ab-fb9aba553293.slp | p1:c2:hu:4st p3:c0:hu:4st | 32 | 480 | 6381 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| tournament/20476_1285c0cd-acb2-4512-b141-ffdc938d5f23.slp | p1:c19:hu:4st p4:c0:hu:4st | 2 | 480 | 10883 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| tournament/20476_28bf8e4c-5126-40bc-aca3-9b6f5fc6f695.slp | p1:c22:hu:4st p4:c12:hu:4st | 3 | 480 | 9026 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| tournament/20476_265b95dd-355b-4519-9726-1688e7e6e383.slp | p2:c0:hu:4st p4:c9:hu:4st | 8 | 480 | 5342 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_1c7b2d10-8872-4f13-8cee-b8a349e14d51.slp | p2:c20:hu:4st p4:c2:hu:4st | 28 | 480 | 6272 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_2619fcd0-bbb3-4f8c-8865-0a53b4f1ca5f.slp | p2:c0:hu:4st p3:c20:hu:4st | 31 | 480 | 8814 | yes | 2 | Some(3) | players | Final(Illegal("players")) |
| tournament/20476_20a1f939-1e39-4aff-ae98-1939d76999ff.slp | p2:c19:hu:4st p3:c9:hu:4st | 32 | 480 | 15186 | yes | 2 | Some(2) | players | Final(Illegal("players")) |
| tournament/20476_1a2af8a2-e21d-40d4-97ae-8e095d1a075a.slp | p1:c22:hu:4st p4:c12:hu:4st | 2 | 480 | 11895 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_2df0f989-fee1-4e0d-aa1f-f6c5cffdd9a3.slp | p1:c20:hu:4st p4:c18:hu:4st | 3 | 480 | 7391 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| tournament/20476_325cee5d-46aa-475d-8f5b-fee967e2cc4a.slp | p1:c2:hu:4st p4:c22:hu:4st | 8 | 480 | 8596 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_0a7cb485-47b1-4ecb-8d02-079c219272cc.slp | p1:c20:hu:4st p4:c16:hu:4st | 28 | 480 | 21515 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_0ecf0448-1368-408b-ba44-951349a79c42.slp | p1:c2:hu:4st p4:c16:hu:4st | 28 | 480 | 12688 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_19fd3cbb-54cc-4805-b479-f7b01d1c32e5.slp | p1:c20:hu:4st p4:c7:hu:4st | 2 | 480 | 12613 | yes | 2 | Some(1) | players | Final(Illegal("players")) |
| tournament/20476_03b84423-9e9d-43c1-b4e6-b591a7a3fbaa.slp | p1:c2:hu:4st p4:c0:hu:4st | 2 | 480 | 8421 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| tournament/20476_06200225-6243-4da9-a90d-6e40cb611d3a.slp | p1:c2:hu:4st p4:c19:hu:4st | 32 | 480 | 9947 | yes | 2 | Some(4) | players | Final(Illegal("players")) |
| edge/lras_20476_14664b32_p1_initiator.slp | p1:c2:hu:4st p2:c18:hu:4st | 32 | 480 | 7212 | yes | 7 lras=p1 | Some(2) | gecko | Final(Illegal("gecko")) |
| edge/lras_20689_fa028fa2_p4_initiator.slp | p2:c19:hu:4st p4:c12:hu:4st | 32 | 480 | 9069 | yes | 7 lras=p4 | Some(2) | players | Final(Illegal("players")) |
| edge/timeout_end_cpu_bf.slp | p1:c0:cpu:4st p2:c20:cpu:4st | 31 | 60 | 3600 | yes | 1 | None | players | Final(Illegal("players")) |
| edge/doubles_4p_teams_cpu_fd.slp | p1:c2:cpu:4st p2:c20:cpu:4st p3:c9:cpu:4st p4:c15:cpu:4st | 32 | 60 | 3600 | yes | 1 | None | players | Final(Illegal("players")) |
| edge/illegal_stage_castle_cpu.slp | p1:c5:cpu:4st p2:c13:cpu:4st | 4 | 60 | 3600 | yes | 1 | None | players | Final(Illegal("players")) |
| edge/illegal_stage_yoshis_story_cpu.slp | p1:c2:cpu:4st p2:c20:cpu:4st | 16 | 60 | 2380 | yes | 2 | None | players | Final(Illegal("players")) |
| edge/mex_check_luigi_replay.slp | p1:c0:hu:4st p2:c7:hu:4st | 2 | 480 | 98 | yes | 7 lras=p1 | Some(2) | gecko | Final(Illegal("gecko")) |
| synthetic/ok00_modern.slp | p1:c11:hu:89st p2:c22:cpu:111st | 2 | 185312588 | -84 | no | 7 lras=p1 | None | players | Final(Illegal("players")) |
| synthetic/ok03_old.slp | p1:c13:hu:209st p2:c7:hu:33st p3:c2:hu:223st p4:c5:hu:37st | 31 | 1474033028 | -100 | no | 7 lras=p1 | None | players | Final(Illegal("players")) |
| synthetic/oneframe.slp | p1:c12:hu:95st p2:c2:hu:42st | 31 | 2948534367 | -123 | no | 7 lras=p1 | Some(2) | teams | Final(Illegal("teams")) |
| synthetic/below123.slp | p1:c25:hu:194st p2:c5:hu:64st | 31 | 4109755440 | -198 | no | 7 lras=p1 | Some(2) | teams | Final(Illegal("teams")) |
| synthetic/rule_frames.slp | p1:c13:hu:57st p2:c1:hu:240st | 31 | 1182761136 | -120 | no | 7 lras=p1 | Some(2) | teams | Final(Illegal("teams")) |
| synthetic/skin.slp | p1:c22:hu:175st p2:c7:hu:152st | 31 | 2511304077 | -119 | no | 7 lras=p1 | Some(2) | teams | Final(Illegal("teams")) |
| synthetic/flip05.slp | p1:c10:hu:205st p2:c13:hu:238st | 31 | 3590715794 | -112 | no | 7 lras=p1 | Some(2) | teams | Final(Illegal("teams")) |
| synthetic/ctl_ok.slp | p1:c17:hu:238st p2:c20:hu:213st | 31 | 704371462 | -120 | no | 7 lras=p1 | Some(2) | teams | Final(Illegal("teams")) |

A Gecko list is present in 37 of the 50 files our reader can read.

## Timing

- `Slpcheck::check`, the server's path, over 50 runs, after `361152e`: min 0.01 s, median
  5.51 s, max 15.79 s. (Before the fix the median was 0.14 s, because most files stopped at their
  first divergence at frame -123, F1.) The minimum is the synthetic files the engine refuses.
- Full-length runs (engine status `match`, so the whole replay was simulated), 42 runs: min
  0.32 s, median 6.38 s, max 15.70 s. Throughput is 715-1400 frames/s, about 1150 frames/s
  overall, which is roughly **19x real time on one core**. The longest corpus game (21639 frames,
  about 6 minutes) took 15.7 s. A full 8-minute timeout game would take about 25 s. The
  own-netplay games (about 3100 frames) take 2.4-3.7 s, of which about 0.3 s is start-up. The
  design's planning figure was 40x; the measured figure here is about half that.
- Whole sweep test: 331.5 s, sequential. That is the 50 server runs plus 56 runs without the
  variable. The whole test binary took 355 s at `--test-threads=2`. After `361152e`, with the
  server's runs now full length too: sweep 302.1 s, whole binary 328 s. (Full-length runs
  without the variable in that run: 42 runs, min 0.31 s, median 6.49 s, max 15.55 s.)

## Early end

`replay_last_frame - csv_last_frame`:

- Server settings, every file the server would read as `Match` or `EarlyEnd`: after `361152e`
  (variable only without a Gecko list) 42 files, min 0, median 0, max 0. Before the fix
  (variable always set) only 11 files reached `Match`, also min 0, median 0, max 0.
- Without the variable, every complete `match` run: 42 files, including all 28 Slippi console
  recordings and the two LRAS quit-outs. Min 0, median 0, max 0.

On a complete run the engine's CSV `last_frame` equals `replay::parse`'s `last_frame` exactly.
`EARLY_END_SLACK_FRAMES = 0` (`resim.rs:68`) is consistent with every honest game in the corpus,
so the constant is unchanged. The corpus has no game in which the engine's match ended early.

## Findings

**F1 (fixed in `361152e`). The verifier sets `SLPCHECK_VANILLA_SPAWN=1` for every file. Every
replay that carries Slippi's Neutral Spawn codes then diverges at frame -123.**
Before the fix: `crates/server/src/resim.rs:62` and `:136` (`.env(SPAWN_ENV, "1")`,
unconditional).
The engine only applies Slippi's replay rules, Neutral Spawn included, when the variable is unset
(`~/melee-engine/rust/melee/src/slpcheck.rs:505-508`). The engine documents the variable as being
for "the port's own .slp exports, played without it".

- Shown by: 31 files. All 6 `dolphin/`, 18 of 20 `tournament/`, `edge/lras_20689_*`,
  `edge/timeout_end_cpu_bf.slp`, `edge/doubles_4p_teams_cpu_fd.slp`, and
  `own-offline/pairs_*_bf.slp`. Example: `tournament/20476_0a7cb485-...slp`.
- Expected: `Match` (the manifest says so). Actual: `Diverged { frame: -123, field: "x" }`, or
  `y` on `1c7b2d10`.
- Without the variable, all 42 manifest-`match` files come back `match`. That includes all five
  own-netplay exports, which match either way.
- The other 11 match even with the variable set. They are FD or other stages where vanilla
  and neutral spawns coincide, plus Yoshi's Story in `23b2b24b`.

The engine's own notes (`docs/rust-port/notes/net.md:226-228`) say own exports need the variable
on stages whose vanilla spawns differ from neutral ones: "FD matched ... with Neutral Spawn on,
Battlefield did not". Every own-netplay fixture is on FD, so the corpus cannot show that case.

Fix (`361152e`): `Slpcheck::check` reads the file and sets the variable only when
`replay::parse` succeeds and reports no Gecko list, which is exactly the own-build exports.
Otherwise it removes the variable (the engine reacts to its presence, so an inherited value must
not leak through). A file that cannot be read or parsed runs without it, the engine's own
default. After the fix all 42 manifest-`match` files come back `Match` from `Slpcheck::check`,
including the three `own-offline/pairs_*_bf.slp` files (Battlefield, with a Gecko list), and all
five own-netplay exports still match. Before the fix the effect was masked in production,
because `legality` rejects any Gecko list (F3) and those replays never reach resim; it would
have become a real fault the moment Slippi replays were admitted.
Missing fixture: an own-netplay export on Battlefield or another non-FD stage, to confirm that
own exports need the variable there.

**F2 (fixed in `361152e`). A missing or unreadable disc image is reported as `Crashed`, so the
game counts as `VerifiedInexact`.**
Before the fix, `crates/server/src/resim.rs:164` treated only exit code 2 as `Unavailable`.
The engine exits 2 only for usage errors: no `--slp`, no `--iso`, or an unreadable `.slp`
(`slpcheck.rs:373/378/385`). A disc that cannot be opened makes it write a CSV row
`boot-error: <disc>: No such file or directory` and exit 1 (`slpcheck.rs:528`, `:762-766`).
`outcome_from_csv` then mapped any non-`match`/`diverged` status to `Crashed`
(`resim.rs:388`), and `finish` maps `Crashed` to `VerifiedInexact`
(`crates/server/src/judge.rs:98-104`).

- Shown by: `missing_disc_is_unavailable`, using `own-netplay/netplay_host.slp`.
- Expected: `Unavailable(..)`, which becomes `PeerOnly`. Actual (before the fix):
  `Crashed("boot-error: <disc>: No such file or directory (os error 2)")`, which becomes
  `VerifiedInexact`.
- The test `resim.rs::run_missing_disc_is_unavailable` passed before the fix only because the
  fake engine script exited 2.
- Fix (`361152e`): a status whose first word is `boot-error` becomes `Unavailable(<status>)`.
  `Slpcheck::new` also refuses, naming the path, an engine that cannot be executed and a disc
  image that is not a readable regular file (it opens the image and reads a few bytes), so both
  `serve` in `in_process` mode and `verify-worker` fail at start-up. A disc that disappears after
  start-up still comes back `Unavailable` through the `boot-error` row. The fake engine's
  `nodisc` mode now imitates the real engine (CSV row `boot-error: ...`, exit 1); a separate
  `usage` mode keeps the exit-2 case, and both give `Unavailable`.

The same mapping raised a question: the engine's `refused: ...` (it will not simulate the file)
and `parse-error: ...` statuses also become `Crashed`, and so `VerifiedInexact` in non-strict
mode. In the corpus only synthetic files reach those statuses, and `legality` stops all of them
first. **Ruling:** they describe the replay (the engine will not or cannot simulate this file),
not the verifier, so they stay `Crashed`. That is now an explicit, commented arm in
`outcome_from_csv` with its own tests, and any status the server does not know is also `Crashed`
with the engine's status text kept.

**F3 (question). The Gecko rule and the ports rule reject every real console recording.**
`judge.rs:48` (`has_gecko` gives `Illegal("gecko")`) and `judge.rs:22-28` (exactly ports 1 and 2).

- 37 of the 42 readable non-synthetic files carry a Gecko list. All except the 5 own-netplay
  exports.
- Of the 28 Slippi console recordings (`dolphin/`, `tournament/`, the two LRAS files), 25 fail
  `players` first, because console players use ports 1/3/4. Only 3 reach the Gecko rule and fail
  there: `dolphin/01c6d260`, `tournament/23b2b24b`, `edge/lras_20476`.
- Whether the lists are the standard Slippi set: the 28 console recordings share **85 codes that
  are byte-identical (same type, address, length and payload hash) in every file**. The files
  come in 4 shapes of 88/89/95/112 distinct (type, address) pairs (the engine counts 88/89/95/113
  codes). They include UCF 0.84 (`C2 8006B460`, the 1.0 cardinals) and Neutral Spawn, and lack
  the online marker `C2 8006A880`. That is consistent with Slippi's console/tournament code set
  plus optional toggles. The engine authors' notes describe the same four shapes over 125
  tournament files (`docs/rust-port/notes/slpcheck.md:105-112`).
- I cannot prove they are "the standard set": no reference list (a Slippi Dolphin
  `GALE01r2.ini` of a known release) exists on this machine. Comparing against one is how to tell.
- The 9 own coverage-corpus files (`own-offline/pairs_*`, `levels_*`, the CPU files in `edge/`)
  carry an 81-code list. It holds 27 codes that are in no console list, including the covtest
  boot codes from `tools/rust-port/covtest/gecko_boot.py`, such as `C2 801A55EC`
  (`gmVsMelee_Mode_OnLoad`). They are custom lists, as their source notes say.
- `edge/mex_check_luigi_replay.slp` carries 300 codes, including the Akaneia marker
  `C2 803D7060`, and lacks 26 of the console core. It is custom (m-ex).

To admit Slippi replays, the Gecko rule would need an allow-list of known code sets: for example
the 85-code core plus known optional codes, with the Akaneia marker and debug codes rejected.
That list would need a reference to build from.

**F4 (question). Files with a declared raw length of 0: our reader says `Truncated`, the engine
reads them.**
`crates/replay/src/raw.rs:20-23` takes the declared length literally.

- `own-offline/c_build_pad_falcon_fox_fd.slp` (571738 bytes) has the raw header
  `{U\x03raw[$U#l` followed by length `00 00 00 00`. The event stream then runs to the end of the
  file, with no `metadata` element and no closing `}`. The writer (the frozen C reference build)
  never went back to patch the length or finish the file. That is what a Slippi file looks like
  while it is still being written, or after the writer died.
- `raw_element` returns an empty slice, and `Events::new` reports `Truncated`. The engine reads
  to the end of the file and simulates it. It gives `diverged@-123:p1:state_frame`, a genuine
  non-match per the manifest.
- `synthetic/zerolen.slp` is the same case: we say `Truncated`, the engine reads it and refuses
  it on its team byte.
- Consequence today: such a pair gets `Undecided` rather than a verdict, and two length-0
  uploads compare equal as empty raw elements (`judge.rs:63-66`). That is harmless only because
  parsing then fails.
- Reading to the end of the file when the length is 0 would match the engine and other Slippi
  parsers. Whether the server should accept unfinished files at all is a policy call.
- `edge/truncated_cut60pct_own_items_bf.slp` is a different case. Its declared length (72951) is
  past the end of the file (43869 bytes of stream). Both sides agree the file is incomplete; the
  engine simulates the partial stream and diverges at -123.

**F5 (note). Our reader accepts 8 of the 12 synthetic files that the engine refuses.**
`ok00_modern`, `ok03_old`, `oneframe`, `below123`, `rule_frames`, `skin`, `flip05` and `ctl_ok`
parse with absurd values: timers of 185312588 s, 89-240 stocks, `last_frame` below -123. The
engine refuses each of them ("team N is outside 0..=2"). `legality` stops them all (`players` or
`teams`), so this is not exploitable today. `replay::parse` does no range checks of its own, and
anything that relaxes `legality` should keep that in mind.

**LRAS (c).** Both quit-outs read as Game End method 7, and our reader applies the
initiator-loses rule correctly in both:

- `edge/lras_20476_14664b32_p1_initiator.slp`: initiator p1, `winner_port == Some(2)`.
- `edge/lras_20689_fa028fa2_p4_initiator.slp`: players on p2 and p4, initiator p4,
  `winner_port == Some(2)`.

Both re-simulate to `match` without the variable, and both come back `Match` from
`Slpcheck::check` since `361152e`. Before that fix the second diverged (F1). Neither
gets a verdict today: the first fails `gecko`, the second fails `players`.
`edge/mex_check_luigi_replay.slp` is also an LRAS ending (p1 quit 98 frames in), and its winner
is `Some(2)`.

**Disconnect.** The host-only export of a game whose guest was killed ends with Game End method 7
and no initiator. `winner_port` is `None`, so even a matching pair of such files would be
`Undecided`, and a single upload is `Single`. The engine re-simulates the 1344 frames to `match`.

**Manifest note.** `MANIFEST.txt` says `netplay_host.slp` / `netplay_guest.slp` have "no file in
this folder". Both files are present, and they match the listed sha256.

## What this means for running an event

- **What gets verified today:** games played with this project's own netplay client. Both
  players' exports are identical, the reader reads them correctly, and the engine re-runs them
  frame for frame. Such a game comes out **Verified**, with the winner taken from the replay.
  This was checked end to end on two real games: a clean one and one with 40 ms lag and 5 %
  packet loss.
- **What goes to the organiser:** a game where only one player uploaded (`Single`). Also a game
  that ended by disconnect or by both players quitting without a winner (`Undecided`), a game
  with the wrong rules, stage, ports or a Gecko list (`Illegal`), and a file the reader cannot
  read (`Undecided`). Replays recorded on Slippi Dolphin or a console are all refused today (F3).
  Admitting them needs the Gecko rule and the ports rule changed; the spawn setting (F1) is
  already per replay.
- **A misconfigured verifier is caught (F2, fixed):** a missing or unreadable disc image, or an
  engine that cannot run, stops the server (`in_process`) or `verify-worker` at start-up with the
  path in the error. A disc that disappears later makes games `PeerOnly`, not
  `VerifiedInexact`.
- **How long verification takes:** about 1 s of one core per 20 s of game time (about 19x real
  time). A typical 3-minute game is checked in about 10 s. A full 8-minute timeout game takes
  about 25 s. A 32-player double-elimination night of about 150 games is about 25 minutes of one
  core, spread across the evening. That is still small, but about twice the design's estimate.
