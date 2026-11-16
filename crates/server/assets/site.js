"use strict";
// Shared by every page. The server joins this file and the page's own script into the page's one
// inline script block, so everything here is reached as `AB.*`.
const AB = (function () {
  const $ = (id) => document.getElementById(id);

  // ---- DOM: every value reaches the page as text -----------------------------------------

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  }

  // One of the shell's own drawings, copied from its <template> (a fixed name, never data).
  function icon(name) {
    const from = $(name === "burst" ? "t-burst" : "t-star");
    return from.content.firstElementChild.cloneNode(true);
  }

  // The JSON at `path`, or an Error carrying the response's `status`.
  async function getJson(path) {
    const res = await fetch(path, { cache: "no-store", headers: { accept: "application/json" } });
    if (!res.ok) {
      const err = new Error(path + " returned " + res.status);
      err.status = res.status;
      throw err;
    }
    return res.json();
  }

  // A notice sticker shows `text`, or is hidden when there is none.
  function setNotice(id, text) {
    const s = $(id);
    s.textContent = text;
    s.hidden = !text;
  }

  // ---- words ------------------------------------------------------------------------------

  const STATE_LABEL = { signup: "Sign-ups open", running: "In progress", complete: "Complete" };
  const FORMAT_LABEL = { SingleElim: "single elimination", DoubleElim: "double elimination" };

  // Round names (docs/api.md, "Round names"): each list counts back from the side's last round.
  const ROUND_NAMES = {
    Winners: ["Winners final", "Winners semis", "Winners quarters"],
    Losers: ["Losers final", "Losers semis"],
    Single: ["Final", "Semis", "Quarters"],
  };

  function roundName(side, round, last, format) {
    if (side === "GrandFinal") return round === 1 ? "Grand final" : "Grand final reset";
    if (format === "SingleElim") return ROUND_NAMES.Single[last - round] || "Round " + round;
    const fromEnd = ROUND_NAMES[side][last - round];
    return fromEnd || (side === "Winners" ? "Winners round " : "Losers round ") + round;
  }

  // "1st", "2nd", "3rd", "4th", "11th", "21st".
  function ordinal(n) {
    const tens = n % 100;
    if (tens >= 11 && tens <= 13) return n + "th";
    switch (n % 10) {
      case 1: return n + "st";
      case 2: return n + "nd";
      case 3: return n + "rd";
      default: return n + "th";
    }
  }

  // ---- bracket reading, shared with the pages that list live sets -------------------------

  function slotEntrant(slot) {
    return slot && typeof slot === "object" && "Entrant" in slot ? slot.Entrant : null;
  }

  function doneWinner(state) {
    return state && typeof state === "object" && "Done" in state ? state.Done.winner : null;
  }

  function isHidden(set) {
    return set.state === "Skipped" || set.slots.some((s) => s === "Bye");
  }

  // The rounds of one side that have something to draw, in order, each with its name and
  // where it stands: "done" (every set decided), "now" (a set is ready to play) or "todo".
  function sideRounds(bracket, side) {
    const byRound = new Map();
    for (const s of bracket.sets) {
      if (s.side !== side) continue;
      if (!byRound.has(s.round)) byRound.set(s.round, []);
      byRound.get(s.round).push(s);
    }
    const nums = [...byRound.keys()].sort((a, b) => a - b);
    const last = nums.length ? nums[nums.length - 1] : 0;
    const out = [];
    for (const r of nums) {
      const sets = byRound.get(r).sort((a, b) => a.id - b.id);
      const shown = sets.filter((s) => !isHidden(s));
      if (!shown.length) continue;
      let status = "todo";
      if (shown.every((s) => doneWinner(s.state) !== null)) status = "done";
      else if (shown.some((s) => s.state === "Ready")) status = "now";
      const isFinal = side === "GrandFinal" || (bracket.format === "SingleElim" && r === last);
      out.push({ name: roundName(side, r, last, bracket.format), sets, status, star: isFinal });
    }
    return out;
  }

  const SIDES = ["Winners", "Losers", "GrandFinal"];

  // Every ready set of an event's view, in bracket order: its id, the two tags, the round, the
  // games won so far, and the whole set in words ("Mango vs Zain, Winners semis, 1–0") for a
  // screen reader; none when the event has no bracket yet.
  function liveSetLines(view) {
    if (!view.bracket) return [];
    const tags = view.tags || [];
    const tagOf = (n) => (typeof tags[n] === "string" ? tags[n] : "Entrant " + (n + 1));
    const scores = new Map((view.scores || []).map(([id, wins]) => [id, wins]));
    const lines = [];
    for (const side of SIDES) {
      for (const round of sideRounds(view.bracket, side)) {
        for (const set of round.sets) {
          if (set.state !== "Ready") continue;
          const [a, b] = set.slots.map(slotEntrant);
          if (a === null || b === null) continue;
          const score = scores.get(set.id) || [0, 0];
          const tagA = tagOf(a), tagB = tagOf(b);
          lines.push({
            id: set.id, a: tagA, b: tagB, round: round.name, score,
            words: tagA + " vs " + tagB + ", " + round.name + ", " + score[0] + "–" + score[1],
          });
        }
      }
    }
    return lines;
  }

  // One live set as a scoreboard row linking to its sticker on the bracket:
  // [P1] Mango  1 – 0  Zain [P2]   Winners semis
  function scoreboard(line, eventId) {
    const a = el("a", "board-row");
    a.href = eventHref(eventId) + "#set-" + line.id;
    a.setAttribute("aria-label", line.words);
    const face = el("span", "board-face");
    face.setAttribute("aria-hidden", "true");
    const lead = line.score[0] === line.score[1] ? -1 : line.score[0] > line.score[1] ? 0 : 1;
    const side = (i) => {
      const s = el("span", "board-side s" + i);
      s.append(el("span", "port p" + (i + 1), "P" + (i + 1)), el("span", "board-tag", i ? line.b : line.a));
      return s;
    };
    const nums = el("span", "board-score");
    nums.append(
      el("span", lead === 1 ? "trail" : "", line.score[0]),
      el("span", "dash", "–"),
      el("span", lead === 0 ? "trail" : "", line.score[1]),
    );
    face.append(side(0), nums, side(1), el("span", "chip round", line.round));
    a.append(face);
    return a;
  }

  // ---- small parts shared by the pages ------------------------------------------------------

  // A pill link; `cls` adds "sunny" for the one main action.
  function pill(text, href, cls) {
    const a = el("a", cls ? "pill " + cls : "pill", text);
    a.href = href;
    return a;
  }

  // A flat label on a sticker: a date, a count, a format.
  function chip(text, cls) {
    return el("span", cls ? "chip " + cls : "chip", text);
  }

  // Where an event stands, in the site's three words: live, sign-ups open, done.
  const STATUS_WORD = { running: "Live", signup: "Sign-ups open", complete: "Done" };
  function statusTag(state) {
    const t = el("span", "status " + (state || "unknown"), STATUS_WORD[state] || state);
    return t;
  }

  // An initial disc for a player, its face picked from the tag so it never changes.
  const DISC_FACES = 5;
  function avatar(tag, big) {
    const text = String(tag);
    let h = 0;
    for (const ch of text.toLowerCase()) h = (h * 31 + ch.codePointAt(0)) >>> 0;
    const d = el("span", "disc f" + (h % DISC_FACES) + (big ? " big" : ""), [...text][0] || "?");
    d.setAttribute("aria-hidden", "true");
    return d;
  }

  // "n entrants", or "1 entrant".
  function entrantsText(n) {
    return n + (n === 1 ? " entrant" : " entrants");
  }

  // The sign-up event that comes first: by start time (unannounced last), then by id.
  function nextUp(events) {
    const open = events.filter((e) => e.state === "signup");
    open.sort((a, b) => {
      const ta = knownTime(a.starts_at) ? a.starts_at : Infinity;
      const tb = knownTime(b.starts_at) ? b.starts_at : Infinity;
      return ta - tb || a.id - b.id;
    });
    return open[0] || null;
  }

  // ---- time, in the viewer's zone ---------------------------------------------------------

  const MINUTE = 60, HOUR = 3600, DAY = 86400;

  function plural(n, unit) {
    return n + " " + unit + (n === 1 ? "" : "s");
  }

  // A unix time as the full local date and time, a relative phrase, and the ISO form for
  // <time datetime>.
  function fmtWhen(unix) {
    const date = new Date(unix * 1000);
    const now = new Date();
    const diff = Math.round((date - now) / 1000);
    const sameDay = date.toDateString() === now.toDateString();
    let rel;
    if (Math.abs(diff) < MINUTE) {
      rel = "now";
    } else if (diff > 0) {
      if (diff >= DAY) rel = "in " + plural(Math.round(diff / DAY), "day");
      else if (sameDay) rel = "today at " + date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
      else if (diff >= HOUR) rel = "in " + plural(Math.round(diff / HOUR), "hour");
      else rel = "in " + plural(Math.round(diff / MINUTE), "minute");
    } else {
      const ago = -diff;
      if (ago >= DAY) rel = plural(Math.round(ago / DAY), "day") + " ago";
      else if (ago >= HOUR) rel = plural(Math.round(ago / HOUR), "hour") + " ago";
      else rel = plural(Math.round(ago / MINUTE), "minute") + " ago";
    }
    return {
      abs: date.toLocaleString(undefined, { dateStyle: "full", timeStyle: "short" }),
      rel,
      iso: date.toISOString(),
    };
  }

  function fmtDate(unix) {
    return new Date(unix * 1000).toLocaleDateString(undefined, { dateStyle: "medium" });
  }

  // A unix time the server knows: `starts_at` and `created_at` arrive as null or 0 when nobody
  // set them, and 0 is 1970, never a real time.
  function knownTime(t) {
    return typeof t === "number" && t > 0;
  }

  // The event's date: when it starts, else when it was created; null when neither is known.
  function eventDate(sum) {
    if (knownTime(sum.starts_at)) return fmtDate(sum.starts_at);
    if (knownTime(sum.created_at)) return fmtDate(sum.created_at);
    return null;
  }

  // ---- links ------------------------------------------------------------------------------

  function eventHref(id) {
    return "/events/" + encodeURIComponent(id);
  }

  function playerHref(tag) {
    return "/players/" + encodeURIComponent(tag);
  }

  // ---- polling: now, every `ms`, and when the tab comes back; one run in flight at a time --

  function poll(fn, ms) {
    let timer = null;
    let busy = false;
    let stopped = false;
    async function tick() {
      if (busy || stopped) return;
      busy = true;
      clearTimeout(timer);
      try {
        await fn();
      } catch (err) {
        console.error(err);
      } finally {
        busy = false;
        if (!stopped) timer = setTimeout(tick, ms);
      }
    }
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) tick();
    });
    tick();
    return {
      stop() {
        stopped = true;
        clearTimeout(timer);
      },
    };
  }

  // ---- the shell's pieces -----------------------------------------------------------------

  // Marks the nav link for this page ("page"), or for the section a page belongs to ("true":
  // a profile under Players); an event page has none.
  function markNav() {
    const here = location.pathname;
    for (const a of document.querySelectorAll(".nav a")) {
      const href = a.getAttribute("href");
      if (href === here) a.setAttribute("aria-current", "page");
      else if (href !== "/" && here.startsWith(href + "/")) a.setAttribute("aria-current", "true");
      else a.removeAttribute("aria-current");
    }
  }

  // How to enter `eventId`, on a sticker: plain words for players, and the two client commands
  // for testers in a collapsed block.
  function howToEnter(eventId) {
    const box = el("div", "howto");
    box.append(el("h2", null, "How to enter"));
    box.append(el("p", null, "Sign-ups are open. Joining from inside the game is coming soon."));
    const testers = el("details");
    testers.append(el("summary", null, "For testers: join from the command line"));
    testers.append(el("p", null, "The tester build is not public yet. Ask an organiser for it, then run:"));
    testers.append(el("code", null, "autobracket-client register --server " + location.origin + " --tag YOURTAG"));
    testers.append(el("code", null, "autobracket-client signup " + eventId));
    box.append(testers);
    return box;
  }

  return {
    el, icon, getJson, setNotice,
    STATE_LABEL, FORMAT_LABEL, ROUND_NAMES, roundName, ordinal,
    slotEntrant, doneWinner, isHidden, sideRounds, liveSetLines, scoreboard,
    pill, chip, statusTag, avatar, entrantsText, nextUp,
    fmtWhen, fmtDate, knownTime, eventDate, eventHref, playerHref,
    poll, markNav, howToEnter,
  };
})();
AB.markNav();
