/**
 * In-window dictation: the app records the microphone (cpal), the page only shows the
 * waveform (levels arrive as "window-level" events) and follows the job.
 */
import { Waveform } from "./waveform";

export type DictationState = "idle" | "starting" | "listening";

export interface DictationCallbacks {
  onState(state: DictationState): void;
  onTick(elapsedMs: number): void;
  onJob(jobId: string): void;
  onError(message: string): void;
}

export class Dictation {
  state: DictationState = "idle";
  private level = 0;
  private startedAt = 0;
  private timer = 0;
  /** A stop or cancel that came while the microphone was still opening. */
  private pending: "stop" | "cancel" | null = null;
  private invoke!: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

  constructor(private waveform: Waveform, private cb: DictationCallbacks) {
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
    this.pending = null;
    this.set("starting");
    try {
      await this.invoke("window_dictation", { action: "start" });
    } catch (err) {
      this.pending = null;
      this.set("idle");
      this.cb.onError(String(err));
      return;
    }
    const pending = this.pending;
    this.pending = null;
    if (pending === "cancel") {
      this.set("idle");
      void this.invoke("window_dictation", { action: "cancel" });
      return;
    }
    this.level = 0;
    this.waveform.start(() => this.level);
    this.startedAt = performance.now();
    this.set("listening");
    this.timer = window.setInterval(() => this.cb.onTick(this.elapsed()), 200);
    if (pending === "stop") void this.stop(); // released before the mic was open
  }

  async stop(): Promise<void> {
    if (this.state === "starting") {
      this.pending ??= "stop";
      return;
    }
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
    if (this.state === "starting") {
      this.pending = "cancel"; // done when the start returns (the app must have opened the mic first)
      return;
    }
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
