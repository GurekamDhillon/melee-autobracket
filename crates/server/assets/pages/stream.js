// The stream page: the organiser's stream in a frame (its origin alone is allowed by this
// page's CSP; the URL arrives as JSON, never in the HTML), and what is being played right now.
// While no set is live the frame stays empty behind an off-air card, so a stream server's own
// "not found" page never shows; "Try the stream anyway" loads it regardless.
(function () {
  const POLL_MS = 10000;
  const { el, getJson, setNotice, liveSetLines, scoreboard, knownTime, fmtWhen } = AB;
  const $ = (id) => document.getElementById(id);
  const now = $("now");
  const player = $("player");
  const cover = $("cover");
  const caption = $("caption");
  let drawnKey = "";

  // The branded cover sits over the frame until the stream page has loaded inside it.
  player.addEventListener("load", () => {
    if (player.hasAttribute("src")) cover.hidden = true;
  });
  let streamUrl = null;
  let onAir = false;
  let forced = false;
  let queueIds = [];
  let queueDrawn = false;
  let liveTop = null;    // the first live set: its event, its line, and how many sets are live
  let queueFirst = null; // the first event in the queue, named while off air
  const side = $("side");
  const tabs = { now: $("tab-now"), next: $("tab-next") };

  // ---- the sidebar tabs: on a phone one panel at a time; on a wide screen both show -----------

  function selectTab(name, focus) {
    side.dataset.tab = name;
    for (const key of Object.keys(tabs)) {
      const on = key === name;
      tabs[key].setAttribute("aria-selected", on ? "true" : "false");
      tabs[key].tabIndex = on ? 0 : -1;
      if (on && focus) tabs[key].focus();
    }
  }

  tabs.now.addEventListener("click", () => selectTab("now", false));
  tabs.next.addEventListener("click", () => selectTab("next", false));
  $("side-tabs").addEventListener("keydown", (ev) => {
    const order = ["now", "next"];
    const cur = order.indexOf(side.dataset.tab);
    let to = null;
    if (ev.key === "ArrowRight") to = order[(cur + 1) % order.length];
    else if (ev.key === "ArrowLeft") to = order[(cur + order.length - 1) % order.length];
    else if (ev.key === "Home") to = order[0];
    else if (ev.key === "End") to = order[order.length - 1];
    if (to === null) return;
    ev.preventDefault();
    selectTab(to, true);
  });

  // ---- the frame: shown while a set is live or when asked for -----------------------------

  function showFrame() {
    const show = streamUrl !== null && (onAir || forced);
    if (show && player.getAttribute("src") !== streamUrl) {
      player.src = streamUrl;
      cover.hidden = false;
    }
    if (!show) {
      player.removeAttribute("src");
      cover.hidden = true;
    }
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

  function nextLink(ev) {
    const a = el("a", null, ev.name);
    a.href = AB.eventHref(ev.id);
    return a;
  }

  // The sign-up events in the order they will run: by start time (unannounced last), then id.
  function queueOf(events) {
    const open = events.filter((e) => e.state === "signup");
    open.sort((a, b) => {
      const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
      const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
      return ta - tb || a.id - b.id;
    });
    return open;
  }

  // "in ~25 min" for the next two hours, then the usual relative phrase; "Time to be announced"
  // when no start time is set.
  function etaText(unix) {
    if (!knownTime(unix)) return "Time to be announced";
    const mins = Math.round((unix - Date.now() / 1000) / 60);
    if (mins <= 0) return "starting now";
    if (mins < 120) return "in ~" + mins + " min";
    return fmtWhen(unix).rel;
  }

  function queueRow(ev, n) {
    const a = el("a", "board-row queue-row");
    a.href = AB.eventHref(ev.id);
    const eta = etaText(ev.starts_at);
    a.setAttribute("aria-label", "Number " + n + " in line: " + ev.name + ", " + eta + ", " + AB.entrantsText(ev.entrants || 0));
    const face = el("span", "queue-face");
    face.setAttribute("aria-hidden", "true");
    face.append(el("span", n === 1 ? "queue-no first" : "queue-no", String(n)));
    face.append(el("span", "queue-name", ev.name));
    const sub = el("span", "queue-sub");
    sub.append(el("span", "queue-eta", eta));
    sub.append(el("span", "queue-meta", AB.entrantsText(ev.entrants || 0) + (knownTime(ev.starts_at) ? " · " + AB.fmtDate(ev.starts_at) : "")));
    face.append(sub);
    a.append(face);
    return a;
  }

  // The queue: every sign-up event in run order, numbered; the off-air card names the first.
  function renderNext(queue) {
    const p = $("offair-next");
    const first = queue[0] || null;
    p.hidden = !first;
    if (first) p.replaceChildren("Next up: ", nextLink(first), knownTime(first.starts_at) ? ", " + fmtWhen(first.starts_at).rel + "." : ", time to be announced.");
    const box = $("up-next");
    const empty = $("queue-empty");
    box.hidden = !queue.length;
    empty.hidden = queue.length > 0;
    queueIds = queue.map((ev) => ev.id);
    queueFirst = queue[0] || null;
    queueDrawn = true;
    if (!queue.length) {
      empty.textContent = "No event is open for sign-ups right now.";
      $("queue").replaceChildren();
      return;
    }
    $("queue").replaceChildren(...queue.map((ev, i) => {
      const li = el("li");
      li.append(queueRow(ev, i + 1));
      return li;
    }));
  }

  // The pinned link at the foot of the sidebar: the first event on the air, else the next one.
  function renderBracketLink(liveId) {
    const id = liveId !== null ? liveId : queueIds.length ? queueIds[0] : null;
    const link = $("bracket-link");
    link.hidden = id === null;
    if (id !== null) link.href = AB.eventHref(id);
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

  // ---- the caption under the frame: the live match, or the next event while off air ----------

  // The live set as a sticker: P1 and P2 chips beside the names, the score between them.
  function matchFace(line, eventId) {
    const a = el("a", "cap-match");
    a.href = AB.eventHref(eventId) + "#set-" + line.id;
    a.setAttribute("aria-label", line.words);
    const side = (i) => {
      const s = el("span", "cap-side s" + i);
      s.append(el("span", "port p" + (i + 1), "P" + (i + 1)), el("span", "cap-tag", i ? line.b : line.a));
      return s;
    };
    const lead = line.score[0] === line.score[1] ? -1 : line.score[0] > line.score[1] ? 0 : 1;
    const score = el("span", "cap-score");
    score.append(
      el("span", lead === 1 ? "trail" : "", line.score[0]),
      el("span", "dash", "–"),
      el("span", lead === 0 ? "trail" : "", line.score[1]),
    );
    a.append(side(0), score, side(1));
    return a;
  }

  function drawCaption() {
    if (!liveTop && !queueFirst) {
      caption.hidden = true;
      return;
    }
    const ev = liveTop || queueFirst;
    const head = el("div", "cap-head");
    head.append(el("span", "cap-event", ev.name), el("span", "cap-round", liveTop ? liveTop.line.round : "Up next"));
    if (liveTop && liveTop.count > 1) head.append(el("span", "cap-more", "+" + (liveTop.count - 1) + " more"));
    const body = liveTop
      ? matchFace(liveTop.line, liveTop.id)
      : el("p", "cap-wait", etaText(queueFirst.starts_at) + " · " + AB.entrantsText(queueFirst.entrants || 0));
    const foot = el("div", "cap-foot");
    foot.append(AB.pill("Open bracket", AB.eventHref(ev.id), "sunny"));
    if (liveTop && queueFirst) foot.append(el("span", "cap-next", "Up next: " + queueFirst.name));
    caption.replaceChildren(head, body, foot);
    caption.hidden = false;
  }

  async function refresh() {
    try {
      const list = await getJson("/v1/events");
      const all = Array.isArray(list) ? list : [];
      renderNext(queueOf(all));
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
      renderBracketLink(shown.length ? shown[0].id : null);
      const top = shown[0] || null;
      liveTop = top ? {
        id: top.id,
        name: top.name,
        line: top.sets[0],
        count: shown.reduce((n, ev) => n + ev.sets.length, 0),
      } : null;
      drawCaption();
    } catch (err) {
      if (!drawnKey) now.replaceChildren(el("p", "notice", "Couldn’t reach the server just now; trying again in a few seconds."));
      if (!queueDrawn) {
        $("up-next").hidden = true;
        setNotice("queue-empty", "Couldn’t reach the server just now; trying again in a few seconds.");
      }
    }
  }

  AB.poll(refresh, POLL_MS);
})();
