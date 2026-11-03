/**
 * Everything the page needs from the app (Tauri commands and events) goes through here.
 */
import type { AiNotesEvent, HistoryEntry, JobEvent, Meeting, MeetingLive, MeetingSummary, Status } from "./api";

export interface FileRef {
  name: string;
  path: string;
}

export interface Options {
  model?: string;
  language?: string;
  idle_unload_minutes?: number;
  live_preview?: boolean;
  preview_engine?: string;
  start_at_login?: boolean;
  ai_engine?: string;
  ai_builtin_model?: string;
  ai_url?: string;
  ai_model?: string;
  /** "" removes the key. */
  ai_key?: string;
  ai_auto?: boolean;
  vocabulary?: { term: string; sounds_like: string[] }[];
  history_keep?: string;
  microphone?: string;
  avoid_bluetooth_mic?: boolean;
  call_detection?: boolean;
  call_policy?: { key: string; policy: string };
}

export interface Microphones {
  mics: { id: string; name: string; bluetooth: boolean; hardware: boolean; default: boolean }[];
  using: { id: string | null; name: string; note: string | null };
  saved: string;
  avoid_bluetooth: boolean;
  /** Bluetooth inputs are recognised here (Linux). */
  bluetooth_aware: boolean;
  /** A microphone can be chosen here (Linux: needs the PipeWire/PulseAudio library). */
  selectable: boolean;
}

export interface Backend {
  getStatus(): Promise<Status>;
  getLanguages(): Promise<Record<string, string>>;
  start(file: FileRef, language: string, model: string): Promise<string>;
  follow(jobId: string, onEvent: (e: JobEvent) => void, onLost: () => void): () => void;
  copy(text: string): Promise<void>;
  /** Returns false when the user cancelled. */
  save(fileName: string, body: string, mime: string): Promise<boolean>;
  saveOptions(o: Options): Promise<void>;
  /** Move the dictation key now (Hyprland syntax, "SUPER + ALT + D"). */
  setHotkey(key: string): Promise<Status>;
  /** Move the meeting key now. */
  setMeetingHotkey(key: string): Promise<Status>;
  /** Move the paste-last key now. */
  setPasteHotkey(key: string): Promise<Status>;
  microphones(): Promise<Microphones>;
  /** Open the chosen microphone for 4 s; its level comes through onMicLevel (-1 = ended). */
  micTest(): Promise<void>;
  onMicLevel(cb: (level: number) => void): void;
  history: {
    list(query: string): Promise<HistoryEntry[]>;
    remove(id: string): Promise<boolean>;
    clear(): Promise<void>;
    /** A new dictation was kept. */
    onAdded(cb: (e: HistoryEntry) => void): void;
  };
  /** Take the key over from the user's Hyprland config block. Returns the backup's path. */
  adoptHotkey(): Promise<{ status: Status; backup: string; start_at_login: boolean }>;
  onStatus(cb: (s: Status) => void): void;
  pickFile(): Promise<FileRef | null>;
  onFileDrop(onHover: (over: boolean) => void, onDrop: (files: FileRef[]) => void): void;
  onOpenFiles(cb: (files: FileRef[]) => void): void;
  meetings: MeetingsBackend;
}

export interface MeetingsBackend {
  start(title: string): Promise<MeetingLive>;
  /** Stops, transcribes the last seconds and saves; returns the saved meeting. */
  stop(): Promise<Meeting>;
  live(): Promise<MeetingLive | null>;
  list(): Promise<MeetingSummary[]>;
  get(id: string): Promise<Meeting>;
  update(id: string, patch: { title?: string; notes?: string }): Promise<void>;
  remove(id: string): Promise<void>;
  /** Notes and transcript as Markdown, with times in this computer's time zone. */
  markdown(id: string): Promise<string>;
  onLive(cb: (l: MeetingLive) => void): void;
  onLevels(cb: (lv: { me: number; them: number }) => void): void;
  /** Start writing the AI notes of a saved meeting; progress comes through onAiNotes. */
  aiNotes(id: string): Promise<void>;
  aiCancel(): Promise<void>;
  onAiNotes(cb: (e: AiNotesEvent) => void): void;
}

const baseName = (p: string) => p.split(/[\\/]/).pop() || p;

