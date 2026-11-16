// A player's profile: /players/{tag}. The tag in the path is matched in any case; the page
// shows the canonical spelling the server answers with.
(function () {
  const { el, getJson, setNotice, ordinal, fmtDate } = AB;
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

  function plural(n, unit) {
    return n + " " + unit + (n === 1 ? "" : "s");
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
    return entry.event.state === "signup" ? "not started" : "in progress";
  }

  function eventRow(entry) {
    const ev = entry.event;
    const li = el("li", "row-line");
    const a = el("a", null, ev.name);
    a.href = AB.eventHref(ev.id);
    li.append(a);
    const when = typeof ev.starts_at === "number" ? ev.starts_at : ev.created_at;
    li.append(el("span", "num quiet", typeof when === "number" ? fmtDate(when) : ""));
    li.append(el("span", "num quiet", "seed " + (entry.seed + 1)));
    li.append(el("span", "num", outcome(entry)));
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
    $("meta").textContent = [
      "playing since " + since,
      plural(events.length, "event"),
      plural(titles, "title"),
      "sets " + record(sum(events.map((e) => e.sets))),
      "games " + record(sum(events.map((e) => e.games))),
    ].join(" · ");

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
