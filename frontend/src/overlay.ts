import "./overlay.css";
import { Waveform } from "./waveform";

/** Global-hotkey overlay: shows what the server-side dictation is doing. Display only. */
interface DictationState {
  state: "idle" | "starting" | "listening" | "transcribing" | "done" | "error";
  level: number;
  text: string;
  error: string | null;
  detail: string;
  delivered: string[];
  elapsed: number;
}

const root = document.getElementById("overlay")!;
const label = document.getElementById("label")!;
const detail = document.getElementById("detail")!;
const timer = document.getElementById("timer")!;
const waveform = new Waveform(document.getElementById("wave") as HTMLCanvasElement);

let level = 0;
let current: DictationState["state"] = "idle";
let startedAt = 0;
let tick = 0;

function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
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
}

function connect(): void {
  const es = new EventSource("/api/dictation/events");
  es.onmessage = (m) => render(JSON.parse(m.data) as DictationState);
  es.onerror = () => {
    es.close();
    setTimeout(connect, 1000);
  };
}

if ("__TAURI_INTERNALS__" in window) {
  // Desktop app: the dictation state arrives as Tauri events.
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
} else {
  connect();
}
