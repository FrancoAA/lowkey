/** Data shapes shared with the app (desktop/src-tauri/src/app.rs, jobs.rs). */
export interface ModelInfo {
  name: string;
  label: string;
  approx_mb: number;
  engine?: "whisper-cpp" | "parakeet";
  ram_mb?: number;
  speed_gpu?: number | null;
  speed_cpu?: number;
  wer?: number;
  state: "missing" | "downloading" | "downloaded" | "loading" | "ready" | "error";
  downloaded: number;
  total: number;
  progress: number;
  error: string | null;
}

export interface Status {
  device: string;
  default_model: string;
  models: ModelInfo[];
  gpu?: boolean;
  auto_model?: string;
  hotkey?: string;
  version?: string;
  /** The dictation key ("SUPER + CTRL + X"; "" = none) and where it lives. */
  hotkey_key?: string;
  hotkey_source?: "app" | "config" | "manual" | "none";
  /** Why the key is not active (source "none" with a key). */
  hotkey_problem?: string;
  start_at_login?: boolean;
  /** The meeting key ("SUPER + CTRL + M"; "" = none), where it lives, why it is not active. */
  meeting_key?: string;
  meeting_key_source?: "app" | "manual" | "none";
  meeting_key_problem?: string;
  /** The paste-last key, like the meeting key. */
  paste_key?: string;
  paste_key_source?: "app" | "manual" | "none";
  paste_key_problem?: string;
  /** off | day | week | month | forever */
  history_keep?: string;
  /** Call detection: on/off, built here (Linux), and each call with its policy. */
  calls?: { detect: boolean; available: boolean; apps: { key: string; label: string; policy: "ask" | "always" | "never" }[] };
  /** A meeting is being recorded. */
  recording?: boolean;
  /** An automated test runs (OPENWHISPER_TEST_LOG): the page reports its player's events. */
  test?: boolean;
  /** Settings → Connections → AI agents: agents may start and stop recordings; the program they start. */
  agents?: { recording: boolean; program: string };
  /** Settings → Meeting settings → Tell the other voices apart: on, this build can, the models are on
   * disk, their download (state "" | downloading | error), and the meetings being worked on. */
  voices?: { on: boolean; supported: boolean; present: boolean; download: { state: string; progress: number; error: string }; working: string[] };
  /** Settings → Meeting settings → Screen video: this build can, a screen is remembered, and what this
   * computer has (state checking | ready | missing; the encoder; what to install). */
  screen?: { supported: boolean; chosen: boolean; check: { state: "checking" | "ready" | "missing"; encoder: string; problem: string } };
  /** Settings → Connections → Note apps: the folder ("" = off), by itself, the parts, the Obsidian vault it is in. */
  export?: { dir: string; auto: boolean; parts: string[]; vault: string | null };
  /** The AI providers (desktop/src-tauri/src/providers.rs), and the key slots that have a key
   * ("custom-speech": your own speech server). The keys never come to the page. */
  providers?: { list: Provider[]; keys: string[] };
  /** Settings → Models → Speech to text: provider "" = the model on this computer; `label`: the provider's model
   * that transcribes now ("gpt-4o-transcribe · OpenAI"), else null. */
  speech?: { provider: string; model: string; url: string; label: string | null };
  /** AI notes settings (the API key itself never comes to the page). */
  ai?: {
    /** "server": a provider (`provider`: "custom" is your own server). */
    engine: "builtin" | "server";
    provider: string;
    /** The built-in model's id (qwen3.5-4b, qwen3.5-2b, gemma-4-e4b). */
    builtin_model: string;
    url: string;
    model: string;
    has_key: boolean;
    auto: boolean;
    /** The user's instructions for the notes ("" = the app's), and the app's own. */
    prompt: string;
    default_prompt: string;
    builtin: {
      supported: boolean;
      downloaded: boolean;
      model: string;
      download_mb: number;
      models: { id: string; label: string; note: string; download_mb: number; downloaded: boolean }[];
    };
  };
  options?: {
    model: string;
    language: string;
    idle_unload_minutes: number;
    live_preview?: boolean;
    preview_engine?: string;
    /** The saved microphone ("" = the system default). */
    microphone?: string;
    vocabulary?: VocabTerm[];
    /** system | light | dark (Settings → General → Appearance). */
    theme?: string;
  };
  /** Live preview: state off | preparing | ready | unavailable; engine whistle | parakeet. */
  preview?: { state: string; error: string | null; engine?: string };
}

/** A provider that can write notes (`chat`) and/or transcribe (`speech`). */
/** A model that Settings offers, with what to know about it in a few words. */
export interface ProviderModel {
  id: string;
  note: string;
}

