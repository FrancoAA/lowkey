import "./style.css";
import { followJob, getLanguages, getStatus, startJob, type JobEvent, type JobResult, type ModelInfo, type Segment, type Status } from "./api";
import { Dictation, type DictationState } from "./dictation";
import { Waveform } from "./waveform";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const dropzone = $<HTMLLabelElement>("dropzone");
const fileInput = $<HTMLInputElement>("file-input");
const languageSelect = $<HTMLSelectElement>("language-select");
const modelSelect = $<HTMLSelectElement>("model-select");
const errorBox = $<HTMLDivElement>("error");
const resultCard = $<HTMLElement>("result");
const resultTitle = $<HTMLHeadingElement>("result-title");
const resultMeta = $<HTMLParagraphElement>("result-meta");
const progress = $<HTMLDivElement>("progress");
const progressFill = $<HTMLDivElement>("progress-fill");
const progressText = $<HTMLSpanElement>("progress-text");
const transcriptEl = $<HTMLDivElement>("transcript");
const copyButton = $<HTMLButtonElement>("copy-button");
const downloadButtons = [...document.querySelectorAll<HTMLButtonElement>("button.download")];
const modelStatus = $<HTMLDivElement>("model-status");
const modelStatusText = $<HTMLSpanElement>("model-status-text");
const modelBar = modelStatus.querySelector<HTMLDivElement>(".model-bar")!;
const modelBarFill = $<HTMLDivElement>("model-bar-fill");
const dictationBox = $<HTMLDivElement>("dictation");
const micButton = $<HTMLButtonElement>("mic-button");
const dictationLabel = $<HTMLSpanElement>("dictation-label");
const dictationHint = $<HTMLSpanElement>("dictation-hint");
const dictationTimer = $<HTMLSpanElement>("dictation-timer");

const MEDIA_EXT = /\.(mp3|wav|wave|m4a|aac|ogg|oga|opus|flac|webm|weba|wma|aiff?|amr|mp4|m4v|mov|mkv|avi|3gp)$/i;
const langNames = new Intl.DisplayNames(["en"], { type: "language" });

let status: Status | null = null;
let busy = false;
let current: { result: JobResult | null; segments: Segment[]; baseName: string } = { result: null, segments: [], baseName: "transcript" };

// ------------------------------------------------------------------ helpers
function showError(message: string | null): void {
  errorBox.hidden = !message;
  errorBox.textContent = message ?? "";
}

function fmtMB(bytes: number): string {
  return `${Math.round(bytes / 1024 / 1024)} MB`;
}

function fmtClock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function langName(code: string | null | undefined): string {
  if (!code) return "";
  try {
    return langNames.of(code) ?? code;
  } catch {
    return code;
  }
}

function selectedModel(): ModelInfo | undefined {
  return status?.models.find((m) => m.name === (modelSelect.value || status?.default_model));
}

function modelPhrase(m: ModelInfo): string {
  switch (m.state) {
    case "downloading":
      return m.total ? `Downloading model… ${Math.floor(m.progress * 100)}% (${fmtMB(m.downloaded)} of ${fmtMB(m.total)})` : "Downloading model…";
    case "loading":
      return "Loading model…";
    case "ready":
      return `Ready · ${m.label.split(" · ")[0]} on ${status?.device === "cuda" ? "GPU" : "CPU"}`;
    case "error":
      return m.error ?? "Model error";
    default:
      return "Preparing model…";
  }
}

// ------------------------------------------------------------------ status
function renderStatus(): void {
  const m = selectedModel();
  if (!m) return;
  modelStatus.dataset.state = m.state;
  modelStatusText.textContent = modelPhrase(m);
  modelBar.hidden = m.state !== "downloading";
  modelBarFill.style.width = `${Math.floor(m.progress * 100)}%`;
  for (const opt of modelSelect.options) {
    const info = status!.models.find((x) => x.name === opt.value)!;
    const here = info.state === "missing" ? ` (download ~${info.approx_mb} MB)` : "";
    opt.textContent = `${info.label}${info.name === status!.default_model ? " — default" : ""}${here}`;
  }
}

let statusTimer = 0;
async function refreshStatus(): Promise<void> {
  try {
    status = await getStatus();
    if (!modelSelect.options.length) {
      for (const m of status.models) modelSelect.add(new Option(m.label, m.name));
      const saved = localStorage.getItem("ow.model");
      modelSelect.value = saved && status.models.some((m) => m.name === saved) ? saved : status.default_model;
    }
    renderStatus();
  } catch {
    modelStatus.dataset.state = "error";
    modelStatusText.textContent = "Server not reachable";
  }
  const m = selectedModel();
  const settling = !m || ["missing", "downloading", "downloaded", "loading"].includes(m.state);
  clearTimeout(statusTimer);
  statusTimer = window.setTimeout(refreshStatus, settling ? 1000 : 10000);
}

