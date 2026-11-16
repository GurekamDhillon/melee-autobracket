// The stream page: the organiser's stream in a frame (its origin alone is allowed by this
// page's CSP; the URL arrives as JSON, never in the HTML), and what is being played right now.
// While no set is live the frame stays empty behind an off-air card, so a stream server's own
// "not found" page never shows; "Try the stream anyway" loads it regardless.
(function () {
  const POLL_MS = 10000;
  const { el, getJson, setNotice, liveSetLines, scoreboard, nextUp, knownTime, fmtWhen } = AB;
  const $ = (id) => document.getElementById(id);
  const now = $("now");
  const player = $("player");
  let drawnKey = "";
  let streamUrl = null;
  let onAir = false;
  let forced = false;

  // ---- the frame: shown while a set is live or when asked for -----------------------------

  function showFrame() {
    const show = streamUrl !== null && (onAir || forced);
    if (show && player.getAttribute("src") !== streamUrl) player.src = streamUrl;
    if (!show) player.removeAttribute("src");
    player.hidden = !show;
    $("offair").hidden = show;
    $("try").hidden = streamUrl === null || show;
    const air = $("air");
    air.textContent = onAir ? "On air" : "Off air";
    air.className = onAir ? "status running" : "status off";
  }

  function showStream(info) {
    const url = info && typeof info.url === "string" ? info.url : null;
    if (!url) {
      setNotice("no-stream", "No stream is configured on this server.");
      showFrame();
      return;
    }
    streamUrl = url;
    $("open-link").href = url;
    $("open").hidden = false;
    showFrame();
  }

  $("try").addEventListener("click", () => {
    forced = true;
    showFrame();
    player.focus();
  });

  getJson("/v1/stream").then(showStream, () => {
    setNotice("no-stream", "Couldn’t reach the server just now; reload to try again.");
  });

  // ---- off air: what comes next --------------------------------------------------------------

  function renderNext(ev) {
    const p = $("offair-next");
    p.hidden = !ev;
    if (!ev) return;
    const a = el("a", null, ev.name);
    a.href = AB.eventHref(ev.id);
    p.replaceChildren("Next up: ", a, knownTime(ev.starts_at) ? ", " + fmtWhen(ev.starts_at).rel + "." : ", time to be announced.");
  }

  // ---- now playing: the ready sets of every running event, as scoreboards --------------------

  function render(events) {
    const key = JSON.stringify(events);
    if (key === drawnKey) return;
    drawnKey = key;
    const shown = events.filter((ev) => ev.sets.length);
    onAir = shown.length > 0;
    showFrame();
    if (!shown.length) {
      now.replaceChildren(el("p", "notice", "Nothing is being played right now."));
      return;
    }
    const parts = [];
    for (const ev of shown) {
      const h = el("h3");
      const link = el("a", null, ev.name);
      link.href = AB.eventHref(ev.id);
      h.append(link);
      parts.push(h);
      const list = el("ol", "boards");
      for (const line of ev.sets) {
        const li = el("li");
        li.append(scoreboard(line, ev.id));
        list.append(li);
      }
      parts.push(list);
    }
    now.replaceChildren(...parts);
  }

  async function refresh() {
    try {
      const list = await getJson("/v1/events");
      const all = Array.isArray(list) ? list : [];
      renderNext(nextUp(all));
      const running = all.filter((ev) => ev.state === "running");
      // An event gone between the two requests (404) is no longer live; the rest still show.
      const views = await Promise.all(running.map((ev) =>
        getJson("/v1/events/" + encodeURIComponent(ev.id)).catch((err) => {
          if (err && err.status === 404) return null;
          throw err;
        })));
      const shown = [];
      running.forEach((ev, i) => {
        if (views[i]) shown.push({ id: ev.id, name: ev.name, sets: liveSetLines(views[i]) });
      });
      render(shown);
    } catch (err) {
      if (!drawnKey) now.replaceChildren(el("p", "notice", "Couldn’t reach the server just now; trying again in a few seconds."));
    }
  }

  AB.poll(refresh, POLL_MS);
})();
