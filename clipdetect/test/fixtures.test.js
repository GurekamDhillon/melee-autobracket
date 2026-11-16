"use strict";

// End-to-end checks against the real replay corpus. The replays are NOT in the
// repo: point AUTOBRACKET_FIXTURES at the directory that holds MANIFEST.txt
// (default /home/gd/autobracket-fixtures/autobracket-fixtures). Every test here
// is skipped when that directory is absent.

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const FIXTURES = process.env.AUTOBRACKET_FIXTURES || "/home/gd/autobracket-fixtures/autobracket-fixtures";
const CLI = path.join(__dirname, "..", "cli.js");
const have = fs.existsSync(path.join(FIXTURES, "MANIFEST.txt"));
const skip = have ? false : `fixtures not found at ${FIXTURES} (set AUTOBRACKET_FIXTURES)`;

const KINDS = new Set(["combo", "low_kill", "edgeguard", "comeback", "zero_to_death"]);

function run(args) {
  const r = spawnSync(process.execPath, [CLI, ...args], { encoding: "utf8" });
  return { status: r.status, stdout: r.stdout, stderr: r.stderr };
}

function checkDoc(doc) {
  assert.ok(doc.game && typeof doc.game === "object");
  assert.ok(Array.isArray(doc.clips));
  assert.ok(Array.isArray(doc.ranges));
  let prev = -Infinity;
  for (const c of doc.clips) {
    assert.ok(KINDS.has(c.kind), `kind ${c.kind}`);
    assert.ok(Number.isInteger(c.start_frame) && Number.isInteger(c.end_frame));
    assert.ok(c.start_frame >= doc.game.first_frame, "clamped start");
    assert.ok(c.end_frame <= doc.game.last_frame, "clamped end");
    assert.ok(c.start_frame <= c.end_frame);
    assert.ok(c.start_frame >= prev, "sorted by start_frame");
    prev = c.start_frame;
    assert.ok(Number.isInteger(c.player_port) && c.player_port >= 1 && c.player_port <= 4);
    assert.ok(Number.isInteger(c.victim_port) && c.victim_port >= 1 && c.victim_port <= 4);
    assert.notEqual(c.player_port, c.victim_port);
    assert.ok(typeof c.score === "number" && c.score >= 0 && c.score <= 1);
    assert.ok(c.details && typeof c.details === "object");
  }
  for (let i = 1; i < doc.ranges.length; i++) {
    assert.ok(doc.ranges[i].start_frame > doc.ranges[i - 1].end_frame + 1, "ranges are merged");
  }
}

test("tournament replay: well-formed, deterministic output with clips", { skip }, () => {
  const file = path.join(FIXTURES, "tournament", "20476_08f9e70f-2f7e-418b-8121-699ef271aacd.slp");
  const a = run([file]);
  assert.equal(a.status, 0, a.stderr);
  assert.equal(a.stderr, "", "stderr is quiet on success");
  const b = run([file]);
  assert.equal(a.stdout, b.stdout, "deterministic");
  const doc = JSON.parse(a.stdout);
  checkDoc(doc);
  assert.equal(doc.game.stage, "Fountain of Dreams");
  assert.deepEqual(
    doc.game.players.map((p) => p.port),
    [1, 3],
  );
  assert.equal(doc.game.game_end.method_name, "game");
  assert.deepEqual(doc.game.winner_ports, [3]);
  const kinds = new Set(doc.clips.map((c) => c.kind));
  assert.ok(kinds.has("combo"));
  assert.ok(kinds.has("low_kill"));
  assert.ok(kinds.has("edgeguard"));
  assert.ok(kinds.has("zero_to_death"));
  // The final stock: Marth (port 3) kills Sheik at 57% -> low_kill ends at the last frame.
  const low = doc.clips.filter((c) => c.kind === "low_kill");
  assert.equal(low[low.length - 1].player_port, 3);
  assert.equal(low[low.length - 1].victim_port, 1);
  assert.equal(low[low.length - 1].end_frame, doc.game.last_frame);
});

test("every manifest replay either parses to a well-formed document or fails cleanly", { skip }, () => {
  const rows = fs
    .readFileSync(path.join(FIXTURES, "MANIFEST.txt"), "utf8")
    .split("\n")
    .slice(1)
    .filter(Boolean)
    .map((l) => l.split("\t")[0]);
  const seen = { ok: 0, failed: 0, missing: 0 };
  for (const rel of rows) {
    const file = path.join(FIXTURES, rel);
    if (!fs.existsSync(file)) {
      seen.missing++;
      continue;
    }
    const r = run([file]);
    if (r.status === 0) {
      checkDoc(JSON.parse(r.stdout));
      seen.ok++;
    } else {
      assert.equal(r.status, 1, `${rel}: exit ${r.status}`);
      assert.equal(r.stdout, "");
      assert.match(r.stderr, /^clipdetect: cannot (read|parse)/, rel);
      seen.failed++;
    }
  }
  assert.ok(seen.ok > 20, JSON.stringify(seen));
  assert.ok(seen.failed >= 1, "the synthetic corpus has unparseable files");
});

test("bad inputs exit non-zero with a message", { skip }, () => {
  const bad = run([path.join(FIXTURES, "synthetic", "badtable_cmd.slp")]);
  assert.equal(bad.status, 1);
  assert.match(bad.stderr, /no Game Start block/);
  const missing = run([path.join(FIXTURES, "does-not-exist.slp")]);
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /cannot read/);
});

test("--config applies overrides and rejects unknown keys", { skip }, () => {
  const file = path.join(FIXTURES, "tournament", "20476_08f9e70f-2f7e-418b-8121-699ef271aacd.slp");
  const dir = fs.mkdtempSync(path.join(require("node:os").tmpdir(), "clipdetect-"));
  try {
    const strict = path.join(dir, "strict.json");
    fs.writeFileSync(strict, JSON.stringify({ comboMinHits: 99, comboMinPercent: 999, lowKillMaxPercent: 0 }));
    const r = run([file, "--config", strict]);
    assert.equal(r.status, 0, r.stderr);
    const doc = JSON.parse(r.stdout);
    assert.equal(doc.config.comboMinHits, 99);
    assert.equal(doc.clips.filter((c) => c.kind === "combo").length, 0);
    assert.equal(doc.clips.filter((c) => c.kind === "low_kill").length, 0);

    const typo = path.join(dir, "typo.json");
    fs.writeFileSync(typo, JSON.stringify({ comboMinHit: 3 }));
    const t = run([file, "--config", typo]);
    assert.equal(t.status, 2);
    assert.match(t.stderr, /unknown config key: comboMinHit/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
