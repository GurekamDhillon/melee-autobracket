// The stream page: the organiser's stream in a frame (its origin alone is allowed by this
// page's CSP; the URL arrives as JSON, never in the HTML), and what is being played right now.
(function () {
  const POLL_MS = 10000;
  const { el, getJson, setNotice, slotEntrant, sideRounds } = AB;
  const $ = (id) => document.getElementById(id);
  const now = $("now");
  let drawnKey = "";

  // ---- the stream, asked for once ----------------------------------------------------------

  function showStream(info) {
    const url = info && typeof info.url === "string" ? info.url : null;
    if (!url) {
      setNotice("no-stream", "No stream is configured on this server.");
      return;
    }
    $("player").src = url;
    $("player").hidden = false;
    $("open-link").href = url;
    $("open").hidden = false;
  }

  getJson("/v1/stream").then(showStream, () => {
    setNotice("no-stream", "Couldn’t reach the server just now; reload to try again.");
  });

  // ---- now playing: the ready sets of every running event -----------------------------------

  const SIDES = ["Winners", "Losers", "GrandFinal"];

  // "A vs B · Winners semis · 1–0" for every ready set of one event, in bracket order.
  function liveSets(view) {
    const b = view.bracket;
    if (!b) return [];
    const tags = view.tags || [];
    const tagOf = (n) => (typeof tags[n] === "string" ? tags[n] : "Entrant " + (n + 1));
    const scores = new Map((view.scores || []).map(([id, wins]) => [id, wins]));
    const out = [];
    for (const side of SIDES) {
      for (const round of sideRounds(b, side)) {
        for (const set of round.sets) {
          if (set.state !== "Ready") continue;
          const [a, c] = set.slots.map(slotEntrant);
          if (a === null || c === null) continue;
          const score = scores.get(set.id) || [0, 0];
          out.push(tagOf(a) + " vs " + tagOf(c) + " · " + round.name + " · " + score[0] + "–" + score[1]);
        }
      }
    }
    return out;
  }

  function render(events) {
    const key = JSON.stringify(events);
    if (key === drawnKey) return;
    drawnKey = key;
    const shown = events.filter((ev) => ev.sets.length);
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
      const list = el("ol", "rows");
      for (const text of ev.sets) {
        const li = el("li", "row-line");
        const a = el("a", null, text);
        a.href = AB.eventHref(ev.id);
        li.append(a);
        list.append(li);
      }
      parts.push(list);
    }
    now.replaceChildren(...parts);
  }

  async function refresh() {
    try {
      const list = await getJson("/v1/events");
      const running = (Array.isArray(list) ? list : []).filter((ev) => ev.state === "running");
      const views = await Promise.all(running.map((ev) => getJson("/v1/events/" + encodeURIComponent(ev.id))));
      render(running.map((ev, i) => ({ id: ev.id, name: ev.name, sets: liveSets(views[i]) })));
    } catch (err) {
      if (!drawnKey) now.replaceChildren(el("p", "notice", "Couldn’t reach the server just now; trying again in a few seconds."));
    }
  }

  AB.poll(refresh, POLL_MS);
})();
