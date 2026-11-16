"use strict";

// Pure hype-moment detection over a replay model (see lib/load.js for the
// model built from a .slp file). Nothing here touches the filesystem, so the
// unit tests feed it small synthetic models.

const { isDamaged, isGrabbed, isCommandGrabbed } = require("@slippi/slippi-js/node");
const { ledgeX } = require("./stages");

const FPS = 60;

const KINDS = ["combo", "low_kill", "edgeguard", "comeback", "zero_to_death"];
const KIND_ORDER = Object.fromEntries(KINDS.map((k, i) => [k, i]));

// Desktop's thresholds, exposed as tunables. Every key may be overridden by a
// --config JSON file; unknown keys are rejected so typos do not pass silently.
const DEFAULT_CONFIG = Object.freeze({
  // combo: >= comboMinHits landed moves OR >= comboMinPercent damage, and no
  // gap longer than comboMaxHitstunGapFrames between the victim leaving
  // hitstun/grab after one hit and the next hit landing.
  comboMinHits: 5,
  comboMinPercent: 40,
  comboMaxHitstunGapFrames: 45,
  // Project Clippi's excludesLargeSingleHit: a "combo" whose single biggest
  // hit is >= this fraction of the total damage is one big hit, not a combo.
  // null disables the check.
  comboLargeHitFraction: 0.8,
  // low_kill: a stock taken at or below this percent.
  lowKillMaxPercent: 60,
  // edgeguard: victim and killer both beyond the stage's ledge x on the frame
  // the last hit landed. Only stages in lib/stages.js qualify.
  edgeguardEnabled: true,
  // comeback: the winner was down by at least this many stocks at some point.
  comebackMinStockDeficit: 2,
  // zero_to_death: one conversion from <= this percent to a KO, with at least
  // zeroToDeathMinHits landed moves.
  zeroToDeathMaxStartPercent: 10,
  zeroToDeathMinHits: 2,
  // Padding around the core event, in frames (60 fps): 2 s before, 1.5 s after.
  padBeforeFrames: 2 * FPS,
  padAfterFrames: 1.5 * FPS,
});

function mergeConfig(overrides) {
  const cfg = { ...DEFAULT_CONFIG };
  if (overrides == null) return cfg;
  if (typeof overrides !== "object" || Array.isArray(overrides)) {
    throw new Error("config must be a JSON object");
  }
  for (const [k, v] of Object.entries(overrides)) {
    if (!Object.prototype.hasOwnProperty.call(DEFAULT_CONFIG, k)) {
      throw new Error(`unknown config key: ${k}`);
    }
    const dflt = DEFAULT_CONFIG[k];
    const okType =
      v === null && k === "comboLargeHitFraction"
        ? true
        : typeof v === typeof dflt && (typeof v !== "number" || Number.isFinite(v));
    if (!okType) throw new Error(`config key ${k}: expected ${typeof dflt}, got ${JSON.stringify(v)}`);
    cfg[k] = v;
  }
  return cfg;
}

function round3(x) {
  return Math.round(x * 1000) / 1000;
}

function clamp01(x) {
  return Math.max(0, Math.min(1, x));
}

function port(playerIndex) {
  return playerIndex == null ? null : playerIndex + 1;
}

function comboDamage(c) {
  const end = c.endPercent == null ? c.currentPercent : c.endPercent;
  return Math.max(0, end - c.startPercent);
}

function lastMoveFrame(c) {
  return c.moves.length ? c.moves[c.moves.length - 1].frame : c.startFrame;
}

function attackerIndex(c) {
  if (c.lastHitBy != null) return c.lastHitBy;
  return c.moves.length ? c.moves[0].playerIndex : null;
}

// First frame strictly after `fromFrame` at which the victim is no longer in a
// damaged/grabbed state. If the frame data runs out first, returns the last
// frame examined + 1 (that is, the victim is treated as stuck in hitstun).
function victimFreeFrame(model, victimIndex, fromFrame, untilFrame) {
  for (let f = fromFrame + 1; f <= untilFrame; f++) {
    const post = model.getPost(f, victimIndex);
    if (!post) continue;
    const s = post.actionStateId;
    if (!(isDamaged(s) || isGrabbed(s) || isCommandGrabbed(s))) return f;
  }
  return untilFrame + 1;
}

// Longest gap between the victim becoming actionable after one hit and the
// next hit landing. 0 when the victim never got out.
function maxHitstunGap(model, combo) {
  let worst = 0;
  for (let i = 0; i + 1 < combo.moves.length; i++) {
    const a = combo.moves[i].frame;
    const b = combo.moves[i + 1].frame;
    const free = victimFreeFrame(model, combo.playerIndex, a, b);
    const gap = Math.max(0, b - free);
    if (gap > worst) worst = gap;
  }
  return worst;
}

function largestHitFraction(combo) {
  const total = combo.moves.reduce((s, m) => s + m.damage, 0);
  if (total <= 0) return 0;
  return Math.max(...combo.moves.map((m) => m.damage)) / total;
}

