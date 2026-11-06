import "./style.css";
import type { AiNotesEvent, JobEvent, JobResult, ModelInfo, Segment, Status } from "./api";
import { getBackend, type Backend, type FileRef } from "./backend";
import { Dictation, type DictationState } from "./dictation";
import { renderCalls } from "./calls";
import { initHistory } from "./history";
import { initMeetings } from "./meetings";
import { initMicrophone } from "./microphone";
import { initVocabulary } from "./vocabulary";
import { confirmDialog } from "./confirm";
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
const keyRow = $<HTMLDivElement>("key-row");
const keyCurrent = $<HTMLElement>("key-current");
const keyChange = $<HTMLButtonElement>("key-change");
const keyAdopt = $<HTMLButtonElement>("key-adopt");
const keyNote = $<HTMLElement>("key-note");
const loginCheck = $<HTMLInputElement>("login-check");
const meetingKeyRow = $<HTMLDivElement>("meeting-key-row");
const meetingKeyCurrent = $<HTMLElement>("meeting-key-current");
const meetingKeyChange = $<HTMLButtonElement>("meeting-key-change");
const meetingKeyNote = $<HTMLElement>("meeting-key-note");
const aiEngine = $<HTMLSelectElement>("ai-engine");
const aiEngineNote = $<HTMLElement>("ai-engine-note");
const aiServer = $<HTMLDivElement>("ai-server");
const aiUrl = $<HTMLInputElement>("ai-url");
const aiUrlNote = $<HTMLElement>("ai-url-note");
const aiModel = $<HTMLInputElement>("ai-model");
const aiKey = $<HTMLInputElement>("ai-key");
const aiKeyClear = $<HTMLButtonElement>("ai-key-clear");
const aiAuto = $<HTMLInputElement>("ai-auto");
const agentCommands = $<HTMLElement>("agent-commands");
const exportDir = $<HTMLElement>("export-dir");
const exportDirNote = $<HTMLElement>("export-dir-note");
const exportChoose = $<HTMLButtonElement>("export-choose");
const exportOff = $<HTMLButtonElement>("export-off");
const exportAuto = $<HTMLInputElement>("export-auto");
const exportParts = [...document.querySelectorAll<HTMLInputElement>(".export-parts input")];
const agentsRecording = $<HTMLInputElement>("agents-recording");
const voicesOn = $<HTMLInputElement>("voices-on");
const screenCard = $<HTMLDivElement>("screen-card");
const screenCheck = $<HTMLButtonElement>("screen-check");
const screenStatus = $<HTMLElement>("screen-status");
const screenForget = $<HTMLButtonElement>("screen-forget");
const voicesStatus = $<HTMLDivElement>("voices-status");
const voicesStatusText = $<HTMLSpanElement>("voices-status-text");
const voicesBar = voicesStatus.querySelector<HTMLDivElement>(".model-bar")!;
const voicesBarFill = $<HTMLDivElement>("voices-bar-fill");
const aiStatus = $<HTMLDivElement>("ai-status");
const aiStatusText = $<HTMLSpanElement>("ai-status-text");
const aiBar = aiStatus.querySelector<HTMLDivElement>(".model-bar")!;
const aiBarFill = $<HTMLDivElement>("ai-bar-fill");
const idleSelect = $<HTMLSelectElement>("idle-select");

const MEDIA_EXT = /\.(mp3|wav|wave|m4a|aac|ogg|oga|opus|flac|webm|weba|wma|aiff?|amr|mp4|m4v|mov|mkv|avi|3gp)$/i;
const langNames = new Intl.DisplayNames(["en"], { type: "language" });

let backend!: Backend;
let jobState = ""; // state of the job shown in the result card
let status: Status | null = null;
/** The Meetings section (its note-app buttons follow the status). */
let meetingsView: { refreshExport(): void } | null = null;
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
      // Parakeet runs on the CPU even when there is a GPU.
      return `Ready · ${m.label.split(" · ")[0]} on ${status?.gpu && m.engine !== "parakeet" ? "GPU" : "CPU"}`;
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
  if (previewCheck.checked && p.state === "preparing") {
    const spin = document.createElement("span");
    spin.className = "spinner spinner-inline";
    spin.setAttribute("aria-hidden", "true");
    previewNote.replaceChildren(`${base} `, spin, "Getting it ready…");
  } else {
    previewNote.textContent = base + (previewCheck.checked && p.state === "unavailable" ? ` Not available: ${p.error ?? "unknown error"}` : "");
  }
  previewEngine.disabled = !previewCheck.checked;
}

