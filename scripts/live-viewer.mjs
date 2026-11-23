// A live relay viewer for tests (Node 22+, built-in WebSocket; no packages).
//
//   node scripts/live-viewer.mjs watch URL EVENT SET OUT.bin [TIMEOUT_SECS]
//     waits for the set to be listed by GET /v1/live, dials GET /v1/live/{event}/{set}, writes
//     the concatenated game_event payloads of the first game it sees to OUT.bin, and exits 0
//     after that game's end_game (1 on timeout). Prints a RESULT line with counts and gaps.
//   node scripts/live-viewer.mjs compare OUT.bin FILE.slp
//     exits 0 when OUT.bin equals the .slp's `raw` element byte for byte.
import { writeFileSync, readFileSync } from "node:fs";

const [mode, ...args] = process.argv.slice(2);

function slpRaw(path) {
  const b = readFileSync(path);
  const head = Buffer.from("{U\x03raw[$U#l", "latin1");
  if (!b.subarray(0, head.length).equals(head)) throw new Error(path + " is not a .slp");
  const n = b.readUInt32BE(head.length);
  return b.subarray(head.length + 4, head.length + 4 + n);
}

if (mode === "compare") {
  const [bin, slp] = args;
  const got = readFileSync(bin);
  const raw = slpRaw(slp);
  const same = got.equals(raw);
  let at = -1;
  if (!same) for (let i = 0; i < Math.min(got.length, raw.length); i++) if (got[i] !== raw[i]) { at = i; break; }
  console.log(`COMPARE ${same ? "IDENTICAL" : "DIFFERENT"} live=${got.length} raw=${raw.length}` + (same ? "" : ` first_diff=${at}`));
  process.exit(same ? 0 : 1);
} else if (mode === "watch") {
  const [url, event, set, out, timeoutSecs = "900"] = args;
  const deadline = Date.now() + Number(timeoutSecs) * 1000;
  setTimeout(() => { console.log("RESULT timeout"); process.exit(1); }, Number(timeoutSecs) * 1000).unref();
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  for (;;) {
    try {
      const list = await (await fetch(`${url}/v1/live?event=${event}`)).json();
      if (list.some((g) => g.set_id === Number(set))) break;
    } catch {}
    if (Date.now() > deadline) process.exit(1);
    await sleep(250);
  }
  const ws = new WebSocket(url.replace(/^http/, "ws") + `/v1/live/${event}/${set}`);
  const parts = [];
  let game = null, msgs = 0, last = -1, gaps = 0, bytes = 0;
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (game === null) game = m.game_no;
    if (m.game_no !== game) return;
    msgs++;
    if (last >= 0 && m.cursor !== last + 1) gaps++;
    last = m.cursor;
    if (m.type === "game_event") {
      const b = Buffer.from(m.payload, "base64");
      parts.push(b);
      bytes += b.length;
    } else if (m.type === "end_game") {
      writeFileSync(out, Buffer.concat(parts));
      console.log(`RESULT game=${game} messages=${msgs} event_bytes=${bytes} gaps=${gaps}`);
      ws.close();
      process.exit(0);
    }
  };
  ws.onclose = () => { if (game === null) { console.log("RESULT closed before a game"); process.exit(1); } };
} else {
  console.error("usage: live-viewer.mjs watch URL EVENT SET OUT.bin [TIMEOUT] | compare OUT.bin FILE.slp");
  process.exit(2);
}
