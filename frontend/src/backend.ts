/**
 * Everything the page needs from the app (Tauri commands and events) goes through here.
 */
import type { JobEvent, Status } from "./api";

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
  /** Take the key over from the user's Hyprland config block. Returns the backup's path. */
  adoptHotkey(): Promise<{ status: Status; backup: string; start_at_login: boolean }>;
  onStatus(cb: (s: Status) => void): void;
  pickFile(): Promise<FileRef | null>;
  onFileDrop(onHover: (over: boolean) => void, onDrop: (files: FileRef[]) => void): void;
  onOpenFiles(cb: (files: FileRef[]) => void): void;
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
  };
}

export async function getBackend(): Promise<Backend> {
  return desktopBackend();
}
