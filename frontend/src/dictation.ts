import { analyserLevel, Waveform } from "./waveform";

export type DictationState = "idle" | "starting" | "listening";

export interface DictationCallbacks {
  onState(state: DictationState): void;
  onTick(elapsedMs: number): void;
  onAudio(blob: Blob, filename: string): void;
  onError(message: string): void;
}

const MIN_RECORDING_MS = 400;

function pickMime(): string {
  for (const m of ["audio/webm;codecs=opus", "audio/ogg;codecs=opus", "audio/webm", "audio/mp4"]) {
    if (typeof MediaRecorder !== "undefined" && MediaRecorder.isTypeSupported(m)) return m;
  }
  return "";
}

function micErrorMessage(err: unknown): string {
  const name = err instanceof DOMException ? err.name : "";
  if (name === "NotAllowedError" || name === "SecurityError")
    return "Microphone access is blocked. Click the lock icon in the address bar, allow the microphone, then try again.";
  if (name === "NotFoundError" || name === "OverconstrainedError") return "No microphone was found. Plug one in and try again.";
  if (name === "NotReadableError") return "The microphone is busy in another app. Close it there and try again.";
  if (!navigator.mediaDevices?.getUserMedia) return "This browser cannot record audio here. Open the app at http://127.0.0.1 (not a LAN address).";
  return `Could not start the microphone${err instanceof Error ? `: ${err.message}` : "."}`;
}

/** Microphone capture + live waveform. One recording at a time. */
export class Dictation {
  state: DictationState = "idle";
  private stream: MediaStream | null = null;
  private audioCtx: AudioContext | null = null;
  private recorder: MediaRecorder | null = null;
  private chunks: Blob[] = [];
  private startedAt = 0;
  private timer = 0;
  private cancelled = false;
  private stopRequested = false;

  constructor(private waveform: Waveform, private cb: DictationCallbacks) {}

  elapsed(): number {
    return this.state === "listening" ? performance.now() - this.startedAt : 0;
  }

  async start(): Promise<void> {
    if (this.state !== "idle") return;
    this.cancelled = false;
    this.stopRequested = false;
    this.setState("starting");
    try {
      this.stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
    } catch (err) {
      this.setState("idle");
      this.cb.onError(micErrorMessage(err));
      return;
    }
    if (this.stopRequested) {
      // Released before the mic opened: treat as a cancelled tap.
      this.release();
      this.setState("idle");
      return;
    }
    try {
      this.audioCtx = new AudioContext();
      // Created after an await, so it may start suspended under autoplay rules.
      await this.audioCtx.resume();
      const src = this.audioCtx.createMediaStreamSource(this.stream);
      const analyser = this.audioCtx.createAnalyser();
      analyser.fftSize = 1024;
      src.connect(analyser);

      const mime = pickMime();
      this.recorder = new MediaRecorder(this.stream, mime ? { mimeType: mime } : undefined);
      this.chunks = [];
      this.recorder.ondataavailable = (e) => e.data.size && this.chunks.push(e.data);
      this.recorder.onstop = () => this.finish();
      this.recorder.start(250);
      this.waveform.start(analyserLevel(analyser));
    } catch (err) {
      this.release();
      this.setState("idle");
      this.cb.onError(`Could not record from the microphone${err instanceof Error ? `: ${err.message}` : "."}`);
      return;
    }
    this.startedAt = performance.now();
    this.setState("listening");
    if (this.stopRequested) {
      // Push-to-talk released while the recorder was starting.
      this.stop();
      return;
    }
    this.timer = window.setInterval(() => this.cb.onTick(this.elapsed()), 200);
  }

  /** Stop and transcribe. */
  stop(): void {
    if (this.state === "starting") {
      this.stopRequested = true;
      return;
    }
    if (this.state !== "listening" || this.recorder?.state !== "recording") return;
    this.recorder.stop();
  }

  /** Stop and throw the audio away. */
  cancel(): void {
    this.cancelled = true;
    this.stop();
  }

  private finish(): void {
    const duration = performance.now() - this.startedAt;
    const type = this.recorder?.mimeType || "audio/webm";
    this.release();
    this.setState("idle");
    if (this.cancelled) return;
    if (duration < MIN_RECORDING_MS) {
      this.cb.onError("That was too short. Hold Space (or click the mic) while you talk.");
      return;
    }
    const ext = type.includes("ogg") ? "ogg" : type.includes("mp4") ? "m4a" : "webm";
    this.cb.onAudio(new Blob(this.chunks, { type }), `dictation.${ext}`);
  }

  private release(): void {
    clearInterval(this.timer);
    this.waveform.stop();
    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = null;
    void this.audioCtx?.close();
    this.audioCtx = null;
    this.recorder = null;
  }

  private setState(s: DictationState): void {
    this.state = s;
    this.cb.onState(s);
  }
}
