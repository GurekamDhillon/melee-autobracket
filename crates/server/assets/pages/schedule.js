// The schedule: running events, then those in sign-up by start time, and how to enter. Loads once.
(function () {
  const { el, getJson, setNotice, STATE_LABEL, knownTime } = AB;
  const $ = (id) => document.getElementById(id);

  function entrants(n) {
    return n + (n === 1 ? " entrant" : " entrants");
  }

  // Unknown start times last, then by start, then by id.
  function byStart(a, b) {
    const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
    const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
    return ta === tb ? a.id - b.id : ta - tb;
  }

  // The start time line: the local time (as <time>) and a quiet relative phrase.
  function when(ev) {
    const p = el("p", "when");
    if (!knownTime(ev.starts_at)) {
      p.append(el("span", "quiet", "time to be announced"));
      return p;
    }
    const w = AB.fmtWhen(ev.starts_at);
    const t = el("time", null, w.abs);
    t.dateTime = w.iso;
    p.append(t, " ");
    const past = ev.starts_at * 1000 < Date.now();
    let rel = w.rel;
    if (past && ev.state === "signup") rel = "was due " + w.rel;
    else if (past && ev.state === "running") rel = "started " + w.rel;
    p.append(el("span", "quiet", rel));
    return p;
  }

  function card(ev) {
    const li = el("li", "card");
    const h = el("h2");
    const a = el("a", null, ev.name);
    a.href = AB.eventHref(ev.id);
    h.append(a);
    li.append(h, when(ev));
    if (ev.description) li.append(el("p", null, ev.description));
    li.append(el("p", "quiet", (STATE_LABEL[ev.state] || ev.state) + " · " + entrants(ev.entrants)));
    return li;
  }

  function fill(sectionId, listId, events) {
    const list = $(listId);
    list.replaceChildren(...events.map(card));
    $(sectionId).hidden = !events.length;
  }

  function render(events) {
    const running = events.filter((e) => e.state === "running").sort(byStart);
    const signup = events.filter((e) => e.state === "signup").sort(byStart);
    fill("now", "now-list", running);
    fill("soon", "soon-list", signup);
    const howto = $("howto");
    howto.replaceChildren();
    if (signup.length) howto.append(AB.howToEnter(signup[0].id));
    setNotice("status", running.length || signup.length ? "" : "Nothing is scheduled.");
  }

  getJson("/v1/events").then(
    (list) => render(Array.isArray(list) ? list : []),
    () => setNotice("status", "Couldn’t reach the server just now; reload to try again."),
  );
})();
