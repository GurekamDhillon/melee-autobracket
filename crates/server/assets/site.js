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

  // Marks the nav link for this page; an event page has none.
  function markNav() {
    for (const a of document.querySelectorAll(".nav a")) {
      if (a.getAttribute("href") === location.pathname) a.setAttribute("aria-current", "page");
      else a.removeAttribute("aria-current");
    }
  }

  // The two client commands that enter `eventId` on this server, on a sticker.
  function howToEnter(eventId) {
    const box = el("div", "howto");
    box.append(el("h2", null, "How to enter"));
    box.append(el("code", null, "autobracket-client register --server " + location.origin + " --tag YOURTAG"));
    box.append(el("code", null, "autobracket-client signup " + eventId));
    return box;
  }

  return {
    el, icon, getJson, setNotice,
    STATE_LABEL, FORMAT_LABEL, ROUND_NAMES, roundName, ordinal,
    slotEntrant, doneWinner, isHidden, sideRounds,
    fmtWhen, fmtDate, eventHref, playerHref,
    poll, markNav, howToEnter,
  };
})();
AB.markNav();
