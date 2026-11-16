"use strict";

process.removeAllListeners("warning");
process.on("warning", () => {});

const test = require("node:test");
const assert = require("node:assert/strict");
const { detect, mergeRanges, mergeConfig, DEFAULT_CONFIG, maxHitstunGap } = require("../lib/detect");

// Action state ids: 0x4B..0x5B are damage states (slippi-js isDamaged),
// 0x0E is Wait (actionable).
const HITSTUN = 0x4b;
const WAIT = 0x0e;

function move(frame, playerIndex, damage, moveId = 1) {
  return { playerIndex, frame, moveId, hitCount: 1, damage };
}

function combo(opts) {
  const moves = opts.moves;
  const dmg = moves.reduce((s, m) => s + m.damage, 0);
  return {
    playerIndex: opts.victim,
    startFrame: moves[0].frame,
    endFrame: opts.endFrame ?? moves[moves.length - 1].frame + 50,
    startPercent: opts.startPercent ?? 0,
    currentPercent: (opts.startPercent ?? 0) + dmg,
    endPercent: (opts.startPercent ?? 0) + dmg,
    moves,
    didKill: Boolean(opts.didKill),
    lastHitBy: moves[0].playerIndex,
    ...(opts.openingType ? { openingType: opts.openingType } : {}),
  };
}

// A model where the victim stays in hitstun for `stunFrames` after every hit,
// and positions are whatever `pos` returns.
function model(opts = {}) {
  const stun = opts.stunFrames ?? 1000; // never leaves hitstun unless told otherwise
  const hitFrames = opts.hitFrames ?? [];
  const pos = opts.pos || (() => ({ x: 0, y: 0 }));
  return {
    settings: {
      stageId: opts.stageId ?? 32, // Final Destination, ledge 85.57
      isTeams: false,
      players: opts.players ?? [
        { playerIndex: 0, characterId: 2, type: 0, startStocks: 4 },
        { playerIndex: 1, characterId: 9, type: 0, startStocks: 4 },
      ],
    },
    stats: { combos: opts.combos ?? [], conversions: opts.conversions ?? [], stocks: opts.stocks ?? [] },
    gameEnd: opts.gameEnd === undefined ? { gameEndMethod: 2, lrasInitiatorIndex: -1 } : opts.gameEnd,
    winners: opts.winners ?? [],
    firstFrame: -123,
    lastFrame: opts.lastFrame ?? 6000,
    getPost(frame, playerIndex) {
      const inStun = hitFrames.some((h) => frame >= h && frame <= h + stun);
      const p = pos(frame, playerIndex);
      return { actionStateId: inStun ? HITSTUN : WAIT, positionX: p.x, positionY: p.y, percent: 0 };
    },
  };
}

test("config: defaults, overrides, unknown keys rejected", () => {
  assert.deepEqual(mergeConfig(null), DEFAULT_CONFIG);
  assert.equal(mergeConfig({ comboMinHits: 3 }).comboMinHits, 3);
  assert.throws(() => mergeConfig({ nope: 1 }), /unknown config key: nope/);
  assert.throws(() => mergeConfig({ comboMinHits: "5" }), /expected number/);
  assert.throws(() => mergeConfig([1]), /JSON object/);
  assert.equal(mergeConfig({ comboLargeHitFraction: null }).comboLargeHitFraction, null);
});

test("combo: 5 hits qualifies, 4 hits under 40% does not", () => {
  const five = combo({ victim: 1, moves: [100, 120, 140, 160, 180].map((f) => move(f, 0, 5)) });
  const four = combo({ victim: 1, moves: [1000, 1020, 1040, 1060].map((f) => move(f, 0, 5)) });
  const r = detect(model({ combos: [five, four] }));
  assert.deepEqual(
    r.clips.map((c) => c.kind),
    ["combo"],
  );
  const c = r.clips[0];
  assert.equal(c.player_port, 1);
  assert.equal(c.victim_port, 2);
  assert.equal(c.details.hits, 5);
  assert.equal(c.details.core_start_frame, 100);
  assert.equal(c.start_frame, 100 - 120);
  assert.equal(c.end_frame, five.endFrame + 90);
});

test("combo: 40% in 3 hits qualifies via the percent threshold", () => {
  const c = combo({ victim: 0, moves: [move(500, 1, 14), move(530, 1, 13), move(560, 1, 13)] });
  const r = detect(model({ combos: [c] }));
  assert.equal(r.clips.length, 1);
  assert.equal(r.clips[0].details.damage, 40);
  assert.equal(r.clips[0].player_port, 2);
});

