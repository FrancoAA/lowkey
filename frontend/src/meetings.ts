/**
 * Meetings section: record the microphone ("Me") and the computer's sound ("Them"), see the
 * transcript as it comes; when it stops, the notes are written (one section for each topic); a
 * click on a point of the notes shows where the transcript says it.
 * A switch shows the transcript in the notes' place; on the right, questions about the meeting
 * are answered by the same model, and saved with it. The app records, transcribes, writes and
 * saves (desktop/src-tauri/src/meeting.rs, llm.rs); this page shows it and sends the title.
 */
import type { AiNotesEvent, ChatMessage, Meeting, MeetingLine, MeetingLive, MeetingSummary, VocabTerm } from "./api";
import { confirmDialog } from "./confirm";
import { highlighted, searchWords } from "./highlight";
import type { Backend } from "./backend";
import { renderMarkdown } from "./markdown";
import { excerpt, SourceIndex } from "./crossref";
import { DualWaveform } from "./waveform";

/** A small spinner (style.css "loading"): the app is at work, with no measure of how far. */
function spinner(): HTMLElement {
  const s = document.createElement("span");
  s.className = "spinner";
  s.setAttribute("aria-hidden", "true");
  return s;
}

/** Gray lines in the place of text that is being written; `label` is for screen readers. */
function skeleton(label: string): HTMLElement {
  const box = document.createElement("div");
  box.className = "skeleton";
  const sr = document.createElement("span");
  sr.className = "sr-only";
  sr.textContent = label;
  box.append(sr);
  for (const [cls, width] of [["head", ""], ["", "92%"], ["", "78%"], ["", "85%"], ["head", ""], ["", "88%"], ["", "64%"]]) {
    const line = document.createElement("div");
    line.className = `skeleton-line ${cls}`.trim();
    if (width) line.style.width = width;
    line.setAttribute("aria-hidden", "true");
    box.append(line);
  }
  return box;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export interface MeetingsUi {
  announce(message: string): void;
  /** The nav item shows a red dot while a meeting records. */
  onRecording(recording: boolean): void;
  /** The meeting key ("SUPER + CTRL + M"; "" = none), for the hints. */
  meetingKey(): string;
  /** Go to the Meetings section (the strip's "Open the notes"). */
  showMeetings(): void;
  /** Settings → Note apps: the folder ("" = off) and the Obsidian vault it is in. */
  exportInfo(): { dir: string; vault: string | null };
  /** Settings changed here (a term added to the Vocabulary): read them again. */
  refreshStatus(): void;
  /** The meetings whose voices are being told apart now (from the app's status). */
  voicesWorking(): string[];
  /** An automated test runs: the player's events go to its log. */
  testing(): boolean;
}

/** "Me", "Them", or the name of one of the others found by voice ("Speaker 2" before one). */
export function speakerLabel(m: Pick<Meeting, "people">, speaker: "me" | "them", person = 0): string {
  if (speaker === "me") return "Me";
  if (!person) return "Them";
  return m.people?.[person - 1]?.trim() || `Speaker ${person}`;
}

/** A paragraph and the lines it is made of (`from`..`to`, in the meeting's lines). */
export type Paragraph = MeetingLine & { from: number; to: number };

/** Lines of one speaker with short gaps form one paragraph (as in the Markdown export). */
export function paragraphs(lines: MeetingLine[]): Paragraph[] {
  const out: Paragraph[] = [];
  lines.forEach((l, i) => {
    const p = out[out.length - 1];
    if (p && p.speaker === l.speaker && (p.person ?? 0) === (l.person ?? 0) && l.start - p.end < 2 && p.text.length < 600) {
      p.text += ` ${l.text}`;
      p.end = Math.max(p.end, l.end);
      p.to = i;
    } else out.push({ ...l, from: i, to: i });
  });
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

/** `refreshExport`: the meeting's note-app buttons follow a change in Settings → Note apps. */
export function initMeetings(backend: Backend, ui: MeetingsUi): { refreshExport(): void } {
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
  const folderBtn = $<HTMLButtonElement>("meeting-folder");
  const obsidianBtn = $<HTMLButtonElement>("meeting-obsidian");
  const notices = $<HTMLUListElement>("meeting-notices");
  const transcript = $<HTMLDivElement>("meeting-transcript");
  const videoWrap = $<HTMLDivElement>("meeting-video");
  const videoToggle = $<HTMLButtonElement>("video-toggle");
  const videoBox = $<HTMLDivElement>("video-box");
  const videoMeta = $<HTMLElement>("video-meta");
  const player = $<HTMLVideoElement>("meeting-player");
  const screenMark = $<HTMLElement>("meeting-screen-mark");
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
  /** The notes job of the app (one at a time): which meeting, and its last event. */
  let aiJob: AiNotesEvent | null = null;
  /** The question being answered (the app answers one at a time, and not while it writes notes). */
  let asking: { id: string; question: string } | null = null;
  let chatJob: AiNotesEvent | null = null;
  let chatError: { id: string; text: string } | null = null;
  /** What the left side shows. */
  let tab: "notes" | "transcript" = "notes";
  /** Meetings whose voices are being told apart (after Stop). */
  const voicesWorking = new Set<string>(ui.voicesWorking());
  /** Where the notes were scrolled to when the transcript took their place. */
  let notesScroll = 0;
  /** The notes now on screen with their points linked (same text and lines: not drawn again). */
  let drawn: { text: string; lines: MeetingLine[] } | null = null;
  /** The transcript as the points are looked up in: built again when its lines change. */
  let sources: { lines: MeetingLine[]; paras: Paragraph[]; index: SourceIndex } | null = null;
  /** The point whose places in the transcript are open: its meeting, its place among the points
   * and its text (notes written again: a point with other text there stays closed). */
  let openPoint: { id: string; n: number; text: string } | null = null;

  /** The meeting whose screen video is in the player (null: none). */
  let videoOf: string | null = null;
  /** The transcript paragraph the video is at (marked while it plays); -1: none. */
  let nowPara = -1;

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
  /** The paragraph being fixed, and its meeting (that meeting's transcript is not drawn again
   * under it; another meeting opened ends the edit). */
  let editing: (Paragraph & { meetingId: string }) | null = null;

  function renderTranscript(m: Meeting, follow: boolean): void {
    if (editing?.meetingId === m.id) return;
    editing = null;
    const nearEnd = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 60;
    transcript.replaceChildren();
    if (voicesWorking.has(m.id)) {
      const n = document.createElement("p");
      n.className = "transcript-note";
      n.setAttribute("role", "status");
      n.replaceChildren(spinner(), "Telling the voices apart… (about 2 minutes for an hour)");
      transcript.append(n);
    } else if (!isLive() && m.people?.length && m.people.every((x) => !x.trim())) {
      const n = document.createElement("p");
      n.className = "transcript-note";
      n.textContent = "Click a speaker to give a name. Give two speakers the same name when they are one person.";
      transcript.append(n);
    }
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
      if (l.person) row.dataset.person = String(((l.person - 1) % 6) + 1);
      const named = !isLive() && l.speaker === "them" && !!l.person && !voicesWorking.has(m.id);
      const who = document.createElement(named ? "button" : "span");
      who.className = named ? "who who-name" : "who";
      who.textContent = speakerLabel(m, l.speaker, l.person);
      if (named && who instanceof HTMLButtonElement) {
        who.type = "button";
        who.title = "Click to give a name";
        who.setAttribute("aria-label", `${who.textContent}: give a name`);
        const person = l.person!;
        who.addEventListener("click", () => startRename(who, person));
      }
      let at: HTMLElement;
      if (playable(m)) {
        // With a screen video: the time plays it from there.
        const b = document.createElement("button");
        b.type = "button";
        b.className = "when when-play";
        b.title = "Play the screen video from here";
        b.setAttribute("aria-label", `Play the screen video from ${clock(l.start)}`);
        b.addEventListener("click", () => playAt(l.start));
        b.dataset.start = String(l.start);
        at = b;
      } else {
        at = document.createElement("span");
        at.className = "when";
      }
      at.textContent = clock(l.start);
      const text = document.createElement("p");
      text.textContent = l.text;
      if (!isLive() && !voicesWorking.has(m.id)) {
        // A saved meeting: a click (or Enter) fixes the paragraph (not while its voices are
        // told apart: the speakers change then).
        text.tabIndex = 0;
        text.className = "fixable";
        text.title = "Click to fix the text";
        text.setAttribute("role", "button");
        text.setAttribute("aria-roledescription", "paragraph, press Enter to fix it");
        text.addEventListener("click", () => startEdit(row, l));
        text.addEventListener("keydown", (e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            startEdit(row, l);
          }
        });
      }
      row.append(who, at, text);
      transcript.append(row);
    }
    if (playable(m) && nowPara >= 0) transcript.querySelectorAll(".tline")[nowPara]?.classList.add("tline-now");
    if (follow && nearEnd) transcript.scrollTop = transcript.scrollHeight;
  }

  // ---------------------------------------------------------------- fixing the transcript
  /** The paragraph as a text field; its speaker as a switch. Enter or a click outside saves,
   * Esc cancels. */
  function startEdit(row: HTMLElement, para: Paragraph): void {
    if (!shown || isLive() || editing) return;
    const id = shown.id; // the meeting these lines belong to, whatever is shown later
    editing = { ...para, meetingId: id };
    const m = shown;
    // Me, then each of the others found by voice who still has lines (one merged into another
    // has none), or "Them" when there are none.
    const persons = [...new Set(m.lines.filter((l) => l.speaker === "them" && l.person).map((l) => l.person!))].sort((a, b) => a - b);
    const choices: { speaker: "me" | "them"; person: number }[] = [
      { speaker: "me", person: 0 },
      ...(persons.length ? persons.map((p) => ({ speaker: "them" as const, person: p })) : [{ speaker: "them" as const, person: 0 }]),
    ];
    let at = Math.max(0, choices.findIndex((c) => c.speaker === para.speaker && c.person === (para.person ?? 0)));
    if (choices[at].speaker !== para.speaker || choices[at].person !== (para.person ?? 0)) choices.splice((at = 1), 0, { speaker: para.speaker, person: para.person ?? 0 });
    let speaker = choices[at].speaker;
    let person = choices[at].person;
    const who = row.querySelector<HTMLElement>(".who")!;
    const swap = document.createElement("button");
    swap.type = "button";
    swap.className = `who who-switch who-${speaker}`;
    const label = () => {
      const name = speakerLabel(m, speaker, person);
      swap.textContent = name;
      swap.className = `who who-switch who-${speaker}`;
      swap.title = `Said by ${speaker === "me" ? "you" : name}: click to switch`;
      swap.setAttribute("aria-label", `Speaker: ${name}. Switch`);
      row.dataset.speaker = speaker;
      if (person) row.dataset.person = String(((person - 1) % 6) + 1);
      else delete row.dataset.person;
    };
    label();
    who.replaceWith(swap);
    const field = document.createElement("textarea");
    field.className = "fix-field";
    field.value = para.text;
    field.setAttribute("aria-label", "The paragraph's text: Enter saves, Escape cancels");
    const fit = () => {
      field.style.height = "auto";
      field.style.height = `${field.scrollHeight}px`;
    };
    field.addEventListener("input", fit);
    row.querySelector("p")!.replaceWith(field);
    fit();
    field.focus();
    field.setSelectionRange(field.value.length, field.value.length);
    let done = false;
    const finish = async (save: boolean) => {
      if (done) return;
      done = true;
      const text = field.value.trim();
      const mine = () => editing?.meetingId === id;
      if (!save || (text === para.text && speaker === para.speaker && person === (para.person ?? 0))) {
        if (mine()) editing = null;
        if (shown?.id === id) renderView();
        return;
      }
      try {
        const r = await api.editParagraph(id, para.from, para.to, para.text, text, speaker, person);
        if (mine()) editing = null;
        if (shown?.id !== id) return; // another meeting is open now
        shown = r.meeting;
        renderView();
        ui.announce(text ? "The paragraph is fixed." : "The paragraph is removed.");
        if (r.suggestion) offerVocabulary(r.suggestion);
      } catch (err) {
        if (mine()) editing = null;
        if (shown?.id === id) {
          // The meeting as it is now (the fix was refused: it changed meanwhile, or…).
          shown = await api.get(id).catch(() => shown);
          renderView();
        }
        showError(`${String(err)} Your text: “${text}”`);
      }
    };
    swap.addEventListener("mousedown", (e) => e.preventDefault()); // the focus stays in the text
    swap.addEventListener("click", () => {
      at = (at + 1) % choices.length;
      ({ speaker, person } = choices[at]);
      label();
      field.focus();
    });
    field.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void finish(true);
      } else if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation(); // not the section's Escape
        void finish(false);
      }
    });
    // A click outside saves (the speaker switch is inside).
    row.addEventListener("focusout", (e) => {
      // Another window (a look-up of the spelling) is not a click outside: the edit stays.
      if (e.relatedTarget === null && !document.hasFocus()) return;
      if (!row.contains(e.relatedTarget as Node | null)) void finish(true);
    });
  }

  /** A name for one of the others: a field in place of the label. Enter or a click outside
   * saves, Escape cancels; the name of another speaker merges the two. */
  function startRename(who: HTMLElement, person: number): void {
    if (!shown || isLive() || editing) return;
    const m = shown;
    const id = m.id;
    editing = { ...paragraphs(m.lines)[0], meetingId: id }; // no redraw under the field
    const field = document.createElement("input");
    field.className = "input name-field";
    field.maxLength = 60;
    field.value = m.people?.[person - 1] ?? "";
    field.placeholder = `Speaker ${person}`;
    field.setAttribute("aria-label", `Name of Speaker ${person}: Enter saves, Escape cancels`);
    who.replaceWith(field);
    field.focus();
    let done = false;
    const finish = async (save: boolean) => {
      if (done) return;
      done = true;
      const name = field.value.trim();
      const mine = () => editing?.meetingId === id;
      if (!save || name === (m.people?.[person - 1] ?? "").trim()) {
        if (mine()) editing = null;
        if (shown?.id === id) renderView();
        return;
      }
      try {
        const next = await api.nameSpeaker(id, person, name);
        if (mine()) editing = null;
        if (shown?.id !== id) return;
        const merged = !next.lines.some((l) => l.speaker === "them" && l.person === person);
        shown = next;
        renderView();
        ui.announce(merged ? `Speaker ${person} is one person with ${name} now.` : name ? `Speaker ${person} is ${name} now.` : `Speaker ${person} has no name now.`);
      } catch (err) {
        if (mine()) editing = null;
        if (shown?.id === id) renderView();
        showError(String(err));
      }
    };
    field.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void finish(true);
      } else if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation(); // not the section's Escape
        void finish(false);
      }
    });
    field.addEventListener("blur", () => {
      if (!document.hasFocus()) return; // another window: the field stays
      void finish(true);
    });
  }

  /** After a fix that looks like a name or term: add it to the Vocabulary (10 s to decide). */
  function offerVocabulary(s: VocabTerm): void {
    transcript.querySelector(".vocab-offer")?.remove();
    const bar = document.createElement("div");
    bar.className = "vocab-offer";
    bar.setAttribute("role", "status");
    const text = document.createElement("span");
    text.textContent = `Add “${s.term}” to the Vocabulary (sounds like “${s.sounds_like[0]}”), so it is written right next time?`;
    const add = document.createElement("button");
    add.type = "button";
    add.className = "btn btn-outline btn-sm";
    add.textContent = "Add";
    const close = document.createElement("button");
    close.type = "button";
    close.className = "btn btn-ghost btn-sm";
    close.textContent = "No";
    // 10 s to decide, not counting the time the pointer or the focus is on it.
    let timer = setTimeout(() => bar.remove(), 10_000);
    const hold = () => clearTimeout(timer);
    const release = () => {
      clearTimeout(timer);
      if (!bar.matches(":hover") && !bar.contains(document.activeElement)) timer = setTimeout(() => bar.remove(), 10_000);
    };
    bar.addEventListener("pointerenter", hold);
    bar.addEventListener("focusin", hold);
    bar.addEventListener("pointerleave", release);
    bar.addEventListener("focusout", () => setTimeout(release, 0));
    add.addEventListener("click", () => {
      clearTimeout(timer);
      void api.addVocabulary(s).then(
        () => {
          bar.replaceChildren(`“${s.term}” is in the Vocabulary (Settings → General).`);
          setTimeout(() => bar.remove(), 3000);
          ui.refreshStatus();
        },
        (err) => showError(String(err)),
      );
    });
    close.addEventListener("click", () => {
      clearTimeout(timer);
      bar.remove();
    });
    bar.append(text, add, close);
    transcript.prepend(bar);
  }

  function renderMeta(): void {
    if (!shown) return;
    if (isLive() && live) {
      const elapsed = live.state === "recording" ? live.elapsed + (performance.now() - liveAt) / 1000 : live.elapsed;
      const mic = live.muted ? "your microphone (muted)" : "your microphone";
      const parts = [mic, ...(live.meeting.system_audio ? ["the computer's sound"] : []), ...(live.screen === "on" ? ["the screen"] : [])];
      const inputs = parts.length > 1 ? `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}` : `${mic} only`;
      const picker = live.screen === "waiting" ? " · choose what to share in your desktop's dialog" : "";
      meta.dataset.live = live.state;
      meta.textContent = live.state === "finishing" ? "Saving: transcribing the last seconds…" : `${when(live.meeting.started_at)} · ${inputs}${picker}`;
      liveClock.textContent = clock(elapsed);
    } else {
      delete meta.dataset.live;
      const interrupted = shown.state === "recording" ? " · interrupted" : "";
      const voices = voicesWorking.has(shown.id) ? " · telling the voices apart…" : "";
      meta.textContent = `${when(shown.started_at)} · ${minutes(shown.duration)}${interrupted}${voices}`;
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
    if (live?.state === "finishing") stopBtn.replaceChildren(spinner(), "Saving…");
    else stopBtn.replaceChildren(square, "Stop");
    copyBtn.hidden = exportBtn.hidden = deleteBtn.hidden = recording;
    renderFolderButtons();
    livePanel.hidden = !recording;
    livePanel.dataset.state = live?.state ?? "";
    screenMark.hidden = !(recording && live?.screen === "on");
    renderVideo();
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

  // ---------------------------------------------------------------- the screen video
  /** The open meeting's video is in the player: its times play it. */
  function playable(m: Meeting): boolean {
    return !!m.video && videoOf === m.id && !videoWrap.hidden && !isLive();
  }

  function size(bytes: number): string {
    const mb = bytes / 1e6;
    if (mb < 1) return `${Math.max(1, Math.round(bytes / 1e3))} kB`;
    return mb >= 1000 ? `${(mb / 1000).toFixed(1)} GB` : mb >= 10 ? `${Math.round(mb)} MB` : `${mb.toFixed(1)} MB`;
  }

  /** A saved meeting with a video: in the player (loaded once per meeting). */
  function renderVideo(): void {
    const v = shown && !isLive() && shown.video && shown.video.bytes > 0 ? shown.video : null;
    if (!shown || !v) return unloadVideo();
    videoMeta.textContent = `${v.codec} · ${size(v.bytes)}`;
    if (videoOf === shown.id) return;
    unloadVideo();
    const id = shown.id;
    videoOf = id;
    void api.videoUrl(id).then(
      (url) => {
        if (videoOf !== id || shown?.id !== id || !url) return; // another meeting, or no file
        player.src = url;
        videoWrap.hidden = false;
        renderTranscript(shown, false); // the times play it now
      },
      () => {},
    );
  }

  function unloadVideo(): void {
    if (videoOf === null) return;
    videoOf = null;
    nowPara = -1;
    player.pause();
    player.removeAttribute("src");
    player.load(); // lets the file go
    videoWrap.hidden = true;
  }

  let videoCollapsed = (() => {
    try {
      return localStorage.getItem("lowkey.video-collapsed") === "1";
    } catch {
      return false;
    }
  })();

  function setCollapsed(c: boolean): void {
    videoCollapsed = c;
    videoBox.hidden = c;
    videoToggle.setAttribute("aria-expanded", String(!c));
    if (c) player.pause();
    try {
      localStorage.setItem("lowkey.video-collapsed", c ? "1" : "0");
    } catch {
      // private mode: only for now
    }
  }
  setCollapsed(videoCollapsed);
  videoToggle.addEventListener("click", () => setCollapsed(!videoCollapsed));

  /** Play the video from moment `t` of the meeting (the transcript's time). */
  function playAt(t: number): void {
    if (!shown?.video || !playable(shown)) return;
    const offset = shown.video.offset;
    if (videoCollapsed) setCollapsed(false);
    const go = () => {
      player.currentTime = Math.max(0, t - offset);
      void player.play().catch(() => {});
    };
    if (player.readyState >= 1) go();
    else player.addEventListener("loadedmetadata", go, { once: true });
    ui.announce(t < offset ? `The screen video starts at ${clock(offset)}: playing from its start.` : `Playing the screen video from ${clock(t)}.`);
  }

  /** While it plays: the paragraph said at that moment is marked, and kept in view. */
  function followVideo(): void {
    if (!shown?.video || !playable(shown)) return;
    const t = player.currentTime + shown.video.offset;
    const paras = sourceIndex(shown).paras;
    let i = -1;
    for (let k = 0; k < paras.length && paras[k].start <= t + 0.3; k++) i = k;
    if (i >= 0 && t > paras[i].end + 5) i = -1; // nobody speaks now
    if (i === nowPara) return;
    transcript.querySelector(".tline-now")?.classList.remove("tline-now");
    nowPara = i;
    const row = i >= 0 ? transcript.querySelectorAll<HTMLElement>(".tline")[i] : undefined;
    if (!row) return;
    row.classList.add("tline-now");
    if (!player.paused && tab === "transcript" && !editing) row.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }

  player.addEventListener("timeupdate", followVideo);
  player.addEventListener("seeked", followVideo);

  /** Automated tests: what the player does, with where to click (the window's CSS pixels). */
  function testLog(state: string): void {
    if (!ui.testing()) return;
    const at = (e: HTMLElement | null) => {
      const r = e?.getBoundingClientRect();
      return r && r.width ? [Math.round(r.x + r.width / 2), Math.round(r.y + r.height / 2)] : null;
    };
    const plays = transcript.querySelectorAll<HTMLElement>(".when-play");
    const last = plays[plays.length - 1] ?? null;
    void backend.testEvent("page-video", {
      state,
      id: videoOf,
      time: player.currentTime,
      duration: player.duration,
      error: player.error ? `${player.error.code} ${player.error.message}` : "",
      tab: at(tabTranscript),
      play: last && at(last) ? [...at(last)!, Number(last.dataset.start)] : null,
    });
  }
  for (const [ev, state] of [["loadedmetadata", "loaded"], ["seeked", "seeked"], ["playing", "playing"], ["error", "error"]] as const) {
    player.addEventListener(ev, () => testLog(state));
  }

  // ---------------------------------------------------------------- Notes | Transcript
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
    if (changed && t === "transcript") notesScroll = aiBody.scrollTop;
    tab = t;
    renderTab();
    renderAi();
    if (changed && t === "notes") aiBody.scrollTop = notesScroll; // back where the reader was
    if (focus) (t === "notes" ? tabNotes : tabTranscript).focus();
    // The transcript opens at its end while it records (the newest words), else at the start.
    if (changed && t === "transcript" && isLive()) transcript.scrollTop = transcript.scrollHeight;
    if (changed && videoOf) testLog("tab");
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

  // ---------------------------------------------------------------- Notes
  const running = (e: AiNotesEvent | null) => !!e && ["preparing", "downloading", "writing"].includes(e.state);
  const aiRunning = () => running(aiJob);
  const chatRunning = () => running(chatJob);

  function renderAi(): void {
    if (!shown) return;
    if (isLive()) {
      // The same place as later: the notes come here when the recording stops.
      aiWrite.hidden = aiCancel.hidden = aiProgress.hidden = aiError.hidden = true;
      aiMeta.textContent = "";
      drawn = null;
      aiBody.replaceChildren("Written here when the recording stops: one section for each topic, with its decisions and tasks. The transcript is in the other tab.");
      aiBody.classList.add("ai-empty");
      return;
    }
    const mine = aiJob?.id === shown.id ? aiJob : null;
    const running = !!mine && aiRunning();
    // When there are no notes yet (not written, or the writing failed), or the transcript was
    // fixed after they were written.
    const stale = !!shown.ai_notes && (shown.transcript_version ?? 0) > (shown.ai_version ?? 0);
    aiWrite.hidden = running || (!!shown.ai_notes && !stale) || tab !== "notes";
    aiWrite.textContent = stale ? "Write notes again" : "Write notes";
    aiWrite.title = stale ? "The transcript was fixed after these notes were written" : "";
    aiWrite.disabled = aiRunning() || chatRunning() || voicesWorking.has(shown.id); // at work for something else, or the speakers come first
    if (voicesWorking.has(shown.id)) aiWrite.title = "Wait until the voices are told apart: then the notes have the speakers";
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
    aiBody.classList.toggle("streaming", running && !!mine!.text);
    const linked = !running && !!text && shown.lines.length > 0;
    if (text) {
      // Drawn again only when they change: a chat event must not take the focus from a point.
      if (!linked || drawn?.text !== text || drawn.lines !== shown.lines) {
        renderMarkdown(aiBody, text);
        if (linked) linkPoints();
        drawn = linked ? { text, lines: shown.lines } : null;
      }
      aiBody.classList.remove("ai-empty");
    } else if (running) {
      drawn = null;
      aiBody.replaceChildren(skeleton("The notes are being written."));
      aiBody.classList.remove("ai-empty");
    } else {
      drawn = null;
      aiBody.replaceChildren("No notes yet. Write notes makes one section for each topic of the meeting, with its decisions and tasks, from the transcript.");
      aiBody.classList.add("ai-empty");
    }
    const check = linked ? "click a point to see it in the transcript" : "check it against the transcript";
    if (running && mine!.state === "writing") aiMeta.replaceChildren(spinner(), "Writing the notes…");
    else aiMeta.textContent = running ? "" : shown.ai_notes ? `${shown.ai_model || "AI"} · a draft: ${check}` : "";
  }

  // ---------------------------------------------------------------- Notes → transcript
  function sourceIndex(m: Meeting): NonNullable<typeof sources> {
    if (sources?.lines !== m.lines) {
      const paras = paragraphs(m.lines);
      sources = { lines: m.lines, paras, index: new SourceIndex(paras) };
    }
    return sources;
  }

  /** Each point of the notes (a bullet's own text, a paragraph) becomes a button: a click shows
   * the transcript's paragraphs it most likely comes from, under it. */
  function linkPoints(): void {
    if (!shown) return;
    const id = shown.id;
    let n = 0;
    for (const host of aiBody.querySelectorAll<HTMLElement>(":scope li, :scope > p")) {
      const point = document.createElement("span");
      point.className = "point";
      // The point's own text: what comes before a list inside it (its sub-points).
      while (host.firstChild && !(host.firstChild instanceof HTMLUListElement || host.firstChild instanceof HTMLOListElement)) {
        point.append(host.firstChild);
      }
      host.prepend(point);
      if (!point.textContent?.trim()) continue;
      const k = n++;
      point.tabIndex = 0;
      point.setAttribute("role", "button");
      point.setAttribute("aria-expanded", "false");
      point.title = "Show where the transcript says this";
      const text = point.textContent;
      const isOpen = () => openPoint?.id === id && openPoint.n === k && openPoint.text === text;
      const toggle = () => {
        const was = isOpen();
        closePoint();
        if (!was) {
          openPoint = { id, n: k, text };
          showSources(point, true);
        }
      };
      point.addEventListener("click", () => {
        if (window.getSelection()?.toString()) return; // selecting text to copy it
        toggle();
      });
      point.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          toggle();
        }
      });
      if (isOpen()) showSources(point, false); // drawn again: stays open
    }
  }

  /** The topic's heading, and the point above a sub-point: they help find the place. */
  function pointContext(point: HTMLElement): string[] {
    const out: string[] = [];
    const parent = point.parentElement?.parentElement?.closest("li")?.querySelector(":scope > .point");
    if (parent?.textContent) out.push(parent.textContent);
    let top: Element | null = point;
    while (top && top.parentElement !== aiBody) top = top.parentElement;
    for (let e = top?.previousElementSibling; e; e = e.previousElementSibling) {
      if (e.tagName === "H4" || e.tagName === "H5") {
        out.push(e.textContent ?? "");
        break;
      }
    }
    return out;
  }

  function closePoint(): void {
    aiBody.querySelector(".sources")?.remove();
    aiBody.querySelector('.point[aria-expanded="true"]')?.setAttribute("aria-expanded", "false");
    openPoint = null;
  }

  // Esc closes the open point first (then, again, it goes back to the list).
  aiBody.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || !aiBody.querySelector(".sources")) return;
    e.preventDefault();
    e.stopPropagation();
    const point = aiBody.querySelector<HTMLElement>('.point[aria-expanded="true"]');
    closePoint();
    point?.focus();
  });

  /** `say`: tell a screen reader what was found (not when the notes are only drawn again). */
  function showSources(point: HTMLElement, say: boolean): void {
    if (!shown) return;
    const { paras, index } = sourceIndex(shown);
    const found = index.find(point.textContent ?? "", pointContext(point));
    const box = document.createElement("div");
    box.className = "sources";
    box.setAttribute("role", "group");
    box.setAttribute("aria-label", "In the transcript");
    if (!found.length) {
      const none = document.createElement("p");
      none.className = "sources-none";
      none.textContent = "No part of the transcript matches this point. Check it: the notes are a draft.";
      box.append(none);
    }
    for (const f of found) {
      const p = paras[f.index];
      const b = document.createElement("button");
      b.type = "button";
      b.className = "source";
      b.dataset.speaker = p.speaker;
      const who = document.createElement("span");
      who.className = "who";
      who.textContent = speakerLabel(shown, p.speaker, p.person);
      const at = document.createElement("span");
      at.className = "when";
      at.textContent = clock(p.start);
      const go = document.createElement("span");
      go.className = "go";
      go.textContent = "Show in transcript";
      const text = document.createElement("span");
      text.className = "source-text";
      const part = excerpt(p.text, f.ranges);
      let last = 0;
      for (const [a, z] of part.ranges) {
        if (a > last) text.append(part.text.slice(last, a));
        const mark = document.createElement("mark");
        mark.textContent = part.text.slice(a, z);
        text.append(mark);
        last = z;
      }
      text.append(part.text.slice(last));
      b.append(who, at, go, text);
      // A short name; the excerpt is read as its description.
      text.id = `source-${f.index}`;
      b.setAttribute("aria-label", `Show in transcript: ${who.textContent} at ${at.textContent}`);
      b.setAttribute("aria-describedby", text.id);
      b.addEventListener("click", () => showInTranscript(f.index));
      if (playable(shown)) {
        // The screen at that moment: next to the place, not inside its button.
        const row = document.createElement("div");
        row.className = "source-row";
        const play = document.createElement("button");
        play.type = "button";
        play.className = "btn btn-ghost btn-sm source-play";
        play.textContent = "Play";
        play.title = "Play the screen video from here";
        play.setAttribute("aria-label", `Play the screen video from ${clock(p.start)}`);
        play.addEventListener("click", () => playAt(p.start));
        row.append(b, play);
        box.append(row);
      } else box.append(b);
    }
    point.after(box);
    point.setAttribute("aria-expanded", "true");
    if (say) {
      ui.announce(
        found.length
          ? `Found in ${found.length} place${found.length > 1 ? "s" : ""} of the transcript, under the point.`
          : "No part of the transcript matches this point.",
      );
    }
  }

  /** The transcript in the notes' place, at that paragraph, marked for a moment. */
  function showInTranscript(i: number): void {
    if (!shown) return;
    const p = sourceIndex(shown).paras[i];
    setTab("transcript");
    const row = transcript.querySelectorAll<HTMLElement>(".tline")[i];
    if (!row || !p) return;
    row.scrollIntoView({ block: "center" });
    row.classList.remove("tline-hit");
    void row.offsetWidth; // the animation again, for a second click
    row.classList.add("tline-hit");
    row.querySelector<HTMLElement>("p")?.focus({ preventScroll: true });
    ui.announce(`The transcript, at ${clock(p.start)}.`);
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
      shown.ai_version = e.ai_version ?? shown.ai_version;
      ui.announce("Notes written.");
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
    const body = el("div", done ? "ai-body" : "ai-body streaming");
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
      else {
        // Before the first words: three dots, and what the model does (getting ready, downloading).
        const wait = el("p", "chat-wait");
        const dots = el("span", "typing");
        dots.setAttribute("aria-hidden", "true");
        dots.append(document.createElement("i"), document.createElement("i"), document.createElement("i"));
        wait.append(dots, ev && ev.state !== "writing" && ev.detail ? ev.detail : "Thinking…");
        nodes.push(wait);
      }
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
        ? "Ask when the notes are written"
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
      editing = null; // a fix of another meeting's paragraph ends here (its save still runs)
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

  // The strip's "Open the notes" on a saved meeting: this meeting, on its notes.
  api.onOpen((id) => {
    ui.showMeetings();
    void flush().then(() => openById(id));
  });

  async function showHome(): Promise<void> {
    await flush();
    editing = null;
    shown = null;
    unloadVideo();
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

  const FOUND_IN: Record<string, string> = { transcript: "Transcript", ai_notes: "Notes", notes: "Your notes" };

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
      body: `"${name}", with its transcript, notes and chat, is deleted from this computer. You cannot undo this.`,
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
      ? "No meeting has all of these words in its title, transcript or notes."
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
    const idle = [...startBtn.childNodes];
    startBtn.replaceChildren(spinner(), "Starting…"); // the microphone (and the model) can take a moment
    try {
      live = await api.start(defaultTitle());
      liveAt = performance.now();
      ui.onRecording(true);
      ui.announce("Recording. The notes are written when it stops.");
      open({ ...live.meeting });
      stopBtn.focus();
    } catch (err) {
      showError(String(err));
      startBtn.disabled = false;
    } finally {
      startBtn.replaceChildren(...idle);
    }
  });

  stopBtn.addEventListener("click", async () => {
    stopping = true;
    stopBtn.disabled = true;
    stopBtn.replaceChildren(spinner(), "Saving…");
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

  // ---------------------------------------------------------------- note apps (a folder)
  /** Save to folder: with a folder chosen. Open in Obsidian: once saved in a vault. */
  function renderFolderButtons(): void {
    const info = ui.exportInfo();
    const off = !shown || isLive() || !info.dir;
    folderBtn.hidden = off;
    const waiting = !!shown && voicesWorking.has(shown.id);
    folderBtn.title = waiting ? "Wait until the voices are told apart (it is saved by itself then)" : info.dir ? `Save this meeting as Markdown in ${info.dir}` : "";
    if (!folderBtn.textContent?.startsWith("Saved")) folderBtn.disabled = waiting;
    exportBtn.disabled = waiting;
    exportBtn.title = waiting ? "Wait until the voices are told apart" : "";
    // Saved in this folder (a note in a folder chosen before is not this vault's).
    obsidianBtn.hidden = off || !info.vault || !shown?.export_file || shown.export_dir !== info.dir;
    obsidianBtn.title = info.vault ? `Open its note in the ${info.vault} vault` : "";
  }

  folderBtn.addEventListener("click", async () => {
    if (!shown) return;
    await flush();
    const id = shown.id;
    folderBtn.disabled = true;
    try {
      const r = await api.saveToFolder(id);
      if (shown?.id === id) {
        shown.export_file = r.file;
        shown.export_dir = ui.exportInfo().dir;
      }
      folderBtn.textContent = "Saved";
      ui.announce(`Saved as ${r.file}`);
      setTimeout(() => (folderBtn.textContent = "Save to folder"), 1500);
    } catch (err) {
      showError(String(err));
    } finally {
      folderBtn.disabled = false;
      renderFolderButtons();
    }
  });

  obsidianBtn.addEventListener("click", () => {
    if (shown) void api.openInObsidian(shown.id).catch((err) => showError(String(err)));
  });

  // Saved by itself (after Stop, after the notes): the buttons follow; a problem is shown.
  api.onExport((e) => {
    if (shown?.id === e.id && e.file && e.state === "saved") {
      shown.export_file = e.file;
      shown.export_dir = ui.exportInfo().dir;
    }
    if (shown?.id === e.id && (e.state === "error" || e.state === "kept")) showError(e.detail);
    renderFolderButtons();
  });

  deleteBtn.addEventListener("click", () => {
    if (shown) void confirmDelete(shown.id, title.value.trim() || shown.title);
  });

  // After Stop, with Settings → Models → Tell the other voices apart: the speakers come later.
  api.onVoices((e) => {
    if (e.state === "working") voicesWorking.add(e.id);
    else voicesWorking.delete(e.id);
    if (shown?.id !== e.id) return;
    if (e.state === "done" && e.meeting) {
      const n = e.meeting.people?.length ?? 0;
      if (editing?.meetingId === e.id) {
        // A fix is open: it saves into the old lines; the speakers show after it.
        void api.get(e.id).then((m) => {
          if (shown?.id === e.id && !editing) {
            shown = m;
            renderView();
          }
        });
      } else {
        shown = e.meeting;
        renderView();
      }
      ui.announce(n > 1 ? `The voices are told apart: ${n} speakers.` : n === 1 ? "The voices are told apart: one speaker." : "No voices were told apart.");
    } else if (e.state === "error") {
      showError(e.detail);
      renderView();
    } else renderView();
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

  return { refreshExport: renderFolderButtons };
}
