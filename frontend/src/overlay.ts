import "./overlay.css";
import { Waveform } from "./waveform";

/** Global-hotkey overlay: shows what the server-side dictation is doing. Display only. */
interface DictationState {
  state: "idle" | "listening" | "transcribing" | "done" | "error";
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

connect();
