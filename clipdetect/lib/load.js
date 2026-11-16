"use strict";

// Builds the detection model from a .slp file with @slippi/slippi-js.

const path = require("node:path");
const fs = require("node:fs");
const { SlippiGame, characters, stages, GameEndMethod } = require("@slippi/slippi-js/node");

class ReplayError extends Error {}

const END_NAMES = Object.fromEntries(
  Object.entries(GameEndMethod)
    .filter(([, v]) => typeof v === "number")
    .map(([k, v]) => [v, k.toLowerCase()]),
);

function loadReplay(file) {
  let game;
  try {
    fs.accessSync(file, fs.constants.R_OK);
    game = new SlippiGame(file);
  } catch (e) {
    throw new ReplayError(`cannot read ${file}: ${e.message}`);
  }
  let settings;
  let stats;
  let frames;
  try {
    settings = game.getSettings();
    stats = game.getStats();
    frames = game.getFrames();
  } catch (e) {
    throw new ReplayError(`cannot parse ${file}: ${e.message}`);
  }
  if (!settings || !settings.players || settings.players.length === 0) {
    throw new ReplayError(`cannot parse ${file}: no Game Start block`);
  }
  if (!stats) {
    throw new ReplayError(`cannot parse ${file}: no stats (not a game replay?)`);
  }
  const frameNumbers = Object.keys(frames).map(Number);
  if (frameNumbers.length === 0) {
    throw new ReplayError(`cannot parse ${file}: no frames`);
  }
  let firstFrame = Infinity;
  let lastFrame = -Infinity;
  for (const f of frameNumbers) {
    if (f < firstFrame) firstFrame = f;
    if (f > lastFrame) lastFrame = f;
  }
  const gameEnd = game.getGameEnd() || null;
  let winners = [];
  try {
    winners = game.getWinners() || [];
  } catch {
    winners = [];
  }
  const metadata = game.getMetadata() || null;

  const players = settings.players
    .slice()
    .sort((a, b) => a.playerIndex - b.playerIndex)
    .map((p) => {
      const md = metadata && metadata.players ? metadata.players[p.playerIndex] : undefined;
      const names = md && md.names ? md.names : {};
      return {
        port: p.playerIndex + 1,
        player_index: p.playerIndex,
        character_id: p.characterId ?? null,
        character: p.characterId == null ? null : characters.getCharacterName(p.characterId),
        type: p.type === 0 ? "human" : p.type === 1 ? "cpu" : p.type == null ? null : `type_${p.type}`,
        team_id: settings.isTeams ? (p.teamId ?? null) : null,
        start_stocks: p.startStocks ?? null,
        netplay_name: names.netplay ?? null,
        connect_code: names.code ?? null,
        display_name: p.displayName || null,
      };
    });

  const game_end = gameEnd
    ? {
        method: gameEnd.gameEndMethod ?? null,
        method_name: gameEnd.gameEndMethod == null ? null : END_NAMES[gameEnd.gameEndMethod] || "unknown",
        lras_initiator_port:
          gameEnd.lrasInitiatorIndex == null || gameEnd.lrasInitiatorIndex < 0 ? null : gameEnd.lrasInitiatorIndex + 1,
      }
    : null;

  const meta = {
    file: path.basename(file),
    slp_version: settings.slpVersion ?? null,
    stage_id: settings.stageId ?? null,
    stage: settings.stageId == null ? null : stages.getStageName(settings.stageId),
    is_teams: Boolean(settings.isTeams),
    players,
    first_frame: firstFrame,
    last_frame: lastFrame,
    frame_count: frameNumbers.length,
    playable_frames: stats.playableFrameCount,
    duration_s: Math.round((stats.playableFrameCount / 60) * 100) / 100,
    game_complete: Boolean(stats.gameComplete),
    game_end,
    winner_ports: winners.map((w) => w.playerIndex + 1).sort((a, b) => a - b),
    started_at: metadata && metadata.startAt ? metadata.startAt : null,
    played_on: metadata && metadata.playedOn ? metadata.playedOn : null,
  };

  const model = {
    settings: {
      stageId: settings.stageId,
      isTeams: Boolean(settings.isTeams),
      players: settings.players.map((p) => ({
        playerIndex: p.playerIndex,
        characterId: p.characterId,
        type: p.type,
        startStocks: p.startStocks,
      })),
    },
    stats: { combos: stats.combos, conversions: stats.conversions, stocks: stats.stocks },
    gameEnd: gameEnd ? { gameEndMethod: gameEnd.gameEndMethod, lrasInitiatorIndex: gameEnd.lrasInitiatorIndex } : null,
    winners: winners.map((w) => ({ playerIndex: w.playerIndex })),
    firstFrame,
    lastFrame,
    getPost(frame, playerIndex) {
      const fr = frames[frame];
      if (!fr || !fr.players) return undefined;
      const p = fr.players[playerIndex];
      return p ? p.post : undefined;
    },
  };

  return { meta, model };
}

module.exports = { loadReplay, ReplayError };
