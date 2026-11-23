// The live view: one set's current game, from the server's live relay (/v1/live/{event}/{set}).
// The messages carry the game's Slippi event bytes (base64); this reads the Event Payloads,
// Game Start and Post-Frame events and draws each fighter as a disc with its damage and stocks.
// Display only: nothing here is a result.
(function () {
  const { el, getJson, setNotice } = AB;
  const $ = (id) => document.getElementById(id);
  const m = location.pathname.match(/^\/live\/(\d+)\/(\d+)$/);
  if (!m) {
    setNotice("status", "This address names no set.");
    return;
  }
  const eventId = Number(m[1]);
  const setId = Number(m[2]);
  const canvas = $("stage");
  const ctx = canvas.getContext("2d");
  const hud = $("hud");

  // External character ids as Game Start records them.
  const CHARS = ["Captain Falcon", "Donkey Kong", "Fox", "Mr. Game & Watch", "Kirby", "Bowser", "Link",
    "Luigi", "Mario", "Marth", "Mewtwo", "Ness", "Peach", "Pikachu", "Ice Climbers", "Jigglypuff", "Samus",
    "Yoshi", "Zelda", "Sheik", "Falco", "Young Link", "Dr. Mario", "Roy", "Pichu", "Ganondorf"];
  const PORT_COLOURS = ["--p1", "--p2", "--accent", "--on-surface-quiet"];

  let tags = ["", ""];
  let sizes = null; // event code -> payload size, from Event Payloads (0x35)
  let players = [null, null, null, null];
  let gameNo = null;
  let ended = false;
  let frame = 0;
  let dirty = true;

  function resetGame(n) {
    gameNo = n;
    ended = false;
    sizes = null;
    frame = 0;
    players = [null, null, null, null];
    dirty = true;
  }

  function b64(s) {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }

  // Walks whole events (each payload starts and ends on an event boundary).
  function events(bytes) {
    const dv = new DataView(bytes.buffer);
    let i = 0;
    while (i < bytes.length) {
      const code = bytes[i];
      if (code === 0x35) {
        const len = bytes[i + 1];
        sizes = {};
        for (let k = i + 2; k + 2 < i + 1 + len; k += 3) sizes[bytes[k]] = dv.getUint16(k + 1);
        i += 1 + len;
        continue;
      }
      if (!sizes || sizes[code] === undefined) return; // cannot walk further
      const at = i;
      if (code === 0x36) {
        for (let p = 0; p < 4; p++) {
          const type = bytes[at + 0x66 + 0x24 * p];
          players[p] = type === 3 ? null : { char: bytes[at + 0x65 + 0x24 * p], x: 0, y: 0, percent: 0, stocks: 0 };
        }
        dirty = true;
      } else if (code === 0x38 && at + 0x22 <= bytes.length) {
        const pl = players[bytes[at + 5]];
        if (pl && !bytes[at + 6]) {
          frame = dv.getInt32(at + 1);
          pl.x = dv.getFloat32(at + 0x0A);
          pl.y = dv.getFloat32(at + 0x0E);
          pl.percent = dv.getFloat32(at + 0x16);
          pl.stocks = bytes[at + 0x21];
          dirty = true;
        }
      }
      i = at + 1 + sizes[code];
    }
  }

  function colour(name) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#888";
  }

  function plural(n) {
    return n === 1 ? "1 stock" : n + " stocks";
  }

  function draw() {
    requestAnimationFrame(draw);
    if (!dirty) return;
    dirty = false;
    const w = canvas.width;
    const h = canvas.height;
    const scale = w / 460;
    const sx = (x) => w / 2 + x * scale;
    const sy = (y) => h * 0.72 - y * scale;
    ctx.clearRect(0, 0, w, h);
    ctx.lineWidth = 6;
    ctx.strokeStyle = colour("--outline");
    ctx.fillStyle = colour("--surface");
    ctx.beginPath();
    ctx.rect(sx(-85), sy(0), sx(85) - sx(-85), 26);
    ctx.fill();
    ctx.stroke();
    const shown = [];
    players.forEach((pl, port) => {
      if (!pl) return;
      shown.push([port, pl]);
      ctx.fillStyle = colour(PORT_COLOURS[port]);
      ctx.beginPath();
      ctx.arc(sx(pl.x), sy(pl.y) - 18, 18, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();
    });
    hud.replaceChildren(...shown.map(([port, pl], i) => {
      const li = el("li", "live-card p" + (port + 1));
      li.append(el("span", "live-tag", tags[i] || "P" + (port + 1)));
      li.append(el("span", "live-pct", Math.round(pl.percent) + "%"));
      li.append(el("span", "live-char", CHARS[pl.char] || "Fighter"));
      li.append(el("span", "live-stocks", plural(pl.stocks)));
      return li;
    }));
  }

  function clock() {
    if (frame <= 0) return "";
    const secs = Math.floor(frame / 60);
    return " · " + Math.floor(secs / 60) + ":" + String(secs % 60).padStart(2, "0");
  }

  function status() {
    if (gameNo === null) setNotice("status", "Waiting for the host's game to start…");
    else if (ended) setNotice("status", "Game " + gameNo + " is over. The next game shows here when it starts.");
    else setNotice("status", "Game " + gameNo + " · live" + clock());
  }

  let backoff = 1000;
  function dial() {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(proto + "//" + location.host + "/v1/live/" + eventId + "/" + setId);
    ws.onopen = () => {
      backoff = 1000;
    };
    ws.onmessage = (e) => {
      let msg;
      try {
        msg = JSON.parse(e.data);
      } catch {
        return;
      }
      if (typeof msg.game_no === "number" && msg.game_no !== gameNo) resetGame(msg.game_no);
      if (msg.type === "game_event" && typeof msg.payload === "string") {
        try {
          events(b64(msg.payload));
        } catch (err) {
          console.error(err);
        }
      } else if (msg.type === "end_game") {
        ended = true;
      }
      status();
    };
    ws.onclose = () => {
      setNotice("status", gameNo === null ? "This set is not live right now; trying again…" : "Connection lost; reconnecting…");
      setTimeout(dial, backoff);
      backoff = Math.min(backoff * 2, 15000);
    };
  }

  async function loadTags() {
    try {
      const view = await getJson("/v1/events/" + eventId);
      const set = view.bracket && view.bracket.sets.find((s) => s.id === setId);
      if (set) {
        tags = set.slots.map((s) => {
          const n = AB.slotEntrant(s);
          return n === null ? "" : view.tags[n] || "";
        });
        $("meta").textContent = view.summary.name + " · " + tags.filter(Boolean).join(" vs ");
      }
      dirty = true;
    } catch (err) {
      console.error(err);
    }
  }

  status();
  loadTags();
  dial();
  requestAnimationFrame(draw);
})();
