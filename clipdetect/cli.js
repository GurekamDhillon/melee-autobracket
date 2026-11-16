#!/usr/bin/env node
"use strict";

// clipdetect: find hype moments in a Slippi replay.
//
//   node clipdetect/cli.js FILE.slp [--config thresholds.json] [--pretty]
//
// Prints one JSON document on stdout. Exit codes: 0 ok, 1 unreadable or
// unparseable replay, 2 bad usage or bad config.

// slippi-js's node build pulls in `enet`, whose asm.js module makes V8 print an
// "Invalid asm.js" warning on stderr. Drop that one warning, keep the rest.
process.removeAllListeners("warning");
process.on("warning", (w) => {
  if (!/asm\.js/i.test(String(w && w.message))) console.error(w.stack || String(w));
});

const fs = require("node:fs");
const { loadReplay, ReplayError } = require("./lib/load");
const { detect, DEFAULT_CONFIG } = require("./lib/detect");

const USAGE = "usage: clipdetect FILE.slp [--config thresholds.json] [--pretty]";

function parseArgs(argv) {
  const out = { file: null, config: null, pretty: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--config") {
      if (i + 1 >= argv.length) throw new Error("--config needs a path");
      out.config = argv[++i];
    } else if (a.startsWith("--config=")) {
      out.config = a.slice("--config=".length);
    } else if (a === "--pretty") {
      out.pretty = true;
    } else if (a === "--help" || a === "-h") {
      out.help = true;
    } else if (a === "--print-defaults") {
      out.printDefaults = true;
    } else if (a.startsWith("-")) {
      throw new Error(`unknown option ${a}`);
    } else if (out.file == null) {
      out.file = a;
    } else {
      throw new Error("only one replay file at a time");
    }
  }
  return out;
}

function main(argv) {
  let args;
  try {
    args = parseArgs(argv);
  } catch (e) {
    console.error(`clipdetect: ${e.message}\n${USAGE}`);
    return 2;
  }
  if (args.help) {
    console.log(USAGE);
    return 0;
  }
  if (args.printDefaults) {
    console.log(JSON.stringify(DEFAULT_CONFIG, null, 2));
    return 0;
  }
  if (!args.file) {
    console.error(`clipdetect: missing replay file\n${USAGE}`);
    return 2;
  }
  let overrides = null;
  if (args.config) {
    try {
      overrides = JSON.parse(fs.readFileSync(args.config, "utf8"));
    } catch (e) {
      console.error(`clipdetect: cannot read config ${args.config}: ${e.message}`);
      return 2;
    }
  }
  let loaded;
  try {
    loaded = loadReplay(args.file);
  } catch (e) {
    if (e instanceof ReplayError) {
      console.error(`clipdetect: ${e.message}`);
      return 1;
    }
    throw e;
  }
  let result;
  try {
    result = detect(loaded.model, overrides);
  } catch (e) {
    if (/config/.test(e.message)) {
      console.error(`clipdetect: bad config: ${e.message}`);
      return 2;
    }
    throw e;
  }
  const doc = { game: loaded.meta, clips: result.clips, ranges: result.ranges, config: result.config };
  process.stdout.write(JSON.stringify(doc, null, args.pretty ? 2 : 0) + "\n");
  return 0;
}

if (require.main === module) {
  process.exitCode = main(process.argv.slice(2));
}

module.exports = { main, parseArgs };
