/**
 * Settings → Speech → Vocabulary: one row per term (term | sounds like | remove). Saved while
 * you type (the app cleans the list: blank rows and duplicates go).
 */
import type { VocabTerm } from "./api";
import type { Backend } from "./backend";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export function initVocabulary(backend: Backend, initial: VocabTerm[], onError: (message: string) => void): void {
  const rows = $<HTMLDivElement>("vocab-rows");
  const add = $<HTMLButtonElement>("vocab-add");
  const note = $<HTMLElement>("vocab-note");
  let terms: VocabTerm[] = initial.map((t) => ({ term: t.term, sounds_like: [...t.sounds_like] }));
  let timer = 0;

  const split = (s: string) =>
    s
      .split(",")
      .map((x) => x.trim())
      .filter(Boolean);

  function count(): void {
    const n = terms.filter((t) => t.term.trim()).length;
    note.textContent = n === 0 ? "No terms yet." : n === 1 ? "1 term." : `${n} terms.`;
  }

  function save(): void {
    clearTimeout(timer);
    timer = window.setTimeout(() => {
      const clean = terms.filter((t) => t.term.trim()).map((t) => ({ term: t.term.trim(), sounds_like: t.sounds_like }));
      backend.saveOptions({ vocabulary: clean }).catch((err) => onError(`The vocabulary was not saved: ${String(err)}`));
    }, 600);
    count();
  }

  function row(t: VocabTerm): HTMLElement {
    const r = document.createElement("div");
    r.className = "vocab-row";
    r.setAttribute("role", "row");
    const term = document.createElement("input");
    term.className = "input";
    term.value = t.term;
    term.placeholder = "Lisandro";
    term.maxLength = 64;
    term.spellcheck = false;
    term.setAttribute("aria-label", "Term");
    term.addEventListener("input", () => {
      t.term = term.value;
      save();
    });
    const sounds = document.createElement("input");
    sounds.className = "input";
    sounds.value = t.sounds_like.join(", ");
    sounds.placeholder = "Lisando, Lee Sandro";
    sounds.spellcheck = false;
    sounds.setAttribute("aria-label", `Sounds like, for ${t.term || "this term"}`);
    sounds.addEventListener("input", () => {
      t.sounds_like = split(sounds.value);
      save();
    });
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "btn btn-ghost btn-sm btn-icon";
    remove.setAttribute("aria-label", `Remove ${t.term || "this term"}`);
    remove.innerHTML = '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M18 6 6 18M6 6l12 12" /></svg>';
    remove.addEventListener("click", () => {
      terms = terms.filter((x) => x !== t);
      r.remove();
      save();
      (rows.querySelector<HTMLInputElement>(".vocab-row:last-child input") ?? add).focus();
    });
    for (const el of [term, sounds]) {
      el.setAttribute("role", "cell");
      // Enter in the last row adds a new one, like a list editor.
      el.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          if (r === rows.lastElementChild) add.click();
          else (r.nextElementSibling?.querySelector("input") as HTMLInputElement | null)?.focus();
        }
      });
    }
    r.append(term, sounds, remove);
    return r;
  }

  rows.replaceChildren(...terms.map(row));
  count();
  add.addEventListener("click", () => {
    const t: VocabTerm = { term: "", sounds_like: [] };
    terms.push(t);
    const r = row(t);
    rows.append(r);
    r.querySelector("input")!.focus();
    count();
  });
}
