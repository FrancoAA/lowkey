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
  /** A meeting is being recorded. */
  recording?: boolean;
  /** AI notes settings (the API key itself never comes to the page). */
  ai?: {
    engine: "builtin" | "server";
    url: string;
    model: string;
    has_key: boolean;
    auto: boolean;
    /** The server address is on this computer. */
    local: boolean;
    busy: boolean;
    builtin: { supported: boolean; downloaded: boolean; model: string; download_mb: number };
  };
  options?: { model: string; language: string; idle_unload_minutes: number; live_preview?: boolean; preview_engine?: string };
  /** Live preview: state off | preparing | ready | unavailable; engine whistle | parakeet. */
  preview?: { state: string; error: string | null; engine?: string };
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
}

/** Progress of the AI notes of one meeting. */
export interface AiNotesEvent {
  id: string;
  state: "preparing" | "downloading" | "writing" | "done" | "error" | "cancelled";
  detail: string;
  progress: number;
  /** The text so far (writing), or all of it (done). */
  text: string;
}

export interface MeetingLive {
  meeting: Meeting;
  state: "recording" | "finishing" | "done";
  elapsed: number;
  pending: number;
  notices: string[];
}

export interface MeetingSummary {
  id: string;
  title: string;
  started_at: number;
  duration: number;
  state: string;
  lines: number;
  excerpt: string;
}
