import "./style.css";
import type { JobEvent, JobResult, ModelInfo, Segment, Status } from "./api";
import { getBackend, type Backend, type FileRef } from "./backend";
import { Dictation, type DictationState } from "./dictation";
import { Waveform } from "./waveform";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const dropzone = $<HTMLDivElement>("dropzone");
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
const modelDetails = $<HTMLParagraphElement>("model-details");
const previewCheck = $<HTMLInputElement>("preview-check");
const previewNote = $<HTMLElement>("preview-note");
const previewEngine = $<HTMLSelectElement>("preview-engine");
const idleSelect = $<HTMLSelectElement>("idle-select");

const MEDIA_EXT = /\.(mp3|wav|wave|m4a|aac|ogg|oga|opus|flac|webm|weba|wma|aiff?|amr|mp4|m4v|mov|mkv|avi|3gp)$/i;
const langNames = new Intl.DisplayNames(["en"], { type: "language" });

let backend!: Backend;
let jobState = ""; // state of the job shown in the result card
let status: Status | null = null;
let busy = false;
let current: { result: JobResult | null; segments: Segment[]; baseName: string } = { result: null, segments: [], baseName: "transcript" };

// ------------------------------------------------------------------ helpers
const announcer = $<HTMLDivElement>("announcer");
function announce(message: string): void {
  announcer.textContent = message;
}

function showError(message: string | null): void {
  modelErrorShown = false;
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
    case "downloaded": // on disk, loads on first use
      return `Ready · ${m.label.split(" · ")[0]} on ${status?.gpu || status?.device === "cuda" ? "GPU" : "CPU"}`;
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
  modelStatus.dataset.state = m.state === "downloaded" ? "ready" : m.state;
  // Parakeet finds the language itself; it has no language setting.
  const finds = m.engine === "parakeet";
  languageSelect.disabled = finds;
  languageSelect.title = finds ? "Parakeet finds the language itself" : "";
  modelStatusText.textContent = modelPhrase(m);
  modelBar.hidden = m.state !== "downloading";
  modelBarFill.style.width = `${Math.floor(m.progress * 100)}%`;
  if (m.state !== lastModelState) {
    if (m.state === "downloading") announce("Downloading the speech model. This happens once.");
    if (m.state === "ready" && lastModelState) announce("Model ready.");
    if (m.state === "error") {
      showError(m.error ?? "The model could not be prepared.");
      modelErrorShown = true;
    } else if (modelErrorShown) {
      showError(null); // the selected model is fine now; drop the old model error
    }
    lastModelState = m.state;
  }
  const auto = status!.auto_model ?? status!.default_model;
  for (const opt of modelSelect.options) {
    const info = status!.models.find((x) => x.name === opt.value)!;
    const here = info.state === "missing" ? ` (download ~${info.approx_mb} MB)` : "";
    opt.textContent = `${info.label}${info.name === auto ? " — recommended here" : ""}${here}`;
  }
  modelDetails.textContent = details(m);
}

/** "0.5 GB download · ~0.9 GB RAM · 10.9× real time on GPU · WER 3.2%". */
function details(m: ModelInfo): string {
  if (m.ram_mb === undefined) return "";
  const gb = (mb: number) => `${(mb / 1024).toFixed(1)} GB`;
  const one = (x: number | null | undefined) => Number((x ?? 0).toFixed(1)); // f32 from Rust: 8.8999996 → 8.9
  const speed = status?.gpu && m.speed_gpu ? `${one(m.speed_gpu)}× real time on GPU` : `${one(m.speed_cpu)}× real time on CPU`;
  const engine = m.engine === "parakeet" ? "Parakeet (NVIDIA), CPU, finds the language itself · " : "whisper.cpp · ";
  return `${engine}${gb(m.approx_mb)} download · ~${gb(m.ram_mb)} memory · ${speed} · WER ${one(m.wer)}% (measured on the test machine)`;
}

/** The live preview's note: what it is, and whether it can run here. */
function renderPreview(): void {
  const p = status?.preview;
  if (!p) return;
  const base =
    p.engine === "parakeet"
      ? "Parakeet (CPU, 25 languages, finds the language itself). Preview only: the model above writes the text."
      : "Whistle (17 MB, CPU). Preview only: the model above writes the text. EN, DE, FR, ES, IT, NL, PL.";
  const state = !previewCheck.checked
    ? ""
    : p.state === "preparing"
      ? " Getting it ready…"
      : p.state === "unavailable"
        ? ` Not available: ${p.error ?? "unknown error"}`
        : "";
  previewNote.textContent = base + state;
  previewEngine.disabled = !previewCheck.checked;
}

let lastModelState = "";
let modelErrorShown = false;
let statusTimer = 0;
async function refreshStatus(): Promise<void> {
  try {
    applyStatus(await backend.getStatus());
  } catch {
    modelStatus.dataset.state = "error";
    modelStatusText.textContent = "Engine not ready";
  }
  const m = selectedModel();
  // Poll fast only while something is changing; a model that is merely not downloaded yet is idle.
  const settling = !m || ["downloading", "loading"].includes(m.state) || (m.name === status?.default_model && m.state === "missing");
  clearTimeout(statusTimer);
  statusTimer = window.setTimeout(refreshStatus, settling ? 1000 : 10000);
}

