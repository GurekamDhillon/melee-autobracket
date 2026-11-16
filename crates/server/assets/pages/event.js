// The bracket viewer: /events/{id}.
(function () {
  const POLL_MS = 10000;
  const { el, icon, getJson, setNotice, STATE_LABEL, FORMAT_LABEL, slotEntrant, doneWinner, isHidden, sideRounds } = AB;

  const $ = (id) => document.getElementById(id);
  const eventId = Number(/^\/events\/(\d+)$/.exec(location.pathname)[1]);
  const picker = $("picker");
  let pickerKey = "";
  let drawnKey = "";
  let enteredFor = "";
  let polling = null;

  // The picker sits beside the title.
  $("head-side").append($("pick-label"));

  function renderPicker(events) {
    const key = JSON.stringify(events.map((e) => [e.id, e.name, e.state]));
    if (key !== pickerKey) {
      pickerKey = key;
      picker.replaceChildren();
      for (const ev of events) {
        const label = ev.name + " (" + (STATE_LABEL[ev.state] || ev.state) + ")";
        const opt = el("option", null, label);
        opt.value = String(ev.id);
        picker.append(opt);
      }
    }
    $("pick-label").hidden = events.length === 0;
    picker.value = String(eventId);
  }

  // ---- bracket drawing ---------------------------------------------------

  // Idle tilt of the j-th sticker of a column: the sign alternates down the column, the
  // amount comes from the set's id, so a refresh never changes it.
  function tiltClass(j, id) {
    const big = id % 2 === 0;
    if (j % 2 === 0) return big ? "tl1" : "tl2";
    return big ? "tr2" : "tr1";
  }

  // How a set decided without play is stamped.
  const DECIDED_LABEL = { award: "Awarded", dq: "DQ", "double-dq": "DQ" };

  // What a set's row shows as its score: games played before an award or DQ still show; a
  // bare 0-0 gives way to the stamp.
  function scoreText(idx, set, score, decided) {
    const winner = doneWinner(set.state);
    const played = Array.isArray(score) && (score[0] > 0 || score[1] > 0);
    if (played || (score && !decided)) return String(score[idx]);
    if (!decided && winner !== null) return winner === idx ? "W" : "";
    return "";
  }

  function slotRow(slot, idx, set, score, decided, tagOf) {
    const entrant = slotEntrant(slot);
    const winner = doneWinner(set.state);
    let cls = "row";
    if (winner !== null && entrant !== null) cls += winner === idx ? " win" : " lose";
    const row = el("div", cls);
    row.append(el("span", "seed", entrant === null ? "" : entrant + 1));
    if (entrant === null) {
      row.append(el("span", "waiting", "Waiting"));
      return row;
    }
    row.append(el("span", "tag", tagOf(entrant)));
    const cell = el("span", "score");
    if (winner === idx) cell.append(el("span", "tick"));
    cell.append(scoreText(idx, set, score, decided));
    row.append(cell);
    return row;
  }

  // The set in words, for a screen reader: "mango 2, zain 0, mango won".
  function setWords(set, score, decided, tagOf) {
    const names = set.slots.map((s) => (slotEntrant(s) === null ? null : tagOf(slotEntrant(s))));
    if (names[0] === null && names[1] === null) return "Waiting for both players";
    const parts = names.map((n, i) => {
      if (n === null) return "waiting for an opponent";
      const sc = scoreText(i, set, score, decided);
      return sc === "" || sc === "W" ? n : n + " " + sc;
    });
    const winner = doneWinner(set.state);
    if (winner !== null && names[winner] !== null) parts.push(names[winner] + " won");
    if (decided) parts.push(DECIDED_LABEL[decided] || decided);
    if (set.state === "Ready") parts.push("live");
    return parts.join(", ");
  }

  function setCard(set, j, scores, decidedBy, tagOf) {
    if (isHidden(set)) {
      const gap = el("li", "set spacer");
      gap.setAttribute("aria-hidden", "true");
      return gap;
    }
    const score = scores.get(set.id);
    const decided = decidedBy.get(set.id);
    const words = el("span", "sr", setWords(set, score, decided, tagOf));
    if (set.slots.every((s) => slotEntrant(s) === null)) {
      const seat = el("li", "set seat");
      const shown = el("span", null, "Waiting");
      shown.setAttribute("aria-hidden", "true");
      seat.append(words, shown);
      return seat;
    }
    const live = set.state === "Ready";
    const card = el("li", live ? "set live" : "set " + tiltClass(j, set.id));
    const face = el("div");
    face.setAttribute("aria-hidden", "true");
    if (live) face.append(el("span", "flag", "Live"));
    if (decided) face.append(el("span", "stamp", DECIDED_LABEL[decided] || decided));
    face.append(slotRow(set.slots[0], 0, set, score, decided, tagOf));
    face.append(slotRow(set.slots[1], 1, set, score, decided, tagOf));
    card.append(words, face);
    return card;
  }

  function champion(bracket) {
    const resolved = bracket.sets.every((s) => s.state === "Skipped" || doneWinner(s.state) !== null);
    if (!resolved) return null;
    for (let i = bracket.sets.length - 1; i >= 0; i--) {
      const s = bracket.sets[i];
      const w = doneWinner(s.state);
      if (s.winner_to === null && w !== null) return slotEntrant(s.slots[w]);
    }
    return null;
  }

  function championBurst(tag) {
    const box = el("p", "champ");
    box.append(icon("burst"));
    const text = el("span", "champ-text");
    text.append(el("span", "champ-label", "Champion"));
    text.append(el("span", "sr", ": "));
    text.append(el("span", tag.length > 9 ? "champ-tag long" : "champ-tag", tag));
    box.append(text);
    return box;
  }

  const SIDES = [
    ["winners", "Winners", "Winners"],
    ["losers", "Losers", "Losers"],
    ["final", "GrandFinal", "Final"],
  ];

  function renderBracket(view, tagOf) {
    const b = view.bracket;
    const scores = new Map((view.scores || []).map(([id, wins]) => [id, wins]));
    // Sets decided without play (award, DQ): stamped instead of showing a 0-0 score.
    const decidedBy = new Map((view.decided_by || []).map(([id, how]) => [id, String(how)]));
    const grid = el("div", "bracket");
    let finalRow = null;
    let widest = 0;
    for (const [cls, side] of SIDES) {
      const rounds = sideRounds(b, side);
      if (!rounds.length) continue;
      const row = el("div", "side " + cls);
      const first = side === "GrandFinal" ? widest : 0;
      rounds.forEach((round, n) => {
        const col = el("section", "round c" + Math.min(first + n, 12));
        col.append(el("h2", null, round.name));
        const list = el("ol", "sets");
        let j = 0;
        for (const s of round.sets) {
          list.append(setCard(s, j, scores, decidedBy, tagOf));
          if (!isHidden(s)) j++;
        }
        col.append(list);
        row.append(col);
      });
      widest = Math.max(widest, rounds.length);
      if (side === "GrandFinal") finalRow = row;
      grid.append(row);
    }
    const champ = champion(b);
    if (champ !== null) {
      if (!finalRow) {
        finalRow = el("div", "side final");
        grid.append(finalRow);
      }
      finalRow.append(championBurst(tagOf(champ)));
    }
    return grid;
  }

  // The run strip: one mark per round, grouped by side; the grand final is a star.
  function renderStrip(view) {
    const b = view.bracket;
    const strip = el("div", "strip");
    const said = [];
    for (const [, side, label] of SIDES) {
      const rounds = sideRounds(b, side);
      if (!rounds.length) continue;
      const name = b.format === "SingleElim" ? "Rounds" : label;
      const grp = el("div", "grp");
      grp.append(el("span", "grp-name", name));
      const marks = el("span", "marks");
      for (const r of rounds) {
        const mark = el("span", "mark " + (r.star ? "star " : "disc ") + r.status);
        if (r.star) mark.append(icon("star"));
        marks.append(mark);
      }
      grp.append(marks);
      strip.append(grp);
      const done = rounds.filter((r) => r.status === "done").length;
      let words = name + ": " + done + " of " + rounds.length + (rounds.length === 1 ? " round" : " rounds") + " done";
      const now = rounds.filter((r) => r.status === "now").map((r) => r.name);
      if (now.length) words += ", " + now.join(" and ") + " in play";
      said.push(words);
    }
    for (const child of strip.children) child.setAttribute("aria-hidden", "true");
    const complete = view.summary.state === "complete";
    if (complete) {
      const stamp = el("span", "stamp big", "Complete");
      stamp.setAttribute("aria-hidden", "true");
      strip.append(stamp);
    }
    strip.prepend(el("span", "sr", "Progress. " + said.join(". ") + (complete ? ". The event is complete." : ".")));
    return strip;
  }

  function renderSignup(tags) {
    const box = el("div", "signup");
    const n = tags.length;
    box.append(el("h2", null, n === 0 ? "Nobody has entered yet" : n + " entered so far"));
    if (n) {
      const list = el("ol", n <= 8 ? "entrants few" : "entrants");
      tags.forEach((t, i) => {
        const li = el("li", "entrant " + (i % 2 ? "tr1" : "tl2") + " c" + Math.min(i, 12));
        li.append(el("span", "tag", t));
        const seed = el("span", "seed");
        seed.append(el("span", "sr", ", "), "Seed " + (i + 1));
        li.append(seed);
        list.append(li);
      });
      box.append(list);
    }
    box.append(el("p", "notice", "The bracket appears here when the event starts."));
    return box;
  }

  function rulesBits(r, format) {
    if (!r) return [];
    const parts = ["best of " + r.best_of];
    // best_of_finals applies to grand final sets, which only double elimination has.
    if (format === "DoubleElim" && r.best_of_finals !== r.best_of) {
      parts.push("grand final best of " + r.best_of_finals);
    }
    parts.push(r.stocks + (r.stocks === 1 ? " stock" : " stocks"));
    const m = Math.floor(r.timer_secs / 60), s = r.timer_secs % 60;
    parts.push(s === 0 ? m + (m === 1 ? " minute" : " minutes") : m + ":" + String(s).padStart(2, "0"));
    return parts;
  }

  function renderView(view) {
    const sum = view.summary;
    const tags = view.tags || [];
    const tagOf = (n) => (typeof tags[n] === "string" ? tags[n] : "Entrant " + (n + 1));
    $("title").textContent = sum.name;
    document.title = sum.name + " · Bracket";

    const format = view.bracket ? view.bracket.format : null;
    const bits = [STATE_LABEL[sum.state] || sum.state];
    if (FORMAT_LABEL[format]) bits.push(FORMAT_LABEL[format]);
    bits.push(sum.entrants + (sum.entrants === 1 ? " entrant" : " entrants"));
    bits.push(...rulesBits(view.ruleset, format));
    if (typeof sum.starts_at === "number") bits.push(AB.fmtWhen(sum.starts_at).abs);
    const meta = $("meta");
    meta.replaceChildren(bits.join(", "));
    if (sum.description) meta.append(". " + sum.description);
    const champ = view.bracket ? champion(view.bracket) : null;
    if (champ !== null) {
      meta.append(". Champion: ");
      meta.append(el("span", "champion", tagOf(champ)));
    }

    const unverified = view.unverified_games || 0;
    setNotice("unverified", unverified > 0
      ? unverified + (unverified === 1 ? " game was" : " games were") + " counted without engine verification"
      : "");

    // Nothing new since the last drawing: leave the page exactly as it is.
    const key = JSON.stringify(view);
    if (key === drawnKey) return;
    drawnKey = key;
    // The entrance plays on the first drawing of the event (and of its bracket), never on a refresh.
    const what = view.bracket ? "bracket" : "signup";
    document.body.classList.toggle("enter", what !== enteredFor);
    enteredFor = what;

    if (view.bracket) {
      $("strip").replaceChildren(renderStrip(view));
      $("board").replaceChildren(renderBracket(view, tagOf));
      $("board").tabIndex = 0;
      $("board").setAttribute("aria-label", "Bracket, scrolls sideways");
    } else {
      $("strip").replaceChildren();
      $("board").replaceChildren(renderSignup(tags));
      $("board").removeAttribute("tabindex");
      $("board").removeAttribute("aria-label");
    }
  }

  // ---- polling -----------------------------------------------------------

  async function refresh() {
    try {
      const [list, view] = await Promise.all([getJson("/v1/events"), getJson("/v1/events/" + eventId)]);
      const events = (Array.isArray(list) ? list : []).slice().sort((a, b) => b.id - a.id);
      renderPicker(events);
      renderView(view);
      setNotice("status", "");
      $("updated").textContent = new Date().toLocaleTimeString();
    } catch (err) {
      if (err && err.status === 404) {
        // The event was deleted under us: say so once and stop asking.
        if (polling) polling.stop();
        setNotice("status", "This event no longer exists");
        return;
      }
      setNotice("status", "Couldn’t reach the server just now. Showing the last update; trying again in a few seconds.");
    }
  }

  picker.addEventListener("change", () => {
    location.href = AB.eventHref(picker.value);
  });
  polling = AB.poll(refresh, POLL_MS);
})();
