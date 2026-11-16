// The landing page: what is live, what is next and how to enter, and the latest results.
(function () {
  // Old bookmarks of the form /#N land on that event's page before anything is drawn or fetched.
  const m = /^#(\d+)$/.exec(location.hash);
  if (m) {
    location.replace("/events/" + m[1]);
    return;
  }
  const POLL_MS = 10000;
  const { el, getJson, setNotice, STATE_LABEL, liveSetLines, fmtWhen, knownTime, eventDate, ordinal, eventHref, playerHref } = AB;
  const $ = (id) => document.getElementById(id);
  let drawnKey = "";
  let drawnOnce = false;

  // ---- pieces --------------------------------------------------------------

  function heading(text) {
    return el("h2", null, text);
  }

  // The event's name, linking to its bracket.
  function nameLink(ev) {
    const h = el("h3");
    const a = el("a", null, ev.name);
    a.href = eventHref(ev.id);
    h.append(a);
    return h;
  }

  function pill(text, href) {
    const a = el("a", "pill", text);
    a.href = href;
    return a;
  }

  function entrantsText(n) {
    return n + (n === 1 ? " entrant" : " entrants");
  }

  // ---- live now: every running event with its sets in play ------------------

  function liveCard(ev, lines, i) {
    const card = el("article", "home-card live-card c" + Math.min(i, 12));
    card.append(el("span", "flag", "Live"));
    card.append(nameLink(ev));
    if (lines.length) {
      // Each line links to the bracket, as on the stream page.
      const list = el("ol", "live-sets");
      for (const line of lines) {
        const li = el("li");
        const a = el("a", null, line);
        a.href = eventHref(ev.id);
        li.append(a);
        list.append(li);
      }
      card.append(list);
    } else {
      card.append(el("p", "quiet", "No set is being played right now."));
    }
    return card;
  }

  function renderLive(section, running, streamUrl) {
    section.hidden = running.length === 0;
    if (section.hidden) {
      section.replaceChildren();
      return;
    }
    section.replaceChildren(heading("Live now"));
    const list = el("div", "live-list");
    running.forEach(({ ev, lines }, i) => list.append(liveCard(ev, lines, i)));
    section.append(list);
    if (streamUrl) {
      const p = el("p", "actions");
      p.append(pill("Watch the stream", "/stream"));
      section.append(p);
    }
  }

  // ---- next up: the soonest sign-up event and how to enter it ----------------

  function renderNext(section, ev) {
    section.hidden = !ev;
    if (!ev) {
      section.replaceChildren();
      return;
    }
    const card = el("article", "home-card card c1");
    card.append(nameLink(ev));
    const when = el("p", "when");
    if (knownTime(ev.starts_at)) {
      const w = fmtWhen(ev.starts_at);
      const t = el("time", null, w.abs);
      t.dateTime = w.iso;
      when.append(t, el("span", "quiet", " · " + w.rel));
    } else {
      when.append(el("span", "quiet", "Time to be announced"));
    }
    card.append(when);
    if (ev.description) card.append(el("p", null, ev.description));
    card.append(el("p", "quiet", (STATE_LABEL[ev.state] || ev.state) + ", " + entrantsText(ev.entrants)));
    section.replaceChildren(heading("Next up"), card, AB.howToEnter(ev.id));
  }

  // ---- latest results: the podium of the newest complete event ---------------

  function renderLatest(section, result) {
    section.hidden = !result;
    if (!result) {
      section.replaceChildren();
      return;
    }
    const sum = result.summary;
    const card = el("article", "home-card card c2");
    card.append(nameLink(sum));
    const date = eventDate(sum);
    const bits = date ? [date, entrantsText(sum.entrants)] : [entrantsText(sum.entrants)];
    card.append(el("p", "quiet", bits.join(" · ")));
    const podium = el("ol", "podium");
    for (const p of (result.placings || []).filter((p) => p.place <= 3)) {
      const li = el("li");
      li.append(el("span", "place", ordinal(p.place)));
      const a = el("a", null, p.tag);
      a.href = playerHref(p.tag);
      li.append(a);
      podium.append(li);
    }
    card.append(podium);
    const actions = el("p", "actions");
    actions.append(pill("Full bracket", eventHref(sum.id)));
    card.append(actions);
    section.replaceChildren(heading("Latest results"), card);
  }

  // ---- the page -------------------------------------------------------------

  // The sign-up event that comes first: by start time (unannounced last), then by id.
  function nextUp(events) {
    const open = events.filter((e) => e.state === "signup");
    open.sort((a, b) => {
      const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
      const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
      return ta - tb || a.id - b.id;
    });
    return open[0] || null;
  }

  function render(events, running, history, streamUrl) {
    // Nothing new since the last drawing (within the minute, so relative times keep up): leave
    // the page exactly as it is.
    const key = JSON.stringify([events, running, history[0] || null, streamUrl, Math.floor(Date.now() / 60000)]);
    if (key === drawnKey) return;
    drawnKey = key;
    if (!events.length) {
      setNotice("status", "No events yet. This page updates when one is created.");
      for (const id of ["live", "next", "latest"]) {
        $(id).hidden = true;
        $(id).replaceChildren();
      }
      return;
    }
    setNotice("status", "");
    // The entrance plays on the first drawing only, never on a refresh.
    document.body.classList.toggle("enter", !drawnOnce);
    drawnOnce = true;
    renderLive($("live"), running, streamUrl);
    renderNext($("next"), nextUp(events));
    renderLatest($("latest"), history[0] || null);
  }

  async function refresh() {
    try {
      const [list, hist, stream] = await Promise.all([
        getJson("/v1/events"),
        // Only the newest result is shown, so only that one is asked for.
        getJson("/v1/history?limit=1"),
        // The stream route may be missing or unconfigured; then there is no stream to watch.
        getJson("/v1/stream").catch(() => null),
      ]);
      const events = (Array.isArray(list) ? list : []).slice().sort((a, b) => a.id - b.id);
      const history = Array.isArray(hist) ? hist : [];
      const streamUrl = stream && typeof stream.url === "string" && stream.url ? stream.url : null;
      const running = [];
      for (const ev of events.filter((e) => e.state === "running")) {
        try {
          const view = await getJson("/v1/events/" + ev.id);
          running.push({ ev, lines: liveSetLines(view) });
        } catch (err) {
          // Gone between the two requests: it is no longer live.
          if (!err || err.status !== 404) throw err;
        }
      }
      render(events, running, history, streamUrl);
    } catch (err) {
      if (!drawnKey) setNotice("status", "Couldn’t reach the server just now; trying again in a few seconds.");
    }
  }

  AB.poll(refresh, POLL_MS);
})();
