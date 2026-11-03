/**
 * Meetings section: record the microphone ("Me") and the computer's sound ("Them"), see the
 * transcript as it comes; when it stops, the AI notes are written (one section for each topic).
 * A switch shows the transcript in the notes' place; on the right, questions about the meeting
 * are answered by the same model, and saved with it. The app records, transcribes, writes and
 * saves (desktop/src-tauri/src/meeting.rs, llm.rs); this page shows it and sends the title.
 */
import type { AiNotesEvent, ChatMessage, Meeting, MeetingLine, MeetingLive, MeetingSummary } from "./api";
import { confirmDialog } from "./confirm";
import { highlighted, searchWords } from "./highlight";
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
  const emptyTitle = $<HTMLElement>("meetings-empty-title");
  const emptyTip = $<HTMLElement>("meetings-empty-tip");
  const search = $<HTMLInputElement>("meetings-search");
  let searchTimer = 0;
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
  const tabNotes = $<HTMLButtonElement>("tab-notes");
  const tabTranscript = $<HTMLButtonElement>("tab-transcript");
  const notesPanel = $<HTMLDivElement>("notes-panel");
  const transcriptPanel = $<HTMLDivElement>("transcript-panel");
  const aiMeta = $<HTMLElement>("ai-meta");
  const aiWrite = $<HTMLButtonElement>("ai-write");
  const aiCancel = $<HTMLButtonElement>("ai-cancel");
  const aiProgress = $<HTMLDivElement>("ai-progress");
  const aiProgressFill = $<HTMLDivElement>("ai-progress-fill");
  const aiProgressText = $<HTMLElement>("ai-progress-text");
  const aiBody = $<HTMLDivElement>("ai-body");
  const aiError = $<HTMLElement>("ai-error");
  const chatLog = $<HTMLDivElement>("chat-log");
  const chatClear = $<HTMLButtonElement>("chat-clear");
  const chatSuggestions = $<HTMLDivElement>("chat-suggestions");
  const chatForm = $<HTMLFormElement>("chat-form");
  const chatInput = $<HTMLTextAreaElement>("chat-input");
  const chatSend = $<HTMLButtonElement>("chat-send");
  /** The AI notes job of the app (one at a time): which meeting, and its last event. */
  let aiJob: AiNotesEvent | null = null;
  /** The question being answered (the app answers one at a time, and not while it writes notes). */
  let asking: { id: string; question: string } | null = null;
  let chatJob: AiNotesEvent | null = null;
  let chatError: { id: string; text: string } | null = null;
  /** What the left side shows. */
  let tab: "notes" | "transcript" = "notes";

  /** The meeting on screen (live or saved), and the live one (if any). */
  let shown: Meeting | null = null;
  let live: MeetingLive | null = null;
  let liveAt = 0; // performance.now() when `live` arrived (for the timer)
  let saveTimer = 0;
  let unsaved: { title?: string } = {};
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
    renderTab();
    renderAi();
    renderChat();
  }

  // ---------------------------------------------------------------- AI notes | Transcript
  function renderTab(): void {
    for (const [b, on] of [
      [tabNotes, tab === "notes"],
      [tabTranscript, tab === "transcript"],
    ] as const) {
      b.setAttribute("aria-selected", String(on));
      b.tabIndex = on ? 0 : -1;
    }
    notesPanel.hidden = tab !== "notes";
    transcriptPanel.hidden = tab !== "transcript";
    // Write notes and the model's name belong to the notes; Cancel shows from both tabs.
    aiMeta.hidden = tab !== "notes";
    pending.hidden = tab !== "transcript";
  }

  function setTab(t: "notes" | "transcript", focus = false): void {
    const changed = tab !== t;
    tab = t;
    renderTab();
    renderAi();
    if (focus) (t === "notes" ? tabNotes : tabTranscript).focus();
    // The transcript opens at its end while it records (the newest words), else at the start.
    if (changed && t === "transcript" && isLive()) transcript.scrollTop = transcript.scrollHeight;
  }

  tabNotes.addEventListener("click", () => setTab("notes"));
  tabTranscript.addEventListener("click", () => setTab("transcript"));
  // The arrow keys move between the two (the tabs pattern); Tab goes on to the panel.
  for (const b of [tabNotes, tabTranscript]) {
    b.addEventListener("keydown", (e) => {
      if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) {
        e.preventDefault();
        const next = e.key === "Home" ? "notes" : e.key === "End" ? "transcript" : tab === "notes" ? "transcript" : "notes";
        setTab(next, true);
      }
    });
  }

  // ---------------------------------------------------------------- AI notes
  const running = (e: AiNotesEvent | null) => !!e && ["preparing", "downloading", "writing"].includes(e.state);
  const aiRunning = () => running(aiJob);
  const chatRunning = () => running(chatJob);

  function renderAi(): void {
    if (!shown) return;
    if (isLive()) {
      // The same place as later: the notes come here when the recording stops.
      aiWrite.hidden = aiCancel.hidden = aiProgress.hidden = aiError.hidden = true;
      aiMeta.textContent = "";
      aiBody.replaceChildren("Written here when the recording stops: one section for each topic, with its decisions and tasks. The transcript is in the other tab.");
      aiBody.classList.add("ai-empty");
      return;
    }
    const mine = aiJob?.id === shown.id ? aiJob : null;
    const running = !!mine && aiRunning();
    // Only when there are no notes yet (not written, or the writing failed).
    aiWrite.hidden = running || !!shown.ai_notes || tab !== "notes";
    aiWrite.disabled = aiRunning() || chatRunning(); // the model is at work for something else
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
      aiBody.replaceChildren(running ? "Writing…" : "No AI notes yet. Write notes makes one section for each topic of the meeting, with its decisions and tasks, from the transcript.");
      aiBody.classList.add("ai-empty");
    }
    aiMeta.textContent = running
      ? mine!.state === "writing" ? "Writing…" : ""
      : shown.ai_notes
        ? `${shown.ai_model || "AI"} · a draft: check it against the transcript`
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
    if (shown?.id === e.id || !aiRunning()) {
      renderAi();
      renderChat(); // the questions wait for the notes
    }
  });

  // ---------------------------------------------------------------- Ask about this meeting
  const el = (tag: string, cls: string, text = "") => {
    const x = document.createElement(tag);
    x.className = cls;
    x.textContent = text;
    return x;
  };

  function question(text: string): HTMLElement {
    return el("div", "chat-q", text);
  }

  function answer(text: string, done: boolean): HTMLElement {
    const box = el("div", "chat-a");
    const body = el("div", "ai-body");
    renderMarkdown(body, text);
    box.append(body);
    if (done) {
      const tools = el("div", "chat-a-tools");
      const copy = el("button", "btn btn-ghost btn-sm", "Copy") as HTMLButtonElement;
      copy.type = "button";
      copy.setAttribute("aria-label", "Copy this answer");
      copy.addEventListener("click", async () => {
        try {
          await backend.copy(text);
          copy.textContent = "Copied";
          setTimeout(() => (copy.textContent = "Copy"), 1500);
        } catch (err) {
          showError(String(err));
        }
      });
      tools.append(copy);
      box.append(tools);
    }
    return box;
  }

  const SEND_ICON = "M12 19V5M5 12l7-7 7 7";

  function sendIcon(stop: boolean): SVGSVGElement {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.classList.add("icon");
    if (stop) {
      const r = document.createElementNS("http://www.w3.org/2000/svg", "rect");
      for (const [k, v] of Object.entries({ x: "7", y: "7", width: "10", height: "10", rx: "1.5" })) r.setAttribute(k, v);
      r.setAttribute("fill", "currentColor");
      svg.append(r);
    } else {
      const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
      p.setAttribute("d", SEND_ICON);
      svg.append(p);
    }
    return svg;
  }

  function renderChat(): void {
    if (!shown) return;
    const recording = isLive();
    const mine = asking?.id === shown.id ? asking : null;
    const answering = !!mine && chatRunning();
    const nearEnd = chatLog.scrollHeight - chatLog.scrollTop - chatLog.clientHeight < 80;
    const nodes: HTMLElement[] = [];
    for (const m of shown.chat ?? []) nodes.push(m.role === "user" ? question(m.text) : answer(m.text, true));
    if (mine) {
      nodes.push(question(mine.question));
      const ev = chatJob?.id === shown.id ? chatJob : null;
      if (ev?.text) nodes.push(answer(ev.text, false));
      else nodes.push(el("p", "chat-wait", ev && ev.state !== "writing" && ev.detail ? ev.detail : "Thinking…"));
    }
    if (chatError?.id === shown.id) nodes.push(el("p", "chat-error", chatError.text));
    if (!nodes.length) {
      nodes.push(
        el(
          "p",
          "chat-empty",
          recording
            ? "Ask about the meeting when the recording stops."
            : "Ask anything about this meeting: what was decided, who does what, the numbers. The answers come from the transcript and the notes.",
        ),
      );
    }
    chatLog.replaceChildren(...nodes);
    if (nearEnd) chatLog.scrollTop = chatLog.scrollHeight; // a reader who scrolled up stays there

    // The box: off while it records, and while the model is at work for something else.
    const busyElsewhere = (aiRunning() || chatRunning()) && !answering;
    chatInput.disabled = recording || busyElsewhere;
    chatInput.placeholder = recording
      ? "Ask when the recording stops"
      : aiRunning()
        ? "Ask when the AI notes are written"
        : busyElsewhere
          ? "Wait for the other answer"
          : "Ask about the meeting…";
    chatSend.replaceChildren(sendIcon(answering));
    chatSend.toggleAttribute("data-stop", answering);
    chatSend.setAttribute("aria-label", answering ? "Stop the answer" : "Ask");
    chatSend.title = answering ? "Stop the answer" : "Ask (Enter)";
    chatSend.disabled = !answering && (chatInput.disabled || !chatInput.value.trim());
    chatSuggestions.hidden = recording || answering;
    for (const b of chatSuggestions.querySelectorAll("button")) b.disabled = chatInput.disabled;
    chatClear.hidden = !(shown.chat ?? []).length || answering;
  }

  async function ask(text: string): Promise<void> {
    const q = text.trim();
    if (!shown || !q || isLive() || aiRunning() || chatRunning()) return;
    const id = shown.id;
    chatError = null;
    asking = { id, question: q };
    chatJob = { id, state: "preparing", detail: "Getting ready…", progress: 0, text: "" };
    chatInput.value = "";
    renderChat();
    chatLog.scrollTop = chatLog.scrollHeight; // the new question
    await flush(); // the model reads the latest title
    try {
      await api.chat(id, q);
    } catch (err) {
      // Refused before it started (e.g. the model is busy): the question goes back in the box.
      if (asking?.id === id && chatJob?.id === id && chatJob.state === "preparing" && !chatJob.text) {
        asking = null;
        chatJob = null;
        chatError = { id, text: String(err) };
        if (shown?.id === id && !chatInput.value) chatInput.value = q;
        renderChat();
        renderAi();
      }
    }
  }

  api.onAiChat((e) => {
    chatJob = e;
    const mine = asking?.id === e.id ? asking : null;
    if (e.state === "done" && mine) {
      if (shown?.id === e.id) {
        const at = Math.floor(Date.now() / 1000);
        const added: ChatMessage[] = [
          { role: "user", text: mine.question, at },
          { role: "assistant", text: e.text, at },
        ];
        const old = shown.chat ?? [];
        const saved = old.length >= 2 && old[old.length - 2].text === mine.question && old[old.length - 1].text === e.text;
        if (!saved) shown.chat = [...old, ...added]; // (opened again after the save: already there)
        ui.announce("Answer ready.");
      }
      asking = null;
    } else if ((e.state === "error" || e.state === "cancelled") && mine) {
      if (e.state === "error") chatError = { id: e.id, text: e.detail };
      if (shown?.id === e.id && !chatInput.value) chatInput.value = mine.question; // ask again, or change it
      asking = null;
    }
    if (shown) {
      renderChat();
      renderAi(); // Write notes waits for the answer
    }
  });

  chatForm.addEventListener("submit", (e) => {
    e.preventDefault();
    if (shown && asking?.id === shown.id && chatRunning()) void api.aiCancel();
    else void ask(chatInput.value);
  });
  chatInput.addEventListener("keydown", (e) => {
    // Enter asks; Shift+Enter is a new line.
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      if (!chatSend.disabled && !chatSend.hasAttribute("data-stop")) chatForm.requestSubmit();
    }
  });
  chatInput.addEventListener("input", () => {
    chatSend.disabled = !chatSend.hasAttribute("data-stop") && (chatInput.disabled || !chatInput.value.trim());
  });
  // Ctrl+K: ask (the box of the meeting on screen).
  document.addEventListener("keydown", (e) => {
    if (e.key.toLowerCase() !== "k" || !e.ctrlKey || e.altKey || e.metaKey || view.hidden || !view.offsetParent) return;
    if (document.querySelector("dialog[open]") || chatInput.disabled) return;
    e.preventDefault();
    chatInput.focus();
  });
  for (const b of chatSuggestions.querySelectorAll<HTMLButtonElement>("button[data-ask]")) {
    b.addEventListener("click", () => void ask(b.dataset.ask ?? ""));
  }
  chatClear.addEventListener("click", async () => {
    if (!shown) return;
    const id = shown.id;
    const yes = await confirmDialog({
      title: "Clear this chat?",
      body: "The questions and answers of this meeting are deleted. Its notes and transcript stay.",
      action: "Clear chat",
      danger: true,
    });
    if (!yes) return;
    try {
      await api.chatClear(id);
      if (shown?.id === id) shown.chat = [];
      chatError = null;
      renderChat();
      chatInput.focus();
    } catch (err) {
      showError(String(err));
    }
  });

  function open(m: Meeting): void {
    if (shown?.id !== m.id) {
      chatInput.value = "";
      chatError = null;
      chatLog.scrollTop = 0;
    }
    shown = m;
    title.value = m.title;
    unsaved = {};
    home.hidden = true;
    view.hidden = false;
    // While it records: the live transcript; once saved: the notes.
    tab = live && live.meeting.id === m.id && live.state !== "done" ? "transcript" : "notes";
    renderView();
    chatLog.scrollTop = chatLog.scrollHeight; // the newest question and answer
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

  const FOUND_IN: Record<string, string> = { transcript: "Transcript", ai_notes: "AI notes", notes: "Notes" };

  function item(s: MeetingSummary, recording: boolean, words: string[]): HTMLElement {
    const row = document.createElement("div");
    row.className = "meeting-item";
    row.setAttribute("role", "listitem");
    const b = document.createElement("button");
    b.type = "button";
    b.className = "meeting-item-open";
    const main = span("meeting-item-main", "");
    const top = span("meeting-item-top", "");
    const t = span("meeting-item-title", "");
    t.append(...highlighted(s.title, words));
    top.append(t);
    if (recording) top.append(span("chip chip-live", "Recording"));
    else if (s.state === "recording") top.append(span("chip chip-warn", "Interrupted"));
    main.append(top);
    if (s.excerpt) {
      const ex = span("meeting-item-excerpt", "");
      // In a search: where the words were found, then the words around them.
      if (s.found_in && FOUND_IN[s.found_in]) ex.append(span("meeting-item-where", `${FOUND_IN[s.found_in]}: `));
      ex.append(...highlighted(s.excerpt, words));
      main.append(ex);
    }
    const side = span("meeting-item-side", "");
    side.append(span("meeting-item-time", timeFmt.format(new Date(s.started_at * 1000))));
    side.append(span("meeting-item-len", recording ? "now" : minutes(s.duration)));
    b.append(main, side);
    b.addEventListener("click", () => void openById(s.id));
    row.append(b);
    if (!recording) {
      // Over the row's right corner on hover or keyboard focus (the History rows do the same).
      const del = document.createElement("button");
      del.type = "button";
      del.className = "btn btn-ghost btn-sm btn-icon danger meeting-item-delete";
      del.setAttribute("aria-label", `Delete ${s.title}`);
      del.title = "Delete";
      del.append(trashIcon());
      del.addEventListener("click", () => void confirmDelete(s.id, s.title));
      row.append(del);
    }
    return row;
  }

  function trashIcon(): SVGSVGElement {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.classList.add("icon");
    const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
    p.setAttribute("d", "M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14");
    svg.append(p);
    return svg;
  }

  /** Ask, then delete. True when it is deleted. */
  async function confirmDelete(id: string, name: string): Promise<boolean> {
    const yes = await confirmDialog({
      title: "Delete this meeting?",
      body: `"${name}", with its transcript, AI notes and chat, is deleted from this computer. You cannot undo this.`,
      action: "Delete meeting",
      danger: true,
    });
    if (!yes) return false;
    try {
      if (shown?.id === id) unsaved = {};
      await api.remove(id);
      ui.announce("Meeting deleted.");
      if (shown?.id === id) await showHome();
      else await renderList();
      return true;
    } catch (err) {
      showError(String(err));
      return false;
    }
  }

  function renderHints(): void {
    const key = ui.meetingKey();
    // "Super+Ctrl+M": the button's tooltip, and its description for screen readers.
    const pretty = key
      .split("+")
      .map((p) => p.trim())
      .map((p) => (p.length > 1 && !/^F\d+$/.test(p) ? p[0] + p.slice(1).toLowerCase() : p))
      .join("+");
    const hint = key ? `Start a meeting recording. Or press ${pretty} in any app.` : "Start a meeting recording.";
    startBtn.title = hint;
    keyHint.textContent = key ? `Or press ${pretty} in any app.` : "";
    emptyText.textContent = search.value.trim()
      ? "No meeting has all of these words in its title, transcript, AI notes or notes."
      : key
        ? `Click Record when your call begins, or press ${pretty} in any app.`
        : "Click Record when your call begins.";
  }

  async function renderList(): Promise<void> {
    const query = search.value.trim();
    const words = searchWords(query);
    let all: MeetingSummary[] = [];
    try {
      all = await api.list(query);
    } catch (err) {
      showError(String(err));
    }
    if (search.value.trim() !== query) return; // typed on meanwhile: that search draws the list
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
      box.append(item(s, s.id === liveId, words));
    }
    list.replaceChildren(...groups);
    empty.hidden = all.length > 0;
    emptyTitle.textContent = query ? "Nothing found" : "No meetings yet";
    emptyTip.hidden = !!query;
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
    if (document.querySelector("dialog[open]")) return; // Esc closes the dialog only
    if (e.target === chatInput && chatInput.value) return; // a question being typed stays
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

  deleteBtn.addEventListener("click", () => {
    if (shown) void confirmDelete(shown.id, title.value.trim() || shown.title);
  });

  // ---------------------------------------------------------------- search
  search.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => void renderList(), 200);
  });
  search.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && search.value) {
      search.value = "";
      void renderList();
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
