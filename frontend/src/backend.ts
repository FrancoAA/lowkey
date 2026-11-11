/**
 * Everything the page needs from the app (Tauri commands and events) goes through here.
 */
import type { AiNotesEvent, HistoryEntry, JobEvent, Meeting, MeetingLive, MeetingSummary, Status, VocabTerm } from "./api";

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
  mcp_recording?: boolean;
  meeting_voices?: boolean;
  /** "" turns the note-app folder off. */
  export_dir?: string;
  export_auto?: boolean;
  export_parts?: string[];
  vocabulary?: { term: string; sounds_like: string[] }[];
  history_keep?: string;
  /** system | light | dark. */
  theme?: string;
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
  /** Quit the app (as the tray menu's Quit). */
  quit(): Promise<void>;
  /** Settings → Connections → Note apps: the system's folder dialog (saved at once); false when cancelled. */
  chooseExportFolder(): Promise<boolean>;
  /** Settings → Screen video → Choose the screen again: the next Record screen shows the picker. */
  screenForget(): Promise<void>;
  /** Settings → Screen video → Check again: what this computer has (comes with the status). */
  screenCheck(): Promise<void>;
  /** Automated tests only: what only the page sees, to the test log ("page-…" events). */
  testEvent(event: string, payload: unknown): Promise<void>;
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
  /** Newest first; `query`: only the meetings with all of its words. */
  list(query?: string): Promise<MeetingSummary[]>;
  get(id: string): Promise<Meeting>;
  update(id: string, patch: { title?: string; notes?: string }): Promise<void>;
  remove(id: string): Promise<void>;
  /** Notes and transcript as Markdown, with times in this computer's time zone. */
  markdown(id: string): Promise<string>;
  /** One part, for the Copy button of its tab: the AI notes, or the transcript (no title or date). */
  text(id: string, part: "notes" | "transcript"): Promise<string>;
  onLive(cb: (l: MeetingLive) => void): void;
  onLevels(cb: (lv: { me: number; them: number }) => void): void;
  /** Start writing the AI notes of a saved meeting; progress comes through onAiNotes. */
  aiNotes(id: string): Promise<void>;
  /** Stop the AI's job: the notes being written, or an answer. */
  aiCancel(): Promise<void>;
  onAiNotes(cb: (e: AiNotesEvent) => void): void;
  /** Ask about a saved meeting; the answer comes through onAiChat, and is saved with it. */
  chat(id: string, question: string): Promise<void>;
  chatClear(id: string): Promise<void>;
  onAiChat(cb: (e: AiNotesEvent) => void): void;
  /** The strip's "Open the notes": the app asks the window to open this meeting. */
  onOpen(cb: (id: string) => void): void;
  /** A transcript paragraph fixed (lines from..to as `paragraphs` made it; `before`: its text then).
   * The saved meeting, and a Vocabulary suggestion when the fix looks like a name or term. */
  editParagraph(id: string, from: number, to: number, before: string, text: string, speaker: "me" | "them", person: number): Promise<{ meeting: Meeting; suggestion: VocabTerm | null }>;
  /** A name for one of the others found by voice; the name of another one merges the two. */
  nameSpeaker(id: string, person: number, name: string): Promise<Meeting>;
  /** Telling the voices apart after Stop: working | done (with the meeting) | error. */
  onVoices(cb: (e: { id: string; state: string; detail: string; meeting?: Meeting }) => void): void;
  /** Add a term (or one more spelling of it) to the Vocabulary. */
  addVocabulary(term: VocabTerm): Promise<void>;
  /** The address the page plays screen video `file` of the meeting from (null: not on disk). */
  videoUrl(id: string, file: string): Promise<string | null>;
  /** Record screen (true) or Stop screen (false) in the meeting being recorded. */
  screen(on: boolean): Promise<void>;
  /** Save to the note-app folder; its file and, in an Obsidian vault, its obsidian:// link. */
  saveToFolder(id: string): Promise<{ file: string; obsidian: string | null }>;
  openInObsidian(id: string): Promise<void>;
  /** A save to the folder (also by itself): saved | kept (edited outside) | error. */
  onExport(cb: (e: { id: string; state: string; file: string; detail: string }) => void): void;
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
    quit: () => invoke("app_quit"),
    // The app opens the save dialog and writes the file; the page never names a path.
    save: (fileName, body) => invoke<boolean>("save_export", { fileName, content: body }),
    async saveOptions(o) {
      await invoke("set_options", { options: o });
    },
    chooseExportFolder: async () => (await invoke<unknown>("choose_export_folder")) !== null,
    screenForget: () => invoke("screen_forget"),
    screenCheck: () => invoke("screen_check"),
    testEvent: (event, payload) => invoke("test_event", { event, payload }),
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
      list: (query) => invoke<MeetingSummary[]>("meetings_list", { query: query || null }),
      get: (id) => invoke<Meeting>("meeting_get", { id }),
      update: (id, patch) => invoke("meeting_update", { id, title: patch.title ?? null, notes: patch.notes ?? null }),
      remove: (id) => invoke("meeting_delete", { id }),
      markdown: (id) => invoke<string>("meeting_markdown", { id, utcOffsetMinutes: -new Date().getTimezoneOffset() }),
      text: (id, part) => invoke<string>("meeting_text", { id, part }),
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
      chat: (id, question) => invoke("meeting_chat", { id, question }),
      chatClear: (id) => invoke("meeting_chat_clear", { id }),
      onAiChat(cb) {
        void listen<AiNotesEvent>("ai-chat", (e) => cb(e.payload));
      },
      onOpen(cb) {
        void listen<string>("open-meeting", (e) => cb(e.payload));
      },
      editParagraph: (id, from, to, before, text, speaker, person) =>
        invoke<{ meeting: Meeting; suggestion: VocabTerm | null }>("meeting_edit_paragraph", { id, from, to, before, text, speaker, person }),
      nameSpeaker: (id, person, name) => invoke<Meeting>("meeting_name_speaker", { id, person, name }),
      onVoices(cb) {
        void listen<{ id: string; state: string; detail: string; meeting?: Meeting }>("meeting-voices", (e) => cb(e.payload));
      },
      addVocabulary: async (term) => {
        await invoke("vocabulary_add", { term });
      },
      videoUrl: (id, file) => invoke<string | null>("meeting_video_url", { id, file }),
      screen: (on) => invoke("meeting_screen", { on }),
      saveToFolder: (id) => invoke<{ file: string; obsidian: string | null }>("meeting_save_to_folder", { id }),
      openInObsidian: (id) => invoke("meeting_open_in_obsidian", { id }),
      onExport(cb) {
        void listen<{ id: string; state: string; file: string; detail: string }>("meeting-export", (e) => cb(e.payload));
      },
    },
  };
}

export async function getBackend(): Promise<Backend> {
  return desktopBackend();
}