let lastModelState = "";
let history: { refresh(): void } | null = null;
let microphone: { refresh(): void } | null = null;
/** The saved microphone as the last status said (the tray menu can change it). */
let savedMic = "";
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

// ------------------------------------------------------------------ appearance and quit
declare global {
  interface Window {
    /** public/theme.js: paint the page light or dark ("system" follows the desktop). */
    lowkeyTheme?: (choice: string) => void;
  }
}
const themeSelect = $<HTMLSelectElement>("theme-select");
themeSelect.value = localStorage.getItem("lowkey-theme") ?? "system";
function setTheme(choice: string): void {
  window.lowkeyTheme?.(choice); // at once; then saved
  void backend.saveOptions({ theme: choice }).then(refreshStatus, (err) => showError(String(err)));
}
themeSelect.addEventListener("change", () => setTheme(themeSelect.value));

// The side menu's switch: light ⇄ dark (from "System", the opposite of what shows now).
const themeToggle = $<HTMLButtonElement>("theme-toggle");
const themeToggleText = $<HTMLElement>("theme-toggle-text");
function renderThemeToggle(): void {
  const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  themeToggleText.textContent = next === "dark" ? "Dark mode" : "Light mode";
  themeToggle.title = `Switch to ${next} mode`;
  themeToggle.dataset.next = next;
}
themeToggle.addEventListener("click", () => {
  const next = themeToggle.dataset.next ?? "dark";
  themeSelect.value = next;
  setTheme(next);
  announce(`${next === "dark" ? "Dark" : "Light"} mode.`);
});
document.addEventListener("lowkey-theme", renderThemeToggle);
renderThemeToggle();

const quitButton = $<HTMLButtonElement>("app-quit");
quitButton.addEventListener("click", async () => {
  if (status?.recording) {
    // Stopped and saved first (as Stop does); quitting alone would lose its last seconds.
    const yes = await confirmDialog({
      title: "Stop the recording and exit?",
      body: "The meeting is saved first. Its notes are not written now: open the meeting later and use Write notes.",
      action: "Stop and exit",
      danger: true,
    });
    if (!yes) return;
    quitButton.disabled = true;
    const spin = document.createElement("span");
    spin.className = "spinner";
    spin.setAttribute("aria-hidden", "true");
    const idle = [...quitButton.childNodes];
    quitButton.replaceChildren(spin, "Saving the meeting…");
    try {
      await backend.meetings.stop();
    } catch (err) {
      quitButton.disabled = false;
      quitButton.replaceChildren(...idle);
      showError(`The meeting was not stopped, so Exit was cancelled: ${String(err)}`);
      return;
    }
  }
  await backend.quit();
});

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
    initVocabulary(backend, status.options?.vocabulary ?? [], showError);
    microphone = initMicrophone(backend, showError);
    history = initHistory(backend, status.history_keep ?? "week", {
      announce,
      onError: showError,
      pasteKey: () => (status?.paste_key_source === "app" ? status.paste_key ?? "" : ""),
    });
    // Parakeet is built on some systems only: offer it where the app has it.
    const pk = status.models.some((m) => m.name === "parakeet-v3");
    previewEngine.querySelector<HTMLOptionElement>('option[value="parakeet"]')!.hidden = !pk;
    savedMic = status.options?.microphone ?? "";
  } else if (document.activeElement !== modelSelect && modelSelect.value !== status.default_model) {
    modelSelect.value = status.default_model; // chosen in the tray menu
  }
  // Appearance: the saved choice (also from config.toml edited by hand) wins over the boot copy.
  const theme = status.options?.theme ?? "system";
  if (document.activeElement !== themeSelect && themeSelect.value !== theme) {
    themeSelect.value = theme;
    window.lowkeyTheme?.(theme);
  }
  if ((status.options?.microphone ?? "") !== savedMic) {
    savedMic = status.options?.microphone ?? "";
    microphone?.refresh(); // chosen in the tray menu
  }
  renderPreview();
  renderKey();
  if (backend) renderCalls(backend, status, showError);
  renderAi();
  renderAgents();
  renderVoices();
  renderScreen();
  renderExport();
  meetingsView?.refreshExport();
  renderAbout();
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
  // Desktop: saved to ~/.config/lowkey/config.toml and loaded at once.
  void backend.saveOptions({ model: modelSelect.value }).catch((err) => showError(String(err)));
  renderStatus();
});
idleSelect.addEventListener("change", () => void backend.saveOptions({ idle_unload_minutes: Number(idleSelect.value) }));
// ------------------------------------------------------------------ dictation key
/** "SUPER + CTRL + X" → "Super + Ctrl + X". */
function prettyKey(key: string): string {
  return key
    .split("+")
    .map((p) => p.trim())
    .map((p) => (p.length > 1 && !/^F\d+$/.test(p) ? p[0] + p.slice(1).toLowerCase() : p))
    .join(" + ");
}

