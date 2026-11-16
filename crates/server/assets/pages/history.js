// Past events, newest first, each with its date, entrants and placings to 8th. Loads once.
(function () {
  const { el, getJson, setNotice } = AB;
  const PLACES_SHOWN = 8;
  const results = document.getElementById("results");

  function entrants(n) {
    return n + (n === 1 ? " entrant" : " entrants");
  }

  function placing(p) {
    const li = el("li", "row-line placing");
    li.append(el("span", "place", AB.ordinal(p.place)));
    const a = el("a", "tag", p.tag);
    a.href = AB.playerHref(p.tag);
    li.append(a);
    return li;
  }

  function card(result) {
    const ev = result.summary;
    const li = el("li", "card");
    const h = el("h2");
    const name = el("a", null, ev.name);
    name.href = AB.eventHref(ev.id);
    h.append(name);
    li.append(h);
    const bits = [AB.eventDate(ev), entrants(ev.entrants)].filter(Boolean);
    li.append(el("p", "quiet", bits.join(" · ")));
    const rows = el("ol", "rows");
    for (const p of result.placings) if (p.place <= PLACES_SHOWN) rows.append(placing(p));
    li.append(rows);
    // A tie across 8th shows every entrant of that place, so count what is left, not 8.
    const hidden = result.placings.length - rows.childElementCount;
    if (hidden > 0) {
      const more = el("a", "quiet", "and " + hidden + " more");
      more.href = AB.eventHref(ev.id);
      const p = el("p");
      p.append(more);
      li.append(p);
    }
    return li;
  }

  function render(list) {
    if (!list.length) {
      setNotice("status", "No completed events yet.");
      return;
    }
    results.replaceChildren(...list.map(card));
  }

  getJson("/v1/history").then(
    (list) => render(Array.isArray(list) ? list : []),
    () => setNotice("status", "Couldn’t reach the server just now; reload to try again."),
  );
})();
