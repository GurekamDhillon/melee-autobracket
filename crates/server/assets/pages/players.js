// The players index: /players. Fetched once, ranked as a leaderboard: most titles first, then the
// best set win rate. The top three stand on a podium; the rest are numbered rows. The search box
// filters in place; while it has text, the podium steps aside and every match is a row.
(function () {
  const { el, icon, getJson, setNotice, avatar } = AB;
  const main = document.getElementById("main");
  const filter = document.getElementById("filter");
  const PODIUM = 3;
  let rows = [];
  let podium = null;
  let generation = 0;

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
    li.append(el("span", "lb-gap"));
    return li;
  }

  // Titles as a gold star and the count; a player with none leaves the cell empty.
  function titles(n) {
    const box = el("span", "num titles");
    if (!n) return box;
    box.append(el("span", "sr", n + (n === 1 ? " title" : " titles")));
    const face = el("span", "stars");
    face.setAttribute("aria-hidden", "true");
    face.append(icon("star"), el("span", "star-n", String(n)));
    box.append(face);
    return box;
  }

  // The last five events that had sets, one pip each, oldest first: sunny when more sets were
  // won than lost, coral when more were lost, quiet on a tie. Filled in once the profile loads.
  function formPips(events) {
    const strip = el("span", "lb-form");
    strip.setAttribute("aria-hidden", "true");
    const played = (Array.isArray(events) ? events : [])
      .filter((e) => Array.isArray(e.sets) && e.sets[0] + e.sets[1] > 0)
      .slice(0, 5)
      .reverse();
    for (const e of played) {
      const pip = el("span", "lb-pip");
      const won = e.sets[0], lost = e.sets[1];
      pip.classList.add(won > lost ? "won" : lost > won ? "lost" : "even");
      pip.title = (e.event && e.event.name ? e.event.name + ": " : "") + record(e.sets);
      strip.append(pip);
    }
    return strip;
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

  // The rank as a number, the medal on the podium.
  function rankBadge(rank) {
    return el("span", "lb-rank", String(rank));
  }

  function medal(rank) {
    const m = el("span", "lb-medal");
    m.setAttribute("aria-hidden", "true");
    m.append(icon("burst"), el("span", "lb-medal-n", String(rank)));
    return m;
  }

  function row(p, rank) {
    const li = el("li");
    const a = el("a", "row-line player-row" + (rank === 1 && p.titles > 0 ? " top" : ""));
    a.href = AB.playerHref(p.tag);
    const who = el("span", "who");
    const form = el("span", "lb-form-slot");
    who.append(rankBadge(rank), avatar(p.tag), el("span", "who-tag", p.tag), form);
    // The stats are table cells on a wide screen; on a phone they read as one labelled line.
    const mini = el("span", "lb-mini");
    const events = el("span", "num lb-stat");
    events.append(el("span", "lb-n", String(p.events)), el("span", "lb-u", p.events === 1 ? "event" : "events"));
    const t = titles(p.titles);
    t.classList.add("lb-stat");
    if (p.titles) t.append(el("span", "lb-u", p.titles === 1 ? "title" : "titles"));
    else t.classList.add("lb-no-titles");
    const sets = el("span", "num lb-stat", Array.isArray(p.sets) ? record(p.sets) : "");
    const rate = rateBar(winRate(p.sets));
    rate.classList.add("lb-stat");
    mini.append(events, t, sets, rate);
    const chev = el("span", "lb-chev");
    chev.setAttribute("aria-hidden", "true");
    chev.append(icon("chev"));
    a.append(who, mini, chev);
    li.append(a);
    return li;
  }

  // The first place's crown, perched on the card's top edge.
  function crown() {
    const c = el("span", "lb-crown");
    c.setAttribute("aria-hidden", "true");
    c.append(icon("crown"));
    return c;
  }

  // "3 events", "1 event".
  function eventsLine(n) {
    const count = Number(n) || 0;
    return el("span", "lb-line lb-events", count + (count === 1 ? " event" : " events"));
  }

  // Titles as a line of stars with the count written out. Only shown for a player who has one.
  function titleLine(n) {
    const count = Number(n) || 0;
    const line = el("span", "lb-line lb-titles");
    const stars = el("span", "stars");
    stars.setAttribute("aria-hidden", "true");
    for (let i = 0; i < Math.min(count, 3); i++) stars.append(icon("star"));
    line.append(stars, el("span", null, count + (count === 1 ? " title" : " titles")));
    return line;
  }

  // The set record as a compact chip, e.g. "2–0 sets", in place of a "no titles" line.
  function recordChip(sets) {
    const chip = el("span", "lb-line lb-record", record(sets) + " sets");
    chip.title = "Sets won–lost";
    return chip;
  }

  // One of the top three: crown or medal, avatar, tag, events, titles and sets, then the win
  // rate pinned to the card's foot so the three bottoms share one baseline.
  function standing(p, rank) {
    const li = el("li", "lb-slot lb-" + rank);
    const a = el("a", "lb-card" + (rank === 1 ? " top" : ""));
    a.href = AB.playerHref(p.tag);
    const head = el("span", "lb-head");
    if (rank === 1) head.append(crown());
    head.append(medal(rank), avatar(p.tag, true));
    a.append(head, el("span", "lb-tag", p.tag));
    const meta = el("span", "lb-meta");
    meta.append(eventsLine(p.events));
    if (Number(p.titles) > 0) meta.append(titleLine(p.titles));
    if (Array.isArray(p.sets)) meta.append(recordChip(p.sets));
    a.append(meta);
    a.append(rateBar(winRate(p.sets)));
    li.append(a);
    return li;
  }

  function render(players) {
    rows = [];
    podium = null;
    generation += 1;
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
    const top = Math.min(PODIUM, ranked.length);
    const parts = [];
    if (top) {
      podium = el("ol", "lb-podium");
      podium.setAttribute("aria-label", "Top " + top);
      for (let i = 0; i < top; i++) podium.append(standing(ranked[i], i + 1));
      parts.push(podium);
    }
    const list = el("ol", "rows board-list cols-5 lb-list");
    list.append(header());
    ranked.forEach((p, i) => {
      const li = row(p, i + 1);
      rows.push({ tag: String(p.tag).toLowerCase(), rank: i + 1, li, player: p });
      list.append(li);
    });
    parts.push(list);
    main.replaceChildren(...parts);
    filter.hidden = false;
    applyFilter();
    for (const r of rows) loadForm(r, generation);
  }

  // Each row's form comes from its own profile; a failed one leaves the strip empty.
  function loadForm(r, gen) {
    getJson("/v1/players/" + encodeURIComponent(r.player.tag))
      .then((profile) => {
        if (gen !== generation) return;
        const slot = r.li.querySelector(".lb-form-slot");
        if (slot) slot.replaceChildren(formPips(profile && profile.events));
      })
      .catch(() => {});
  }

  // With no search text the podium shows and its three players leave the list; a search
  // shows every match as a row, ranks kept.
  function applyFilter() {
    const q = filter.value.trim().toLowerCase();
    const searching = q !== "";
    if (podium) podium.hidden = searching;
    for (const r of rows) r.li.hidden = searching ? !r.tag.includes(q) : r.rank <= PODIUM;
  }

  filter.addEventListener("input", applyFilter);

  getJson("/v1/players")
    .then((list) => render(Array.isArray(list) ? list : []))
    .catch(() => {
      main.replaceChildren();
      setNotice("status", "Couldn’t reach the server just now. Reload to try again.");
    });
})();