test("combo: a hitstun gap over 45 frames disqualifies; under it passes", () => {
  const moves = [100, 160, 220, 280, 340].map((f) => move(f, 0, 6));
  const c = combo({ victim: 1, moves });
  // victim actionable 10 frames after each hit: gap = 60 - 11 = 49 > 45
  const bad = model({ combos: [c], hitFrames: moves.map((m) => m.frame), stunFrames: 10 });
  assert.equal(maxHitstunGap(bad, c), 49);
  assert.equal(detect(bad).clips.length, 0);
  // victim stuck for 20 frames: gap = 60 - 21 = 39 <= 45
  const ok = model({ combos: [c], hitFrames: moves.map((m) => m.frame), stunFrames: 20 });
  assert.equal(maxHitstunGap(ok, c), 39);
  assert.equal(detect(ok).clips.length, 1);
  assert.equal(detect(ok).clips[0].details.max_hitstun_gap_frames, 39);
  // and the threshold is tunable
  assert.equal(detect(bad, { comboMaxHitstunGapFrames: 50 }).clips.length, 1);
});

test("combo: one hit carrying 80% of the damage is not a combo (Clippi's large-hit rule)", () => {
  const c = combo({ victim: 1, moves: [move(100, 0, 1), move(110, 0, 1), move(120, 0, 1), move(130, 0, 1), move(140, 0, 40)] });
  assert.equal(detect(model({ combos: [c] })).clips.length, 0);
  assert.equal(detect(model({ combos: [c] }), { comboLargeHitFraction: null }).clips.length, 1);
});

test("low_kill: stock lost at <= 60% with a killing sequence; self-destructs ignored", () => {
  const killer = combo({ victim: 1, moves: [move(2000, 0, 20)], didKill: true, endFrame: 2040, startPercent: 30 });
  const stocks = [
    { playerIndex: 1, startFrame: -123, endFrame: 2040, startPercent: 0, currentPercent: 50, endPercent: 50, count: 4 },
    { playerIndex: 0, startFrame: -123, endFrame: 3000, startPercent: 0, currentPercent: 12, endPercent: 12, count: 4 }, // SD
    { playerIndex: 1, startFrame: 2100, endFrame: undefined, startPercent: 0, currentPercent: 7, endPercent: undefined, count: 3 },
  ];
  const r = detect(model({ conversions: [killer], stocks }));
  const low = r.clips.filter((c) => c.kind === "low_kill");
  assert.equal(low.length, 1);
  assert.equal(low[0].player_port, 1);
  assert.equal(low[0].victim_port, 2);
  assert.equal(low[0].details.kill_percent, 50);
  assert.equal(low[0].details.core_start_frame, 2000);
  assert.equal(low[0].details.core_end_frame, 2040);
  // 61% is not a low kill
  stocks[0].endPercent = 61;
  assert.equal(detect(model({ conversions: [killer], stocks })).clips.filter((c) => c.kind === "low_kill").length, 0);
});

test("edgeguard: both players beyond ledge x on the last-hit frame, on a known stage only", () => {
  const killer = combo({ victim: 1, moves: [move(2000, 0, 12)], didKill: true, endFrame: 2060, startPercent: 90 });
  const stocks = [{ playerIndex: 1, startFrame: -123, endFrame: 2060, startPercent: 0, currentPercent: 102, endPercent: 102, count: 4 }];
  const offstage = (frame, idx) => (idx === 1 ? { x: -110, y: -20 } : { x: -95, y: -5 });
  const r = detect(model({ conversions: [killer], stocks, pos: offstage }));
  assert.deepEqual(
    r.clips.map((c) => c.kind),
    ["edgeguard"],
  );
  assert.equal(r.clips[0].details.ledge_x, 85.57);
  assert.equal(r.clips[0].details.victim_x, -110);
  // killer onstage: not an edgeguard
  const killerOn = (frame, idx) => (idx === 1 ? { x: -110, y: -20 } : { x: -60, y: 0 });
  assert.equal(detect(model({ conversions: [killer], stocks, pos: killerOn })).clips.length, 0);
  // unknown stage: no ledge data, no edgeguard
  assert.equal(detect(model({ conversions: [killer], stocks, pos: offstage, stageId: 4 })).clips.length, 0);
  // disabled by config
  assert.equal(detect(model({ conversions: [killer], stocks, pos: offstage }), { edgeguardEnabled: false }).clips.length, 0);
});

