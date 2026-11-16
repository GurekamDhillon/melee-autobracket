// Past events, newest first: a summary strip above a timeline. A rail runs down the left with a
// month marker wherever the month changes and a dot per event; each event card has its champion's
// colour as its left edge, and its podium with the placings to 8th folded underneath. Loads once.
(function () {
  const { el, icon, getJson, setNotice, chip, pill, entrantsText, knownTime, avatar } = AB;
  const PLACES_SHOWN = 8;
  const TONES = ["sunny", "coral", "lagoon", "frost", "technique", "lilac"];
  const results = document.getElementById("results");
  const stats = document.getElementById("stats");

  // A champion's tone: the same tag always gets the same colour, so a name is recognised down
  // the page. No champion (an event with no placings) gets the neutral tone.
  function toneOf(tag) {
    if (typeof tag !== "string" || !tag) return "none";
    let h = 0;
    for (const ch of tag) h = (h * 31 + ch.codePointAt(0)) >>> 0;
    return TONES[h % TONES.length];
  }

  // The event's time as the date chip shows it: when it starts, else when it was created.
  function eventTime(sum) {
    if (knownTime(sum.starts_at)) return sum.starts_at;
    if (knownTime(sum.created_at)) return sum.created_at;
    return null;
  }

  // One finished placing as a card: its medal badge, the avatar and name (linked to the profile),
  // and the set record when the placing carries one.
  function placing(p) {
    const li = el("li", "placing-card");
    const medal = p.place <= 3 ? " m" + p.place : "";
    li.append(el("span", "place-badge" + medal, AB.ordinal(p.place)));
    const who = el("span", "placing-who");
    const a = el("a", "tag", p.tag);
    a.href = AB.playerHref(p.tag);
    who.append(avatar(p.tag), a);
    li.append(who);
    if (Array.isArray(p.sets) && p.sets.length === 2) {
      li.append(el("span", "placing-sets", p.sets[0] + "–" + p.sets[1] + " sets"));
    }
    return li;
  }

  function tagLink(tag, cls) {
    const a = el("a", cls, tag);
    a.href = AB.playerHref(tag);
    return a;
  }

  // The top three as a podium: 2nd left, the champion centre and tallest, 3rd right. A missing
  // place keeps its column (hidden) so a lone champion still sits in the middle. Null when no
  // top-three placing exists.
  function podiumFor(placings) {
    const top = placings.filter((p) => p.place <= 3);
    if (!top.length) return null;
    const ol = el("ol", "hist-podium");
    ol.setAttribute("aria-label", "Top three");
    for (const place of [2, 1, 3]) {
      const list = top.filter((p) => p.place === place);
      const li = el("li", "step step-" + place);
      if (!list.length) {
        li.classList.add("step-none");
        ol.append(li);
        continue;
      }
      if (place === 1) {
        const star = el("span", "hist-star");
        star.append(icon("star"));
        li.append(star, el("span", "hist-label", "Champion"));
        for (const p of list) li.append(tagLink(p.tag, "step-tag step-champ"));
      } else {
        li.append(el("span", "step-place", AB.ordinal(place)));
        for (const p of list) li.append(tagLink(p.tag, "step-tag"));
      }
      ol.append(li);
    }
    return ol;
  }

  // The strip's numbers, over every event listed: how many were held, how many different players
  // placed in any of them, and who has won the most (ties listed together).
  function summary(list) {
    const players = new Set();
    const titles = new Map();
    for (const result of list) {
      for (const p of result.placings || []) {
        players.add(p.tag);
        if (p.place === 1) titles.set(p.tag, (titles.get(p.tag) || 0) + 1);
      }
    }
    let most = 0;
    for (const n of titles.values()) most = Math.max(most, n);
    const leaders = [...titles].filter(([, n]) => n === most).map(([tag]) => tag);
    return { events: list.length, players: players.size, most, leaders };
  }

  function stat(label, value, note) {
    const box = el("div", "hist-stat");
    box.append(el("dt", null, label));
    const dd = el("dd", null, value);
    if (note) dd.append(el("span", "hist-stat-note", note));
    box.append(dd);
    return box;
  }

  function mostStat(s) {
    if (s.most === 0) return stat("Most titles", "–", "No champions yet");
    const first = s.leaders[0];
    const note = s.leaders.length > 1 ? first + " and " + (s.leaders.length - 1) + " more" : first;
    const box = stat("Most titles", String(s.most), note);
    box.classList.add("hot");
    return box;
  }

  function row(result) {
    const ev = result.summary;
    const placings = result.placings || [];
    const champ = placings.find((p) => p.place === 1);
    const li = el("li", "hist-row tone-" + toneOf(champ && champ.tag));
    const dot = el("span", "hist-dot");
    dot.setAttribute("aria-hidden", "true");

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

    const podium = podiumFor(placings);

    const go = el("p", "hist-go");
    go.append(pill("Bracket", AB.eventHref(ev.id)));

    const more = el("details", "hist-more");
    more.append(el("summary", "pill hist-toggle", "All placings"));
    const rows = el("ol", "placing-grid");
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

    if (podium) li.append(dot, main, podium, go, more);
    else li.append(dot, main, go, more);
    return li;
  }

  // A month marker on the rail: the month the events after it fall in, or "No date yet".
  function monthMarker(t) {
    const li = el("li", "hist-month");
    const dot = el("span", "hist-month-dot");
    dot.setAttribute("aria-hidden", "true");
    const label = t === null
      ? "No date yet"
      : new Date(t * 1000).toLocaleDateString(undefined, { month: "long", year: "numeric" });
    li.append(dot, el("span", "hist-month-name", label));
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
    if (!list.length) {
      results.classList.remove("timeline");
      stats.hidden = true;
      results.replaceChildren(empty());
      return;
    }
    const s = summary(list);
    const newest = AB.eventDate(list[0].summary);
    stats.replaceChildren(
      stat("Events held", String(s.events), newest ? "Newest: " + newest : null),
      stat("Unique players", String(s.players), "across every placing"),
      mostStat(s),
    );
    stats.hidden = false;

    results.classList.add("timeline");
    const items = [];
    let month = null;
    for (const result of list) {
      const t = eventTime(result.summary);
      const key = t === null ? "none" : new Date(t * 1000).toLocaleDateString(undefined, { year: "numeric", month: "numeric" });
      if (key !== month) {
        month = key;
        items.push(monthMarker(t));
      }
      items.push(row(result));
    }
    results.replaceChildren(...items);
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