async function loadLanguages(): Promise<void> {
  try {
    const langs = await getLanguages();
    const entries = Object.keys(langs)
      .map((code) => [code, langName(code)] as const)
      .sort((a, b) => a[1].localeCompare(b[1]));
    for (const [code, name] of entries) languageSelect.add(new Option(name, code));
    const saved = localStorage.getItem("ow.language");
    if (saved && langs[saved] !== undefined) languageSelect.value = saved;
  } catch {
    /* Auto-detect still works without the list */
  }
}

// ------------------------------------------------------------------ transcription
function setResultReady(ready: boolean): void {
  copyButton.disabled = !ready;
  for (const b of downloadButtons) b.disabled = !ready;
}

function renderTranscript(): void {
  const text = current.result?.text ?? current.segments.map((s) => s.text).join(" ");
  transcriptEl.textContent = text;
  transcriptEl.classList.toggle("empty", !text);
}

function setProgress(fraction: number | null, text: string): void {
  progress.hidden = false;
  progress.classList.toggle("indeterminate", fraction === null);
  progressFill.style.width = fraction === null ? "30%" : `${Math.max(2, Math.round(fraction * 100))}%`;
  progressText.textContent = text;
}

function onJobEvent(ev: JobEvent): void {
  if (ev.new_segments && ev.segment_count !== undefined) {
    // Idempotent merge: a reconnecting EventSource resends from the start.
    current.segments = current.segments.slice(0, ev.segment_count - ev.new_segments.length).concat(ev.new_segments);
    renderTranscript();
  }
  if (ev.language) {
    const pct = ev.language_probability ? ` (${Math.round(ev.language_probability * 100)}%)` : "";
    resultMeta.textContent = `Language: ${langName(ev.language)}${languageSelect.value === "auto" ? ` · detected${pct}` : ""}`;
  }
  switch (ev.state) {
    case "queued":
      setProgress(null, "Waiting for the previous job…");
      break;
    case "preparing": {
      const m = ev.model_state;
      if (m?.state === "downloading") setProgress(m.progress, `First run: ${modelPhrase(m).replace("Downloading model…", "downloading the model…")}`);
      else setProgress(null, m?.state === "loading" ? "Loading model…" : "Preparing…");
      if (m && status) {
        status.models = status.models.map((x) => (x.name === m.name ? { ...x, ...m } : x));
        renderStatus();
      }
      break;
    }
    case "transcribing":
      setProgress(ev.progress ?? 0, `Transcribing… ${Math.floor((ev.progress ?? 0) * 100)}%`);
      break;
    case "done":
      busy = false;
      current.result = ev.result ?? null;
      progress.hidden = true;
      renderTranscript();
      if (!current.result?.text) transcriptEl.textContent = "No speech was found in this audio.";
      setResultReady(Boolean(current.result?.text));
      void refreshStatus();
      break;
    case "error":
      busy = false;
      progress.hidden = true;
      showError(ev.error ?? "Transcription failed.");
      void refreshStatus();
      break;
  }
}

async function transcribe(blob: Blob, filename: string, title: string): Promise<void> {
  if (busy) {
    showError("A transcription is already running. Wait for it to finish.");
    return;
  }
  showError(null);
  busy = true;
  current = { result: null, segments: [], baseName: filename.replace(/\.[^.]+$/, "") || "transcript" };
  resultCard.hidden = false;
  resultTitle.textContent = title;
  resultMeta.textContent = "";
  setResultReady(false);
  renderTranscript();
  setProgress(null, "Uploading…");
  try {
    const jobId = await startJob(blob, filename, languageSelect.value, modelSelect.value);
    followJob(jobId, onJobEvent, () => {
      busy = false;
      progress.hidden = true;
      showError("Lost the connection to the server. Is it still running?");
    });
  } catch (err) {
    busy = false;
    progress.hidden = true;
    resultCard.hidden = !current.segments.length;
    showError(err instanceof Error ? err.message : String(err));
  }
}

function handleFile(file: File | undefined): void {
  if (!file) return;
  const looksLikeMedia = file.type.startsWith("audio/") || file.type.startsWith("video/") || MEDIA_EXT.test(file.name);
  if (!looksLikeMedia) {
    showError(`“${file.name}” is not an audio or video file. Try MP3, WAV, M4A, OGG, FLAC, WEBM or a video with sound.`);
    return;
  }
  void transcribe(file, file.name, file.name);
}

// ------------------------------------------------------------------ file input + drag and drop
fileInput.addEventListener("change", () => {
  handleFile(fileInput.files?.[0]);
  fileInput.value = "";
});
dropzone.addEventListener("keydown", (e) => {
  if (e.key === "Enter") fileInput.click();
});
for (const type of ["dragenter", "dragover"]) {
  document.addEventListener(type, (e) => {
    e.preventDefault();
    dropzone.classList.add("over");
  });
}
document.addEventListener("dragleave", (e) => {
  if (!(e as DragEvent).relatedTarget) dropzone.classList.remove("over");
});
document.addEventListener("drop", (e) => {
  e.preventDefault();
  dropzone.classList.remove("over");
  handleFile((e as DragEvent).dataTransfer?.files[0]);
});

