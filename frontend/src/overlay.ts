import "./overlay.css";
import { DualWaveform, Waveform } from "./waveform";

/**
 * The strip at the bottom of the screen: what the dictation is doing, and while a meeting
 * records, the recording (time, levels, the newest line). The dictation goes first; the
 * meeting shows when no dictation runs. Drag it to move it; its button collapses it to a pill.
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
const mini = document.getElementById("mini")!;
const collapseBtn = document.getElementById("collapse") as HTMLButtonElement;
const muteBtn = document.getElementById("mute") as HTMLButtonElement;
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
  /** Your microphone is muted (silence goes in its place). */
  muted?: boolean;
  /** The screen video: "" | waiting | on | stopped. */
  screen?: string;
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

/** How long a saved meeting (and its link) shows; longer while the app holds the strip (the
 * pointer is on it). */
const SAVED_MS = 8500;
let held = false;

function meetingShows(now: number): boolean {
  if (meetError && now - meetError.at < 4000) return true;
  if (callNoteNow(now)?.state === "offer") return true;
  if (!meet) return false;
  return meet.state !== "done" || now - meetAt < SAVED_MS || held;
}

function show(): void {
  const now = performance.now();
  // A dictation at work comes first; a finished one's short message never covers a meeting
  // that records or saves (its mute button would be under the dictation's text).
  const dictating = !!dict && ["starting", "listening", "transcribing"].includes(dict.state);
  const meetingActive = !!meet && meet.state !== "done";
  if (meetingShows(now) && (!dictationShows(now) || (meetingActive && !dictating))) renderMeeting(now);
  else render(dict ?? { state: "idle", level: 0, text: "", error: null, detail: "", delivered: [], elapsed: 0 });
  renderMini();
}

/** The pill's text: the time while it records, else one word for what it does. */
function renderMini(): void {
  const state = root.dataset.state ?? "";
  const meeting = root.dataset.mode === "meeting";
  const words: Record<string, string> = {
    starting: "Starting",
    transcribing: meeting ? "Saving" : "",
    done: meeting ? "Saved" : label.textContent?.startsWith("Typed") ? "Typed" : "Copied",
    error: "Error",
    offer: "Call",
  };
  mini.textContent = state === "listening" || state === "recording" ? timer.textContent ?? "" : words[state] ?? "Ready";
  mini.style.color = state === "recording" ? "#f4f4f5" : state === "error" ? "#fca5a5" : "";
}

// ------------------------------------------------------------------ move and collapse
let collapsed = false;
function setCollapsed(on: boolean): void {
  collapsed = on;
  root.dataset.collapsed = on ? "1" : "";
  root.title = on ? "Click to open · drag to move" : "";
}

async function askCollapse(on: boolean): Promise<void> {
  setCollapsed(on);
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("overlay_collapse", { collapsed: on });
}

/** The mute button: its look and its words follow the meeting (the app says when it changed). */
function renderMute(muted: boolean): void {
  root.dataset.muted = muted ? "1" : "";
  muteBtn.setAttribute("aria-pressed", String(muted));
  const what = muted ? "Unmute your microphone" : "Mute your microphone";
  muteBtn.title = what;
  muteBtn.setAttribute("aria-label", what);
}

muteBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  const on = !meet?.muted;
  if (meet) meet.muted = on;
  renderMute(on);
  show();
  void call("meeting_mute", { muted: on }).catch(() => {
    if (meet) meet.muted = !on;
    show();
  });
  void call("overlay_unfocus"); // after the click (see the collapse button)
});

// "Open the notes" on a saved meeting: the Lowkey window opens on it (the app hides the strip).
const openLink = document.createElement("a");
openLink.className = "open-link";
openLink.href = "#";
openLink.textContent = "Open the notes";
openLink.addEventListener("click", (e) => {
  e.preventDefault();
  e.stopPropagation();
  const id = openLink.dataset.id;
  if (id) void call("open_meeting", { id });
});

// Where Hyprland does not say where the cursor is, the page tells the app when the pointer is on
// the strip (it keeps a saved meeting there while it is).
root.addEventListener("pointerenter", () => void call("overlay_hover", { on: true }));
root.addEventListener("pointerleave", () => void call("overlay_hover", { on: false }));

collapseBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  void askCollapse(true);
  // Hyprland focused the strip on the press: back to the app you type in (only now: given
  // back on the press, the click would be cancelled).
  void call("overlay_unfocus");
});

