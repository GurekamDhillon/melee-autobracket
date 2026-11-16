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

  // One of the four number stickers under the name.
  function stat(label, value, extra) {
    const li = el("li", "stat");
    li.append(el("span", "stat-num", value));
    if (extra) li.append(el("span", "stat-extra", extra));
    li.append(el("span", "stat-label", label));
    return li;
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

  function opponentRow(h) {
    const li = el("li", "row-line");
    const a = el("a", null, h.tag);
    a.href = AB.playerHref(h.tag);
    li.append(a);
    li.append(el("span", "num", record(h.sets)));
    li.append(el("span", "num", record(h.games)));
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
    $("stats").replaceChildren(
      stat(events.length === 1 ? "Event" : "Events", events.length),
      stat(titles === 1 ? "Title" : "Titles", titles),
      stat("Sets", record(sets), pct(sets)),
      stat("Games", record(games), pct(games)),
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
      opp.replaceChildren(headerRow(["Opponent", "Sets", "Games"]));
      for (const h of h2h) opp.append(opponentRow(h));
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