// ------------------------------------------------------------------ copy / download
copyButton.addEventListener("click", async () => {
  const text = current.result?.text ?? "";
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = Object.assign(document.createElement("textarea"), { value: text });
    document.body.append(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
  copyButton.textContent = "Copied";
  setTimeout(() => (copyButton.textContent = "Copy"), 1500);
});

for (const btn of downloadButtons) {
  btn.addEventListener("click", () => {
    const r = current.result;
    if (!r) return;
    const fmt = btn.dataset.format as "txt" | "srt" | "vtt";
    const body = fmt === "txt" ? `${r.text}\n` : r[fmt];
    const type = fmt === "vtt" ? "text/vtt" : fmt === "srt" ? "application/x-subrip" : "text/plain";
    const url = URL.createObjectURL(new Blob([body], { type: `${type};charset=utf-8` }));
    const a = Object.assign(document.createElement("a"), { href: url, download: `${current.baseName}.${fmt}` });
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
}

// ------------------------------------------------------------------ options
languageSelect.addEventListener("change", () => localStorage.setItem("ow.language", languageSelect.value));
modelSelect.addEventListener("change", () => {
  localStorage.setItem("ow.model", modelSelect.value);
  renderStatus();
});

// ------------------------------------------------------------------ dictation
const waveform = new Waveform($<HTMLCanvasElement>("waveform"));
const dictation = new Dictation(waveform, {
  onState(state: DictationState, elapsed: number) {
    dictationBox.dataset.state = state;
    micButton.setAttribute("aria-label", state === "idle" ? "Start dictation" : "Stop dictation");
    micButton.setAttribute("aria-pressed", String(state !== "idle"));
    dictationTimer.hidden = state !== "listening";
    dictationTimer.textContent = fmtClock(elapsed);
    if (state === "idle") {
      dictationLabel.textContent = "Dictate";
      dictationHint.innerHTML = "Hold <kbd>Space</kbd> to talk, or tap it to start and stop";
    } else if (state === "starting") {
      dictationLabel.textContent = "Opening microphone…";
      dictationHint.textContent = "Allow microphone access if the browser asks";
    } else {
      dictationLabel.textContent = "Listening…";
      dictationHint.innerHTML = held ? "Release <kbd>Space</kbd> to transcribe · <kbd>Esc</kbd> cancels" : "Press <kbd>Space</kbd> or the mic to transcribe · <kbd>Esc</kbd> cancels";
    }
  },
  onAudio(blob, filename) {
    void transcribe(blob, filename, "Dictation");
  },
  onError(message) {
    showError(message);
  },
});

const HOLD_MS = 350;
let spaceDownAt = 0;
let held = false; // true while Space is physically held for push-to-talk
let holdTimer = 0;

function isTyping(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  return !!el && (el.isContentEditable || ["INPUT", "SELECT", "TEXTAREA"].includes(el.tagName));
}

function toggleDictation(): void {
  if (dictation.state === "idle") {
    if (busy) return showError("A transcription is already running. Wait for it to finish.");
    showError(null);
    void dictation.start();
  } else dictation.stop();
}

document.addEventListener(
  "keydown",
  (e) => {
    if (e.code === "Escape" && dictation.state !== "idle") {
      e.preventDefault();
      dictation.cancel();
      return;
    }
    if (e.code !== "Space" || e.ctrlKey || e.metaKey || e.altKey || isTyping(e.target)) return;
    e.preventDefault(); // no page scroll, no accidental button "click"
    if (e.repeat) return;
    spaceDownAt = performance.now();
    if (dictation.state === "idle") {
      toggleDictation();
      clearTimeout(holdTimer);
      holdTimer = window.setTimeout(() => {
        held = true;
        if (dictation.state !== "idle") dictationHint.innerHTML = "Release <kbd>Space</kbd> to transcribe · <kbd>Esc</kbd> cancels";
      }, HOLD_MS);
    } else {
      dictation.stop(); // second tap in toggle mode
    }
  },
  true,
);

document.addEventListener(
  "keyup",
  (e) => {
    if (e.code !== "Space" || isTyping(e.target)) return;
    e.preventDefault();
    clearTimeout(holdTimer);
    const heldFor = performance.now() - spaceDownAt;
    if (held || heldFor >= HOLD_MS) dictation.stop(); // push-to-talk release
    held = false;
  },
  true,
);

// Leaving the tab while holding Space would otherwise keep recording.
window.addEventListener("blur", () => {
  if (held) dictation.stop();
  held = false;
});

// Space is handled globally (and its default "click" is suppressed), so only real clicks/Enter land here.
micButton.addEventListener("click", toggleDictation);

// Expose a tiny hook for automated UI checks (no effect on normal use).
(window as unknown as { __ow: unknown }).__ow = { dictation, waveform };

void loadLanguages();
void refreshStatus();