/** One key row in Settings → Keys (the dictation key, the meeting key). */
interface KeyUi {
  row: HTMLElement;
  current: HTMLElement;
  change: HTMLButtonElement;
  note: HTMLElement;
  save(key: string): Promise<Status>;
  message: { text: string; kind: "" | "ok" | "error" };
}
/** The row that is taking the new keys now. */
let capturing: KeyUi | null = null;
/** WebKitGTK does not report Super as a modifier of other keys: follow the key itself. */
let superHeld = false;
const SUPER_KEYS = ["Meta", "OS", "Super", "Hyper"];
const SUPER_CODES = ["MetaLeft", "MetaRight", "OSLeft", "OSRight"];
const isSuper = (e: KeyboardEvent) => SUPER_KEYS.includes(e.key) || SUPER_CODES.includes(e.code);
const dictationKeyUi: KeyUi = {
  row: keyRow,
  current: keyCurrent,
  change: keyChange,
  note: keyNote,
  save: (k) => backend.setHotkey(k),
  message: { text: "", kind: "" },
};
const pasteKeyUi: KeyUi = {
  row: $<HTMLDivElement>("paste-key-row"),
  current: $<HTMLElement>("paste-key-current"),
  change: $<HTMLButtonElement>("paste-key-change"),
  note: $<HTMLElement>("paste-key-note"),
  save: (k) => backend.setPasteHotkey(k),
  message: { text: "", kind: "" },
};
const meetingKeyUi: KeyUi = {
  row: meetingKeyRow,
  current: meetingKeyCurrent,
  change: meetingKeyChange,
  note: meetingKeyNote,
  save: (k) => backend.setMeetingHotkey(k),
  message: { text: "", kind: "" },
};

function renderKey(): void {
  renderDictationKey();
  renderMeetingKey();
  renderPasteKey();
}

function renderPasteKey(): void {
  const ui = pasteKeyUi;
  if (!status || capturing === ui) return;
  const key = status.paste_key ?? "";
  const source = status.paste_key_source ?? "none";
  const inactive = source === "none" && !!key;
  ui.current.textContent = key ? prettyKey(key) + (inactive ? " (not active)" : "") : "None";
  ui.change.textContent = key ? "Change" : "Choose a key";
  ui.change.hidden = source === "manual";
  const explain =
    source === "manual"
      ? "This desktop does not let apps add keys: bind `lowkey --paste-last` in its keyboard settings."
      : inactive
        ? status.paste_key_problem || "This key is not active. Choose another one."
        : "Types your last dictation again into the app you are in (also when the history is off).";
  ui.note.textContent = ui.message.text || explain;
  ui.note.dataset.kind = ui.message.kind || (inactive ? "error" : "");
}

function renderMeetingKey(): void {
  if (!status || capturing === meetingKeyUi) return;
  const key = status.meeting_key ?? "";
  const source = status.meeting_key_source ?? "none";
  const inactive = source === "none" && !!key;
  meetingKeyCurrent.textContent = key ? prettyKey(key) + (inactive ? " (not active)" : "") : "None";
  meetingKeyChange.textContent = key ? "Change" : "Choose a key";
  meetingKeyChange.hidden = source === "manual";
  const explain =
    source === "manual"
      ? "This desktop does not let apps add keys: bind `lowkey --meeting toggle` in its keyboard settings."
      : inactive
        ? status.meeting_key_problem || "This key is not active. Choose another one."
        : "Tap it to start recording a meeting (your mic and the computer's sound), and again to stop. A strip at the bottom of the screen shows the recording.";
  meetingKeyNote.textContent = meetingKeyUi.message.text || explain;
  meetingKeyNote.dataset.kind = meetingKeyUi.message.kind || (inactive ? "error" : "");
}

