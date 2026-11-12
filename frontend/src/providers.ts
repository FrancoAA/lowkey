/**
 * The form of an AI provider, in Settings → Models: Speech to text (it transcribes) and Meeting
 * notes (it writes the notes and answers questions): your own server's address, the model, the
 * API key, and where the data goes.
 *
 * The model is a short list of the provider's models (providers.rs, with a note on each), the
 * default first, and "Other model…" for any name the provider knows (its whole list as
 * suggestions). Your own server has no list: its model is typed.
 *
 * The key goes to the app once and is never shown again (the status says only that one is
 * saved). Listing the models uses the saved key, so it is also the check that the key works.
 */
import type { Provider, ProviderModel, Status } from "./api";
import type { Backend, Options } from "./backend";

export type Use = "speech" | "chat";

export const CUSTOM = "custom";

/** The value of "Other model…" in the model menu. */
const OTHER = "\u0000other";

/** Where the provider's key is saved: your own speech server has a key of its own. */
export function keySlot(id: string, use: Use): string {
  return id === CUSTOM && use === "speech" ? "custom-speech" : id;
}

export function provider(status: Status | null, id: string): Provider | undefined {
  return status?.providers?.list.find((p) => p.id === id);
}

export function hasKey(status: Status | null, id: string, use: Use): boolean {
  return status?.providers?.keys.includes(keySlot(id, use)) ?? false;
}

/** True on this computer, false elsewhere, null when it is not an address. */
export function isLocal(url: string): boolean | null {
  try {
    const host = new URL(url.trim()).hostname.replace(/^\[|\]$/g, "");
    return host === "localhost" || host.endsWith(".localhost") || host === "::1" || /^127\.\d+\.\d+\.\d+$/.test(host);
  } catch {
    return null;
  }
}

/** A note in a tinted box with an icon; "busy" is an info box with a spinner. */
export type AlertKind = "info" | "ok" | "error" | "warn" | "busy";

const ICONS: Record<Exclude<AlertKind, "busy">, string> = {
  info: '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/>',
  ok: '<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>',
  error: '<circle cx="12" cy="12" r="10"/><path d="M12 8v4M12 16h.01"/>',
  warn: '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4M12 17h.01"/>',
};

/** Shows the text in the alert (the text goes in as text, never as HTML); empty hides it. */
export function setAlert(el: HTMLElement, kind: AlertKind, text: string): void {
  el.hidden = !text;
  el.dataset.kind = kind === "busy" ? "info" : kind;
  el.innerHTML = kind === "busy" ? '<span class="spinner" aria-hidden="true"></span>' : `<svg class="alert-icon" viewBox="0 0 24 24" aria-hidden="true">${ICONS[kind]}</svg>`;
  const span = document.createElement("span");
  span.textContent = text;
  el.append(span);
}

export const hostOf = (url: string) => {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
};

/** Words for a failed test: a server that is not there says where nothing answers (not the
 * HTTP library's words). Other errors come as the app wrote them. */
export function failureText(err: unknown, url: string): string {
  const e = String(err instanceof Error ? err.message : err).replace(/^Error:\s*/, "");
  if (!/could not reach/i.test(e)) return e;
  let host = url;
  try {
    host = new URL(url).host;
  } catch {
    /* not an address: shown as it is */
  }
  return `Nothing answers at ${host}. Start the server, then test again.`;
}

export interface ProviderForm {
  /** Show the form for this provider; null hides it. */
  render(status: Status, p: Provider | null): void;
}