// Drag: on Hyprland the app moves the strip with the pointer while the button is down (and
// gives the focus straight back to the app you type in: Hyprland focuses a clicked window).
// Elsewhere the system drags it (the window never takes the focus). A press that does not
// move is a click: on the pill, it opens the strip again.
let press: { x: number; y: number; appDrag: Promise<boolean> } | null = null;
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}
root.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  if ((e.target as Element).closest("#collapse, #mute, .open-link")) return; // their clicks (above)
  press = { x: e.screenX, y: e.screenY, appDrag: call<boolean>("overlay_drag_start").catch(() => false) };
});
root.addEventListener("pointermove", (e) => {
  const p = press;
  if (!p || !(e.buttons & 1) || Math.hypot(e.screenX - p.x, e.screenY - p.y) < 4) return;
  void p.appDrag.then((appDrag) => {
    if (appDrag || press !== p) return;
    press = null; // the system drags it: no "up" comes back to the page
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().startDragging());
  });
});
async function release(): Promise<void> {
  const p = press;
  press = null;
  if (!p) return;
  const moved = (await p.appDrag) ? await call<boolean>("overlay_drag_end") : false;
  if (!moved && collapsed) void askCollapse(false);
}
root.addEventListener("pointerup", () => void release());
root.addEventListener("pointercancel", () => void release());

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
    detail.textContent = note.key ? `${prettyKey(note.key)} records it` : "Record it in Meetings";
    return;
  }
  const m = meet!;
  const elapsed = m.state === "recording" ? m.elapsed + (now - meetAt) / 1000 : m.elapsed;
  root.dataset.state = m.state === "recording" ? "recording" : m.state === "finishing" ? "transcribing" : "done";
  const last = m.meeting.lines[m.meeting.lines.length - 1];
  renderMute(m.state === "recording" && !!m.muted);
  root.dataset.screen = m.state === "recording" && m.screen === "on" ? "1" : "";
  if (m.state === "recording") {
    label.textContent = ""; // the time, the wave and the mute button say it (no "Recording")
    timer.textContent = clock(elapsed * 1000); // aria-hidden: no announcement every second
    const stop = meetingKey ? ` · ${prettyKey(meetingKey)} stops` : "";
    if (m.muted) {
      const tag = document.createElement("span");
      tag.className = "muted-note";
      tag.textContent = "Mic muted";
      const rest = m.meeting.system_audio
        ? m.screen === "on" ? " · only the computer's sound and the screen are recorded" : " · only the computer's sound is recorded"
        : m.screen === "on" ? " · only the screen is recorded" : "";
      detail.replaceChildren(tag, rest);
    } else if (note?.state === "ended") detail.textContent = `The ${note.label} call ended${note.key ? ` · ${prettyKey(note.key)} stops` : ""}`;
    else if (note?.state === "auto") detail.textContent = `Started automatically · ${note.label} call`;
    else if (last) speakerLine(last.speaker, last.text);
    else {
      const what = m.meeting.system_audio ? "Your mic and the computer's sound" : "Your mic only";
      detail.textContent = `${m.screen === "on" ? what.replace(" and ", ", ").replace(" only", "") + " and the screen" : what}${stop}`;
    }
  } else if (m.state === "finishing") {
    label.textContent = "Saving the meeting…";
    detail.textContent = m.pending ? `${m.pending} part${m.pending > 1 ? "s" : ""} to transcribe` : "One moment";
  } else {
    label.textContent = "Meeting saved";
    // A link to the meeting: Lowkey opens on its notes.
    const id = m.meeting.id;
    if (openLink.dataset.id !== id || openLink.parentElement !== detail) {
      openLink.dataset.id = id;
      detail.replaceChildren(`${clock(m.meeting.duration * 1000)} · `, openLink);
    }
  }
}

function render(d: DictationState): void {
  if (root.dataset.mode === "meeting") mwave.stop();
  renderMute(false);
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
      label.textContent = ""; // the spinning ring says it
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
    // A late word about a meeting that ended, while another one records: not shown.
    if (e.payload.state === "done" && meet?.state === "recording" && meet.meeting.id !== e.payload.meeting.id) return;
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
  await listen<{ collapsed: boolean }>("overlay-place", (e) => setCollapsed(e.payload.collapsed));
  await listen<boolean>("overlay-hold", (e) => {
    held = e.payload;
    show();
  });
  // The clock, and the moment after "done" / "saved" when the view changes.
  window.setInterval(() => {
    if (meet || dict) show();
  }, 500);
  const st = await invoke<{ meeting_key?: string; overlay_collapsed?: boolean }>("get_status");
  meetingKey = st.meeting_key ?? "";
  setCollapsed(!!st.overlay_collapsed);
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