test("zero_to_death: conversion from <= 10% that kills, with at least 2 hits", () => {
  const z = combo({
    victim: 1,
    moves: [move(100, 0, 10), move(140, 0, 15), move(180, 0, 20)],
    didKill: true,
    startPercent: 8,
    openingType: "neutral-win",
  });
  const r = detect(model({ conversions: [z] }));
  assert.equal(r.clips.filter((c) => c.kind === "zero_to_death").length, 1);
  assert.equal(r.clips[0].details.opening_type, "neutral-win");
  const late = { ...z, startPercent: 11 };
  assert.equal(detect(model({ conversions: [late] })).clips.length, 0);
  const oneHit = { ...z, moves: [move(100, 0, 60)] };
  assert.equal(detect(model({ conversions: [oneHit] })).clips.length, 0);
  assert.equal(detect(model({ conversions: [oneHit] }), { zeroToDeathMinHits: 1 }).clips.length, 1);
});

function stock(playerIndex, endFrame, endPercent, count) {
  return { playerIndex, startFrame: 0, endFrame, startPercent: 0, currentPercent: endPercent, endPercent, count };
}

test("comeback: winner was down two stocks, clip is the final kill", () => {
  // p1 (index 0) loses two stocks, then takes four.
  const stocks = [
    stock(0, 1000, 100, 4),
    stock(0, 2000, 100, 3),
    stock(1, 3000, 100, 4),
    stock(1, 4000, 100, 3),
    stock(1, 5000, 100, 2),
    stock(1, 5500, 80, 1),
  ];
  const finalKill = combo({ victim: 1, moves: [move(5400, 0, 30), move(5450, 0, 20)], didKill: true, endFrame: 5500, startPercent: 30 });
  const r = detect(model({ stocks, conversions: [finalKill], winners: [{ playerIndex: 0 }] }));
  const cb = r.clips.filter((c) => c.kind === "comeback");
  assert.equal(cb.length, 1);
  assert.equal(cb[0].player_port, 1);
  assert.equal(cb[0].victim_port, 2);
  assert.equal(cb[0].details.max_stock_deficit, 2);
  assert.equal(cb[0].details.deficit_frame, 2000);
  assert.equal(cb[0].details.winner_stocks_at_deficit, 2);
  assert.equal(cb[0].details.loser_stocks_at_deficit, 4);
  assert.equal(cb[0].details.core_start_frame, 5400);
  assert.equal(cb[0].details.core_end_frame, 5500);
  assert.equal(cb[0].score, 0.5);
  // only one stock down: no comeback
  const r2 = detect(model({ stocks: stocks.slice(1), conversions: [finalKill], winners: [{ playerIndex: 0 }] }));
  assert.equal(r2.clips.filter((c) => c.kind === "comeback").length, 0);
  // LRAS game: no comeback
  const r3 = detect(
    model({ stocks, conversions: [finalKill], winners: [{ playerIndex: 0 }], gameEnd: { gameEndMethod: 7, lrasInitiatorIndex: 1 } }),
  );
  assert.equal(r3.clips.filter((c) => c.kind === "comeback").length, 0);
});

test("padding clamps to game bounds and merged ranges join overlaps", () => {
  const early = combo({ victim: 1, moves: [-100, -90, -80, -70, -60].map((f) => move(f, 0, 5)), endFrame: -20 });
  const late = combo({ victim: 1, moves: [5900, 5910, 5920, 5930, 5940].map((f) => move(f, 0, 5)), endFrame: 5990 });
  const r = detect(model({ combos: [early, late], lastFrame: 6000 }));
  assert.equal(r.clips[0].start_frame, -123);
  assert.equal(r.clips[1].end_frame, 6000);
  assert.equal(r.ranges.length, 2);

  const merged = mergeRanges([
    { kind: "combo", start_frame: 100, end_frame: 300, score: 0.4 },
    { kind: "low_kill", start_frame: 250, end_frame: 400, score: 0.9 },
    { kind: "edgeguard", start_frame: 401, end_frame: 500, score: 0.7 }, // touching
    { kind: "combo", start_frame: 600, end_frame: 700, score: 0.5 },
  ]);
  assert.deepEqual(merged, [
    { start_frame: 100, end_frame: 500, kinds: ["combo", "low_kill", "edgeguard"], score: 0.9 },
    { start_frame: 600, end_frame: 700, kinds: ["combo"], score: 0.5 },
  ]);
});

test("output order is stable: by start frame, then kind, then ports", () => {
  const a = combo({ victim: 1, moves: [100, 110, 120, 130, 140].map((f) => move(f, 0, 5)) });
  const b = combo({ victim: 0, moves: [100, 110, 120, 130, 140].map((f) => move(f, 1, 5)) });
  const r1 = detect(model({ combos: [a, b] }));
  const r2 = detect(model({ combos: [b, a] }));
  assert.deepEqual(r1, r2);
  assert.deepEqual(
    r1.clips.map((c) => c.player_port),
    [1, 2],
  );
});
