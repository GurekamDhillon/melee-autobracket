// The players index: /players. Fetched once; the search box filters the rows in place.
(function () {
  const { el, getJson, setNotice } = AB;
  const main = document.getElementById("main");
  const filter = document.getElementById("filter");
  let rows = [];

  function record([won, lost]) {
    return won + "–" + lost;
  }

  function header() {
    const li = el("li", "row-line head");
    li.append(el("span", null, "Player"));
    for (const name of ["Events", "Titles", "Sets"]) li.append(el("span", "num", name));
    return li;
  }

  function row(p) {
    const li = el("li", "row-line");
    const a = el("a", null, p.tag);
    a.href = AB.playerHref(p.tag);
    li.append(a);
    li.append(el("span", "num", p.events));
    li.append(el("span", "num", p.titles));
    li.append(el("span", "num", Array.isArray(p.sets) ? record(p.sets) : ""));
    return li;
  }

  function render(players) {
    rows = [];
    if (!players.length) {
      main.replaceChildren(el("p", "notice", "No players yet."));
      return;
    }
    const list = el("ol", "rows");
    list.append(header());
    for (const p of players) {
      const li = row(p);
      rows.push([String(p.tag).toLowerCase(), li]);
      list.append(li);
    }
    main.replaceChildren(list);
    filter.hidden = false;
    applyFilter();
  }

  function applyFilter() {
    const q = filter.value.trim().toLowerCase();
    for (const [tag, li] of rows) li.hidden = q !== "" && !tag.includes(q);
  }

  filter.addEventListener("input", applyFilter);

  getJson("/v1/players")
    .then((list) => render(Array.isArray(list) ? list : []))
    .catch(() => setNotice("status", "Couldn’t reach the server just now. Reload to try again."));
})();
