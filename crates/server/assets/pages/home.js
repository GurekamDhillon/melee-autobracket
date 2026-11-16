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
    fmtWhen, knownTime, eventDate, ordinal, eventHref, playerHref, avatar } = AB;
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

  // Entered players as a stack of their avatars (the first six, then a count), once the names are
  // known; until then, filled seats up to the next bracket size.
  function seats(n, tags) {
    const box = el("div", "seats");
    const words = el("span", "seat-words", n === 0 ? "Nobody has entered yet" : n + " entered so far");
    if (tags && tags.length) {
      const stack = el("span", "avatar-stack");
      for (const t of tags.slice(0, 6)) stack.append(avatar(t));
      if (tags.length > 6) stack.append(el("span", "disc more", "+" + (tags.length - 6)));
      box.append(stack, words);
      return box;
    }
    let size = 4;
    while (size < n && size < 64) size *= 2;
    const row = el("span", "seat-row");
    row.setAttribute("aria-hidden", "true");
    for (let i = 0; i < Math.min(size, 32); i++) row.append(el("span", i < n ? "seat-dot in" : "seat-dot"));
    box.append(row, words);
    return box;
  }

  // The header band shared by the side cards: a title on the ink band, a chip on its right.
  function sideHead(title, extra) {
    const head = el("header", "side-head");
    head.append(el("h2", "side-title", title));
    if (extra) head.append(extra);
    return head;
  }

  // The time to go as a banner: the relative phrase large, the date beneath; "Date TBA" when the
  // start is not set.
  function countdown(ev) {
    const b = el("div", "countdown");
    if (!knownTime(ev.starts_at)) {
      b.classList.add("tba");
      b.append(el("span", "cd-big", "Date TBA"), el("span", "cd-small", "Time to be announced"));
      return b;
    }
    const w = fmtWhen(ev.starts_at);
    const t = el("time", "cd-big", w.rel);
    t.dateTime = w.iso;
    b.append(t, el("span", "cd-small", w.abs));
    return b;
  }

  // The matches still in play against the ones finished: byes and skipped sets are not matches.
  function matchCount(view) {
    const sets = view && view.bracket
      ? view.bracket.sets.filter((s) => !(s.state === "Skipped" || s.slots.some((x) => x === "Bye")))
      : [];
    const done = sets.filter((s) => s.state && typeof s.state === "object" && "Done" in s.state && s.state.Done.winner != null).length;
    return { total: sets.length, done, left: sets.length - done };
  }

  // One live set as a slim row: P1 on the left, the score in a fixed centre column, P2 on the
  // right, and the round as a muted pill at the end.
  function matchRow(line, eventId) {
    const a = el("a", "match");
    a.href = eventHref(eventId) + "#set-" + line.id;
    a.setAttribute("aria-label", line.words);
    const face = el("span", "match-face");
    face.setAttribute("aria-hidden", "true");
    const lead = line.score[0] === line.score[1] ? -1 : line.score[0] > line.score[1] ? 0 : 1;
    const p1 = el("span", "match-side p1");
    const p2 = el("span", "match-side p2");
    // The leading player's name is set bold; a tied set bolds neither.
    if (lead === 0) p1.classList.add("winning");
    if (lead === 1) p2.classList.add("winning");
    p1.append(el("span", "port p1", "P1"), el("span", "match-tag", line.a));
    p2.append(el("span", "match-tag", line.b), el("span", "port p2", "P2"));
    const score = el("span", "match-score");
    score.append(
      el("span", lead === 1 ? "trail" : "", line.score[0]),
      el("span", "dash", "–"),
      el("span", lead === 0 ? "trail" : "", line.score[1]),
    );
    face.append(p1, score, p2, el("span", "match-round", line.round));
    a.append(face);
    return a;
  }

  // ---- live now: the running event leads, its sets as slim match rows ----------

  const MATCH_ROWS = 4;

  function liveCard(ev, lines, view, i) {
    const card = el("article", "hero live c" + Math.min(i, 12));
    card.append(stamp("Live now", "live"));
    card.append(heroName(ev));
    const facts = el("div", "hero-facts");
    facts.append(chip(entrantsText(ev.entrants)), miniStrip(view));
    const count = matchCount(view);
    if (count.total) facts.append(chip(count.done + " finished · " + count.left + " remaining", "count"));
    card.append(facts);
    if (lines.length) {
      // Sets already under way come first; the rest of the live ones wait behind them.
      const played = (l) => l.score[0] + l.score[1];
      const ordered = lines.slice().sort((x, y) => (played(y) > 0) - (played(x) > 0));
      const list = el("ol", "match-list");
      for (const line of ordered.slice(0, MATCH_ROWS)) {
        const li = el("li");
        li.append(matchRow(line, ev.id));
        list.append(li);
      }
      card.append(list);
      const more = ordered.length - MATCH_ROWS;
      if (more > 0) {
        const link = el("a", "match-more", "+" + more + " more " + (more === 1 ? "match" : "matches"));
        link.href = eventHref(ev.id);
        card.append(link);
      }
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

  function nextCard(ev, big, tags) {
    const card = el("article", big ? "hero next" : "side-card next");
    if (big) {
      card.append(stamp("Next up", "next"));
      card.append(heroName(ev));
      card.append(whenLine(ev, big));
    } else {
      card.append(sideHead("Next up", chip(ev.entrants ? entrantsText(ev.entrants) : "No entrants yet")));
      const body = el("div", "side-body");
      const h = el("h3");
      const a = el("a", null, ev.name);
      a.href = eventHref(ev.id);
      h.append(a);
      body.append(h, countdown(ev));
      card.append(body);
    }
    if (ev.description) card.append(el("p", "hero-desc", ev.description));
    card.append(seats(ev.entrants, tags));
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

  // ---- latest results: the newest event, its top three as a podium list ----------

  function renderLatest(result) {
    const sum = result.summary;
    const box = el("section", "latest");
    const date = eventDate(sum);
    const card = el("div", "side-card result");
    card.append(sideHead("Latest results", date ? chip(date) : null));
    const body = el("div", "side-body");
    const h = el("h3");
    const a = el("a", null, sum.name);
    a.href = eventHref(sum.id);
    h.append(a);
    body.append(h, el("p", "side-meta", entrantsText(sum.entrants)));
    const placings = (result.placings || []).filter((p) => p.place <= 3).sort((x, y) => x.place - y.place);
    if (placings.length) {
      const podium = el("ol", "ranks");
      podium.setAttribute("aria-label", "Top three");
      for (const p of placings) {
        const li = el("li", "rank r" + p.place);
        const label = el("span", "rank-place", ordinal(p.place));
        const t = el("a", p.tag.length > 12 ? "rank-tag long" : "rank-tag", p.tag);
        t.href = playerHref(p.tag);
        li.append(avatar(p.tag), label, t);
        podium.append(li);
      }
      body.append(podium);
    }
    card.append(body);
    const actions = el("p", "actions");
    actions.append(pill("Full results", eventHref(sum.id)));
    card.append(actions);
    box.append(card);
    return box;
  }

  // ---- the run of events: done, live, then what is coming, one mark each ---------

  // ---- coming up: the run of events as tickets, one row of them across the column. Each ticket
  // says where its event stands, how many entered, who they are when known, its date or TBA, and
  // what to do next. The ticket's name link covers the whole ticket.

  const RUN_STATUS = { now: "Live now", next: "Up next", todo: "Sign-ups open", done: "Done" };
  const RUN_CTA = { now: "Open the bracket", next: "See who’s in", todo: "See who’s in", done: "See results" };

  // The start as a short local date and time, the relative phrase beneath; "Date to be announced"
  // when the start is not set; the finished event's date alone.
  function runWhen(ev) {
    const p = el("p", "run-when");
    if (ev.state === "complete") {
      p.append(el("span", "run-date", eventDate(ev) || "Done"));
      return p;
    }
    if (!knownTime(ev.starts_at)) {
      p.classList.add("tba");
      p.append(el("span", "run-date", "Date to be announced"));
      return p;
    }
    const w = fmtWhen(ev.starts_at);
    const short = new Date(ev.starts_at * 1000).toLocaleString(undefined, { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
    const t = el("time", "run-date", short);
    t.dateTime = w.iso;
    p.append(t, el("span", "run-rel", w.rel));
    return p;
  }

  // The first five entrants as avatars, then a count of the rest.
  function runPeople(tags) {
    const row = el("div", "run-people");
    const stack = el("span", "avatar-stack");
    for (const t of tags.slice(0, 5)) stack.append(avatar(t));
    if (tags.length > 5) stack.append(el("span", "disc more", "+" + (tags.length - 5)));
    row.append(stack);
    return row;
  }

  // The champion of a finished event, from the newest result when it is that event's.
  function runChampion(result, ev) {
    if (!result || !result.summary || result.summary.id !== ev.id) return null;
    const first = (result.placings || []).find((p) => p.place === 1);
    if (!first || typeof first.tag !== "string") return null;
    const row = el("p", "run-champ");
    const a = el("a", null, first.tag);
    a.href = playerHref(first.tag);
    row.append(avatar(first.tag), el("span", "run-champ-label", "Champion"), a);
    return row;
  }

  function runTicket(ev, status, tags, champ) {
    const li = el("li", "run-item " + status);
    const top = el("div", "run-top");
    top.append(el("span", "run-status " + status, RUN_STATUS[status]), chip(entrantsText(ev.entrants || 0)));
    const a = el("a", "run-name", ev.name);
    a.href = eventHref(ev.id);
    li.append(top, a);
    if (tags && tags.length) li.append(runPeople(tags));
    if (champ) li.append(champ);
    const foot = el("div", "run-foot");
    foot.append(runWhen(ev), el("span", "run-cta", RUN_CTA[status]));
    li.append(foot);
    return li;
  }

  function renderRun(section, events, tagsById, result) {
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
      list.append(runTicket(ev, status, tagsById[ev.id], status === "done" ? runChampion(result, ev) : null));
    }
    section.replaceChildren(el("h2", "section-title", "Coming up"), list);
  }

  // ---- by the numbers: four sticker totals over the finished events -------------

  // The sets a finished event played, counted once: a finished bracket never changes again.
  const setsPlayed = new Map();

  function tallyTile(label, value) {
    const box = el("div", "tally-tile");
    box.append(el("dt", "tally-label", label), el("dd", "tally-value", value));
    return box;
  }

  function renderTally(section, stats) {
    if (!stats || !stats.events) {
      section.hidden = true;
      section.replaceChildren();
      return;
    }
    const champ = el("div", "tally-tile tally-champ");
    const name = el("dd", "tally-value tally-name");
    if (stats.champion) {
      const a = el("a", null, stats.champion);
      a.href = playerHref(stats.champion);
      name.append(icon("crown"), a);
    } else {
      name.textContent = "–";
    }
    champ.append(el("dt", "tally-label", "Current champion"), name);
    section.replaceChildren(
      tallyTile("Events run", String(stats.events)),
      tallyTile("Players", String(stats.players)),
      tallyTile("Sets played", String(stats.sets)),
      champ,
    );
    section.hidden = false;
  }

  async function loadTally(complete) {
    const [hist, players] = await Promise.all([
      getJson("/v1/history").catch(() => []),
      getJson("/v1/players").catch(() => []),
    ]);
    const list = Array.isArray(hist) ? hist : [];
    const sets = await Promise.all(complete.map((ev) => {
      if (setsPlayed.has(ev.id)) return setsPlayed.get(ev.id);
      return getJson("/v1/events/" + ev.id)
        .then((view) => {
          const n = matchCount(view).done;
          setsPlayed.set(ev.id, n);
          return n;
        })
        .catch(() => 0);
    }));
    // History is newest first, so its first result names the current champion.
    const top = list[0] && Array.isArray(list[0].placings) ? list[0].placings.find((p) => p.place === 1) : null;
    return {
      events: list.length,
      players: Array.isArray(players) ? players.length : 0,
      sets: sets.reduce((a, b) => a + b, 0),
      champion: top && typeof top.tag === "string" ? top.tag : null,
    };
  }

  // Redrawn when the set of finished events changes, and once a minute so the counts keep up.
  let tallyKey = "";
  async function refreshTally(events) {
    const complete = events.filter((e) => e.state === "complete");
    const key = complete.map((e) => e.id).join(",") + "@" + Math.floor(Date.now() / 60000);
    if (key === tallyKey) return;
    tallyKey = key;
    try {
      renderTally($("tally"), await loadTally(complete));
    } catch (err) {
      tallyKey = "";
    }
  }

  // ---- the page -------------------------------------------------------------

  function render(events, running, history, streamUrl, nextTags, tagsById) {
    // Nothing new since the last drawing (within the minute, so relative times keep up): leave
    // the page exactly as it is.
    const key = JSON.stringify([events, running, history[0] || null, streamUrl, nextTags, tagsById, Math.floor(Date.now() / 60000)]);
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
    else lead.replaceChildren(next ? nextCard(next, true, nextTags) : quietCard());
    const side = $("side");
    side.replaceChildren();
    if (history[0]) side.append(renderLatest(history[0]));
    if (running.length && next) side.append(nextCard(next, false, nextTags));
    renderRun($("run"), events, tagsById, history[0] || null);
  }

  // An event view's entrants as names, if it has them.
  function namesOf(view) {
    return view && Array.isArray(view.tags) ? view.tags.filter((t) => typeof t === "string") : [];
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
      // The entrants by name for the tickets coming up (the next one always), for their avatar
      // stacks; without them a ticket shows only its entrant count.
      const upcoming = nextUp(events);
      const soonIds = events
        .filter((e) => e.state === "signup")
        .sort((a, b) => {
          const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
          const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
          return ta - tb || a.id - b.id;
        })
        .slice(0, 5)
        .map((e) => e.id);
      if (upcoming && !soonIds.includes(upcoming.id)) soonIds.push(upcoming.id);
      const soonViews = await Promise.all(soonIds.map((id) => getJson("/v1/events/" + id).catch(() => null)));
      const tagsById = {};
      soonIds.forEach((id, i) => { tagsById[id] = namesOf(soonViews[i]); });
      const nextTags = upcoming ? tagsById[upcoming.id] : [];
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
      for (const r of running) tagsById[r.ev.id] = namesOf(r.view);
      render(events, running, history, streamUrl, nextTags, tagsById);
      refreshTally(events);
    } catch (err) {
      if (!drawnKey) setNotice("status", "Couldn’t reach the server just now; trying again in a few seconds.");
    }
  }

  AB.poll(refresh, POLL_MS);
})();
