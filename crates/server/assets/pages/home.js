const m = /^#(\d+)$/.exec(location.hash); if (m) { location.replace("/events/" + m[1]); }
// The landing page. This is a stub: every event, newest first, as cards linking to its bracket.
(function () {
  const POLL_MS = 10000;
  const { el, getJson, STATE_LABEL } = AB;
  const main = document.getElementById("main");
  let drawnKey = "";

  function card(ev) {
    const a = el("a", "card");
    a.href = AB.eventHref(ev.id);
    a.append(el("h2", null, ev.name));
    const bits = [STATE_LABEL[ev.state] || ev.state, ev.entrants + (ev.entrants === 1 ? " entrant" : " entrants")];
    a.append(el("p", "quiet", bits.join(", ")));
    const li = el("li");
    li.append(a);
    return li;
  }

  function render(events) {
    const key = JSON.stringify(events);
    if (key === drawnKey) return;
    drawnKey = key;
    if (!events.length) {
      main.replaceChildren(el("p", "notice", "No events yet."));
      return;
    }
    const list = el("ol", "cards");
    for (const ev of events) list.append(card(ev));
    main.replaceChildren(list);
  }

  async function refresh() {
    try {
      const list = await getJson("/v1/events");
      render((Array.isArray(list) ? list : []).slice().sort((a, b) => b.id - a.id));
    } catch (err) {
      if (!drawnKey) main.replaceChildren(el("p", "notice", "Couldn’t reach the server just now; trying again in a few seconds."));
    }
  }

  AB.poll(refresh, POLL_MS);
})();
