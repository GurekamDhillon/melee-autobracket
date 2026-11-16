// The schedule: every event as a date block, filtered by status (All, Live, Open, Done). Each row
// shows its day, its status, its entrants (the first few as a stack of discs) and its action. How
// to enter sits beside the list as three numbered steps. Loads once.
(function () {
  const { el, icon, getJson, setNotice, knownTime, chip, pill, statusTag, entrantsText, avatar } = AB;
  const $ = (id) => document.getElementById(id);

  // The filters: a label, and the event states each one shows.
  const FILTERS = [
    { key: "all", label: "All", states: ["running", "signup", "complete"] },
    { key: "live", label: "Live", states: ["running"] },
    { key: "open", label: "Open", states: ["signup"] },
    { key: "done", label: "Done", states: ["complete"] },
  ];
  // The list section each state lives in.
  const SECTION_OF = { running: "now", signup: "soon", complete: "past" };
  const STACK = 5;

  let events = [];
  let filter = "all";
  const tagsById = new Map();

  // Unknown start times last, then by start, then by id.
  function byStart(a, b) {
    const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
    const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
    return ta === tb ? a.id - b.id : ta - tb;
  }

  // Most recent first; unknown last.
  function byRecent(a, b) {
    const ta = knownTime(a.starts_at) ? a.starts_at : 0;
    const tb = knownTime(b.starts_at) ? b.starts_at : 0;
    return tb - ta || a.id - b.id;
  }

  // A tile with no date yet: the status as an icon in the status colour (bolt live, door open, trophy done).
  const STATUS_ICON = { running: "bolt", signup: "door", complete: "trophy" };
  function statusTile(state) {
    const tile = el("div", "sched-date sd-status s-" + (STATUS_ICON[state] ? state : "unknown"));
    tile.setAttribute("aria-hidden", "true");
    if (STATUS_ICON[state]) tile.append(icon(STATUS_ICON[state]));
    return tile;
  }

  // The day as a block: the month, the big day of the month, the weekday. Unannounced: a status tile.
  function dateTile(ev) {
    const tile = el("div", "sched-date");
    tile.setAttribute("aria-hidden", "true");
    if (!knownTime(ev.starts_at)) return statusTile(ev.state);
    const d = new Date(ev.starts_at * 1000);
    tile.append(
      el("span", "sd-month", d.toLocaleDateString(undefined, { month: "short" })),
      el("span", "sd-day", String(d.getDate())),
      el("span", "sd-wk", d.toLocaleDateString(undefined, { weekday: "short" })),
    );
    return tile;
  }

  // The start: the clock time, and a relative phrase that knows the event's state.
  function when(ev) {
    const box = el("div", "sched-time");
    if (!knownTime(ev.starts_at)) {
      box.append(el("span", "seat-chip", "time to be announced"));
      return box;
    }
    const w = AB.fmtWhen(ev.starts_at);
    const t = el("time", "when-t", new Date(ev.starts_at * 1000).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" }));
    t.dateTime = w.iso;
    t.title = w.abs;
    const past = ev.starts_at * 1000 < Date.now();
    let rel = w.rel;
    if (past && ev.state === "signup") rel = "was due " + w.rel;
    else if (past && ev.state === "running") rel = "started " + w.rel;
    box.append(t, el("span", "rel", rel));
    return box;
  }

  // The first few entrants as overlapping discs, and "+n" for the rest; none until tags arrive.
  function stack(ev) {
    const tags = tagsById.get(ev.id) || [];
    if (!tags.length) return null;
    const wrap = el("span", "stack");
    wrap.setAttribute("aria-hidden", "true");
    const shown = tags.slice(0, STACK);
    for (const t of shown) wrap.append(avatar(t));
    const more = (ev.entrants || 0) - shown.length;
    if (more > 0) wrap.append(el("span", "disc more", "+" + more));
    return wrap;
  }

  function row(ev, i) {
    const extra = ev.state === "running" ? " live" : ev.state === "complete" ? " is-done" : "";
    const li = el("li", "sched-row " + (i % 2 ? "tr1" : "tl2") + extra);
    const body = el("div", "sched-body");
    const h = el("h3");
    const a = el("a", null, ev.name);
    a.href = AB.eventHref(ev.id);
    h.append(a);
    const chips = el("p", "chips");
    chips.append(statusTag(ev.state), chip(entrantsText(ev.entrants)));
    const foot = el("div", "sched-foot");
    foot.append(when(ev));
    const s = stack(ev);
    if (s) foot.append(s);
    body.append(h, chips, foot);
    if (ev.description) body.append(el("p", "sched-desc", ev.description));
    const go = el("p", "sched-go");
    go.append(pill(ev.state === "signup" ? "Who’s in" : "Bracket", AB.eventHref(ev.id), ev.state === "running" ? "sunny" : ""));
    li.append(dateTile(ev), body, go);
    return li;
  }

  // How to enter, as three numbered steps. The commands for testers sit in a terminal-style
  // disclosure under the last step.
  function steps(eventId) {
    const card = el("section", "how");
    card.append(el("h2", null, "How to enter"));
    const list = el("ol", "how-steps");
    const item = (title, text) => {
      const li = el("li");
      li.append(el("p", "how-t", title), el("p", "how-d", text));
      return li;
    };
    list.append(
      item("Pick an event", "Choose one that says Sign-ups open. Joining from inside the game is coming soon."),
      item("Get the tester build", "The tester build is not public yet. Ask an organiser for it."),
    );
    const last = item("Run two commands", "Once you have the build, run these in a terminal.");
    const testers = el("details");
    testers.append(el("summary", null, "For testers: join from the command line"));
    const snip = el("div", "snip");
    snip.append(
      el("code", null, "autobracket-client register --server " + location.origin + " --tag YOURTAG"),
      el("code", null, "autobracket-client signup " + eventId),
    );
    testers.append(snip);
    last.append(testers);
    list.append(last);
    card.append(list);
    return card;
  }

  function render() {
    const running = events.filter((e) => e.state === "running").sort(byStart);
    const signup = events.filter((e) => e.state === "signup").sort(byStart);
    const done = events.filter((e) => e.state === "complete").sort(byRecent);
    const current = FILTERS.find((f) => f.key === filter).states;
    const lists = { now: [running, "now-list"], soon: [signup, "soon-list"], past: [done, "past-list"] };
    let visible = 0;
    $("loading").remove();
    for (const sec of Object.keys(lists)) {
      const [list, listId] = lists[sec];
      const state = Object.keys(SECTION_OF).find((k) => SECTION_OF[k] === sec);
      const on = current.includes(state);
      $(listId).replaceChildren(...list.map(row));
      $(sec).hidden = !on || !list.length;
      if (on) visible += list.length;
    }
    $("none").hidden = visible > 0 || !events.length;
    $("howto").replaceChildren(...(signup.length ? [steps(signup[0].id)] : []));
  }

  // The status filter, beside the heading.
  function buildFilter() {
    const group = el("div", "sched-filter");
    group.setAttribute("role", "group");
    group.setAttribute("aria-label", "Show events by status");
    for (const f of FILTERS) {
      const b = el("button", null, f.label);
      b.type = "button";
      b.setAttribute("aria-pressed", String(f.key === filter));
      b.addEventListener("click", () => {
        filter = f.key;
        for (const other of group.children) other.setAttribute("aria-pressed", String(other === b));
        render();
      });
      group.append(b);
    }
    return group;
  }

  getJson("/v1/events").then(
    async (list) => {
      events = Array.isArray(list) ? list : [];
      // Each event's entrants, for the stack of discs; a failed one just shows its count.
      await Promise.all(
        events.map((ev) =>
          getJson("/v1/events/" + ev.id).then(
            (view) => { if (view && Array.isArray(view.tags)) tagsById.set(ev.id, view.tags.filter((t) => typeof t === "string")); },
            () => {},
          ),
        ),
      );
      if (events.length) {
        $("head-side").append(buildFilter());
        setNotice("status", "");
      } else {
        const box = el("div", "empty");
        box.append(el("h3", null, "Nothing is scheduled."));
        box.append(el("p", null, "New events show up here as soon as an organiser creates them."));
        const p = el("p", "actions");
        p.append(pill("Past results", "/history", "sunny"));
        box.append(p);
        $("now").before(box);
      }
      render();
    },
    () => {
      $("loading").remove();
      setNotice("status", "Couldn’t reach the server just now; reload to try again.");
    },
  );
})();
