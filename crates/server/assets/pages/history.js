// Past events, newest first, each with its date, entrants and placings to 8th. Loads once.
(function () {
  const { el, getJson, setNotice } = AB;
  const PLACES_SHOWN = 8;
  const results = document.getElementById("results");

  function entrants(n) {
    return n + (n === 1 ? " entrant" : " entrants");
  }

  // The event's date: its start time, else when it was created (0 means unknown: no date).
  function dateOf(summary) {
    const t = summary.starts_at ?? summary.created_at;
    return t ? AB.fmtDate(t) : "";
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
    const bits = [dateOf(ev), entrants(ev.entrants)].filter(Boolean);
    li.append(el("p", "quiet", bits.join(" · ")));
    const rows = el("ol", "rows");
    for (const p of result.placings) if (p.place <= PLACES_SHOWN) rows.append(placing(p));
    li.append(rows);
    if (ev.entrants > PLACES_SHOWN) {
      const more = el("a", "quiet", "and " + (ev.entrants - PLACES_SHOWN) + " more");
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
