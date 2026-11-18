/**
 * Ask: questions across all meetings (desktop/src-tauri/src/library.rs). The app finds the parts
 * of the meetings that fit the question (its words, and the dates and people it names), and the
 * notes model answers from them only, with a number in brackets after each fact: [2]. Here each
 * number becomes a link to that moment of that meeting, and the sources are listed under the
 * answer. The conversation lives in this window until "New conversation".
 */
import type { Backend, LibraryAnswerEvent, LibrarySource } from "./backend";
import { renderMarkdown } from "./markdown";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

interface Turn {
  question: string;
  answer: string;
  sources: LibrarySource[];
  state: string;
  detail: string;
}

export interface AskUi {
  /** Open a meeting at a moment of its transcript (seconds), or at its summary (null). */
  openMeeting(id: string, at: number | null): void;
}

function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const pad = (n: number) => String(n).padStart(2, "0");
  return h ? `${h}:${pad(m)}:${pad(s % 60)}` : `${pad(m)}:${pad(s % 60)}`;
}

const dateFmt = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric" });

/** "[2]" and "[1, 3]" in the answer's text become links to their sources. */
export function linkCitations(root: HTMLElement, sources: LibrarySource[], open: (s: LibrarySource) => void): void {
  const byN = new Map(sources.map((s) => [s.n, s]));
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const texts: Text[] = [];
  for (let n = walker.nextNode(); n; n = walker.nextNode()) texts.push(n as Text);
  const CITE = /\[(\d{1,3}(?:\s*,\s*\d{1,3})*)\]/g;
  for (const t of texts) {
    const value = t.nodeValue ?? "";
    if (!CITE.test(value)) continue;
    CITE.lastIndex = 0;
    const frag = document.createDocumentFragment();
    let last = 0;
    for (const m of value.matchAll(CITE)) {
      frag.append(value.slice(last, m.index));
      for (const num of m[1].split(",").map((x) => Number(x.trim()))) {
        const s = byN.get(num);
        if (!s) continue; // a number the model made up: left out
        const a = document.createElement("button");
        a.type = "button";
        a.className = "cite";
        a.textContent = String(num);
        a.title = `${s.title} · ${s.at === null ? "summary" : clock(s.at)}`;
        a.setAttribute("aria-label", `Source ${num}: ${a.title}`);
        a.addEventListener("click", () => open(s));
        frag.append(a);
      }
      last = (m.index ?? 0) + m[0].length;
    }
    frag.append(value.slice(last));
    t.replaceWith(frag);
  }
}

