// A player's profile: /players/{tag}. The tag in the path is matched in any case; the page
// shows the canonical spelling the server answers with.
(function () {
  const { el, icon, getJson, setNotice, ordinal, chip } = AB;
  const $ = (id) => document.getElementById(id);

  // The tag as typed in the address bar; a malformed escape is just an unknown player.
  function pathTag() {
    try {
      return decodeURIComponent(location.pathname.slice("/players/".length));
    } catch (err) {
      return "";
    }
  }

  // "W–L", held together (word joiners) so the meta line never breaks after the dash.
  function record([won, lost]) {
    return won + "⁠–⁠" + lost;
  }

  function sum(pairs) {
    return pairs.reduce((acc, [w, l]) => [acc[0] + w, acc[1] + l], [0, 0]);
  }

  function headerRow(names) {
    const li = el("li", "row-line head");
    li.append(el("span", null, names[0]));
    for (const name of names.slice(1)) li.append(el("span", "num", name));
    return li;
  }

  // What the player's entry came to: a place once the event is complete, otherwise where
  // the event stands.
  function outcome(entry) {
    if (typeof entry.place === "number") return ordinal(entry.place);
    return entry.event.state === "signup" ? "Not started" : "In progress";
  }

  // The outcome as a chip: a title is sunny with a star; an open entry says where it stands.
  function outcomeChip(entry) {
    const box = el("span", "num");
    let cls = "";
    if (entry.place === 1) cls = "gold";
    else if (typeof entry.place !== "number") cls = entry.event.state === "signup" ? "seat-chip" : "hot";
    const c = cls === "seat-chip" ? el("span", "seat-chip", outcome(entry)) : chip(outcome(entry), cls);
    if (entry.place === 1) c.prepend(icon("star"));
    box.append(c);
    return box;
  }

  // Won as a whole percentage of the pair, or "" before anything is played.
  function pct([won, lost]) {
    return won + lost ? Math.round((100 * won) / (won + lost)) + "%" : "";
  }

  // The share won as a bar step: 0 to 100 in steps of 5, each one a stepped class in site.css.
  function barStep(won, total) {
    return total ? Math.round((won * 20) / total) * 5 : 0;
  }

  // When an event happened: its start, else its creation, else 0 when neither is known.
  function eventTime(e) {
    if (AB.knownTime(e.event.starts_at)) return e.event.starts_at;
    if (AB.knownTime(e.event.created_at)) return e.event.created_at;
    return 0;
  }

  // The most recent of the entries, or null when there are none.
  function latest(entries) {
    return entries.reduce((best, e) => (!best || eventTime(e) > eventTime(best) ? e : best), null);
  }

  // One of the four number stickers under the name: the number, its label, a one-line caption
  // of context, and optionally a thin bar for a share. A gold sticker is a title.
  function stat(label, value, o = {}) {
    const li = el("li", o.gold ? "stat gold" : "stat");
    const top = el("span", "stat-top");
    if (o.gold) top.append(icon("star"));
    top.append(el("span", "stat-num", value));
    if (o.extra) top.append(el("span", "stat-extra", o.extra));
    li.append(top, el("span", "stat-label", label));
    if (o.cap) {
      const cap = el("span", "stat-cap", o.cap);
      cap.title = o.cap;
      li.append(cap);
    }
    if (o.bar !== undefined) {
      const bar = el("span", "stat-bar");
      bar.setAttribute("aria-hidden", "true");
      bar.append(el("span", "stat-fill f" + o.bar));
      li.append(bar);
    }
    return li;
  }

  // Titles as gold stars under the name: one per title up to five, then a star and the count.
  function titleStars(n) {
    const box = el("span", "hero-titles");
    if (n === 0) {
      box.append(el("span", "hero-none", "No titles yet"));
      return box;
    }
    const face = el("span", "stars");
    face.setAttribute("aria-hidden", "true");
    for (let i = 0; i < Math.min(n, 5); i++) face.append(icon("star"));
    if (n > 5) face.append(el("span", "more", "×" + n));
    box.append(face, el("span", "hero-count", n === 1 ? "1 title" : n + " titles"));
    return box;
  }

  // The highest finish in any event, or null before the player has placed anywhere.
  function bestFinish(events) {
    const places = events.map((e) => e.place).filter((p) => typeof p === "number");
    return places.length ? Math.min(...places) : null;
  }

  function bestBadge(best) {
    const box = el("span", "hero-best");
    box.append(el("span", "hero-best-label", "Best finish"));
    if (best === null) {
      box.append(el("span", "hero-none", "Not placed yet"));
      return box;
    }
    const c = chip(ordinal(best), best === 1 ? "gold" : "");
    if (best === 1) c.prepend(icon("star"));
    box.append(c);
    return box;
  }

  // The last ten events that had sets, oldest first: a row per event, its name, then one dot per
  // set (sunny for a set won, coral for a set lost) in the order played, then the record.
  function formCard(events) {
    const card = el("section", "form-card");
    card.append(el("h2", "form-title", "Recent form"));
    const played = events.filter((e) => e.sets[0] + e.sets[1] > 0).slice(0, 10).reverse();
    if (!played.length) {
      card.append(el("p", "form-none", "No sets played yet."));
      return card;
    }
    const list = el("ol", "form-strip");
    list.setAttribute("aria-label", "Sets won and lost, oldest event first");
    for (const e of played) {
      const li = el("li", "form-event");
      li.title = e.event.name + ": " + record(e.sets);
      const name = el("span", "form-name", e.event.name);
      name.setAttribute("aria-hidden", "true");
      const dots = el("span", "form-dots");
      dots.setAttribute("aria-hidden", "true");
      for (let i = 0; i < e.sets[0]; i++) dots.append(el("span", "dot won"));
      for (let i = 0; i < e.sets[1]; i++) dots.append(el("span", "dot lost"));
      const rec = el("span", "form-rec", record(e.sets));
      rec.setAttribute("aria-hidden", "true");
      li.append(name, dots, rec, el("span", "sr", e.event.name + ", sets " + record(e.sets)));
      list.append(li);
    }
    const key = el("p", "form-key");
    key.append(el("span", "dot won"), el("span", null, "Set won"), el("span", "dot lost"), el("span", null, "Set lost"));
    key.setAttribute("aria-hidden", "true");
    card.append(list, key);
    return card;
  }

  function eventRow(entry) {
    const ev = entry.event;
    const li = el("li", "row-line");
    const a = el("a", null, ev.name);
    a.href = AB.eventHref(ev.id);
    li.append(a);
    li.append(el("span", "num quiet", AB.eventDate(ev) || ""));
    li.append(el("span", "num quiet", "seed " + (entry.seed + 1)));
    li.append(outcomeChip(entry));
    li.append(el("span", "num", record(entry.sets)));
    li.append(el("span", "num", record(entry.games)));
    return li;
  }

  // One head-to-head sticker: the two names at the outer edges, a bar split by sets won (the
  // leader's half sunny), and the set record as the big result title.
  function versus(me, h) {
    const [won, lost] = h.sets;
    const total = won + lost;
    const li = el("li", "versus");

    const names = el("div", "versus-names");
    names.append(el("span", "versus-me", me));
    const them = el("a", "versus-them", h.tag);
    them.href = AB.playerHref(h.tag);
    names.append(them);

    // Shares in steps of 5 percent, so each width is one of the stepped classes in site.css.
    const bar = el("div", total ? "versus-bar" : "versus-bar empty");
    bar.setAttribute("aria-hidden", "true");
    if (total) {
      const share = Math.round((won * 20) / total) * 5;
      bar.append(el("span", "seg seg-me p" + share + (won > lost ? " lead" : "")));
      bar.append(el("span", "seg seg-them" + (lost > won ? " lead" : "")));
    }

    const result = el("div", "versus-result");
    result.append(el("span", "versus-games", "Games " + record(h.games)));
    const score = el("span", "versus-score-wrap");
    score.append(el("span", "versus-score", record(h.sets)));
    score.append(el("span", "versus-cap", "Sets"));
    result.append(score);
    const state = won > lost ? chip("Ahead", "gold") : won < lost ? chip("Behind") : chip("Level");
    const st = el("span", "versus-state");
    st.append(state);
    result.append(st);

    li.append(names, bar, result);
    return li;
  }

  function render(profile) {
    const events = Array.isArray(profile.events) ? profile.events : [];
    const h2h = Array.isArray(profile.head_to_head) ? profile.head_to_head : [];
    $("title").textContent = profile.tag;
    document.title = profile.tag;

    const titles = events.filter((e) => e.place === 1 && e.event.state === "complete").length;
    const since = new Date(profile.since * 1000).toLocaleDateString(undefined, { month: "long", year: "numeric" });
    const sets = sum(events.map((e) => e.sets));
    const games = sum(events.map((e) => e.games));
    $("meta").textContent = "playing since " + since;
    $("title").before(AB.avatar(profile.tag, true));
    const hero = el("div", "hero-line");
    hero.append(titleStars(titles), bestBadge(bestFinish(events)));
    $("meta").after(hero);
    $("head-side").replaceChildren(formCard(events));
    const lastEvent = latest(events);
    const lastWhen = lastEvent && eventTime(lastEvent) ? AB.eventDate(lastEvent.event) : null;
    const titleEvent = latest(events.filter((e) => e.place === 1 && e.event.state === "complete"));
    const best = bestFinish(events);
    const titleCap = titleEvent
      ? ordinal(1) + " at " + titleEvent.event.name
      : best === null ? "No wins yet" : "Best finish " + ordinal(best);
    $("stats").replaceChildren(
      stat(events.length === 1 ? "Event" : "Events", events.length, {
        cap: lastWhen ? "last: " + lastWhen : "no dates yet",
      }),
      stat(titles === 1 ? "Title" : "Titles", titles, { gold: titles > 0, cap: titleCap }),
      stat("Sets", record(sets), {
        extra: pct(sets),
        cap: sets[0] + sets[1] ? sets[0] + " won of " + (sets[0] + sets[1]) + " played" : "none played yet",
        bar: barStep(sets[0], sets[0] + sets[1]),
      }),
      stat("Games", record(games), {
        extra: pct(games),
        cap: games[0] + games[1] ? games[0] + " games won of " + (games[0] + games[1]) + " played" : "none played yet",
        bar: barStep(games[0], games[0] + games[1]),
      }),
    );

    const list = $("events");
    if (events.length) {
      list.replaceChildren(headerRow(["Event", "Date", "Seed", "Place", "Sets", "Games"]));
      for (const e of events) list.append(eventRow(e));
    } else {
      list.replaceChildren(el("li", "notice", "No events entered yet."));
    }

    const opp = $("h2h");
    if (h2h.length) {
      opp.replaceChildren();
      for (const h of h2h) opp.append(versus(profile.tag, h));
    } else {
      opp.replaceChildren(el("li", "notice", "No sets played yet."));
    }
    $("main").hidden = false;
  }

  getJson("/v1/players/" + encodeURIComponent(pathTag()))
    .then(render)
    .catch((err) => {
      setNotice("status", err && err.status === 404
        ? "This player no longer exists."
        : "Couldn’t reach the server just now. Reload to try again.");
    });
})();
