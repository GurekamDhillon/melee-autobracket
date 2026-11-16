// The landing page: one lead sticker for what is on (the live event, else the next one), the
// last champion beside it, and the run of events underneath.
(function () {
  // Old bookmarks of the form /#N land on that event's page before anything is drawn or fetched.
  const m = /^#(\d+)$/.exec(location.hash);
  if (m) {
    location.replace("/events/" + m[1]);
    return;
  }
  const POLL_MS = 10000;
  const { el, icon, getJson, setNotice, liveSetLines, scoreboard, pill, chip, entrantsText, nextUp, sideRounds,
    fmtWhen, knownTime, eventDate, ordinal, eventHref, playerHref } = AB;
  const $ = (id) => document.getElementById(id);
  let drawnKey = "";
  let drawnOnce = false;

  // ---- pieces --------------------------------------------------------------

  // The event's name at display size, linking to its bracket.
  function heroName(ev) {
    const h = el("h2", "hero-name");
    const a = el("a", null, ev.name);
    a.href = eventHref(ev.id);
    h.append(a);
    return h;
  }

  function stamp(text, cls) {
    const s = el("span", "stamp hero-stamp " + cls, text);
    return s;
  }

  // The start time: the local time large, the relative phrase as a chip.
  function whenLine(ev, big) {
    const p = el("p", "hero-when");
    if (!knownTime(ev.starts_at)) {
      p.append(el("span", "seat-chip", "Time to be announced"));
      return p;
    }
    const w = fmtWhen(ev.starts_at);
    const short = new Date(ev.starts_at * 1000).toLocaleString(undefined, { weekday: "long", hour: "numeric", minute: "2-digit" });
    const t = el("time", "when-big", big ? w.abs : short);
    t.dateTime = w.iso;
    p.append(t, chip(w.rel, "hot"));
    return p;
  }

  // The event's progress as the kit's run strip, one mark per round, small.
  function miniStrip(view) {
    const strip = el("div", "mini-strip");
    strip.setAttribute("aria-hidden", "true");
    if (!view || !view.bracket) return strip;
    for (const side of ["Winners", "Losers", "GrandFinal"]) {
      const rounds = sideRounds(view.bracket, side);
      if (!rounds.length) continue;
      const grp = el("span", "marks");
      for (const r of rounds) {
        const mark = el("span", "mark " + (r.star ? "star " : "disc ") + r.status);
        if (r.star) mark.append(icon("star"));
        grp.append(mark);
      }
      strip.append(grp);
    }
    return strip;
  }

  // Entered players as filled seats, then the empty seats up to the next bracket size.
  function seats(n) {
    const box = el("div", "seats");
    let size = 4;
    while (size < n && size < 64) size *= 2;
    const row = el("span", "seat-row");
    row.setAttribute("aria-hidden", "true");
    for (let i = 0; i < Math.min(size, 32); i++) row.append(el("span", i < n ? "seat-dot in" : "seat-dot"));
    box.append(row, el("span", "seat-words", n === 0 ? "Nobody has entered yet" : n + " entered so far"));
    return box;
  }

  // ---- live now: the running event leads, its sets as scoreboards --------------

  function liveCard(ev, lines, view, i) {
    const card = el("article", "hero live c" + Math.min(i, 12));
    card.append(stamp("Live now", "live"));
    card.append(heroName(ev));
    const facts = el("div", "hero-facts");
    facts.append(chip(entrantsText(ev.entrants)), miniStrip(view));
    card.append(facts);
    if (lines.length) {
      const list = el("ol", "boards");
      for (const line of lines) {
        const li = el("li");
        li.append(scoreboard(line, ev.id));
        list.append(li);
      }
      card.append(list);
    } else {
      card.append(el("p", "hero-quiet", "No set is being played right now. The next one starts when both players are ready."));
    }
    const actions = el("p", "actions");
    const a = pill("Open the bracket", "", "sunny");
    a.href = eventHref(ev.id);
    actions.append(a);
    card.append(actions);
    return card;
  }

  function renderLive(section, running, streamUrl) {
    const [first, ...rest] = running;
    const card = liveCard(first.ev, first.lines, first.view, 0);
    if (streamUrl) card.querySelector(".actions").append(pill("Watch the stream", "/stream"));
    if (rest.length) {
      const also = el("p", "also");
      also.append(el("span", null, "Also live: "));
      rest.forEach(({ ev }, i) => {
        if (i) also.append(", ");
        const a = el("a", null, ev.name);
        a.href = eventHref(ev.id);
        also.append(a);
      });
      card.append(also);
    }
    section.replaceChildren(card);
  }

  // ---- next up: the soonest sign-up event and how to enter it ----------------

  function nextCard(ev, big) {
    const card = el("article", big ? "hero next" : "side-card next");
    card.append(stamp("Next up", "next"));
    if (big) card.append(heroName(ev));
    else {
      const h = el("h3");
      const a = el("a", null, ev.name);
      a.href = eventHref(ev.id);
      h.append(a);
      card.append(h);
    }
    card.append(whenLine(ev, big));
    if (ev.description) card.append(el("p", "hero-desc", ev.description));
    card.append(seats(ev.entrants));
    if (big) {
      const actions = el("p", "actions");
      actions.append(pill("See who’s in", eventHref(ev.id), "sunny"), pill("Full schedule", "/schedule"));
      card.append(actions, AB.howToEnter(ev.id));
    } else {
      const actions = el("p", "actions");
      actions.append(pill("How to enter", "/schedule"));
      card.append(actions);
    }
    return card;
  }

  // When nothing is on and nothing is planned.
  function quietCard() {
    const card = el("article", "hero quiet");
    card.append(el("h2", "hero-name", "Nothing on right now"));
    card.append(el("p", "hero-desc", "No event is running or open for sign-ups. Past brackets stay up in the history."));
    const actions = el("p", "actions");
    actions.append(pill("Browse past events", "/history", "sunny"));
    card.append(actions);
    return card;
  }

  // ---- latest results: the newest champion in the kit's burst ------------------

  function renderLatest(result) {
    const sum = result.summary;
    const box = el("section", "latest");
    box.append(el("h2", "section-title", "Latest results"));
    const placings = result.placings || [];
    const champ = placings.find((p) => p.place === 1);
    if (champ) {
      const burst = el("p", "champ home-champ");
      burst.append(icon("burst"));
      const text = el("span", "champ-text");
      text.append(el("span", "champ-label", "Champion"), el("span", "sr", ": "));
      const name = el("a", champ.tag.length > 9 ? "champ-tag long" : "champ-tag", champ.tag);
      name.href = playerHref(champ.tag);
      text.append(name);
      burst.append(text);
      box.append(burst);
    }
    const card = el("div", "side-card result");
    const h = el("h3");
    const a = el("a", null, sum.name);
    a.href = eventHref(sum.id);
    h.append(a);
    card.append(h);
    const chips = el("p", "chips");
    const date = eventDate(sum);
    if (date) chips.append(chip(date));
    chips.append(chip(entrantsText(sum.entrants)));
    card.append(chips);
    const podium = el("ol", "podium");
    for (const p of placings.filter((p) => p.place > 1 && p.place <= 3)) {
      const li = el("li");
      li.append(el("span", "place", ordinal(p.place)));
      const t = el("a", null, p.tag);
      t.href = playerHref(p.tag);
      li.append(t);
      podium.append(li);
    }
    if (podium.childElementCount) card.append(podium);
    const actions = el("p", "actions");
    actions.append(pill("Full results", eventHref(sum.id)));
    card.append(actions);
    box.append(card);
    return box;
  }

  // ---- the run of events: done, live, then what is coming, one mark each ---------

  function renderRun(section, events) {
    const done = events.filter((e) => e.state === "complete").slice(-1);
    const live = events.filter((e) => e.state === "running");
    const soon = events.filter((e) => e.state === "signup").sort((a, b) => {
      const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
      const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
      return ta - tb || a.id - b.id;
    });
    const items = [...done, ...live, ...soon];
    section.hidden = items.length < 2 || (live.length + soon.length) === 0;
    if (section.hidden) {
      section.replaceChildren();
      return;
    }
    const list = el("ol", "event-run");
    let nextMarked = live.length > 0;
    for (const ev of items) {
      let status = "todo";
      if (ev.state === "complete") status = "done";
      else if (ev.state === "running") status = "now";
      else if (!nextMarked) {
        status = "next";
        nextMarked = true;
      }
      const li = el("li", "run-item " + status);
      li.append(el("span", "mark disc " + (status === "next" ? "now" : status)));
      const a = el("a", "run-name", ev.name);
      a.href = eventHref(ev.id);
      li.append(a);
      let when = "Date to be announced";
      if (ev.state === "running") when = "Live now";
      else if (knownTime(ev.starts_at)) when = fmtWhen(ev.starts_at).rel;
      else if (ev.state === "complete") when = eventDate(ev) || "Done";
      li.append(el("span", "run-when", when));
      list.append(li);
    }
    section.replaceChildren(el("h2", "section-title", "Coming up"), list);
  }

  // ---- the page -------------------------------------------------------------

  function render(events, running, history, streamUrl) {
    // Nothing new since the last drawing (within the minute, so relative times keep up): leave
    // the page exactly as it is.
    const key = JSON.stringify([events, running, history[0] || null, streamUrl, Math.floor(Date.now() / 60000)]);
    if (key === drawnKey) return;
    drawnKey = key;
    if (!events.length) {
      setNotice("status", "No events yet. This page updates when one is created.");
      for (const id of ["lead", "side", "run"]) $(id).replaceChildren();
      return;
    }
    setNotice("status", "");
    // The entrance plays on the first drawing only, never on a refresh.
    document.body.classList.toggle("enter", !drawnOnce);
    drawnOnce = true;
    const next = nextUp(events);
    const lead = $("lead");
    if (running.length) renderLive(lead, running, streamUrl);
    else lead.replaceChildren(next ? nextCard(next, true) : quietCard());
    const side = $("side");
    side.replaceChildren();
    if (history[0]) side.append(renderLatest(history[0]));
    if (running.length && next) side.append(nextCard(next, false));
    renderRun($("run"), events);
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
          running.push({ ev, lines: liveSetLines(view), view });
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
