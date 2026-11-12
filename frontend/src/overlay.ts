import "./overlay.css";
import { LiveMark, Waveform } from "./waveform";

/**
 * The strip at the bottom of the screen: what the dictation is doing. While a meeting shows, the
 * app makes the window a small capsule: the live mark (its bars move with the voices, in the
 * colour of who talks) and the time; on hover Mute, Record screen and Stop. The dictation goes
 * first; the meeting shows when no dictation runs. Drag it to move it; its button collapses the
 * strip to a pill.
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
const screenBtn = document.getElementById("screen") as HTMLButtonElement;
const stopBtn = document.getElementById("stop") as HTMLButtonElement;
const capTime = document.getElementById("cap-time")!;
const capText = document.getElementById("cap-text")!;
const capSr = document.getElementById("cap-sr")!;
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
  /** The screen video: "" | waiting (the screen picker is open) | on. */
  screen?: string;
  /** Record screen can be offered (this build records the screen). */
  screen_supported?: boolean;
}

/** The capsule's mark, in the colour of who talks (overlay.css --me / --them). */
const mark = new LiveMark(document.getElementById("mark")!);
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
  renderShape();
  renderMini();
}

/** The pill's text: the time while a dictation listens, else one word for what it does (a
 * meeting's recording is in the capsule; a call offered, or an error, can be in the pill). */
function renderMini(): void {
  const state = root.dataset.state ?? "";
  const words: Record<string, string> = {
    starting: "Starting",
    transcribing: "",
    done: label.textContent?.startsWith("Typed") ? "Typed" : "Copied",
    error: "Error",
    offer: "Call",
  };
  mini.textContent = state === "listening" ? timer.textContent ?? "" : words[state] ?? "Ready";
  mini.style.color = state === "error" ? "#fca5a5" : "";
}

// ------------------------------------------------------------------ move and collapse
/** The user collapsed the strip (saved); the app made the window the meeting's capsule. */
let collapsed = false;
let capsule = false;
function setPlace(c: boolean, cap: boolean): void {
  collapsed = c;
  capsule = cap;
  renderShape();
}

/** What the capsule shows: a recording, "Saving" and "saved". */
const CAPSULE_STATES = ["recording", "transcribing", "done"];

/** The capsule for a meeting; else the pill (collapsed, or what else shows in the capsule's
 * window: a dictation while a meeting records, a call offered), or the strip. Never the two
 * shapes at once: the pill hides all but the dot and its word, the capsule hides those two,
 * and nothing would be left (an empty circle). */
function renderShape(): void {
  const meeting = root.dataset.mode === "meeting";
  const cap = capsule && meeting && CAPSULE_STATES.includes(root.dataset.state ?? "");
  root.dataset.capsule = cap ? "1" : "";
  root.dataset.collapsed = !cap && (collapsed || capsule) ? "1" : "";
  root.title = capsule ? (meeting ? "Click to open the meeting · drag to move" : "") : collapsed ? "Click to open · drag to move" : "";
}

async function askCollapse(on: boolean): Promise<void> {
  collapsed = on;
  renderShape();
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

/** Record screen / Stop screen: its look and its words follow the meeting. */
function renderScreen(m: MeetingLive | null): void {
  const s = m && m.state === "recording" && m.screen_supported ? (m.screen ?? "") : null;
  root.dataset.screenBtn = s === null ? "" : "1";
  root.dataset.screen = s ?? "";
  screenBtn.setAttribute("aria-pressed", String(s === "on"));
  const what =
    s === "on" ? "Stop recording the screen"
    : s === "waiting" ? "Choose what to share in your desktop's dialog (click to cancel)"
    : s === "stopping" ? "The screen video is being saved"
    : "Record the screen";
  screenBtn.disabled = s === "stopping";
  screenBtn.title = what;
  screenBtn.setAttribute("aria-label", what);
}

stopBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  stopBtn.disabled = true; // the app says "finishing" next
  void call("meeting_stop").catch(() => (stopBtn.disabled = false));
  void call("overlay_unfocus"); // after the click (see the collapse button)
});

screenBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  const on = (meet?.screen ?? "") === "";
  if (meet) meet.screen = on ? "waiting" : "stopping";
  show();
  const before = meet?.screen;
  void call("meeting_screen", { on }).catch(() => {
    if (meet) meet.screen = before; // refused (the app says why in the meeting's notices)
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
// On the whole page: around the capsule the window is transparent, and a press there drags it
// too (else Hyprland focuses the window for it, and nothing gives the focus back).
document.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  if ((e.target as Element).closest("#collapse, #mute, #screen, #stop, .open-link")) return; // their clicks (above)
  press = { x: e.screenX, y: e.screenY, appDrag: call<boolean>("overlay_drag_start").catch(() => false) };
  // The release comes here also when the pointer is outside the window by then (a fast drag
  // leaves the small capsule before the window follows it).
  try {
    root.setPointerCapture(e.pointerId);
  } catch {
    /* no capture: the release over the window still ends the drag */
  }
});
document.addEventListener("pointermove", (e) => {
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
  if (moved) return;
  // A click: the capsule opens its meeting in Lowkey; the pill opens the strip.
  if (capsule) {
    if (root.dataset.capsule === "1" && meet) void call("open_meeting", { id: meet.meeting.id });
  } else if (collapsed) void askCollapse(false);
}
document.addEventListener("pointerup", () => void release());
document.addEventListener("pointercancel", () => void release());

function renderMeeting(now: number): void {
  if (current !== "meeting") {
    waveform.stop();
    clearInterval(tick);
    current = "meeting";
  }
  root.dataset.mode = "meeting";
  root.dataset.preview = "";
  root.dataset.note = "";
  preview.textContent = "";
  capText.title = "";
  if (meetError && now - meetError.at < 4000 && (!meet || meet.state === "done")) {
    mark.stop();
    renderMute(false);
    renderScreen(null);
    root.dataset.state = "error";
    label.textContent = "Meeting not started";
    detail.textContent = meetError.text;
    return;
  }
  const note = callNoteNow(now);
  if (note?.state === "offer" && (!meet || meet.state === "done")) {
    mark.stop();
    renderMute(false);
    renderScreen(null);
    root.dataset.state = "offer";
    label.textContent = `${note.label} call detected`;
    detail.textContent = note.key ? `${prettyKey(note.key)} records it` : "Record it in Meetings";
    return;
  }
  const m = meet!;
  const elapsed = m.state === "recording" ? m.elapsed + (now - meetAt) / 1000 : m.elapsed;
  root.dataset.state = m.state === "recording" ? "recording" : m.state === "finishing" ? "transcribing" : "done";
  renderMute(m.state === "recording" && !!m.muted);
  renderScreen(m);
  stopBtn.disabled = m.state !== "recording";
  if (m.state === "recording") {
    mark.muted = !!m.muted;
    mark.start();
    capTime.textContent = clock(elapsed * 1000); // aria-hidden: no announcement every second
    capSr.textContent = m.muted ? "Recording a meeting. Your microphone is muted." : "Recording a meeting.";
    // A call that ended (stop it), or a recording that started by itself: a word for a while.
    if (note?.state === "ended" || note?.state === "auto") {
      root.dataset.note = note.state;
      capText.textContent = note.state === "ended" ? "Call ended" : `${note.label} call`;
      capText.title =
        note.state === "ended"
          ? `The ${note.label} call ended${note.key ? ` · ${prettyKey(note.key)} stops` : ""}`
          : `Started automatically · ${note.label} call`;
    } else capText.textContent = "";
  } else if (m.state === "finishing") {
    mark.stop();
    capSr.textContent = "";
    capText.textContent = "Saving";
    capText.title = m.pending ? `${m.pending} part${m.pending > 1 ? "s" : ""} to transcribe` : "";
  } else {
    mark.stop();
    capSr.textContent = "Meeting saved.";
    // A link to the meeting: Lowkey opens on its notes.
    const id = m.meeting.id;
    if (openLink.dataset.id !== id || openLink.parentElement !== capText) {
      openLink.dataset.id = id;
      capText.replaceChildren(openLink);
    }
    capText.title = `${clock(m.meeting.duration * 1000)} recorded`;
  }
}

function render(d: DictationState): void {
  if (root.dataset.mode === "meeting") mark.stop();
  renderMute(false);
  renderScreen(null);
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
    if (meet.state === "recording") {
      meetError = null;
      // A meeting records: the call offer is taken (or not needed). Kept, it would come back
      // in place of the saved meeting when a short meeting ends within its 20 s.
      if (callNote?.state === "offer") callNote = null;
    }
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
    mark.levels = e.payload;
  });
  await listen<{ meeting_key?: string }>("status", (e) => (meetingKey = e.payload.meeting_key ?? meetingKey));
  await listen<{ collapsed: boolean; capsule?: boolean }>("overlay-place", (e) => {
    setPlace(e.payload.collapsed, !!e.payload.capsule);
    show();
  });
  await listen<boolean>("overlay-hold", (e) => {
    held = e.payload;
    show();
  });
  // The clock, and the moment after "done" / "saved" when the view changes.
  window.setInterval(() => {
    if (meet || dict) show();
  }, 500);
  const st = await invoke<{ meeting_key?: string; overlay_collapsed?: boolean; overlay_capsule?: boolean }>("get_status");
  meetingKey = st.meeting_key ?? "";
  setPlace(!!st.overlay_collapsed, !!st.overlay_capsule);
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