function renderDictationKey(): void {
  const keyMessage = dictationKeyUi.message;
  if (!status || capturing === dictationKeyUi) return;
  const key = status.hotkey_key ?? "";
  const source = status.hotkey_source ?? "none";
  const inactive = source === "none" && !!key;
  keyCurrent.textContent = key ? prettyKey(key) + (inactive ? " (not active)" : "") : "None";
  keyChange.textContent = key ? "Change" : "Choose a key";
  keyChange.hidden = source === "config" || source === "manual";
  keyAdopt.hidden = source !== "config";
  loginCheck.checked = !!status.start_at_login;
  const explain =
    source === "config"
      ? "This key comes from your Hyprland config (a block in bindings.lua). To change it here, let the app manage it: the block is removed from your config, and a backup is kept next to it."
      : source === "manual"
        ? "This desktop does not let apps add keys: bind `lowkey --dictate toggle` in its keyboard settings."
        : inactive
          ? status.hotkey_problem || "This key is not active. Choose another one."
          : "Hold it to talk, or tap it to start and again to stop.";
  keyNote.textContent = keyMessage.text || explain;
  keyNote.dataset.kind = keyMessage.kind || (inactive ? "error" : "");
}

/** A key event as "SUPER + ALT + D", or an explanation of why it cannot be used. */
function comboOf(e: KeyboardEvent): { key?: string; why?: string } {
  const mods: string[] = [];
  if (superHeld || e.metaKey || e.getModifierState("OS") || e.getModifierState("Super")) mods.push("SUPER");
  if (e.ctrlKey) mods.push("CTRL");
  if (e.altKey) mods.push("ALT");
  if (e.shiftKey) mods.push("SHIFT");
  const code = e.code;
  const main = /^Key[A-Z]$/.test(code)
    ? code.slice(3)
    : /^Digit\d$/.test(code)
      ? code.slice(5)
      : /^F\d{1,2}$/.test(code)
        ? code
        : code === "Space"
          ? "SPACE"
          : "";
  if (!main) return { why: "Use a letter, a digit, F1-F24 or Space as the last key." };
  if (!mods.some((m) => m !== "SHIFT")) return { why: "Hold Super, Ctrl or Alt too, so the key does not get in the way of typing." };
  return { key: [...mods, main].join(" + ") };
}

function stopCapture(): void {
  const ui = capturing;
  capturing = null;
  superHeld = false;
  if (ui) delete ui.row.dataset.capturing;
  renderKey();
}

function startCapture(ui: KeyUi): void {
  if (capturing === ui) {
    ui.message = { text: "", kind: "" };
    return stopCapture();
  }
  if (capturing) stopCapture(); // the other row was waiting
  capturing = ui;
  superHeld = false; // only a Super key pressed from now on counts
  ui.row.dataset.capturing = "";
  ui.current.textContent = "Press the new keys…";
  ui.change.textContent = "Cancel";
  ui.note.dataset.kind = "";
  ui.note.textContent = "Hold Super, Ctrl or Alt and press a letter, a digit, F1-F24 or Space. Esc cancels. A combination that another shortcut already uses does not reach this window.";
}

keyChange.addEventListener("click", () => startCapture(dictationKeyUi));
meetingKeyChange.addEventListener("click", () => startCapture(meetingKeyUi));
pasteKeyUi.change.addEventListener("click", () => startCapture(pasteKeyUi));

// Before every other key handler (the Space dictation key, Esc cancel): while choosing a key,
// keys only choose the key.
window.addEventListener(
  "keydown",
  (e) => {
    if (isSuper(e)) superHeld = true;
    const ui = capturing;
    if (!ui) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    if (e.repeat || isSuper(e) || ["Shift", "Control", "Alt", "AltGraph"].includes(e.key)) return;
    if (e.key === "Escape") {
      ui.message = { text: "", kind: "" };
      return stopCapture();
    }
    const c = comboOf(e);
    if (!c.key) {
      ui.note.dataset.kind = "error";
      ui.note.textContent = c.why ?? "";
      return;
    }
    const chosen = c.key;
    ui.current.textContent = prettyKey(chosen);
    void ui.save(chosen).then(
      (st) => {
        ui.message = { text: `Saved. ${prettyKey(chosen)} works now.`, kind: "ok" };
        stopCapture();
        applyStatus(st);
      },
      (err) => {
        ui.message = { text: String(err), kind: "error" };
        stopCapture();
      },
    );
  },
  true,
);
window.addEventListener(
  "keyup",
  (e) => {
    if (isSuper(e)) superHeld = false;
    if (capturing) {
      e.preventDefault();
      e.stopImmediatePropagation();
    }
  },
  true,
);
window.addEventListener("blur", () => (superHeld = false));
// Never leave the keys captured behind a closed view or a hidden window (see also showSection).
document.addEventListener("visibilitychange", () => {
  if (capturing && document.hidden) stopCapture();
});

