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
  options?: { model: string; language: string; idle_unload_minutes: number; live_preview?: boolean };
  /** Live preview engine (Whistle): off | preparing | ready | unavailable. */
  preview?: { state: string; error: string | null };
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
