/** A function that returns the current input level, 0..1. */
export type LevelSource = () => number;

/** Level from a Web Audio AnalyserNode (the in-page microphone). */
export function analyserLevel(analyser: AnalyserNode): LevelSource {
  const buf = new Float32Array(new ArrayBuffer(analyser.fftSize * 4));
  return () => {
    analyser.getFloatTimeDomainData(buf);
    let sum = 0;
    for (const v of buf) sum += v * v;
    const rms = Math.sqrt(sum / buf.length);
    // Speech RMS is roughly 0.01..0.3; map to 0..1 on a log-ish curve so quiet voices still move.
    // The server uses the same curve for the global-hotkey overlay (openwhisper/hotkey.py).
    return Math.min(1, Math.max(0, (20 * Math.log10(rms + 1e-6) + 55) / 45));
  };
}

/**
 * Live "listening" waveform: rounded bars that scroll left, each bar the input level at one
 * moment. Used by the page (mic via Web Audio) and by the global-hotkey overlay (levels
 * streamed from the server).
 */
export class Waveform {
  private ctx: CanvasRenderingContext2D;
  private levels: number[] = [];
  private raf = 0;
  private source: LevelSource | null = null;
  private lastPush = 0;
  private color = "#c026d3";

  constructor(private canvas: HTMLCanvasElement) {
    this.ctx = canvas.getContext("2d")!;
  }

  /** Most recent normalized level, 0..1 (for tests and the mic glow). */
  level = 0;

  start(source: LevelSource): void {
    this.source = source;
    this.levels = [];
    this.lastPush = 0;
    this.color = getComputedStyle(this.canvas).color || this.color;
    const tick = (t: number) => {
      this.sample(t);
      this.draw();
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  stop(): void {
    cancelAnimationFrame(this.raf);
    this.source = null;
    this.level = 0;
    this.levels = [];
    this.ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
  }

  private barCount(): number {
    return Math.max(8, Math.floor(this.canvas.width / (6 * devicePixelRatio)));
  }

  private sample(t: number): void {
    if (!this.source) return;
    const level = this.source();
    this.level = this.level * 0.6 + level * 0.4;
    if (t - this.lastPush > 45) {
      this.lastPush = t;
      this.levels.push(this.level);
      const n = this.barCount();
      if (this.levels.length > n) this.levels.splice(0, this.levels.length - n);
    }
  }

  private draw(): void {
    const { canvas, ctx } = this;
    const dpr = devicePixelRatio || 1;
    const w = Math.round(canvas.clientWidth * dpr);
    const h = Math.round(canvas.clientHeight * dpr);
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
    ctx.clearRect(0, 0, w, h);
    if (w < 8 || h < 8) return; // hidden or not laid out yet
    const n = this.barCount();
    const gap = 2 * dpr;
    const bw = Math.max(1, w / n - gap);
    ctx.fillStyle = this.color;
    const offset = n - this.levels.length;
    for (let i = 0; i < n; i++) {
      const lv = i < offset ? 0 : this.levels[i - offset];
      const bh = Math.max(3 * dpr, lv * (h - 4 * dpr));
      const x = i * (bw + gap) + gap / 2;
      const y = (h - bh) / 2;
      ctx.globalAlpha = i < offset ? 0.25 : 0.35 + 0.65 * (i / n);
      roundRect(ctx, x, y, bw, bh, Math.max(0, Math.min(bw, bh) / 2));
    }
    ctx.globalAlpha = 1;
  }
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number): void {
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, r);
  ctx.fill();
}
