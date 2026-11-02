/**
 * Meetings section: record the microphone ("Me") and the computer's sound ("Them"), see the
 * transcript as it comes; when it stops, the AI notes are written next to the transcript. The
 * app records, transcribes, writes and saves (desktop/src-tauri/src/meeting.rs, llm.rs); this
 * page shows it and sends the title.
 */
import type { AiNotesEvent, Meeting, MeetingLine, MeetingLive, MeetingSummary } from "./api";
import type { Backend } from "./backend";
import { renderMarkdown } from "./markdown";
import { DualWaveform } from "./waveform";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export interface MeetingsUi {
  announce(message: string): void;
  /** The nav item shows a red dot while a meeting records. */
  onRecording(recording: boolean): void;
  /** The meeting key ("SUPER + CTRL + M"; "" = none), for the hints. */
  meetingKey(): string;
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
  const list = $<HTMLDivElement>("meetings-list");
  const keyHint = $<HTMLElement>("meetings-key");
  const emptyText = $<HTMLElement>("meetings-empty-text");
  const livePanel = $<HTMLDivElement>("meeting-live");
  const liveClock = $<HTMLElement>("meeting-clock");
  const wave = new DualWaveform($<HTMLCanvasElement>("meeting-wave"));
  const empty = $<HTMLElement>("meetings-empty");
  const view = $<HTMLDivElement>("meeting-view");
  const back = $<HTMLButtonElement>("meeting-back");
  const title = $<HTMLInputElement>("meeting-title");
  const meta = $<HTMLElement>("meeting-meta");
  const stopBtn = $<HTMLButtonElement>("meeting-stop");
  const copyBtn = $<HTMLButtonElement>("meeting-copy");
  const exportBtn = $<HTMLButtonElement>("meeting-export");
  const deleteBtn = $<HTMLButtonElement>("meeting-delete");
  const notices = $<HTMLUListElement>("meeting-notices");
  const transcript = $<HTMLDivElement>("meeting-transcript");
  const pending = $<HTMLElement>("meeting-pending");
  const aiCard = $<HTMLElement>("ai-card");
  const aiMeta = $<HTMLElement>("ai-meta");
  const aiWrite = $<HTMLButtonElement>("ai-write");
  const aiCancel = $<HTMLButtonElement>("ai-cancel");
  const aiProgress = $<HTMLDivElement>("ai-progress");
  const aiProgressFill = $<HTMLDivElement>("ai-progress-fill");
  const aiProgressText = $<HTMLElement>("ai-progress-text");
  const aiBody = $<HTMLDivElement>("ai-body");
  const aiError = $<HTMLElement>("ai-error");
  /** The AI notes job of the app (one at a time): which meeting, and its last event. */
  let aiJob: AiNotesEvent | null = null;

  /** The meeting on screen (live or saved), and the live one (if any). */
  let shown: Meeting | null = null;
  let live: MeetingLive | null = null;
  let liveAt = 0; // performance.now() when `live` arrived (for the timer)
  let saveTimer = 0;
  let unsaved: { title?: string } = {};
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
    if (!shown || unsaved.title === undefined) return;
    const patch = unsaved;
    unsaved = {};
    try {
      await api.update(shown.id, patch);
    } catch (err) {
      unsaved = { ...patch, ...unsaved }; // kept: the next change or Back tries again
      showError(`The title was not saved: ${String(err)}`);
    }
  }

  function later(patch: { title?: string }): void {
    unsaved = { ...unsaved, ...patch };
    clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => void flush(), 600);
  }

  title.addEventListener("input", () => {
    if (shown) shown.title = title.value;
    later({ title: title.value });
  });
  title.addEventListener("blur", () => void flush());
  title.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      title.blur(); // saves it
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
      const sources = live.meeting.system_audio ? "your microphone and the computer's sound" : "your microphone only";
      meta.dataset.live = live.state;
      meta.textContent = live.state === "finishing" ? "Saving: transcribing the last seconds…" : `${when(live.meeting.started_at)} · ${sources}`;
      liveClock.textContent = clock(elapsed);
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
    const square = document.createElement("span");
    square.className = "stop-square";
    square.setAttribute("aria-hidden", "true");
    stopBtn.replaceChildren(square, live?.state === "finishing" ? "Saving…" : "Stop");
    copyBtn.hidden = exportBtn.hidden = deleteBtn.hidden = recording;
    livePanel.hidden = !recording;
    livePanel.dataset.state = live?.state ?? "";
    if (recording && live?.state === "recording") wave.start();
    else wave.stop();
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
    renderAi();
  }

  // ---------------------------------------------------------------- AI notes
  const aiRunning = () => !!aiJob && ["preparing", "downloading", "writing"].includes(aiJob.state);

  function renderAi(): void {
    if (!shown) return;
    aiCard.hidden = false;
    if (isLive()) {
      // The same place as later: the notes come here when the recording stops.
      aiWrite.hidden = aiCancel.hidden = aiProgress.hidden = aiError.hidden = true;
      aiMeta.textContent = "";
      aiBody.replaceChildren("Written here when the recording stops: summary, decisions, tasks and open questions.");
      aiBody.classList.add("ai-empty");
      return;
    }
    const mine = aiJob?.id === shown.id ? aiJob : null;
    const running = !!mine && aiRunning();
    aiWrite.hidden = running;
    aiWrite.disabled = aiRunning(); // another meeting's notes are being written
    aiWrite.textContent = shown.ai_notes ? "Write again" : "Write notes";
    aiCancel.hidden = !running;
    aiProgress.hidden = !running || mine!.state === "writing";
    if (running) {
      aiProgress.classList.toggle("indeterminate", mine!.state !== "downloading");
      aiProgressFill.style.width = mine!.state === "downloading" ? `${Math.max(2, Math.round(mine!.progress * 100))}%` : "30%";
      aiProgressText.textContent = mine!.detail;
    }
    aiError.hidden = !(mine && mine.state === "error");
    aiError.textContent = mine && mine.state === "error" ? mine.detail : "";
    const text = running && mine!.text ? mine!.text : shown.ai_notes ?? "";
    if (text) {
      renderMarkdown(aiBody, text);
      aiBody.classList.remove("ai-empty");
    } else {
      aiBody.replaceChildren(running ? "Writing…" : "Summary, decisions, tasks and open questions, from the transcript. On this computer.");
      aiBody.classList.add("ai-empty");
    }
    aiMeta.textContent = running
      ? mine!.state === "writing" ? "· writing…" : ""
      : shown.ai_notes
        ? `· ${shown.ai_model || "AI"} · a draft: check it against the transcript`
        : "";
  }

  aiWrite.addEventListener("click", async () => {
    if (!shown || aiRunning()) return;
    const id = shown.id;
    aiWrite.disabled = true; // no second job from a double click
    aiJob = { id, state: "preparing", detail: "Getting ready…", progress: 0, text: "" };
    renderAi();
    await flush(); // the model reads the latest title
    try {
      await api.aiNotes(id);
    } catch (err) {
      // A job that runs already (this meeting or another) keeps its card; only a real error shows.
      if (aiJob?.id === id && aiJob.state === "preparing" && !aiJob.text) {
        aiJob = { id, state: "error", detail: String(err), progress: 0, text: "" };
      }
      renderAi();
    }
  });

  aiCancel.addEventListener("click", () => void api.aiCancel());

  api.onAiNotes((e) => {
    aiJob = e;
    if (e.state === "done" && shown?.id === e.id) {
      shown.ai_notes = e.text;
      shown.ai_model = e.detail;
      ui.announce("AI notes written.");
    }
    if (shown?.id === e.id || !aiRunning()) renderAi();
  });

  function open(m: Meeting): void {
    shown = m;
    title.value = m.title;
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
  const span = (cls: string, text: string) => {
    const el = document.createElement("span");
    el.className = cls;
    el.textContent = text;
    return el;
  };

  /** "Today", "Yesterday", "Mon, Nov 2" (with the year when it is not this year). */
  function dayLabel(unix: number): string {
    const d = new Date(unix * 1000);
    const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const diff = Math.round((day(new Date()) - day(d)) / 86_400_000);
    if (diff === 0) return "Today";
    if (diff === 1) return "Yesterday";
    const sameYear = d.getFullYear() === new Date().getFullYear();
    return new Intl.DateTimeFormat(undefined, { weekday: "short", month: "short", day: "numeric", ...(sameYear ? {} : { year: "numeric" }) }).format(d);
  }

  const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });

  function item(s: MeetingSummary, recording: boolean): HTMLElement {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "meeting-item";
    b.setAttribute("role", "listitem");
    const main = span("meeting-item-main", "");
    const top = span("meeting-item-top", "");
    top.append(span("meeting-item-title", s.title));
    if (recording) top.append(span("chip chip-live", "Recording"));
    else if (s.state === "recording") top.append(span("chip chip-warn", "Interrupted"));
    if (s.ai) {
      const chip = span("chip chip-ai", "AI notes");
      chip.title = "This meeting has AI notes";
      top.append(chip);
    }
    main.append(top);
    if (s.excerpt) main.append(span("meeting-item-excerpt", s.excerpt));
    const side = span("meeting-item-side", "");
    side.append(span("meeting-item-time", timeFmt.format(new Date(s.started_at * 1000))));
    side.append(span("meeting-item-len", recording ? "now" : minutes(s.duration)));
    b.append(main, side);
    b.addEventListener("click", () => void openById(s.id));
    return b;
  }

  function renderHints(): void {
    const key = ui.meetingKey();
    keyHint.replaceChildren();
    if (key) {
      keyHint.append("or ");
      for (const [i, part] of key.split("+").map((p) => p.trim()).entries()) {
        if (i) keyHint.append(" ");
        const k = document.createElement("kbd");
        k.textContent = part.length > 1 && !/^F\d+$/.test(part) ? part[0] + part.slice(1).toLowerCase() : part;
        keyHint.append(k);
      }
      keyHint.append(" in any app");
    }
    emptyText.textContent = key
      ? "Start a recording when your call begins, here or with the meeting key from any app."
      : "Start a recording when your call begins.";
  }

  async function renderList(): Promise<void> {
    let all: MeetingSummary[] = [];
    try {
      all = await api.list();
    } catch (err) {
      showError(String(err));
    }
    const liveId = live && live.state !== "done" ? live.meeting.id : null;
    // Grouped by day, newest first (the app sorts them).
    const groups: HTMLElement[] = [];
    let current = "";
    let box: HTMLElement | null = null;
    for (const s of all) {
      const label = dayLabel(s.started_at);
      if (label !== current || !box) {
        current = label;
        const group = document.createElement("section");
        group.className = "meeting-day";
        const h = document.createElement("h3");
        h.textContent = label;
        box = document.createElement("div");
        box.className = "meeting-day-items";
        group.append(h, box);
        groups.push(group);
      }
      box.append(item(s, s.id === liveId));
    }
    list.replaceChildren(...groups);
    empty.hidden = all.length > 0;
    startBtn.disabled = !!liveId;
    renderHints();
  }

  // ---------------------------------------------------------------- start / stop
  startBtn.addEventListener("click", async () => {
    showError(null);
    startBtn.disabled = true;
    try {
      live = await api.start(defaultTitle());
      liveAt = performance.now();
      ui.onRecording(true);
      ui.announce("Recording. The AI notes are written when it stops.");
      open({ ...live.meeting });
      stopBtn.focus();
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
        // A title typed while it finished (not saved yet) goes over the saved meeting.
        const typed = unsaved;
        open(saved);
        if (typed.title !== undefined) {
          title.value = typed.title;
          later(typed);
        }
      }
      void renderList();
    } catch (err) {
      // The meeting key stopped it at the same moment: it is being saved; its "done" event
      // opens the saved meeting.
      if (String(err).includes("being saved")) return;
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
    wave.levels = lv;
  });

  let hintKey: string | null = null;
  window.setInterval(() => {
    if (isLive() && live?.state === "recording") renderMeta();
    // The key comes with the status, maybe after the list: show it once it is known.
    if (!home.hidden && hintKey !== ui.meetingKey()) {
      hintKey = ui.meetingKey();
      renderHints();
    }
  }, 1000);

  // ---------------------------------------------------------------- saved meeting actions
  back.addEventListener("click", () => void showHome());
  // Esc: back to the list (the title is saved first). A recording goes on.
  document.addEventListener("keydown", (e) => {
    // Only while this view is on screen (its section shown), and not while keys are chosen.
    if (e.key !== "Escape" || e.defaultPrevented || view.hidden || !view.offsetParent) return;
    e.preventDefault();
    (document.activeElement as HTMLElement | null)?.blur();
    void showHome();
  });

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