function detectCombos(model, cfg) {
  const out = [];
  for (const c of model.stats.combos) {
    const hits = c.moves.length;
    const dmg = comboDamage(c);
    if (hits < cfg.comboMinHits && dmg < cfg.comboMinPercent) continue;
    if (hits < 2) continue;
    const gap = maxHitstunGap(model, c);
    if (gap > cfg.comboMaxHitstunGapFrames) continue;
    const bigHit = largestHitFraction(c);
    if (cfg.comboLargeHitFraction != null && bigHit >= cfg.comboLargeHitFraction) continue;
    const coreEnd = c.endFrame == null ? lastMoveFrame(c) : c.endFrame;
    const score = clamp01(0.5 * Math.min(1, hits / 10) + 0.5 * Math.min(1, dmg / 100) + (c.didKill ? 0.15 : 0));
    out.push({
      kind: "combo",
      core_start: c.startFrame,
      core_end: coreEnd,
      player_port: port(attackerIndex(c)),
      victim_port: port(c.playerIndex),
      score: round3(score),
      details: {
        hits,
        damage: round3(dmg),
        start_percent: round3(c.startPercent),
        end_percent: round3(c.endPercent == null ? c.currentPercent : c.endPercent),
        did_kill: Boolean(c.didKill),
        max_hitstun_gap_frames: gap,
        largest_hit_fraction: round3(bigHit),
        move_ids: c.moves.map((m) => m.moveId),
      },
    });
  }
  return out;
}

// The conversion (preferred) or combo that killed this stock, if any.
function killingSequence(model, stock) {
  const pick = (list) =>
    list.find(
      (c) =>
        c.didKill && c.playerIndex === stock.playerIndex && c.endFrame != null && Math.abs(c.endFrame - stock.endFrame) <= 1,
    );
  return pick(model.stats.conversions) || pick(model.stats.combos) || null;
}

function detectKills(model, cfg) {
  const low = [];
  const edge = [];
  const lx = ledgeX(model.settings.stageId);
  for (const stock of model.stats.stocks) {
    if (stock.endFrame == null) continue;
    const seq = killingSequence(model, stock);
    if (!seq) continue; // no attacker: self-destruct or unknown, not a highlight
    const killer = attackerIndex(seq);
    const hitFrame = lastMoveFrame(seq);
    const base = {
      core_start: hitFrame,
      core_end: stock.endFrame,
      player_port: port(killer),
      victim_port: port(stock.playerIndex),
    };
    const pct = stock.endPercent == null ? stock.currentPercent : stock.endPercent;
    if (pct <= cfg.lowKillMaxPercent) {
      low.push({
        kind: "low_kill",
        ...base,
        score: round3(clamp01(0.3 + 0.7 * (1 - pct / cfg.lowKillMaxPercent))),
        details: {
          kill_percent: round3(pct),
          stock_count_before: stock.count,
          last_move_id: seq.moves.length ? seq.moves[seq.moves.length - 1].moveId : null,
          sequence_start_frame: seq.startFrame,
        },
      });
    }
    if (cfg.edgeguardEnabled && lx != null && killer != null) {
      const v = model.getPost(hitFrame, stock.playerIndex);
      const k = model.getPost(hitFrame, killer);
      if (v && k && v.positionX != null && k.positionX != null) {
        const vOff = Math.abs(v.positionX) > lx;
        const kOff = Math.abs(k.positionX) > lx;
        if (vOff && kOff) {
          const depth = Math.abs(v.positionX) - lx;
          edge.push({
            kind: "edgeguard",
            ...base,
            score: round3(clamp01(0.6 + 0.4 * Math.min(1, depth / 40))),
            details: {
              kill_percent: round3(pct),
              ledge_x: lx,
              victim_x: round3(v.positionX),
              victim_y: round3(v.positionY ?? 0),
              killer_x: round3(k.positionX),
              killer_y: round3(k.positionY ?? 0),
              last_move_id: seq.moves.length ? seq.moves[seq.moves.length - 1].moveId : null,
              sequence_start_frame: seq.startFrame,
            },
          });
        }
      }
    }
  }
  return low.concat(edge);
}

function detectZeroToDeath(model, cfg) {
  const out = [];
  for (const c of model.stats.conversions) {
    if (!c.didKill) continue;
    if (c.startPercent > cfg.zeroToDeathMaxStartPercent) continue;
    if (c.moves.length < cfg.zeroToDeathMinHits) continue;
    out.push({
      kind: "zero_to_death",
      core_start: c.startFrame,
      core_end: c.endFrame == null ? lastMoveFrame(c) : c.endFrame,
      player_port: port(attackerIndex(c)),
      victim_port: port(c.playerIndex),
      score: round3(clamp01(0.8 + 0.2 * Math.min(1, c.moves.length / 10))),
      details: {
        hits: c.moves.length,
        start_percent: round3(c.startPercent),
        damage: round3(comboDamage(c)),
        opening_type: c.openingType ?? null,
        move_ids: c.moves.map((m) => m.moveId),
      },
    });
  }
  return out;
}

