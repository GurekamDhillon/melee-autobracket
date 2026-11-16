// Past events, newest first: one row each with its champion and podium, the placings to 8th
// folded underneath. Loads once.
(function () {
  const { el, icon, getJson, setNotice, chip, pill, entrantsText } = AB;
  const PLACES_SHOWN = 8;
  const results = document.getElementById("results");

  function placing(p) {
    const li = el("li", "row-line placing");
    li.append(el("span", "place", AB.ordinal(p.place)));
    const a = el("a", "tag", p.tag);
    a.href = AB.playerHref(p.tag);
    li.append(a);
    return li;
  }

  function tagLink(tag, cls) {
    const a = el("a", cls, tag);
    a.href = AB.playerHref(tag);
    return a;
  }

  function row(result, i) {
    const ev = result.summary;
    const placings = result.placings || [];
    const li = el("li", "hist-row " + (i % 2 ? "tr1" : "tl2"));

    const main = el("div", "hist-main");
    const h = el("h3");
    const name = el("a", null, ev.name);
    name.href = AB.eventHref(ev.id);
    h.append(name);
    const chips = el("p", "chips");
    const date = AB.eventDate(ev);
    if (date) chips.append(chip(date));
    chips.append(chip(entrantsText(ev.entrants)));
    main.append(h, chips);

    const champ = el("div", "hist-champ");
    const first = placings.filter((p) => p.place === 1);
    if (first.length) {
      const star = el("span", "hist-star");
      star.append(icon("star"));
      champ.append(star);
      const who = el("span", "hist-who");
      who.append(el("span", "hist-label", "Champion"));
      for (const p of first) who.append(tagLink(p.tag, "hist-champ-tag"));
      champ.append(who);
    }

    const podium = el("p", "chips hist-podium");
    for (const p of placings.filter((p) => p.place === 2 || p.place === 3)) {
      const c = chip(AB.ordinal(p.place) + " ");
      c.append(tagLink(p.tag));
      podium.append(c);
    }

    const go = el("p", "hist-go");
    go.append(pill("Bracket", AB.eventHref(ev.id)));

    const more = el("details", "hist-more");
    more.append(el("summary", null, "All placings"));
    const rows = el("ol", "rows");
    for (const p of placings) if (p.place <= PLACES_SHOWN) rows.append(placing(p));
    more.append(rows);
    // A tie across 8th shows every entrant of that place, so count what is left, not 8.
    const hidden = result.placings.length - rows.childElementCount;
    if (hidden > 0) {
      const rest = el("a", "quiet", "and " + hidden + " more on the bracket");
      rest.href = AB.eventHref(ev.id);
      const p = el("p");
      p.append(rest);
      more.append(p);
    }

    li.append(main, champ, podium, go, more);
    return li;
  }

  // Nothing finished yet: say what will appear, and where to look meanwhile.
  function empty() {
    const li = el("li", "empty");
    li.append(el("h3", null, "No completed events yet."));
    li.append(el("p", null, "Results appear here when an event finishes."));
    const p = el("p", "actions");
    p.append(pill("See the schedule", "/schedule", "sunny"));
    li.append(p);
    return li;
  }

  function render(list) {
    results.removeAttribute("aria-busy");
    results.replaceChildren(...(list.length ? list.map(row) : [empty()]));
  }

  getJson("/v1/history").then(
    (list) => render(Array.isArray(list) ? list : []),
    () => {
      results.removeAttribute("aria-busy");
      results.replaceChildren();
      setNotice("status", "Couldn’t reach the server just now; reload to try again.");
    },
  );
})();
