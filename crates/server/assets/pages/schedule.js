// The schedule: running events, then those in sign-up grouped by day, and how to enter. Loads once.
(function () {
  const { el, getJson, setNotice, knownTime, chip, pill, statusTag, entrantsText } = AB;
  const $ = (id) => document.getElementById(id);

  // Unknown start times last, then by start, then by id.
  function byStart(a, b) {
    const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
    const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
    return ta === tb ? a.id - b.id : ta - tb;
  }

  // The day an event falls on, as a heading: "Today", "Tomorrow", "Saturday 21 November".
  function dayLabel(ev) {
    if (!knownTime(ev.starts_at)) return "Date to be announced";
    const d = new Date(ev.starts_at * 1000);
    const today = new Date();
    const tomorrow = new Date(today.getFullYear(), today.getMonth(), today.getDate() + 1);
    if (d.toDateString() === today.toDateString()) return "Today";
    if (d.toDateString() === tomorrow.toDateString()) return "Tomorrow";
    return d.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" });
  }

  // The start: the clock time large, and a relative phrase that knows the event's state.
  function when(ev) {
    const box = el("div", "sched-time");
    if (!knownTime(ev.starts_at)) {
      box.append(el("span", "seat-chip", "time to be announced"));
      return box;
    }
    const w = AB.fmtWhen(ev.starts_at);
    const t = el("time", "clock", new Date(ev.starts_at * 1000).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" }));
    t.dateTime = w.iso;
    t.title = w.abs;
    const past = ev.starts_at * 1000 < Date.now();
    let rel = w.rel;
    if (past && ev.state === "signup") rel = "was due " + w.rel;
    else if (past && ev.state === "running") rel = "started " + w.rel;
    box.append(t, el("span", "rel", rel));
    return box;
  }

  function row(ev, i) {
    const li = el("li", "sched-row " + (i % 2 ? "tr1" : "tl2") + (ev.state === "running" ? " live" : ""));
    const body = el("div", "sched-body");
    const h = el("h3");
    const a = el("a", null, ev.name);
    a.href = AB.eventHref(ev.id);
    h.append(a);
    const chips = el("p", "chips");
    chips.append(statusTag(ev.state), chip(entrantsText(ev.entrants)));
    body.append(h, chips);
    if (ev.description) body.append(el("p", "sched-desc", ev.description));
    const go = el("p", "sched-go");
    go.append(pill(ev.state === "running" ? "Bracket" : "Who’s in", AB.eventHref(ev.id), ev.state === "running" ? "sunny" : ""));
    li.append(when(ev), body, go);
    return li;
  }

  function render(events) {
    $("loading").remove();
    const running = events.filter((e) => e.state === "running").sort(byStart);
    const signup = events.filter((e) => e.state === "signup").sort(byStart);
    $("now-list").replaceChildren(...running.map(row));
    $("now").hidden = !running.length;
    const groups = [];
    let i = 0;
    for (const ev of signup) {
      const label = dayLabel(ev);
      let g = groups[groups.length - 1];
      if (!g || g.label !== label) {
        g = { label, list: el("ol", "sched") };
        groups.push(g);
      }
      g.list.append(row(ev, i++));
    }
    $("soon-list").replaceChildren(...groups.flatMap((g) => [el("h3", "day", g.label), g.list]));
    $("soon").hidden = !signup.length;
    const howto = $("howto");
    howto.replaceChildren();
    if (signup.length) howto.append(AB.howToEnter(signup[0].id));
    if (running.length || signup.length) {
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
  }

  getJson("/v1/events").then(
    (list) => render(Array.isArray(list) ? list : []),
    () => {
      $("loading").remove();
      setNotice("status", "Couldn’t reach the server just now; reload to try again.");
    },
  );
})();