// Game end methods from slippi-js GameEndMethod.
const END_GAME = 2;
const END_TIME = 1;

function detectComeback(model, cfg) {
  const players = model.settings.players;
  if (players.length !== 2 || model.settings.isTeams) return [];
  const method = model.gameEnd ? model.gameEnd.gameEndMethod : undefined;
  if (method !== END_GAME && method !== END_TIME) return [];
  const winners = model.winners || [];
  if (winners.length !== 1) return [];
  const winner = winners[0].playerIndex;
  const loserPlayer = players.find((p) => p.playerIndex !== winner);
  if (!loserPlayer) return [];
  const loser = loserPlayer.playerIndex;

  const remaining = {};
  for (const p of players) remaining[p.playerIndex] = p.startStocks ?? 4;
  const deaths = model.stats.stocks
    .filter((s) => s.endFrame != null)
    .slice()
    .sort((a, b) => a.endFrame - b.endFrame || a.playerIndex - b.playerIndex);
  let maxDeficit = 0;
  let deficitFrame = null;
  let stocksAt = null;
  for (const d of deaths) {
    remaining[d.playerIndex] -= 1;
    const deficit = remaining[loser] - remaining[winner];
    if (deficit > maxDeficit) {
      maxDeficit = deficit;
      deficitFrame = d.endFrame;
      stocksAt = { winner: remaining[winner], loser: remaining[loser] };
    }
  }
  if (maxDeficit < cfg.comebackMinStockDeficit) return [];

  // Clip the final kill (or, on a timeout with no final kill, the deficit
  // moment through the end of the game).
  const loserDeaths = deaths.filter((d) => d.playerIndex === loser);
  const last = loserDeaths.length ? loserDeaths[loserDeaths.length - 1] : null;
  const seq = last ? killingSequence(model, last) : null;
  let coreStart;
  let coreEnd;
  if (last) {
    coreStart = seq ? seq.startFrame : last.endFrame;
    coreEnd = last.endFrame;
  } else {
    coreStart = deficitFrame;
    coreEnd = model.lastFrame;
  }
  return [
    {
      kind: "comeback",
      core_start: coreStart,
      core_end: coreEnd,
      player_port: port(winner),
      victim_port: port(loser),
      score: round3(clamp01(0.5 + 0.25 * (maxDeficit - cfg.comebackMinStockDeficit))),
      details: {
        max_stock_deficit: maxDeficit,
        deficit_frame: deficitFrame,
        winner_stocks_at_deficit: stocksAt.winner,
        loser_stocks_at_deficit: stocksAt.loser,
        game_end_method: method,
      },
    },
  ];
}

function padAndClamp(clip, model, cfg) {
  const start = Math.max(model.firstFrame, Math.round(clip.core_start - cfg.padBeforeFrames));
  const end = Math.min(model.lastFrame, Math.round(clip.core_end + cfg.padAfterFrames));
  const { core_start, core_end, details, ...rest } = clip;
  return {
    ...rest,
    start_frame: start,
    end_frame: Math.max(start, end),
    details: { core_start_frame: core_start, core_end_frame: core_end, ...details },
  };
}

function compareClips(a, b) {
  return (
    a.start_frame - b.start_frame ||
    KIND_ORDER[a.kind] - KIND_ORDER[b.kind] ||
    (a.player_port ?? 0) - (b.player_port ?? 0) ||
    (a.victim_port ?? 0) - (b.victim_port ?? 0) ||
    a.end_frame - b.end_frame
  );
}

// Merge overlapping or touching [start_frame, end_frame] ranges. Input need not
// be sorted. Each merged range lists the kinds it covers, sorted.
function mergeRanges(clips) {
  const sorted = clips.slice().sort((a, b) => a.start_frame - b.start_frame || a.end_frame - b.end_frame);
  const out = [];
  for (const c of sorted) {
    const last = out[out.length - 1];
    if (last && c.start_frame <= last.end_frame + 1) {
      last.end_frame = Math.max(last.end_frame, c.end_frame);
      if (!last.kinds.includes(c.kind)) last.kinds.push(c.kind);
      last.score = Math.max(last.score, c.score);
    } else {
      out.push({ start_frame: c.start_frame, end_frame: c.end_frame, kinds: [c.kind], score: c.score });
    }
  }
  for (const r of out) r.kinds.sort((a, b) => KIND_ORDER[a] - KIND_ORDER[b]);
  return out;
}

function detect(model, config) {
  const cfg = mergeConfig(config);
  const raw = [
    ...detectCombos(model, cfg),
    ...detectKills(model, cfg),
    ...detectComeback(model, cfg),
    ...detectZeroToDeath(model, cfg),
  ];
  const clips = raw.map((c) => padAndClamp(c, model, cfg)).sort(compareClips);
  return { clips, ranges: mergeRanges(clips), config: cfg };
}

module.exports = {
  DEFAULT_CONFIG,
  KINDS,
  FPS,
  mergeConfig,
  detect,
  mergeRanges,
  maxHitstunGap,
  largestHitFraction,
  killingSequence,
};
