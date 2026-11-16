"use strict";

// Ledge x coordinates (absolute value; the stages are symmetric) for the
// tournament-legal stages, by Slippi external stage id. A player whose |x| is
// beyond this value is off stage. Values are the main-platform edges as
// commonly quoted in the Melee community (SSBWiki / the Slippi stage data);
// they are used only for the edgeguard heuristic, so a few tenths of a unit
// either way does not matter.
const LEDGE_X = Object.freeze({
  2: 63.35, // Fountain of Dreams
  3: 87.75, // Pokemon Stadium
  8: 56.0, // Yoshi's Story
  28: 77.27, // Dream Land N64
  31: 68.4, // Battlefield
  32: 85.57, // Final Destination
});

function ledgeX(stageId) {
  return Object.prototype.hasOwnProperty.call(LEDGE_X, stageId) ? LEDGE_X[stageId] : null;
}

module.exports = { LEDGE_X, ledgeX };
