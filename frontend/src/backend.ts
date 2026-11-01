/**
 * One UI, two backends: the Python web server (fetch + Server-Sent Events) and the Tauri
 * desktop app (invoke + events). Everything the page needs from "the app" goes through here.
 */
import { followJob, getLanguages, getStatus, startJob, type JobEvent, type Status } from "./api";

export interface FileRef {
  name: string;
  blob?: Blob; // web
  path?: string; // desktop
  size?: number;
  type?: string;
}

export interface Options {
  model?: string;
  language?: string;
  idle_unload_minutes?: number;
}

export interface Backend {
  desktop: boolean;
  getStatus(): Promise<Status>;
  getLanguages(): Promise<Record<string, string>>;
  start(file: FileRef, language: string, model: string): Promise<string>;
  follow(jobId: string, onEvent: (e: JobEvent) => void, onLost: () => void): () => void;
  copy(text: string): Promise<void>;
  /** Returns false when the user cancelled. */
  save(fileName: string, body: string, mime: string): Promise<boolean>;
  saveOptions(o: Options): Promise<void>;
  onStatus(cb: (s: Status) => void): void;
  pickFile(): Promise<FileRef | null>;
  onFileDrop(onHover: (over: boolean) => void, onDrop: (files: FileRef[]) => void): void;
  onOpenFiles(cb: (files: FileRef[]) => void): void;
}

export const isDesktop = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const baseName = (p: string) => p.split(/[\\/]/).pop() || p;

// ------------------------------------------------------------------ web (Python server)
const web: Backend = {
  desktop: false,
  getStatus,
  getLanguages,
  start: (f, language, model) => startJob(f.blob!, f.name, language, model),
  follow: followJob,
  async copy(text) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const ta = Object.assign(document.createElement("textarea"), { value: text });
      document.body.append(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
  },
  async save(fileName, body, mime) {
    const url = URL.createObjectURL(new Blob([body], { type: `${mime};charset=utf-8` }));
    const a = Object.assign(document.createElement("a"), { href: url, download: fileName });
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return true;
  },
  async saveOptions(o) {
    if (o.model !== undefined) localStorage.setItem("ow.model", o.model);
    if (o.language !== undefined) localStorage.setItem("ow.language", o.language);
  },
  onStatus() {},
  async pickFile() {
    return null; // the <input type=file> handles it
  },
  onFileDrop() {}, // HTML drag and drop handles it
  onOpenFiles() {},
};

// ------------------------------------------------------------------ desktop (Tauri)
async function desktopBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  const dialog = await import("@tauri-apps/plugin-dialog");
  const toRefs = (paths: string[]): FileRef[] => paths.map((path) => ({ path, name: baseName(path) }));

  return {
    desktop: true,
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
    async save(fileName, body) {
      const ext = fileName.split(".").pop() ?? "txt";
      const path = await dialog.save({ defaultPath: fileName, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
      if (!path) return false;
      await invoke("save_text", { path, content: body });
      return true;
    },
    async saveOptions(o) {
      await invoke("set_options", { options: o });
    },
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
  return isDesktop ? desktopBackend() : web;
}
