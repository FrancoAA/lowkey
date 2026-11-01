/**
 * In-window dictation for the desktop app: the app records the microphone (cpal), the page
 * only shows the waveform (levels arrive as "window-level" events) and follows the job.
 * Same surface as the browser Dictation class.
 */
import type { DictationState } from "./dictation";
import { Waveform } from "./waveform";

export interface DesktopDictationCallbacks {
  onState(state: DictationState): void;
  onTick(elapsedMs: number): void;
  onJob(jobId: string): void;
  onError(message: string): void;
}

export class DesktopDictation {
  state: DictationState = "idle";
  private level = 0;
  private startedAt = 0;
  private timer = 0;
  private invoke!: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

  constructor(private waveform: Waveform, private cb: DesktopDictationCallbacks) {
    void (async () => {
      const core = await import("@tauri-apps/api/core");
      this.invoke = core.invoke;
      const { listen } = await import("@tauri-apps/api/event");
      void listen<number>("window-level", (e) => (this.level = e.payload));
      void listen<string>("window-dictation-end", (e) => {
        if (this.state !== "listening") return;
        if (e.payload === "limit") void this.stop();
        else {
          this.reset();
          this.cb.onError(`The microphone stopped: ${e.payload.replace(/^failed: /, "")}`);
        }
      });
    })();
  }

  elapsed(): number {
    return this.state === "listening" ? performance.now() - this.startedAt : 0;
  }

  async start(): Promise<void> {
    if (this.state !== "idle") return;
    this.set("starting");
    try {
      await this.invoke("window_dictation", { action: "start" });
    } catch (err) {
      this.set("idle");
      this.cb.onError(String(err));
      return;
    }
    this.level = 0;
    this.waveform.start(() => this.level);
    this.startedAt = performance.now();
    this.set("listening");
    this.timer = window.setInterval(() => this.cb.onTick(this.elapsed()), 200);
  }

  async stop(): Promise<void> {
    if (this.state !== "listening") return;
    this.reset();
    try {
      const jobId = await this.invoke<string | null>("window_dictation", { action: "stop" });
      if (jobId) this.cb.onJob(jobId);
    } catch (err) {
      this.cb.onError(String(err));
    }
  }

  cancel(): void {
    if (this.state === "idle") return;
    this.reset();
    void this.invoke("window_dictation", { action: "cancel" });
  }

  private reset(): void {
    clearInterval(this.timer);
    this.waveform.stop();
    this.set("idle");
  }

  private set(s: DictationState): void {
    this.state = s;
    this.cb.onState(s);
  }
}