export function initProviderForm(backend: Backend, use: Use, root: HTMLElement, save: (o: Options) => void): ProviderForm {
  const id = `${use}-provider`;
  root.innerHTML = `
    <div class="field field-stacked" data-row="url">
      <label for="${id}-url" class="label">Server address</label>
      <input id="${id}-url" class="input" type="url" spellcheck="false" />
    </div>
    <div class="field field-stacked">
      <label for="${id}-choice" class="label" data-role="model-label">Model</label>
      <select id="${id}-choice" class="select select-wide"></select>
      <input id="${id}-model" class="input" type="text" list="${id}-models" spellcheck="false" autocomplete="off" />
      <datalist id="${id}-models"></datalist>
      <p id="${id}-model-note" class="alert" aria-live="polite" hidden></p>
    </div>
    <div class="field field-stacked">
      <label for="${id}-key" class="label" data-role="key-label">API key</label>
      <div class="key-line">
        <input id="${id}-key" class="input" type="password" autocomplete="off" spellcheck="false" />
        <button type="button" class="btn btn-outline btn-sm" data-role="key-clear" hidden>Remove the key</button>
      </div>
      <p class="alert" data-role="key-note" hidden></p>
    </div>
    <p class="alert" data-role="where" hidden></p>`;
  const el = <T extends HTMLElement>(sel: string) => root.querySelector<T>(sel)!;
  const urlRow = el<HTMLElement>('[data-row="url"]');
  const url = el<HTMLInputElement>(`#${id}-url`);
  const modelLabel = el<HTMLLabelElement>('[data-role="model-label"]');
  const choice = el<HTMLSelectElement>(`#${id}-choice`);
  const model = el<HTMLInputElement>(`#${id}-model`);
  const models = el<HTMLDataListElement>(`#${id}-models`);
  const modelNote = el<HTMLElement>(`#${id}-model-note`);
  const key = el<HTMLInputElement>(`#${id}-key`);
  const keyLabel = el<HTMLElement>('[data-role="key-label"]');
  const keyClear = el<HTMLButtonElement>('[data-role="key-clear"]');
  const keyNote = el<HTMLElement>('[data-role="key-note"]');
  const where = el<HTMLElement>('[data-role="where"]');

  let shown: Provider | null = null;
  /** The provider whose saved model and address are in the fields. */
  let filledFor = "";
  /** What the model list was asked for (provider, key, address): asked again when it changes. */
  let listedFor = "";
  /** The saved model of the provider shown ("" = its default). */
  let current = "";
  /** The models the provider has (its list), to hide an offered one it no longer has. */
  let available: Set<string> | null = null;
  /** "Other model…" is chosen and the name is not typed yet. */
  let otherOpen = false;

  const offered = (p: Provider): ProviderModel[] => (use === "speech" ? p.speech_models : p.chat_models);
  const fallbackOf = (p: Provider) => (use === "speech" ? p.speech_model : p.chat_model);
  const saveModel = (m: string) => save(use === "speech" ? { speech_model: m } : { ai_model: m });

  const saved = (s: Status) => (use === "speech" ? { provider: s.speech?.provider ?? "", model: s.speech?.model ?? "", url: s.speech?.url ?? "" } : { provider: s.ai?.provider ?? "", model: s.ai?.model ?? "", url: s.ai?.url ?? "" });

  function renderWhere(): void {
    const p = shown;
    if (!p) return;
    const sent = use === "speech" ? "your audio goes" : "the transcript, your notes and your questions go";
    if (p.id === CUSTOM) {
      const local = isLocal(url.value);
      if (local === null) {
        setAlert(where, "info", use === "speech" ? "Speaches: http://localhost:8000/v1 · LocalAI: http://localhost:8080/v1" : "Ollama: http://localhost:11434/v1 · LM Studio: http://localhost:1234/v1");
      } else {
        setAlert(where, local ? "info" : "warn", local ? "On this computer: nothing leaves it." : `Not on this computer: ${sent} to ${hostOf(url.value)}.`);
      }
      return;
    }
    const when = use === "speech" ? " each time you dictate, record a meeting or transcribe a file. The live preview still runs on this computer" : "";
    setAlert(where, "warn", `In the cloud: ${sent} to ${p.label} (${hostOf(p.url)})${when}. Their prices and privacy terms apply.`);
  }

  /** The menu: the offered models the provider has (and the chosen one), then "Other model…". */
  function renderChoices(): void {
    const p = shown;
    if (!p) return;
    const list = offered(p);
    const chosen = current || fallbackOf(p);
    const isOffered = list.some((m) => m.id === chosen);
    choice.hidden = list.length === 0;
    modelLabel.htmlFor = list.length ? choice.id : model.id;
    const options = list
      .filter((m) => !available || available.has(m.id) || m.id === chosen)
      .map((m) => new Option(m.note ? `${m.id} — ${m.note}` : m.id, m.id));
    options.push(new Option("Other model…", OTHER));
    choice.replaceChildren(...options);
    choice.value = otherOpen || !isOffered ? OTHER : chosen;
    model.hidden = list.length > 0 && choice.value !== OTHER;
    model.setAttribute("aria-label", list.length ? `Model name at ${p.label}` : "Model");
    model.placeholder = list.length ? `Any model name at ${p.label}` : use === "chat" ? "e.g. qwen3.5:4b" : fallbackOf(p) ? `${fallbackOf(p)} (the default)` : "e.g. whisper-1";
  }

  /** Says when the chosen model is no longer in the provider's list. */
  function checkChosen(p: Provider): boolean {
    const chosen = current || fallbackOf(p);
    if (!available || available.has(chosen) || !offered(p).some((m) => m.id === chosen)) return true;
    setAlert(modelNote, "error", `${p.label} no longer lists ${chosen}: choose another model.`);
    return false;
  }

  function listModels(status: Status, p: Provider): void {
    const has = hasKey(status, p.id, use);
    const sig = `${p.id}|${has}|${saved(status).url}`;
    if (sig === listedFor) return;
    listedFor = sig;
    available = null;
    renderChoices();
    models.replaceChildren(...offered(p).map((m) => new Option(m.id)));
    if (!has && p.id !== CUSTOM) {
      setAlert(modelNote, "info", "Add the key: then the app checks it and lists the models.");
      return;
    }
    setAlert(modelNote, "busy", p.id === CUSTOM ? "Asking the server for its models…" : "Checking the key…");
    backend.providerModels(p.id, use === "speech").then(
      (list) => {
        if (listedFor !== sig) return; // another provider meanwhile
        models.replaceChildren(...list.map((m) => new Option(m)));
        available = list.length ? new Set(list) : null;
        renderChoices();
        const n = `${list.length} model${list.length === 1 ? "" : "s"}`;
        const all = offered(p).length ? `${p.label} has ${n}: “Other model…” lists them.` : `${n} to choose from.`;
        setAlert(modelNote, "ok", p.id === CUSTOM ? `The server answers · ${n} to choose from.` : p.public_models ? `${all} The key is checked when it is first used.` : `The key works · ${all}`);
        checkChosen(p);
      },
      (err) => {
        if (listedFor !== sig) return;
        setAlert(modelNote, "error", failureText(err, p.id === CUSTOM ? saved(status).url : p.url));
        listedFor = ""; // the next status asks again (the server may be up by then)
      },
    );
  }

  url.addEventListener("input", renderWhere);
  url.addEventListener("change", () => {
    listedFor = "";
    save(use === "speech" ? { speech_url: url.value } : { ai_url: url.value });
  });
  choice.addEventListener("change", () => {
    const p = shown;
    if (!p) return;
    if (choice.value === OTHER) {
      otherOpen = true;
      if (offered(p).some((m) => m.id === (current || fallbackOf(p)))) model.value = "";
      renderChoices();
      model.focus();
      return;
    }
    otherOpen = false;
    current = choice.value === fallbackOf(p) ? "" : choice.value;
    renderChoices();
    saveModel(current); // the default is saved as "": it follows a new default
  });
  model.addEventListener("change", () => {
    const p = shown;
    if (!p) return;
    current = model.value.trim();
    // A typed name that is in the menu shows there; empty is the default.
    if (!current || offered(p).some((m) => m.id === current)) otherOpen = false;
    renderChoices();
    saveModel(current === fallbackOf(p) ? "" : current);
  });
  key.addEventListener("change", () => {
    if (!shown || !key.value.trim()) return;
    save({ api_key: { slot: keySlot(shown.id, use), key: key.value } });
    key.value = ""; // never kept in the page
  });
  keyClear.addEventListener("click", () => {
    if (shown) save({ api_key: { slot: keySlot(shown.id, use), key: "" } });
  });

  return {
    render(status, p) {
      root.hidden = !p;
      shown = p;
      if (!p) {
        filledFor = "";
        return;
      }
      const s = saved(status);
      if (filledFor !== p.id) otherOpen = false;
      if (filledFor !== p.id || (document.activeElement !== model && s.provider === p.id && current !== s.model)) {
        current = s.provider === p.id ? s.model : "";
        model.value = current;
        if (document.activeElement !== url) url.value = s.url;
        filledFor = p.id;
      }
      urlRow.hidden = p.id !== CUSTOM;
      renderChoices();
      const has = hasKey(status, p.id, use);
      keyLabel.textContent = p.id === CUSTOM ? "API key (only if the server asks for one)" : `${p.label} API key`;
      key.placeholder = has ? "A key is saved (type to replace it)" : p.id === CUSTOM ? "None" : "Paste the key here";
      keyClear.hidden = !has;
      setAlert(keyNote, "info", p.keys_url ? `Get one at ${p.keys_url.replace(/^https:\/\//, "")}. It is kept in the settings file on this computer, readable by you only.` : "");
      renderWhere();
      listModels(status, p);
    },
  };
}
