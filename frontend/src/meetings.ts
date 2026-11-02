/**
 * Meetings section: record the microphone ("Me") and the computer's sound ("Them"), see the
 * transcript as it comes, and write notes next to it. The app records, transcribes and saves
 * (desktop/src-tauri/src/meeting.rs); this page shows it and sends the notes.
 */
import type { Meeting, MeetingLine, MeetingLive, MeetingSummary } from "./api";
import type { Backend } from "./backend";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export interface MeetingsUi {
  announce(message: string): void;
  /** The nav item shows a red dot while a meeting records. */
  onRecording(recording: boolean): void;
}

/** Lines of one speaker with short gaps form one paragraph (as in the Markdown export). */
export function paragraphs(lines: MeetingLine[]): MeetingLine[] {
  const out: MeetingLine[] = [];
  for (const l of lines) {
    const p = out[out.length - 1];
    if (p && p.speaker === l.speaker && l.start - p.end < 2 && p.text.length < 600) {
      p.text += ` ${l.text}`;
      p.end = Math.max(p.end, l.end);
    } else out.push({ ...l });
  }
  return out;
}

function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const mm = String(Math.floor(s / 60) % 60).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return s >= 3600 ? `${Math.floor(s / 3600)}:${mm}:${ss}` : `${mm}:${ss}`;
}

function minutes(seconds: number): string {
  const m = Math.round(seconds / 60);
  return m < 1 ? "under 1 min" : m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")} min`;
}

const dateFmt = new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const when = (unix: number) => dateFmt.format(new Date(unix * 1000));

function defaultTitle(): string {
  return `Meeting · ${new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(new Date())}`;
}