export interface Provider {
  id: string;
  label: string;
  /** "" for your own server (its address is in the settings). */
  url: string;
  chat: string | null;
  chat_model: string;
  /** The notes models to offer, the default first. */
  chat_models: ProviderModel[];
  speech: string | null;
  speech_model: string;
  /** The speech models to offer, the default first. */
  speech_models: ProviderModel[];
  keys_url: string;
  /** Its model list answers without a key (listing does not check the key). */
  public_models: boolean;
}

export interface Segment {
  start: number;
  end: number;
  text: string;
}

export interface JobResult {
  language: string | null;
  duration: number | null;
  segments: Segment[];
  text: string;
  srt: string;
  vtt: string;
}

export interface JobEvent {
  id: string;
  state: "queued" | "preparing" | "transcribing" | "done" | "error";
  progress?: number;
  message?: string;
  language?: string | null;
  language_probability?: number | null;
  new_segments?: Segment[];
  segment_count?: number;
  result?: JobResult | null;
  error?: string | null;
  model_state?: ModelInfo;
}

/** Meetings (desktop/src-tauri/src/meeting.rs). "me" = the microphone, "them" = the computer's sound. */
export interface MeetingLine {
  speaker: "me" | "them";
  start: number;
  end: number;
  text: string;
  /** "them" only: which of the others, told apart by voice (1, 2, …; see Meeting.people). */
  person?: number;
}

export interface Meeting {
  id: string;
  title: string;
  /** Unix seconds. */
  started_at: number;
  duration: number;
  /** "recording" while it runs (or when it was interrupted), else "done". */
  state: string;
  model: string;
  system_audio: boolean;
  notes: string;
  lines: MeetingLine[];
  /** Notes written by a language model (Markdown, a draft). */
  ai_notes?: string;
  ai_model?: string;
  ai_at?: number;
  /** The last time the notes could not be written: why, and when (unix seconds). Cleared when they are written. */
  ai_error?: string;
  ai_error_at?: number;
  /** Questions about the meeting and their answers, oldest first. */
  chat?: ChatMessage[];
  /** Settings → Connections → Note apps: the file it was saved as in the chosen folder ("" = not yet). */
  export_file?: string;
  /** The folder it was saved in. */
  export_dir?: string;
  /** One more for each fix of the transcript; `ai_version`: the version the AI notes were
   * written from (lower: Write notes again). */
  transcript_version?: number;
  ai_version?: number;
  /** Names of the others found by voice: people[n - 1] names person n ("" = "Speaker n"). */
  people?: string[];
  /** The screen videos (Record screen during the meeting), in the order they were recorded. */
  videos?: MeetingVideo[];
}

export interface MeetingVideo {
  /** Its file in the meetings folder. */
  file: string;
  /** Seconds from the meeting's start to the video's start: transcript time t is t - offset in it.
   * -1: not known (a crash came before it joined the meeting). */
  offset: number;
  width: number;
  height: number;
  codec: string;
  /** Bytes on disk; 0 while it records. */
  bytes: number;
  /** Its length in seconds; 0: not known (after a crash). */
  duration: number;
}

export interface ChatMessage {
  role: "user" | "assistant";
  text: string;
  /** Unix seconds. */
  at: number;
}

/** Progress of the AI notes of one meeting, or of the answer to a question about it. */
export interface AiNotesEvent {
  id: string;
  state: "preparing" | "downloading" | "writing" | "done" | "error" | "cancelled";
  detail: string;
  progress: number;
  /** The text so far (writing), or all of it (done). */
  text: string;
  /** done: the transcript version the notes were written from. */
  ai_version?: number;
}

export interface MeetingLive {
  meeting: Meeting;
  state: "recording" | "finishing" | "done";
  elapsed: number;
  pending: number;
  notices: string[];
  /** Your microphone is muted (the strip's button): silence goes in its place. */
  muted?: boolean;
  /** The screen video: "" (not recorded now) | waiting (the screen picker is open) | on. */
  screen?: "" | "waiting" | "on" | "stopping";
  /** Record screen can be offered (this build records the screen). */
  screen_supported?: boolean;
}

export interface MeetingSummary {
  id: string;
  title: string;
  started_at: number;
  duration: number;
  state: string;
  lines: number;
  /** The first words; in a search, the words around the first match. */
  excerpt: string;
  /** In a search: where the excerpt's match is (transcript | ai_notes | notes; "" = the title). */
  found_in?: string;
}

/** A vocabulary term (desktop/src-tauri/src/vocabulary.rs). */
export interface VocabTerm {
  term: string;
  sounds_like: string[];
}

/** A finished dictation (desktop/src-tauri/src/history.rs). */
export interface HistoryEntry {
  id: string;
  /** Unix seconds. */
  at: number;
  text: string;
  /** "key" (the global dictation key) or "window". */
  source: string;
  model: string;
  seconds: number;
}