export function initAsk(backend: Backend, ui: AskUi): { shown(): void } {
  const log = $<HTMLDivElement>("ask-log");
  const form = $<HTMLFormElement>("ask-form");
  const input = $<HTMLTextAreaElement>("ask-input");
  const send = $<HTMLButtonElement>("ask-send");
  const starters = $<HTMLDivElement>("ask-starters");
  const newBtn = $<HTMLButtonElement>("ask-new");
  const turns: Turn[] = [];
  let working = false;

  const el = (tag: string, cls: string, text = "") => {
    const x = document.createElement(tag);
    x.className = cls;
    x.textContent = text;
    return x;
  };

  function sourcesList(t: Turn): HTMLElement {
    const box = el("div", "ask-sources");
    const cited = new Set([...t.answer.matchAll(/\[(\d{1,3}(?:\s*,\s*\d{1,3})*)\]/g)].flatMap((m) => m[1].split(",").map((x) => Number(x.trim()))));
    for (const s of t.sources.filter((x) => cited.has(x.n))) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "ask-source";
      const n = el("span", "ask-source-n", String(s.n));
      const head = el("span", "ask-source-head");
      head.append(el("span", "ask-source-title", s.title), el("span", "ask-source-when", `${dateFmt.format(new Date(s.started_at * 1000))} · ${s.at === null ? "Summary" : clock(s.at)}`));
      const text = el("span", "ask-source-text", s.text.length > 220 ? `${s.text.slice(0, 220).trimEnd()}…` : s.text);
      b.append(n, head, text);
      b.addEventListener("click", () => ui.openMeeting(s.meeting_id, s.at));
      box.append(b);
    }
    return box;
  }

  function render(): void {
    const nodes: HTMLElement[] = [];
    for (const [i, t] of turns.entries()) {
      nodes.push(el("div", "chat-q", t.question));
      const a = el("div", "ask-a");
      const last = i === turns.length - 1;
      if (t.answer) {
        const body = el("div", "ai-body");
        renderMarkdown(body, t.answer);
        linkCitations(body, t.sources, (s) => ui.openMeeting(s.meeting_id, s.at));
        body.classList.toggle("streaming", last && working);
        a.append(body);
        if (!(last && working)) a.append(sourcesList(t));
      } else if (t.state === "error") {
        a.append(el("p", "chat-error", t.detail));
      } else if (last && working) {
        const wait = el("p", "chat-wait");
        const dots = el("span", "typing");
        dots.setAttribute("aria-hidden", "true");
        dots.append(document.createElement("i"), document.createElement("i"), document.createElement("i"));
        wait.append(dots, t.detail || "Looking through your meetings…");
        a.append(wait);
      } else if (t.state === "cancelled") a.append(el("p", "chat-wait", "Stopped."));
      if (t.answer && t.state === "error") a.append(el("p", "chat-error", t.detail));
      nodes.push(a);
    }
    log.replaceChildren(...nodes);
    log.hidden = !turns.length;
    starters.hidden = turns.length > 0;
    newBtn.hidden = !turns.length || working;
    send.toggleAttribute("data-stop", working);
    send.setAttribute("aria-label", working ? "Stop the answer" : "Ask");
    send.title = working ? "Stop the answer" : "Ask (Enter)";
    send.replaceChildren(icon(working));
    send.disabled = !working && !input.value.trim();
    for (const b of starters.querySelectorAll("button")) b.disabled = working;
  }

  function icon(stop: boolean): SVGSVGElement {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.classList.add("icon");
    if (stop) {
      const r = document.createElementNS("http://www.w3.org/2000/svg", "rect");
      for (const [k, v] of Object.entries({ x: "7", y: "7", width: "10", height: "10", rx: "1.5", fill: "currentColor" })) r.setAttribute(k, v);
      svg.append(r);
    } else {
      const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
      p.setAttribute("d", "M12 19V5M5 12l7-7 7 7");
      svg.append(p);
    }
    return svg;
  }

  async function ask(text: string): Promise<void> {
    const q = text.trim();
    if (!q || working) return;
    const history = turns.filter((t) => t.answer && t.state === "done").flatMap((t) => [
      { role: "user" as const, text: t.question },
      { role: "assistant" as const, text: t.answer },
    ]);
    turns.push({ question: q, answer: "", sources: [], state: "preparing", detail: "" });
    working = true;
    input.value = "";
    render();
    log.lastElementChild?.scrollIntoView({ block: "end" });
    try {
      await backend.library.ask(q, history);
    } catch (err) {
      const t = turns[turns.length - 1];
      t.state = "error";
      t.detail = String(err);
      working = false;
      if (!input.value) input.value = q;
      render();
    }
  }

  backend.library.onAnswer((e: LibraryAnswerEvent) => {
    const t = turns[turns.length - 1];
    if (!t || !working) return;
    t.state = e.state;
    t.detail = e.detail;
    if (e.text) t.answer = e.text;
    if (e.sources?.length) t.sources = e.sources;
    if (["done", "error", "cancelled"].includes(e.state)) working = false;
    const nearEnd = log.scrollHeight - log.scrollTop - log.clientHeight < 120;
    render();
    if (nearEnd) log.lastElementChild?.scrollIntoView({ block: "end" });
  });

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (working) void backend.meetings.aiCancel();
    else void ask(input.value);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      if (!working) form.requestSubmit();
    }
  });
  input.addEventListener("input", () => {
    send.disabled = !working && !input.value.trim();
  });
  for (const b of starters.querySelectorAll<HTMLButtonElement>("button[data-ask]")) b.addEventListener("click", () => void ask(b.dataset.ask ?? ""));
  newBtn.addEventListener("click", () => {
    turns.length = 0;
    render();
    input.focus();
  });
  render();

  return {
    shown() {
      if (!working) input.focus();
    },
  };
}