keyAdopt.addEventListener("click", () => {
  keyAdopt.disabled = true;
  void backend.adoptHotkey().then(
    (r) => {
      const login = r.start_at_login ? " Start at login is on, so the key works after a restart too." : "";
      dictationKeyUi.message = { text: `Done: the app manages the key now. Your old config is saved as ${r.backup}.${login}`, kind: "ok" };
      keyAdopt.disabled = false;
      applyStatus(r.status);
      keyChange.focus(); // it takes the place of the hidden button
    },
    (err) => {
      dictationKeyUi.message = { text: String(err), kind: "error" };
      keyAdopt.disabled = false;
      renderKey();
    },
  );
});

loginCheck.addEventListener("change", () => {
  void backend.saveOptions({ start_at_login: loginCheck.checked }).catch((err) => {
    loginCheck.checked = !loginCheck.checked;
    showError(String(err));
  });
});

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
    if (section !== "transcribe") return; // Space works the controls (or types) there, it does not dictate
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

// ------------------------------------------------------------------ sections
// One window: Transcribe (files, dictation here, the result), Meetings, and the settings sections.
const tabs = [...document.querySelectorAll<HTMLButtonElement>("#settings-tabs .nav-item")];
let section = "transcribe";

/** In a settings section (not Transcribe or Meetings): Ctrl+, and Esc go back to Transcribe. */
function inSettings(): boolean {
  return !["transcribe", "meetings", "history"].includes(section);
}

function showSection(name: string, focus = false): void {
  if (capturing) stopCapture();
  if (name !== "transcribe" && dictation.state !== "idle") return; // finish the dictation first
  section = name;
  for (const t of tabs) {
    const on = t.dataset.section === name;
    t.setAttribute("aria-selected", String(on));
    t.tabIndex = on ? 0 : -1; // one tab stop for the list; arrows move inside it
    if (on && focus) t.focus();
  }
  for (const p of document.querySelectorAll<HTMLElement>(".settings-content .section")) p.hidden = p.dataset.section !== name;
  if (name === "history") history?.refresh();
  if (name === "general") microphone?.refresh();
}
showSection("transcribe");

for (const t of tabs) t.addEventListener("click", () => showSection(t.dataset.section!));
$<HTMLElement>("settings-tabs").addEventListener("keydown", (e) => {
  const i = tabs.findIndex((t) => t.dataset.section === section);
  const to = { ArrowDown: i + 1, ArrowRight: i + 1, ArrowUp: i - 1, ArrowLeft: i - 1, Home: 0, End: tabs.length - 1 }[e.key];
  if (to === undefined) return;
  e.preventDefault();
  showSection(tabs[(to + tabs.length) % tabs.length].dataset.section!, true);
});
document.addEventListener("keydown", (e) => {
  if (e.key === "," && e.ctrlKey && !e.altKey && !e.metaKey) {
    e.preventDefault();
    showSection(inSettings() ? "transcribe" : "general", true);
  } else if (e.key.toLowerCase() === "f" && e.ctrlKey && !e.altKey && !e.metaKey && (section === "meetings" || section === "history")) {
    // Ctrl+F: the section's search field (Meetings: on the list, not inside a meeting).
    const field = $<HTMLInputElement>(section === "meetings" ? "meetings-search" : "history-search");
    if (field.offsetParent) {
      e.preventDefault();
      field.focus();
      field.select();
    }
  } else if (e.key === "Escape" && inSettings() && !capturing && dictation.state === "idle") {
    e.preventDefault();
    showSection("transcribe", true);
  }
});

