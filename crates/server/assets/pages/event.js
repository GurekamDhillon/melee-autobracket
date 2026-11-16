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
  let scrolledToHash = false;

  // The picker, the layout toggle, "Jump to live" and when the page last heard from the server
  // sit together as one toolbar, one row under the facts: every control the same height, the
  // "Updated" stamp a small pill at the end of the same row.
  const toolbar = el("div", "toolbar");
  toolbar.append($("pick-label"), $("lay-h").parentNode, $("jump-live"), $("fresh"));
  $("head-side").append(toolbar);

  // ---- layout: rounds side by side, or stacked for portrait screens -------------

  const LAYOUT_KEY = "bracket-layout";
  const PORTRAIT = matchMedia("(orientation: portrait)");
  const CALM = matchMedia("(prefers-reduced-motion: reduce)");
  const board = $("board");

  // Storage is touched only here and in storeLayout, each guarded: a browser that refuses it
  // (private mode, blocked site data) gets the automatic choice.
  function readLayout() {
    try { return localStorage.getItem(LAYOUT_KEY); } catch (e) { return null; }
  }

  // The remembered choice, or null where nothing (or nonsense) is stored.
  function storedLayout() {
    const value = readLayout();
    return value === "horizontal" || value === "vertical" ? value : null;
  }

  function storeLayout(value) {
    try { localStorage.setItem(LAYOUT_KEY, value); } catch (e) {}
  }

  // The layout clicked this visit: it wins even where storage refused to remember it, so the
  // toggle always answers; storage only carries the choice to the next visit.
  let chosen = null;
  // Whether a bracket (rather than the sign-up list) is on the board.
  let hasBracket = false;

  // The board is a keyboard-scrollable region only in the horizontal layout, where it scrolls
  // sideways; stacked, it is a plain region.
  function labelBoard() {
    if (!hasBracket) {
      board.removeAttribute("tabindex");
      board.removeAttribute("aria-label");
      return;
    }
    if (board.dataset.layout === "vertical") {
      board.removeAttribute("tabindex");
      board.setAttribute("aria-label", "Bracket");
    } else {
      board.tabIndex = 0;
      board.setAttribute("aria-label", "Bracket, scrolls sideways");
    }
  }

  function applyLayout() {
    const layout = chosen || storedLayout() || (PORTRAIT.matches ? "vertical" : "horizontal");
    board.dataset.layout = layout;
    $("lay-h").setAttribute("aria-pressed", String(layout === "horizontal"));
    $("lay-v").setAttribute("aria-pressed", String(layout === "vertical"));
    labelBoard();
  }

  function chooseLayout(value) {
    chosen = value;
    storeLayout(value);
    applyLayout();
  }

  function jumpToLive() {
    document.querySelector(".set.live")?.scrollIntoView({ block: "center", behavior: "auto" });
  }

  PORTRAIT.addEventListener("change", applyLayout);
  $("lay-h").addEventListener("click", () => chooseLayout("horizontal"));
  $("lay-v").addEventListener("click", () => chooseLayout("vertical"));
  $("jump-live").addEventListener("click", jumpToLive);
  applyLayout();

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

  // A tag, linking to the player's profile as every other page does.
  function tagLink(tag) {
    const a = el("a", "tag", tag);
    a.href = AB.playerHref(tag);
    return a;
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
    // The face is aria-hidden (the set's words carry its meaning), so its links stay off the
    // tab order rather than landing focus on something a screen reader cannot see.
    const link = tagLink(tagOf(entrant));
    link.tabIndex = -1;
    row.append(link);
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
    // Both players known, nothing played or decided yet: the set is still to come.
    const upcoming = !live && decided === undefined && doneWinner(set.state) === null &&
      set.slots.every((s) => slotEntrant(s) !== null);
    const card = el("li", live ? "set live" : "set " + tiltClass(j, set.id) + (upcoming ? " upcoming" : ""));
    // Other pages link straight to a set: /events/N#set-M.
    card.id = "set-" + set.id;
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
    // A dashed orbit, the burst, and four sparks around it: all drawn by the shell's symbols or CSS.
    box.append(el("span", "champ-orbit"), icon("burst"));
    for (const k of ["s1", "s2", "s3", "s4"]) {
      const spark = el("span", "champ-spark " + k);
      spark.append(icon("star"));
      spark.setAttribute("aria-hidden", "true");
      box.append(spark);
    }
    const text = el("span", "champ-text");
    text.append(el("span", "champ-label", "Champion"));
    text.append(el("span", "sr", ": "));
    const name = tagLink(tag);
    name.className = tag.length > 9 ? "champ-tag long" : "champ-tag";
    text.append(name);
    box.append(text);
    return box;
  }

  const SIDES = [
    ["winners", "Winners", "Winners"],
    ["losers", "Losers", "Losers"],
    ["final", "GrandFinal", "Final"],
  ];

  // A round's heading: a chip with its place on the side (the round's number), its name, the
  // matches it holds and its state. The chip's colour carries the state; the words say it too.
  const ROUND_STATE = { done: "Done", now: "Live", todo: "Upcoming" };

  function roundHead(round, n) {
    const head = el("header", "round-head st-" + round.status);
    const chip = el("div", "round-chip");
    const no = el("span", "round-no", String(n + 1));
    no.setAttribute("aria-hidden", "true");
    const shown = round.sets.filter((s) => !isHidden(s)).length;
    const meta = el("p", "round-meta");
    meta.append(el("span", null, shown + (shown === 1 ? " match" : " matches")), " \u00b7 ", el("span", "round-state", ROUND_STATE[round.status] || ""));
    chip.append(no, el("h2", null, round.name), meta);
    head.append(chip);
    return head;
  }

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
        // The run strip's disc for this round jumps here.
        col.dataset.round = cls + "-" + n;
        col.append(roundHead(round, n));
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

  // The run strip: one disc per round, grouped by side; the grand final is a star. Each disc
  // jumps to its round below. A round is done, now (a set is in play), next (the first still to
  // come on its side) or later; the legend under the count says so. On a phone the strip sticks
  // to the top of the screen while the bracket scrolls under it.
  const STATE_HOP = { done: "done", now: "in play", next: "up next", todo: "to come" };
  // Whether the Key tooltip is open; kept across redraws so a poll does not close it.
  let keyOpen = false;

  function renderStrip(view) {
    const b = view.bracket;
    const strip = el("div", "strip");
    const row = el("div", "strip-row");
    const said = [];
    let total = 0;
    let doneTotal = 0;
    for (const [cls, side, label] of SIDES) {
      const rounds = sideRounds(b, side);
      if (!rounds.length) continue;
      const name = b.format === "SingleElim" ? "Rounds" : label;
      const grp = el("div", "grp");
      const grpName = el("span", "grp-name", name);
      grpName.setAttribute("aria-hidden", "true");
      grp.append(grpName);
      const marks = el("span", "marks");
      let nextTaken = false;
      rounds.forEach((r, n) => {
        let state = r.status;
        if (state === "todo" && !nextTaken) {
          state = "next";
          nextTaken = true;
        }
        const hop = el("button", "hop " + (r.star ? "star " : "disc ") + state);
        hop.type = "button";
        hop.dataset.round = cls + "-" + n;
        hop.setAttribute("aria-label", r.name + ", " + STATE_HOP[state]);
        // Each disc scrolls to its round column; the title says which one on hover.
        hop.title = r.name;
        if (r.star) hop.append(icon("star"));
        else hop.append(String(n + 1));
        marks.append(hop);
      });
      grp.append(marks);
      row.append(grp);
      const done = rounds.filter((r) => r.status === "done").length;
      total += rounds.length;
      doneTotal += done;
      let words = name + ": " + done + " of " + rounds.length + (rounds.length === 1 ? " round" : " rounds") + " done";
      const now = rounds.filter((r) => r.status === "now").map((r) => r.name);
      if (now.length) words += ", " + now.join(" and ") + " in play";
      said.push(words);
    }
    // One slim line: the count, a bar, the stamp when complete, and a Key button whose tooltip
    // holds the legend. The count and bar are decoration; the Progress words above speak for them.
    const note = el("div", "strip-note");
    const count = el("span", "strip-count", doneTotal + " of " + total + (total === 1 ? " round" : " rounds") + " done");
    count.setAttribute("aria-hidden", "true");
    const bar = el("progress", "strip-bar");
    bar.max = Math.max(total, 1);
    bar.value = doneTotal;
    bar.setAttribute("aria-hidden", "true");
    note.append(count, bar);
    const complete = view.summary.state === "complete";
    if (complete) {
      const stamp = el("span", "stamp big", "Complete");
      stamp.setAttribute("aria-hidden", "true");
      note.append(stamp);
    }
    // The legend sits under the discs, always visible at a desktop width. The Key button only
    // shows on a phone, where it opens the same legend under the discs.
    const keyBtn = el("button", "key-toggle", "Key");
    keyBtn.type = "button";
    keyBtn.setAttribute("aria-controls", "strip-key");
    keyBtn.setAttribute("aria-expanded", String(keyOpen));
    const legend = el("span", "key-tip");
    legend.id = "strip-key";
    legend.classList.toggle("open", keyOpen);
    for (const [key, word] of [["done", "Done"], ["now", "Live"], ["upcoming", "Upcoming"]]) legend.append(el("span", "key " + key, word));
    keyBtn.addEventListener("click", () => {
      keyOpen = !keyOpen;
      legend.classList.toggle("open", keyOpen);
      keyBtn.setAttribute("aria-expanded", String(keyOpen));
    });
    note.append(keyBtn);
    row.addEventListener("click", (e) => {
      const hop = e.target.closest(".hop");
      const target = hop && board.querySelector('[data-round="' + hop.dataset.round + '"]');
      if (target) target.scrollIntoView({ block: "start", behavior: CALM.matches ? "auto" : "smooth" });
    });
    strip.append(note, row, legend);
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
        li.append(tagLink(t));
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

  // ---- the hero: where the event stands, its date, its facts as labels, and the champion ----

  // Built once and filled on each drawing; the shell's title sits between them.
  const heroKicker = el("div", "ev-kicker");
  const heroState = el("span", "status");
  const heroDate = el("span", "ev-date");
  heroKicker.append(heroState, heroDate);
  const heroFacts = el("div", "chips ev-facts");
  heroFacts.setAttribute("aria-hidden", "true");
  const heroChamp = el("p", "hero-champ");
  heroChamp.hidden = true;
  const heroDesc = el("p", "ev-desc");
  heroDesc.hidden = true;
  const topText = $("title").parentNode;
  topText.insertBefore(heroKicker, $("title"));
  $("title").after(heroFacts, heroChamp, heroDesc);

  const STATES = ["signup", "running", "complete"];

  // The champion's sets won and lost across the bracket, counted from the sets that were played.
  function champRecord(bracket, who) {
    let won = 0;
    let lost = 0;
    for (const s of bracket.sets) {
      const w = doneWinner(s.state);
      if (w === null) continue;
      const idx = s.slots.findIndex((slot) => slotEntrant(slot) === who);
      if (idx < 0) continue;
      if (idx === w) won++;
      else lost++;
    }
    return [won, lost];
  }

  function renderHero(sum, stateLabel, facts, when, champ, tagOf, view) {
    heroState.className = STATES.includes(sum.state) ? "status " + sum.state : "status";
    heroState.textContent = stateLabel;
    heroDate.textContent = when;
    heroDate.hidden = when === "";
    heroFacts.replaceChildren(...facts.map((f, i) => el("span", i === 0 && f.hot ? "chip hot" : "chip", f.text)));
    heroDesc.textContent = sum.description || "";
    heroDesc.hidden = !sum.description;
    if (champ === null) {
      heroChamp.hidden = true;
      heroChamp.replaceChildren();
      return;
    }
    const name = tagLink(tagOf(champ));
    name.className = "tag hc-name";
    // The profile link carries a chevron, so it reads as a way somewhere.
    const nameRow = el("span", "hc-name-row");
    const [won, lost] = champRecord(view.bracket, champ);
    const record = el("span", "hc-record", won + "-" + lost + (won + lost === 1 ? " set" : " sets"));
    const badge = el("span", "hc-badge");
    const burst = icon("burst");
    burst.classList.add("hc-burst");
    const star = icon("star");
    star.classList.add("hc-star");
    badge.append(burst, star);
    badge.setAttribute("aria-hidden", "true");
    const text = el("span", "hc-text");
    const chev = icon("chev");
    chev.classList.add("hc-chev");
    nameRow.append(name, chev);
    text.append(el("span", "hc-label", "Champion"), nameRow, record);
    heroChamp.replaceChildren(badge, text);
    heroChamp.hidden = false;
  }

  function renderView(view) {
    const sum = view.summary;
    const tags = view.tags || [];
    const tagOf = (n) => (typeof tags[n] === "string" ? tags[n] : "Entrant " + (n + 1));
    $("title").textContent = sum.name;
    document.title = sum.name + " · Bracket";

    const format = view.bracket ? view.bracket.format : null;
    const stateLabel = STATE_LABEL[sum.state] || sum.state;
    const facts = [];
    if (FORMAT_LABEL[format]) facts.push({ text: FORMAT_LABEL[format], hot: true });
    facts.push({ text: sum.entrants + (sum.entrants === 1 ? " entrant" : " entrants") });
    for (const bit of rulesBits(view.ruleset, format)) facts.push({ text: bit });
    const when = AB.knownTime(sum.starts_at) ? AB.fmtWhen(sum.starts_at).abs : "";
    const bits = [stateLabel, ...facts.map((f) => f.text)];
    if (when) bits.push(when);
    let text = bits.join(", ");
    if (sum.description) text += ". " + sum.description;
    const meta = $("meta");
    meta.replaceChildren(text);
    const champ = view.bracket ? champion(view.bracket) : null;
    if (champ !== null) {
      // A description that already ends a sentence is not given a second full stop.
      meta.append((/[.!?]$/.test(text) ? " " : ". ") + "Champion: ");
      meta.append(el("span", "champion", tagOf(champ)));
    }
    renderHero(sum, stateLabel, facts, when, champ, tagOf, view);

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
      hasBracket = true;
      $("jump-live").hidden = !document.querySelector(".set.live");
    } else {
      $("strip").replaceChildren();
      $("board").replaceChildren(renderSignup(tags));
      hasBracket = false;
      $("jump-live").hidden = true;
    }
    labelBoard();
    // Arrived from a scoreboard elsewhere: bring that set into view, once.
    if (!scrolledToHash) {
      scrolledToHash = true;
      const target = /^#set-\d+$/.test(location.hash) ? document.getElementById(location.hash.slice(1)) : null;
      if (target) target.scrollIntoView({ block: "center", inline: "center" });
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
      $("updated").textContent = new Date().toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });
      // The dot blinks once per refresh (only where motion is allowed; see site.css).
      const dot = document.querySelector(".fresh-dot");
      dot.classList.remove("blink");
      void dot.offsetWidth;
      dot.classList.add("blink");
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
