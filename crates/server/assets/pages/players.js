// The players index: /players. Fetched once; most titles first, then the best set record; the
// search box filters the rows in place.
(function () {
  const { el, icon, getJson, setNotice, avatar } = AB;
  const main = document.getElementById("main");
  const filter = document.getElementById("filter");
  let rows = [];

  // The filter sits beside the heading.
  document.getElementById("head-side").append(filter);

  function record([won, lost]) {
    return won + "–" + lost;
  }

  // Sets won as a whole percentage, or null before any set is played.
  function winRate(sets) {
    if (!Array.isArray(sets)) return null;
    const total = sets[0] + sets[1];
    return total ? Math.round((100 * sets[0]) / total) : null;
  }

  function header() {
    const li = el("li", "row-line head");
    li.append(el("span", null, "Player"));
    for (const name of ["Events", "Titles", "Sets", "Won"]) li.append(el("span", "num", name));
    return li;
  }

  // Titles as stars, up to three; past that, one star and the count.
  function titles(n) {
    const box = el("span", "num titles");
    box.append(el("span", "sr", String(n)));
    const face = el("span", "stars");
    face.setAttribute("aria-hidden", "true");
    if (n === 0) face.append(el("span", "none", "–"));
    else if (n <= 3) for (let i = 0; i < n; i++) face.append(icon("star"));
    else face.append(icon("star"), "×" + n);
    box.append(face);
    return box;
  }

  function rateBar(rate) {
    const box = el("span", "num rate");
    if (rate === null) {
      box.append(el("span", "quiet", "–"));
      return box;
    }
    const track = el("span", "track");
    track.setAttribute("aria-hidden", "true");
    const fill = el("span", "fill");
    // The width is a class step (CSP: no inline styles), to the nearest 10 %.
    fill.classList.add("w" + Math.round(rate / 10));
    track.append(fill);
    box.append(track, el("span", "pct", rate + "%"));
    return box;
  }

  function row(p, i) {
    const li = el("li");
    const a = el("a", "row-line player-row" + (i === 0 && p.titles > 0 ? " top" : ""));
    a.href = AB.playerHref(p.tag);
    const who = el("span", "who");
    who.append(avatar(p.tag), el("span", "who-tag", p.tag));
    a.append(who);
    a.append(el("span", "num", p.events));
    a.append(titles(p.titles));
    a.append(el("span", "num", Array.isArray(p.sets) ? record(p.sets) : ""));
    a.append(rateBar(winRate(p.sets)));
    li.append(a);
    return li;
  }

  function render(players) {
    rows = [];
    if (!players.length) {
      const box = el("div", "empty");
      box.append(el("h3", null, "No players yet."));
      box.append(el("p", null, "Players appear here once they register from the game client."));
      main.replaceChildren(box);
      return;
    }
    const ranked = players.slice().sort((x, y) =>
      y.titles - x.titles || (winRate(y.sets) ?? -1) - (winRate(x.sets) ?? -1) ||
      String(x.tag).localeCompare(String(y.tag), undefined, { sensitivity: "base" }));
    const list = el("ol", "rows board-list cols-5");
    list.append(header());
    ranked.forEach((p, i) => {
      const li = row(p, i);
      rows.push([String(p.tag).toLowerCase(), li]);
      list.append(li);
    });
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
    .catch(() => {
      main.replaceChildren();
      setNotice("status", "Couldn’t reach the server just now. Reload to try again.");
    });
})();