// ------------------------------------------------------------------ Notes settings
let aiFilled = false;
/** Settings → Models → Notes, from the status. Fields being edited are left alone. */
function renderAi(): void {
  const ai = status?.ai;
  if (!ai) return;
  // The menu: every built-in model (with its size, or "downloaded"), then the own server.
  const b = ai.builtin;
  const gb = (mb: number) => `${(mb / 1000).toFixed(1)} GB`;
  const options = b.models.map((m) => new Option(`${m.label}${m.id === b.models[0].id ? " (recommended)" : ""} — ${m.downloaded ? "downloaded" : `${gb(m.download_mb)} download`}`, `builtin:${m.id}`));
  options.push(new Option("My own server: Ollama, LM Studio, or an OpenAI-compatible API", "server"));
  const chosen = ai.engine === "server" ? "server" : `builtin:${ai.builtin_model}`;
  const keep = aiFilled ? aiEngine.value : chosen;
  // Only when the menu's content changed: never under an open menu for nothing.
  const signature = options.map((o) => o.value + o.text).join("|");
  if (aiEngine.dataset.signature !== signature) {
    aiEngine.replaceChildren(...options);
    aiEngine.dataset.signature = signature;
  }
  aiEngine.value = keep;
  if (!aiFilled) {
    aiUrl.value = ai.url;
    aiModel.value = ai.model;
    aiFilled = true;
  }
  aiAuto.checked = ai.auto;
  aiServer.hidden = aiEngine.value !== "server";
  const pick = b.models.find((m) => `builtin:${m.id}` === aiEngine.value);
  aiEngineNote.textContent =
    aiEngine.value === "server"
      ? "The app sends the transcript to this server, and shows its answer."
      : !b.supported
        ? "Not available for this system: use your own server."
        : `${pick?.note ?? ""} ${pick?.downloaded ? "On this computer." : `Downloads once (${gb(b.download_mb)}) the first time notes are written.`} Apache-2.0.`;
  renderAiStatus();
  aiKey.placeholder = ai.has_key ? "A key is saved (type to replace it)" : "None";
  aiKeyClear.hidden = !ai.has_key;
  renderAiUrlNote();
}

/** The AI job now running (its events), else null: notes, or an answer to a question. */
let aiLive: AiNotesEvent | null = null;
let aiLiveKind: "notes" | "chat" = "notes";

/** Models → Notes: its status, as the speech model's (ready / not downloaded / at work). */
function renderAiStatus(): void {
  const ai = status?.ai;
  if (!ai) return;
  const set = (state: string, text: string, progress: number | null = null) => {
    aiStatus.dataset.state = state;
    aiStatusText.textContent = text;
    aiBar.hidden = progress === null;
    aiBarFill.style.width = `${Math.floor((progress ?? 0) * 100)}%`;
  };
  if (aiLive && ["preparing", "downloading", "writing"].includes(aiLive.state)) {
    if (aiLive.state === "downloading") return set("downloading", aiLive.detail, aiLive.progress);
    if (aiLive.state === "writing") return set("loading", aiLiveKind === "chat" ? "Answering a question…" : "Writing notes…");
    return set("loading", aiLive.detail);
  }
  if (ai.engine === "server") {
    let host = "";
    try {
      host = new URL(ai.url).host;
    } catch {
      return set("error", "Own server: no valid address yet");
    }
    return set("ready", `Own server · ${ai.model ? `${ai.model} on ` : ""}${host}`);
  }
  const b = ai.builtin;
  if (!b.supported) return set("error", "Not available on this system: use your own server");
  if (b.downloaded) return set("ready", `Ready · ${b.model} on ${status?.gpu ? "GPU" : "CPU"}`);
  return set("missing", `${b.model} · downloads on first use (${(b.download_mb / 1000).toFixed(1)} GB)`);
}

function renderAiUrlNote(): void {
  const url = aiUrl.value.trim();
  let local = false;
  try {
    const host = new URL(url).hostname.replace(/^\[|\]$/g, "");
    local = host === "localhost" || host.endsWith(".localhost") || host === "::1" || /^127\.\d+\.\d+\.\d+$/.test(host);
  } catch {
    aiUrlNote.textContent = "Ollama: http://localhost:11434/v1 · LM Studio: http://localhost:1234/v1";
    aiUrlNote.dataset.kind = "";
    return;
  }
  aiUrlNote.textContent = local
    ? "On this computer: nothing leaves it."
    : `Not on this computer: your notes and the transcript go to ${new URL(url).hostname}.`;
  aiUrlNote.dataset.kind = local ? "" : "warn";
}

function saveAi(o: Parameters<Backend["saveOptions"]>[0]): void {
  void backend.saveOptions(o).then(refreshStatus, (err) => showError(String(err)));
}