async function desktopBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  const dialog = await import("@tauri-apps/plugin-dialog");
  const toRefs = (paths: string[]): FileRef[] => paths.map((path) => ({ path, name: baseName(path) }));

  return {
    getStatus: () => invoke<Status>("get_status"),
    getLanguages: () => invoke<Record<string, string>>("get_languages"),
    start: (f, language, model) => invoke<string>("transcribe_path", { path: f.path, language, model }),
    follow(jobId, onEvent) {
      let stopped = false;
      const unlisten = listen<JobEvent>("job", (e) => {
        if (stopped || e.payload.id !== jobId) return;
        onEvent(e.payload);
        if (e.payload.state === "done" || e.payload.state === "error") {
          stopped = true;
          void unlisten.then((f) => f());
        }
      });
      // Catch up in case the job moved before the listener was ready.
      void invoke<JobEvent | null>("job_snapshot", { id: jobId }).then((s) => {
        if (s && !stopped) {
          onEvent(s);
          if (s.state === "done" || s.state === "error") {
            stopped = true;
            void unlisten.then((f) => f());
          }
        }
      });
      return () => {
        stopped = true;
        void unlisten.then((f) => f());
      };
    },
    copy: (text) => invoke("copy_text", { text }),
    // The app opens the save dialog and writes the file; the page never names a path.
    save: (fileName, body) => invoke<boolean>("save_export", { fileName, content: body }),
    async saveOptions(o) {
      await invoke("set_options", { options: o });
    },
    setHotkey: (key) => invoke<Status>("set_hotkey", { key }),
    setMeetingHotkey: (key) => invoke<Status>("set_meeting_hotkey", { key }),
    setPasteHotkey: (key) => invoke<Status>("set_paste_hotkey", { key }),
    microphones: () => invoke<Microphones>("microphones"),
    micTest: () => invoke("mic_test"),
    onMicLevel(cb) {
      void listen<number>("mic-level", (e) => cb(e.payload));
    },
    history: {
      list: (query) => invoke<HistoryEntry[]>("history_list", { query }),
      remove: (id) => invoke<boolean>("history_delete", { id }),
      clear: () => invoke("history_clear"),
      onAdded(cb) {
        void listen<HistoryEntry>("history", (e) => cb(e.payload));
      },
    },
    adoptHotkey: () => invoke<{ status: Status; backup: string; start_at_login: boolean }>("adopt_hotkey"),
    onStatus(cb) {
      void listen<Status>("status", (e) => cb(e.payload));
    },
    async pickFile() {
      const path = await dialog.open({
        multiple: false,
        directory: false,
        filters: [
          { name: "Audio and video", extensions: ["mp3", "wav", "m4a", "aac", "ogg", "oga", "opus", "flac", "webm", "wma", "aif", "aiff", "amr", "mp4", "m4v", "mov", "mkv", "avi", "3gp"] },
          { name: "All files", extensions: ["*"] },
        ],
      });
      return typeof path === "string" ? { path, name: baseName(path) } : null;
    },
    onFileDrop(onHover, onDrop) {
      void getCurrentWebview().onDragDropEvent((e) => {
        const p = e.payload;
        if (p.type === "enter" || p.type === "over") onHover(true);
        else if (p.type === "leave") onHover(false);
        else if (p.type === "drop") {
          onHover(false);
          onDrop(toRefs(p.paths));
        }
      });
    },
    onOpenFiles(cb) {
      void listen<string[]>("open-files", (e) => cb(toRefs(e.payload)));
      void invoke<string[]>("take_open_files").then((paths) => paths.length && cb(toRefs(paths)));
    },
    meetings: {
      start: (title) => invoke<MeetingLive>("meeting_start", { title }),
      stop: () => invoke<Meeting>("meeting_stop"),
      live: () => invoke<MeetingLive | null>("meeting_live"),
      list: () => invoke<MeetingSummary[]>("meetings_list"),
      get: (id) => invoke<Meeting>("meeting_get", { id }),
      update: (id, patch) => invoke("meeting_update", { id, title: patch.title ?? null, notes: patch.notes ?? null }),
      remove: (id) => invoke("meeting_delete", { id }),
      markdown: (id) => invoke<string>("meeting_markdown", { id, utcOffsetMinutes: -new Date().getTimezoneOffset() }),
      onLive(cb) {
        void listen<MeetingLive>("meeting", (e) => cb(e.payload));
      },
      onLevels(cb) {
        void listen<{ me: number; them: number }>("meeting-level", (e) => cb(e.payload));
      },
      aiNotes: (id) => invoke("meeting_ai_notes", { id }),
      aiCancel: () => invoke("ai_notes_cancel"),
      onAiNotes(cb) {
        void listen<AiNotesEvent>("ai-notes", (e) => cb(e.payload));
      },
    },
  };
}

export async function getBackend(): Promise<Backend> {
  return desktopBackend();
}
