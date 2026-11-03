import "./overlay.css";
import { DualWaveform, Waveform } from "./waveform";

/**
 * The strip at the bottom of the screen: what the dictation is doing, and while a meeting
 * records, the recording (time, levels, the newest line). The dictation goes first; the
 * meeting shows when no dictation runs. Display only.
 */
interface DictationState {
  state: "idle" | "starting" | "listening" | "transcribing" | "done" | "error";
  level: number;
  text: string;
  error: string | null;
  detail: string;
  delivered: string[];
  elapsed: number;
  /** Live preview words (Whistle); never what gets typed. */
  preview?: string;
}

const root = document.getElementById("overlay")!;
const label = document.getElementById("label")!;
const detail = document.getElementById("detail")!;
const timer = document.getElementById("timer")!;
const preview = document.getElementById("preview")!;
const measure = document.createElement("canvas").getContext("2d")!;
const waveform = new Waveform(document.getElementById("wave") as HTMLCanvasElement);

/** A meeting as the app sends it (desktop/src-tauri/src/meeting.rs, Live). */
interface MeetingLive {
  meeting: { id: string; title: string; duration: number; system_audio: boolean; lines: { speaker: "me" | "them"; text: string }[] };
  state: "recording" | "finishing" | "done";
  elapsed: number;
  pending: number;
}

/** Me above the line, Them below, in their colours (overlay.css --me / --them). */
const mwave = new DualWaveform(document.getElementById("mwave") as HTMLCanvasElement);
let dict: DictationState | null = null;
let dictAt = 0; // when the dictation state arrived (done/error show for a moment)
let meet: MeetingLive | null = null;
let meetAt = 0;
let meetError: { text: string; at: number } | null = null;
/** Call detection: an offer, an ended call, or a recording started by itself (each for a while). */
let callNote: { state: "offer" | "ended" | "auto"; label: string; key: string; at: number } | null = null;
const CALL_NOTE_MS = { offer: 20_000, ended: 20_000, auto: 10_000 };
let meetingKey = "";

let level = 0;
let current: DictationState["state"] | "meeting" = "idle";
let startedAt = 0;
let tick = 0;

function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** The newest words that fit on the line, with "… " in front when older words are cut. */
function fitTail(text: string, el: HTMLElement, prefix = ""): string {
  measure.font = getComputedStyle(el).font;
  const width = el.clientWidth - (prefix ? measure.measureText(prefix).width : 0);
  if (!width || measure.measureText(text).width <= width) return text;
  const words = text.split(/\s+/);
  while (words.length > 1 && measure.measureText(`… ${words.join(" ")}`).width > width) words.shift();
  return `… ${words.join(" ")}`;
}

function renderPreview(d: DictationState): void {
  const text = (d.state === "listening" || d.state === "transcribing" ? d.preview ?? "" : "").trim();
  root.dataset.preview = text ? "1" : "";
  preview.textContent = text ? fitTail(text, preview) : "";
}

/** "SUPER + CTRL + M" → "Super+Ctrl+M". */
function prettyKey(key: string): string {
  return key
    .split("+")
    .map((p) => p.trim())
    .map((p) => (p.length > 1 && !/^F\d+$/.test(p) ? p[0] + p.slice(1).toLowerCase() : p))
    .join("+");
}

function dictationShows(now: number): boolean {
  if (!dict) return false;
  if (["starting", "listening", "transcribing"].includes(dict.state)) return true;
  return (dict.state === "done" || dict.state === "error") && now - dictAt < 2500;
}

function callNoteNow(now: number) {
  return callNote && now - callNote.at < CALL_NOTE_MS[callNote.state] ? callNote : null;
}

function meetingShows(now: number): boolean {
  if (meetError && now - meetError.at < 4000) return true;
  if (callNoteNow(now)?.state === "offer") return true;
  if (!meet) return false;
  return meet.state !== "done" || now - meetAt < 3500;
}

function show(): void {
  const now = performance.now();
  if (!dictationShows(now) && meetingShows(now)) renderMeeting(now);
  else render(dict ?? { state: "idle", level: 0, text: "", error: null, detail: "", delivered: [], elapsed: 0 });
}

/** "Them  … the newest words": the speaker always shows (in its colour), older words are cut. */
function speakerLine(who: "me" | "them", text: string): void {
  const name = document.createElement("b");
  name.className = who === "me" ? "who-me" : "who-them";
  name.textContent = who === "me" ? "Me" : "Them";
  detail.replaceChildren(name, " " + fitTail(text, detail, `${name.textContent}  `));
}