/** A file name from the title: letters, digits and dashes. */
function fileName(title: string): string {
  const base = title
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/[^A-Za-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .toLowerCase();
  return `${base || "meeting"}.md`;
}

export function initMeetings(backend: Backend, ui: MeetingsUi): void {
  const api = backend.meetings;
  const home = $<HTMLDivElement>("meetings-home");
  const startBtn = $<HTMLButtonElement>("meeting-start");
  const errorBox = $<HTMLDivElement>("meetings-error");
  const list = $<HTMLUListElement>("meetings-list");
  const empty = $<HTMLElement>("meetings-empty");
  const view = $<HTMLDivElement>("meeting-view");
  const back = $<HTMLButtonElement>("meeting-back");
  const title = $<HTMLInputElement>("meeting-title");
  const meta = $<HTMLElement>("meeting-meta");
  const stopBtn = $<HTMLButtonElement>("meeting-stop");
  const copyBtn = $<HTMLButtonElement>("meeting-copy");
  const exportBtn = $<HTMLButtonElement>("meeting-export");
  const deleteBtn = $<HTMLButtonElement>("meeting-delete");
  const meters = $<HTMLDivElement>("meeting-meters");
  const meterMe = $<HTMLDivElement>("meter-me");
  const meterThem = $<HTMLDivElement>("meter-them");
  const notices = $<HTMLUListElement>("meeting-notices");
  const notes = $<HTMLTextAreaElement>("meeting-notes");
  const transcript = $<HTMLDivElement>("meeting-transcript");
  const pending = $<HTMLElement>("meeting-pending");

  /** The meeting on screen (live or saved), and the live one (if any). */
  let shown: Meeting | null = null;
  let live: MeetingLive | null = null;
  let liveAt = 0; // performance.now() when `live` arrived (for the timer)
  let saveTimer = 0;
  let unsaved: { title?: string; notes?: string } = {};
  let deleteArmed = 0;
  /** The Stop button is waiting for the app (it opens the saved meeting itself). */
  let stopping = false;

  const isLive = () => !!live && !!shown && live.meeting.id === shown.id && live.state !== "done";

  function showError(message: string | null): void {
    errorBox.hidden = !message;
    errorBox.textContent = message ?? "";
  }

  // ---------------------------------------------------------------- saving what you type
  async function flush(): Promise<void> {
    clearTimeout(saveTimer);
    if (!shown || (unsaved.title === undefined && unsaved.notes === undefined)) return;
    const patch = unsaved;
    unsaved = {};
    try {
      await api.update(shown.id, patch);
    } catch (err) {
      unsaved = { ...patch, ...unsaved }; // kept: the next change or Back tries again
      showError(`Your notes were not saved: ${String(err)}`);
    }
  }

  function later(patch: { title?: string; notes?: string }): void {
    unsaved = { ...unsaved, ...patch };
    clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => void flush(), 600);
  }

  notes.addEventListener("input", () => {
    if (shown) shown.notes = notes.value;
    later({ notes: notes.value });
  });
  title.addEventListener("input", () => {
    if (shown) shown.title = title.value;
    later({ title: title.value });
  });
  notes.addEventListener("blur", () => void flush());
  title.addEventListener("blur", () => void flush());
  title.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      notes.focus();
    }
  });

  // ---------------------------------------------------------------- one meeting
  function renderTranscript(m: Meeting, follow: boolean): void {
    const nearEnd = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 60;
    transcript.replaceChildren();
    const paras = paragraphs(m.lines);
    if (!paras.length) {
      const p = document.createElement("p");
      p.className = "transcript-empty";
      p.textContent = isLive() ? "The text comes here a few seconds after someone speaks." : "No speech was found.";
      transcript.append(p);
    }
    for (const l of paras) {
      const row = document.createElement("div");
      row.className = "tline";
      row.dataset.speaker = l.speaker;
      const who = document.createElement("span");
      who.className = "who";
      who.textContent = l.speaker === "me" ? "Me" : "Them";
      const at = document.createElement("span");
      at.className = "when";
      at.textContent = clock(l.start);
      const text = document.createElement("p");
      text.textContent = l.text;
      row.append(who, at, text);
      transcript.append(row);
    }
    if (follow && nearEnd) transcript.scrollTop = transcript.scrollHeight;
  }

  function renderMeta(): void {
    if (!shown) return;
    if (isLive() && live) {
      const elapsed = live.state === "recording" ? live.elapsed + (performance.now() - liveAt) / 1000 : live.elapsed;
      const sources = live.meeting.system_audio ? "microphone and computer sound" : "microphone only";
      meta.dataset.live = live.state;
      meta.textContent = live.state === "finishing" ? `Finishing… ${clock(elapsed)} recorded` : `Recording ${clock(elapsed)} · ${sources}`;
    } else {
      delete meta.dataset.live;
      const interrupted = shown.state === "recording" ? " · interrupted" : "";
      meta.textContent = `${when(shown.started_at)} · ${minutes(shown.duration)}${interrupted}`;
    }
  }

  function renderView(): void {
    if (!shown) return;
    const recording = isLive();
    stopBtn.hidden = !recording;
    stopBtn.disabled = live?.state === "finishing";
    stopBtn.textContent = live?.state === "finishing" ? "Finishing…" : "Stop";
    copyBtn.hidden = exportBtn.hidden = deleteBtn.hidden = recording;
    meters.hidden = !recording || live?.state !== "recording";
    const items = recording && live ? live.notices : [];
    notices.replaceChildren(
      ...items.map((n) => {
        const li = document.createElement("li");
        li.textContent = n;
        return li;
      }),
    );
    notices.hidden = !items.length;
    const waiting = recording && live ? live.pending : 0;
    pending.textContent = waiting ? `· ${waiting} part${waiting > 1 ? "s" : ""} to transcribe` : "";
    renderMeta();
    renderTranscript(recording && live ? live.meeting : shown, recording);
  }

  function open(m: Meeting): void {
    shown = m;
    title.value = m.title;
    notes.value = m.notes;
    unsaved = {};
    disarmDelete();
    home.hidden = true;
    view.hidden = false;
    renderView();
  }

  async function openById(id: string): Promise<void> {
    try {
      open(await api.get(id)); // the app's copy: it has the notes saved meanwhile
    } catch (err) {
      showError(String(err));
    }
  }

  async function showHome(): Promise<void> {
    await flush();
    shown = null;
    view.hidden = true;
    home.hidden = false;
    await renderList();
  }

  // ---------------------------------------------------------------- the list
  function item(s: MeetingSummary, recording: boolean): HTMLLIElement {
    const li = document.createElement("li");
    const b = document.createElement("button");
    b.type = "button";
    b.className = "meeting-item";
    const head = document.createElement("span");
    head.className = "meeting-item-title";
    head.textContent = s.title;
    const sub = document.createElement("span");
    sub.className = "meeting-item-meta";
    sub.textContent = recording ? "Recording now" : `${when(s.started_at)} · ${minutes(s.duration)}${s.state === "recording" ? " · interrupted" : ""}`;
    if (recording) sub.dataset.live = "";
    b.append(head, sub);
    if (s.excerpt) {
      const ex = document.createElement("span");
      ex.className = "meeting-item-excerpt";
      ex.textContent = s.excerpt;
      b.append(ex);
    }
    b.addEventListener("click", () => void openById(s.id));
    li.append(b);
    return li;
  }

  async function renderList(): Promise<void> {
    let all: MeetingSummary[] = [];
    try {
      all = await api.list();
    } catch (err) {
      showError(String(err));
    }
    const liveId = live && live.state !== "done" ? live.meeting.id : null;
    list.replaceChildren(...all.map((s) => item(s, s.id === liveId)));
    empty.hidden = all.length > 0;
    startBtn.disabled = !!liveId;
  }

  // ---------------------------------------------------------------- start / stop
  startBtn.addEventListener("click", async () => {
    showError(null);
    startBtn.disabled = true;
    try {
      live = await api.start(defaultTitle());
      liveAt = performance.now();
      ui.onRecording(true);
      ui.announce("Recording. Write your notes while you listen.");
      open({ ...live.meeting });
      notes.focus();
    } catch (err) {
      showError(String(err));
      startBtn.disabled = false;
    }
  });

  stopBtn.addEventListener("click", async () => {
    stopping = true;
    stopBtn.disabled = true;
    stopBtn.textContent = "Finishing…";
    await flush();
    try {
      const saved = await api.stop();
      live = null;
      ui.onRecording(false);
      ui.announce("Meeting saved.");
      if (!shown || shown.id === saved.id) {
        // Only what you typed while it finished (not saved yet) goes over the saved meeting.
        const typed = unsaved;
        open(saved);
        if (typed.title !== undefined) title.value = typed.title;
        if (typed.notes !== undefined) notes.value = typed.notes;
        if (typed.title !== undefined || typed.notes !== undefined) later(typed);
      }
      void renderList();
    } catch (err) {
      showError(String(err));
      // The app freed the recording (and saved what it had): back to a usable list.
      live = null;
      ui.onRecording(false);
      await showHome();
    } finally {
      stopping = false;
    }
  });

  // Also for a meeting started or stopped with the meeting key, in another app.
  api.onLive((l) => {
    if (l.state === "done") {
      live = null;
      ui.onRecording(false);
      if (stopping) return; // the Stop button's answer opens the saved meeting
      if (shown?.id === l.meeting.id) {
        void (async () => {
          await flush();
          try {
            open(await api.get(l.meeting.id));
          } catch (err) {
            showError(String(err));
          }
        })();
      }
      if (!home.hidden) void renderList();
      return;
    }
    const isNew = live?.meeting.id !== l.meeting.id;
    live = l;
    liveAt = performance.now();
    ui.onRecording(true);
    if (shown?.id === l.meeting.id) renderView();
    else if (isNew && !home.hidden) void renderList();
  });

  api.onLevels((lv) => {
    // A square root, so quiet speech still shows.
    meterMe.style.width = `${Math.round(Math.min(1, Math.sqrt(lv.me)) * 100)}%`;
    meterThem.style.width = `${Math.round(Math.min(1, Math.sqrt(lv.them)) * 100)}%`;
  });

  window.setInterval(() => {
    if (isLive() && live?.state === "recording") renderMeta();
  }, 1000);

  // ---------------------------------------------------------------- saved meeting actions
  back.addEventListener("click", () => void showHome());

  copyBtn.addEventListener("click", async () => {
    if (!shown) return;
    await flush();
    try {
      await backend.copy(await api.markdown(shown.id));
      copyBtn.textContent = "Copied";
      setTimeout(() => (copyBtn.textContent = "Copy"), 1500);
    } catch (err) {
      showError(String(err));
    }
  });

  exportBtn.addEventListener("click", async () => {
    if (!shown) return;
    await flush();
    try {
      await backend.save(fileName(shown.title), await api.markdown(shown.id), "text/markdown");
    } catch (err) {
      showError(String(err));
    }
  });

  function disarmDelete(): void {
    clearTimeout(deleteArmed);
    deleteArmed = 0;
    deleteBtn.textContent = "Delete";
    delete deleteBtn.dataset.armed;
  }

  // Two clicks: the first asks, the second deletes (within 4 s).
  deleteBtn.addEventListener("click", async () => {
    if (!shown) return;
    if (!deleteArmed) {
      deleteBtn.textContent = "Click again to delete";
      deleteBtn.dataset.armed = "";
      deleteArmed = window.setTimeout(disarmDelete, 4000);
      return;
    }
    disarmDelete();
    try {
      unsaved = {};
      await api.remove(shown.id);
      ui.announce("Meeting deleted.");
      await showHome();
    } catch (err) {
      showError(String(err));
    }
  });

  // ---------------------------------------------------------------- start-up
  void api.live().then((l) => {
    if (l && l.state !== "done") {
      live = l;
      liveAt = performance.now();
      ui.onRecording(true);
    }
    void renderList();
  });
}