aiEngine.addEventListener("change", () => {
  aiServer.hidden = aiEngine.value !== "server";
  const v = aiEngine.value;
  saveAi(v.startsWith("builtin:") ? { ai_engine: "builtin", ai_builtin_model: v.slice("builtin:".length) } : { ai_engine: "server" });
});
aiUrl.addEventListener("input", renderAiUrlNote);
aiUrl.addEventListener("change", () => saveAi({ ai_url: aiUrl.value }));
aiModel.addEventListener("change", () => saveAi({ ai_model: aiModel.value }));
aiKey.addEventListener("change", () => {
  if (!aiKey.value) return;
  saveAi({ ai_key: aiKey.value });
  aiKey.value = ""; // never kept in the page
});
aiKeyClear.addEventListener("click", () => saveAi({ ai_key: "" }));
aiAuto.addEventListener("change", () => saveAi({ ai_auto: aiAuto.checked }));

// ------------------------------------------------------------------ note apps (a folder)
/** Settings → Note apps, from the status. */
function renderExport(): void {
  const e = status?.export;
  if (!e) return;
  exportDir.textContent = e.dir || "No folder";
  exportOff.hidden = !e.dir;
  exportDirNote.textContent = e.vault
    ? `In the Obsidian vault “${e.vault}”: each meeting gets an Open in Obsidian button.`
    : "Obsidian: your vault, or a folder in it. Logseq: its pages folder. Google Drive, Dropbox or iCloud: a folder that their app keeps in sync.";
  exportAuto.checked = e.auto;
  exportAuto.disabled = !e.dir;
  for (const box of exportParts) box.checked = e.parts.includes(box.value);
}

function saveExport(o: Parameters<Backend["saveOptions"]>[0]): void {
  void backend.saveOptions(o).then(refreshStatus, (err) => showError(String(err)));
}

exportChoose.addEventListener("click", () => {
  void backend.chooseExportFolder().then(
    async (chosen) => {
      if (chosen) await refreshStatus();
    },
    (err) => showError(String(err)),
  );
});
exportOff.addEventListener("click", () => saveExport({ export_dir: "" }));
exportAuto.addEventListener("change", () => saveExport({ export_auto: exportAuto.checked }));
for (const box of exportParts) {
  box.addEventListener("change", () => saveExport({ export_parts: exportParts.filter((b) => b.checked).map((b) => b.value) }));
}

// ------------------------------------------------------------------ AI agents (MCP)

/** A path as one shell word: as it is when safe; a Windows path in double quotes (cmd and
 * PowerShell); else in single quotes, which bash and zsh never expand ($, `, \ stay as they are). */