function renderMeeting(now: number): void {
  if (current !== "meeting") {
    waveform.stop();
    clearInterval(tick);
    current = "meeting";
  }
  if (meet?.state === "recording") mwave.start();
  else mwave.stop();
  root.dataset.mode = "meeting";
  root.dataset.preview = "";
  preview.textContent = "";
  if (meetError && now - meetError.at < 4000 && (!meet || meet.state === "done")) {
    root.dataset.state = "error";
    label.textContent = "Meeting not started";
    detail.textContent = meetError.text;
    return;
  }
  const note = callNoteNow(now);
  if (note?.state === "offer" && (!meet || meet.state === "done")) {
    root.dataset.state = "offer";
    label.textContent = `${note.label} call detected`;
    detail.textContent = note.key ? `${prettyKey(note.key)} records it` : "Record it in Lowkey → Meetings";
    return;
  }
  const m = meet!;
  const elapsed = m.state === "recording" ? m.elapsed + (now - meetAt) / 1000 : m.elapsed;
  root.dataset.state = m.state === "recording" ? "recording" : m.state === "finishing" ? "transcribing" : "done";
  const last = m.meeting.lines[m.meeting.lines.length - 1];
  if (m.state === "recording") {
    label.textContent = "Recording";
    timer.textContent = clock(elapsed * 1000); // aria-hidden: no announcement every second
    const stop = meetingKey ? ` · ${prettyKey(meetingKey)} stops` : "";
    if (note?.state === "ended") detail.textContent = `The ${note.label} call ended${note.key ? ` · ${prettyKey(note.key)} stops` : ""}`;
    else if (note?.state === "auto") detail.textContent = `Started automatically · ${note.label} call`;
    else if (last) speakerLine(last.speaker, last.text);
    else detail.textContent = `${m.meeting.system_audio ? "Your mic and the computer's sound" : "Your mic only"}${stop}`;
  } else if (m.state === "finishing") {
    label.textContent = "Saving the meeting…";
    detail.textContent = m.pending ? `${m.pending} part${m.pending > 1 ? "s" : ""} to transcribe` : "One moment";
  } else {
    label.textContent = "Meeting saved";
    detail.textContent = `${clock(m.meeting.duration * 1000)} · open it in Lowkey → Meetings`;
  }
}

function render(d: DictationState): void {
  if (root.dataset.mode === "meeting") mwave.stop();
  delete root.dataset.mode;
  level = d.level;
  if (d.state !== current) {
    if (d.state === "listening") {
      waveform.start(() => level);
      startedAt = performance.now() - d.elapsed * 1000;
      clearInterval(tick);
      tick = window.setInterval(() => (timer.textContent = clock(performance.now() - startedAt)), 250);
    } else {
      waveform.stop();
      clearInterval(tick);
    }
    current = d.state;
    root.dataset.state = d.state;
  }
  switch (d.state) {
    case "listening":
      label.textContent = "Listening";
      detail.textContent = "release or tap the key to finish";
      break;
    case "starting":
      // The key started the app: show start-up / first-run download until it can listen.
      label.textContent = "Getting ready…";
      detail.textContent = d.detail;
      break;
    case "transcribing":
      label.textContent = "Transcribing…";
      detail.textContent = d.detail; // e.g. "Still transcribing the last one", download progress
      break;
    case "done":
      label.textContent =
        d.delivered.length === 2 ? "Typed · copied" : d.delivered.includes("typed") ? "Typed" : "Copied";
      detail.textContent = d.text;
      break;
    case "error":
      label.textContent = "Not transcribed";
      detail.textContent = d.error ?? "";
      break;
    default:
      label.textContent = "Ready";
      detail.textContent = "";
  }
  renderPreview(d);
}

// The dictation state arrives as Tauri events.
void (async () => {
  const { listen } = await import("@tauri-apps/api/event");
  const { invoke } = await import("@tauri-apps/api/core");
  await listen<DictationState>("dictation", (e) => {
    dict = e.payload;
    dictAt = performance.now();
    show();
  });
  await listen<MeetingLive>("meeting", (e) => {
    meet = e.payload;
    meetAt = performance.now();
    if (meet.state === "recording") meetError = null;
    show();
  });
  await listen<{ result: { Ok?: string; Err?: string } }>("meeting-start", (e) => {
    if (e.payload.result.Err) {
      meetError = { text: e.payload.result.Err, at: performance.now() };
      show();
    }
  });
  await listen<{ state: string; label?: string; meeting_key?: string; auto?: boolean }>("call", (e) => {
    const p = e.payload;
    if (p.state === "offer" || p.state === "auto" || (p.state === "ended" && !p.auto)) {
      callNote = { state: p.state as "offer" | "ended" | "auto", label: p.label ?? "", key: p.meeting_key ?? meetingKey, at: performance.now() };
    } else if (p.state === "gone" && callNote?.state === "offer") {
      callNote = null;
    }
    show();
  });
  await listen<{ me: number; them: number }>("meeting-level", (e) => {
    mwave.levels = e.payload;
  });
  await listen<{ meeting_key?: string }>("status", (e) => (meetingKey = e.payload.meeting_key ?? meetingKey));
  // The clock, and the moment after "done" / "saved" when the view changes.
  window.setInterval(() => {
    if (meet || dict) show();
  }, 500);
  meetingKey = (await invoke<{ meeting_key?: string }>("get_status")).meeting_key ?? "";
  const live = await invoke<MeetingLive | null>("meeting_live");
  if (live) {
    meet = live;
    meetAt = performance.now();
  }
  const first = await invoke<DictationState | null>("dictation_state");
  if (first) {
    dict = first;
    dictAt = performance.now();
  }
  show();
  // Let the app show this window only now that the page is loaded and rendered (no blank
  // first frame). Not in requestAnimationFrame: a hidden page gets no animation frames.
  await invoke("overlay_ready");
})();