function applyStatus(next: Status): void {
  status = next;
  // Desktop: model download progress arrives as status events, not job events.
  if (jobState === "preparing" && busy) {
    const m = selectedModel();
    if (m?.state === "downloading") setProgress(m.progress, `First run: ${modelPhrase(m).replace("Downloading model…", "downloading the model…")}`);
    else if (m?.state === "loading") setProgress(null, "Loading model…");
  }
  if (!modelSelect.options.length) {
    for (const m of status.models) modelSelect.add(new Option(m.label, m.name));
    modelSelect.value = status.default_model;
    if (status.options) idleSelect.value = String(status.options.idle_unload_minutes);
    if (status.options?.live_preview !== undefined) previewCheck.checked = status.options.live_preview;
    if (status.options?.preview_engine) previewEngine.value = status.options.preview_engine;
    // Parakeet is built on some systems only: offer it where the app has it.
    const pk = status.models.some((m) => m.name === "parakeet-v3");
    previewEngine.querySelector<HTMLOptionElement>('option[value="parakeet"]')!.hidden = !pk;
  }
  renderPreview();
  renderStatus();
}

async function loadLanguages(): Promise<void> {
  try {
    const langs = await backend.getLanguages();
    const entries = Object.keys(langs)
      .map((code) => [code, langName(code)] as const)
      .sort((a, b) => a[1].localeCompare(b[1]));
    for (const [code, name] of entries) languageSelect.add(new Option(name, code));
    const saved = (await backend.getStatus()).options?.language;
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
  jobState = ev.state;
  if (ev.new_segments && ev.segment_count !== undefined) {
    // Idempotent merge: a snapshot can repeat segments that an event already sent.
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
      announce(current.result?.text ? "Transcript ready." : "No speech found.");
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

function beginResult(filename: string, title: string): void {
  showError(null);
  busy = true;
  current = { result: null, segments: [], baseName: filename.replace(/\.[^.]+$/, "") || "transcript" };
  resultCard.hidden = false;
  resultTitle.textContent = title;
  resultMeta.textContent = "";
  setResultReady(false);
  renderTranscript();
}

function follow(jobId: string): void {
  backend.follow(jobId, onJobEvent, () => {
    busy = false;
    progress.hidden = true;
    showError("Lost track of the transcription. Try again.");
  });
}

async function transcribe(file: FileRef, title: string): Promise<void> {
  if (busy) {
    showError("A transcription is already running. Wait for it to finish.");
    return;
  }
  beginResult(file.name, title);
  setProgress(null, "Starting…");
  try {
    follow(await backend.start(file, languageSelect.value, modelSelect.value));
  } catch (err) {
    busy = false;
    progress.hidden = true;
    resultCard.hidden = !current.segments.length;
    showError(err instanceof Error ? err.message : String(err));
  }
}

function handleRef(file: FileRef): void {
  if (dictation.state !== "idle") {
    // Do not throw away what the user is saying; they can finish or cancel first.
    showError("Finish the dictation first (Space or the mic button), or press Esc to cancel it.");
    return;
  }
  // The app checks the size and the content; this only catches an obviously wrong file early.
  if (!MEDIA_EXT.test(file.name)) {
    showError(`“${file.name}” is not an audio or video file. Try MP3, WAV, M4A, OGG, FLAC, WEBM or a video with sound.`);
    return;
  }
  void transcribe(file, file.name);
}

// ------------------------------------------------------------------ choose a file (drops: see getBackend below)
async function chooseFile(): Promise<void> {
  const f = await backend.pickFile(); // native dialog: gives a path
  if (f) handleRef(f);
}
dropzone.addEventListener("click", (e) => {
  e.preventDefault();
  void chooseFile();
});
dropzone.addEventListener("keydown", (e) => {
  // Space reaches here only when the dropzone has keyboard focus (see the global Space handler).
  if (e.key === "Enter" || isSpace(e)) {
    e.preventDefault();
    void chooseFile();
  }
});

// ------------------------------------------------------------------ copy / download
copyButton.addEventListener("click", async () => {
  try {
    await backend.copy(current.result?.text ?? "");
  } catch (err) {
    return showError(String(err));
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
    backend.save(`${current.baseName}.${fmt}`, body, type).catch((err) => showError(String(err)));
  });
}

// ------------------------------------------------------------------ options
languageSelect.addEventListener("change", () => void backend.saveOptions({ language: languageSelect.value }));
modelSelect.addEventListener("change", () => {
  // Desktop: saved to ~/.config/open-whisper/config.toml and loaded at once.
  void backend.saveOptions({ model: modelSelect.value }).catch((err) => showError(String(err)));
  renderStatus();
});
idleSelect.addEventListener("change", () => void backend.saveOptions({ idle_unload_minutes: Number(idleSelect.value) }));
previewEngine.addEventListener("change", () => {
  void backend.saveOptions({ preview_engine: previewEngine.value }).then(renderPreview, (err) => showError(String(err)));
});
previewCheck.addEventListener("change", () => {
  void backend.saveOptions({ live_preview: previewCheck.checked }).then(renderPreview, (err) => showError(String(err)));
});

// ------------------------------------------------------------------ dictation
const waveform = new Waveform($<HTMLCanvasElement>("waveform"));
const dictationUi = {
  onState(state: DictationState) {
    dictationBox.dataset.state = state;
    micButton.setAttribute("aria-label", state === "idle" ? "Start dictation" : "Stop dictation");
    micButton.setAttribute("aria-pressed", String(state !== "idle"));
    dictationTimer.hidden = state !== "listening";
    dictationTimer.textContent = fmtClock(0);
    if (state === "idle") {
      dictationLabel.textContent = "Dictate";
      dictationHint.innerHTML = "Hold <kbd>Space</kbd> to talk, or tap it to start and stop";
    } else if (state === "starting") {
      dictationLabel.textContent = "Opening microphone…";
      dictationHint.textContent = "One moment";
    } else {
      dictationLabel.textContent = "Listening…";
      dictationHint.innerHTML = held || spaceDownAt ? HINT_HOLD : HINT_TOGGLE;
    }
  },
  onTick(elapsed: number) {
    dictationTimer.textContent = fmtClock(elapsed);
  },
  onJob(jobId: string) {
    beginResult("dictation", "Dictation");
    setProgress(null, "Transcribing…");
    follow(jobId);
  },
  onError(message: string) {
    showError(message);
  },
};
// The app records the microphone; the page shows the waveform and follows the job.
const dictation = new Dictation(waveform, dictationUi);

const HOLD_MS = 350;
const HINT_HOLD = "Release <kbd>Space</kbd> to transcribe · <kbd>Esc</kbd> cancels";
const HINT_TOGGLE = "Press <kbd>Space</kbd> or the mic to transcribe · <kbd>Esc</kbd> cancels";
let spaceDownAt = 0;
let held = false; // true while Space is physically held for push-to-talk
let holdTimer = 0;

/** Match keys by `key` (the character/name), not `code` (the physical position): virtual
 * keyboards and some layouts put keys at other positions (wtype's first key is keycode 9,
 * which `code` reports as "Escape"). */
function isSpace(e: KeyboardEvent): boolean {
  return e.key === " " || e.key === "Spacebar";
}

function isTyping(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  return !!el && (el.isContentEditable || ["INPUT", "SELECT", "TEXTAREA"].includes(el.tagName));
}

/** Space should keep its normal meaning on a control that a keyboard user focused (Tab). */
function keyboardFocusedControl(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || el === micButton || !el.matches?.("button, summary, a[href], [role=button]")) return false;
  return el.matches(":focus-visible");
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
    if (e.key === "Escape" && dictation.state !== "idle") {
      e.preventDefault();
      dictation.cancel();
      return;
    }
    if (!isSpace(e) || e.ctrlKey || e.metaKey || e.altKey || isTyping(e.target) || keyboardFocusedControl(e.target)) return;
    e.preventDefault(); // no page scroll, no accidental "click" on a mouse-focused button
    if (e.repeat) return;
    spaceDownAt = performance.now();
    if (dictation.state === "idle") {
      toggleDictation();
      clearTimeout(holdTimer);
      holdTimer = window.setTimeout(() => {
        held = true;
        if (dictation.state !== "idle") dictationHint.innerHTML = HINT_HOLD;
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
    if (!isSpace(e) || isTyping(e.target) || (keyboardFocusedControl(e.target) && !spaceDownAt)) return;
    e.preventDefault();
    clearTimeout(holdTimer);
    const heldFor = performance.now() - spaceDownAt;
    if (spaceDownAt && (held || heldFor >= HOLD_MS)) dictation.stop(); // push-to-talk release
    else if (spaceDownAt && dictation.state === "listening") dictationHint.innerHTML = HINT_TOGGLE; // tap mode
    held = false;
    spaceDownAt = 0;
  },
  true,
);

// Leaving the tab while holding Space would otherwise keep recording.
window.addEventListener("blur", () => {
  if (held || spaceDownAt) dictation.stop(); // Space was down: the keyup will never arrive
  clearTimeout(holdTimer);
  held = false;
  spaceDownAt = 0;
});

// Space is handled globally (and its default "click" is suppressed), so only real clicks/Enter land here.
micButton.addEventListener("click", toggleDictation);

void getBackend().then((b) => {
  backend = b;
  b.onStatus(applyStatus);
  b.onFileDrop(
    (over) => dropzone.classList.toggle("over", over),
    (files) => files[0] && handleRef(files[0]),
  );
  b.onOpenFiles((files) => files[0] && handleRef(files[0]));
  void loadLanguages();
  void refreshStatus();
});