function shellWord(path: string): string {
  if (/^[\w./+-]+$/.test(path)) return path;
  if (/^[A-Za-z]:\\/.test(path)) return `"${path}"`;
  return `'${path.replace(/'/g, `'\\''`)}'`;
}

/** The switch's save is on its way: a status from before it must not flip the switch back. */
let agentsSaving = false;

/** Settings → AI agents: the line that connects each agent, and the recording switch. */
function renderAgents(): void {
  const a = status?.agents;
  if (!a) return;
  if (!agentsSaving) agentsRecording.checked = a.recording;
  if (agentCommands.dataset.program === a.program) return;
  agentCommands.dataset.program = a.program;
  const p = shellWord(a.program);
  const config = JSON.stringify({ mcpServers: { lowkey: { command: a.program, args: ["--mcp"] } } }, null, 2);
  const rows: [string, string][] = [
    ["Claude Code", `claude mcp add -s user lowkey -- ${p} --mcp`],
    ["Codex", `codex mcp add lowkey -- ${p} --mcp`],
    ["Gemini CLI", `gemini mcp add -s user lowkey ${p} --mcp`],
    ["Other agents (Cursor, Claude Desktop, VS Code…): add this to their MCP settings", config],
  ];
  agentCommands.replaceChildren(
    ...rows.map(([name, text]) => {
      const row = document.createElement("div");
      row.className = "agent-command";
      const label = document.createElement("span");
      label.className = "label";
      label.textContent = name;
      const code = document.createElement("code");
      code.textContent = text;
      const copy = document.createElement("button");
      copy.type = "button";
      copy.className = "btn btn-outline btn-sm";
      copy.textContent = "Copy";
      copy.setAttribute("aria-label", `Copy the line for ${name.split(" (")[0].split(":")[0]}`);
      copy.addEventListener("click", () => {
        void backend.copy(text).then(() => {
          copy.textContent = "Copied";
          setTimeout(() => (copy.textContent = "Copy"), 1500);
        }, (err) => showError(String(err)));
      });
      row.append(label, code, copy);
      return row;
    }),
  );
}

agentsRecording.addEventListener("change", () => {
  agentsSaving = true;
  void backend
    .saveOptions({ mcp_recording: agentsRecording.checked })
    .then(refreshStatus, (err) => showError(String(err)))
    .finally(() => (agentsSaving = false));
});

/** The switch's save is on its way (as for the agents' switch). */
let voicesSaving = false;

/** Models → Who is speaking: the switch and the models' state. */
function renderVoices(): void {
  const v = status?.voices;
  if (!v) return;
  if (!voicesSaving) voicesOn.checked = v.on;
  voicesOn.disabled = !v.supported && !v.on; // on from another computer's config: it can be turned off
  const set = (state: string, text: string, progress: number | null = null) => {
    voicesStatus.dataset.state = state;
    voicesStatusText.textContent = text;
    voicesBar.hidden = progress === null;
    voicesBarFill.style.width = `${Math.floor((progress ?? 0) * 100)}%`;
  };
  if (!v.supported) return set("missing", "Not available in this build (Linux x86_64 only)");
  if (v.download.state === "downloading") return set("downloading", `Downloading… ${Math.floor(v.download.progress * 100)}% of 46 MB`, v.download.progress);
  if (v.download.state === "error") return set("error", `Download failed: ${v.download.error}`);
  if (!v.on) return set("missing", "Off");
  if (!v.present) return set("missing", "Models not downloaded");
  if (v.working.length) return set("loading", "Telling voices apart…");
  return set("ready", "Ready · on CPU");
}

voicesOn.addEventListener("change", () => {
  voicesSaving = true;
  void backend
    .saveOptions({ meeting_voices: voicesOn.checked })
    .then(refreshStatus, (err) => showError(String(err)))
    .finally(() => (voicesSaving = false));
});

/** Settings → General → Screen video: what this computer has, the screen remembered. */
let screenChecking = false;
function renderScreen(): void {
  const s = status?.screen;
  screenCard.hidden = !s?.supported;
  if (!s) return;
  const c = s.check;
  const checking = screenChecking || c.state === "checking";
  screenStatus.dataset.kind = !checking && c.state === "missing" ? "warn" : "";
  screenStatus.textContent = checking ? "Looking for GStreamer…" : c.state === "ready" ? `Ready: ${c.encoder}.` : c.problem;
  screenCheck.hidden = c.state !== "missing";
  screenCheck.disabled = checking;
  screenForget.hidden = !s.chosen;
}

screenCheck.addEventListener("click", () => {
  screenChecking = true;
  renderScreen();
  void backend
    .screenCheck()
    .catch((err) => showError(String(err)))
    .finally(() => {
      screenChecking = false;
      void refreshStatus();
    });
});

screenForget.addEventListener("click", () => {
  void backend.screenForget().then(() => {
    announce("The next Record screen asks which screen to share.");
    return refreshStatus();
  }, (err) => showError(String(err)));
});

function renderAbout(): void {
  if (!status) return;
  $<HTMLElement>("about-version").textContent = status.version ?? "–";
  $<HTMLElement>("about-device").textContent = status.device ?? "–";
}

void getBackend().then((b) => {
  backend = b;
  b.onStatus(applyStatus);
  b.onFileDrop(
    (over) => dropzone.classList.toggle("over", over),
    (files) => files[0] && handleRef(files[0]),
  );
  b.onOpenFiles((files) => files[0] && handleRef(files[0]));
  const liveDot = document.querySelector<HTMLElement>('.nav-item[data-section="meetings"] .nav-live')!;
  // Models → Notes shows the model at work: notes, or an answer (also its first download).
  const onAi = (kind: "notes" | "chat") => (e: AiNotesEvent) => {
    aiLive = e;
    aiLiveKind = kind;
    renderAiStatus();
    if (["done", "error", "cancelled"].includes(e.state)) {
      aiLive = null;
      void refreshStatus(); // "downloaded" may have changed
    }
  };
  b.meetings.onAiNotes(onAi("notes"));
  b.meetings.onAiChat(onAi("chat"));
  meetingsView = initMeetings(b, {
    announce,
    onRecording: (on) => (liveDot.hidden = !on),
    meetingKey: () => (status?.meeting_key_source === "app" ? status.meeting_key ?? "" : ""),
    showMeetings: () => showSection("meetings"),
    exportInfo: () => ({ dir: status?.export?.dir ?? "", vault: status?.export?.vault ?? null }),
    refreshStatus: () => void refreshStatus(),
    voicesWorking: () => status?.voices?.working ?? [],
    testing: () => !!status?.test,
  });
  void loadLanguages();
  void refreshStatus();
});
