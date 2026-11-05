/**
 * History section: finished dictations (desktop/src-tauri/src/history.rs), grouped by day,
 * searchable (the words of the dictations' text, marked in the results), each with Copy and
 * Delete; how long they stay; Clear all (after a confirmation).
 */
import type { HistoryEntry } from "./api";
import { confirmDialog } from "./confirm";
import { highlighted, searchWords } from "./highlight";
import type { Backend } from "./backend";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export interface HistoryUi {
  announce(message: string): void;
  onError(message: string): void;
  /** The paste-last key ("SUPER + ALT + V"; "" = none), for the hint. */
  pasteKey(): string;
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });

function dayLabel(unix: number): string {
  const d = new Date(unix * 1000);
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(new Date()) - day(d)) / 86_400_000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  const sameYear = d.getFullYear() === new Date().getFullYear();
  return new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", ...(sameYear ? {} : { year: "numeric" }) }).format(d);
}

function icon(path: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.classList.add("icon");
  const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
  p.setAttribute("d", path);
  svg.append(p);
  return svg;
}

export function initHistory(backend: Backend, keep: string, ui: HistoryUi): { refresh(): void } {
  const api = backend.history;
  const list = $<HTMLDivElement>("history-list");
  const empty = $<HTMLElement>("history-empty");
  const emptyTitle = $<HTMLElement>("history-empty-title");
  const emptyText = $<HTMLElement>("history-empty-text");
  const search = $<HTMLInputElement>("history-search");
  const keepSelect = $<HTMLSelectElement>("history-keep");
  const clearBtn = $<HTMLButtonElement>("history-clear");
  const hint = $<HTMLElement>("history-paste-hint");
  keepSelect.value = keep;
  let searchTimer = 0;

  function row(e: HistoryEntry, words: string[]): HTMLElement {
    const r = document.createElement("div");
    r.className = "history-item";
    r.setAttribute("role", "listitem");
    const time = document.createElement("span");
    time.className = "history-time";
    time.textContent = timeFmt.format(new Date(e.at * 1000));
    const text = document.createElement("p");
    text.className = "history-text";
    text.append(...highlighted(e.text, words));
    const meta = document.createElement("span");
    meta.className = "history-meta";
    meta.textContent = `${e.source === "key" ? "Dictation key" : "In this window"} · ${Math.max(1, Math.round(e.seconds))} s`;
    const actions = document.createElement("div");
    actions.className = "history-actions";
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "btn btn-ghost btn-sm";
    copy.append(icon("M8 8h12v12H8zM4 16V4h12"), "Copy");
    copy.addEventListener("click", async () => {
      try {
        await backend.copy(e.text);
        copy.lastChild!.textContent = "Copied";
        setTimeout(() => (copy.lastChild!.textContent = "Copy"), 1500);
      } catch (err) {
        ui.onError(String(err));
      }
    });
    const del = document.createElement("button");
    del.type = "button";
    del.className = "btn btn-ghost btn-sm btn-icon danger";
    del.setAttribute("aria-label", "Delete this dictation");
    del.append(icon("M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14"));
    del.addEventListener("click", async () => {
      await api.remove(e.id);
      r.remove();
      ui.announce("Dictation deleted.");
      void refresh();
    });
    actions.append(copy, del);
    r.append(time, text, meta, actions);
    return r;
  }

  async function refresh(): Promise<void> {
    const words = searchWords(search.value);
    let all: HistoryEntry[] = [];
    try {
      all = await api.list(search.value);
    } catch (err) {
      ui.onError(String(err));
    }
    const groups: HTMLElement[] = [];
    let current = "";
    let box: HTMLElement | null = null;
    for (const e of all) {
      const label = dayLabel(e.at);
      if (label !== current || !box) {
        current = label;
        const g = document.createElement("section");
        g.className = "meeting-day";
        const h = document.createElement("h3");
        h.textContent = label;
        box = document.createElement("div");
        box.className = "meeting-day-items";
        g.append(h, box);
        groups.push(g);
      }
      box.append(row(e, words));
    }
    list.replaceChildren(...groups);
    empty.hidden = all.length > 0;
    clearBtn.hidden = !all.length || !!search.value;
    if (search.value) {
      emptyTitle.textContent = "Nothing found";
      emptyText.textContent = "No dictation has all of these words in its text.";
    } else if (keepSelect.value === "off") {
      emptyTitle.textContent = "The history is off";
      emptyText.textContent = "Dictations are not kept. Choose how long to keep them above.";
    } else {
      emptyTitle.textContent = "No dictations yet";
      emptyText.textContent = "Dictate with the dictation key, or here in Transcribe: each one shows up here.";
    }
    const key = ui.pasteKey();
    hint.textContent = key ? `${key.split("+").map((p) => p.trim()).map((p) => (p.length > 1 ? p[0] + p.slice(1).toLowerCase() : p)).join("+")} types the last one again in any app.` : "";
  }

  search.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => void refresh(), 200);
  });
  keepSelect.addEventListener("change", () => {
    void backend.saveOptions({ history_keep: keepSelect.value }).then(refresh, (err) => ui.onError(String(err)));
  });
  clearBtn.addEventListener("click", async () => {
    const yes = await confirmDialog({
      title: "Clear the history?",
      body: "Every dictation in the history is deleted from this computer. You cannot undo this.",
      action: "Clear all",
      danger: true,
    });
    if (!yes) return;
    await api.clear();
    ui.announce("History cleared.");
    void refresh();
  });
  // A new dictation: rebuilt now only when the section is on screen (opening it refreshes).
  api.onAdded(() => {
    if (list.offsetParent) void refresh();
  });
  void refresh();
  return { refresh: () => void refresh() };
}
