import "./overlay.css";
import { Waveform } from "./waveform";

/** Global-key overlay: shows what the app's dictation is doing. Display only. */
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

let level = 0;
let current: DictationState["state"] = "idle";
let startedAt = 0;
let tick = 0;

function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** The newest words that fit on the line, with "… " in front when older words are cut. */
function fitTail(text: string, el: HTMLElement): string {
  measure.font = getComputedStyle(el).font;
  const width = el.clientWidth;
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

function render(d: DictationState): void {
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
  await listen<DictationState>("dictation", (e) => render(e.payload));
  const first = await invoke<DictationState | null>("dictation_state");
  if (first) render(first);
  // Let the app show this window only now that the page is loaded and rendered (no blank
  // first frame). Not in requestAnimationFrame: a hidden page gets no animation frames.
  await invoke("overlay_ready");
})();
