/** A function that returns the current input level, 0..1. */
export type LevelSource = () => number;

/**
 * Live "listening" waveform: rounded bars that scroll left, each bar the input level at one
 * moment. Used by the page and by the global-key overlay (levels from the
 * app's microphone recorder, src-tauri/src/audio.rs `level`).
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

/**
 * A meeting's two sources in one wave: "Me" (the microphone) above the centre line, "Them" (the
 * computer's sound) below it, each in its own colour (the canvas's --me and --them). Bars
 * scroll left like the dictation wave; levels come from the app's "meeting-level" events.
 */
export class DualWaveform {
  private ctx: CanvasRenderingContext2D;
  private me: number[] = [];
  private them: number[] = [];
  private raf = 0;
  private lastPush = 0;
  private smooth = [0, 0];
  private colors = ["#c026d3", "#0e7490"];
  private running = false;
  /** The newest levels, 0..1, set by whoever receives them. */
  levels = { me: 0, them: 0 };

  constructor(private canvas: HTMLCanvasElement) {
    this.ctx = canvas.getContext("2d")!;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.me = [];
    this.them = [];
    this.lastPush = 0;
    const css = getComputedStyle(this.canvas);
    this.colors = [css.getPropertyValue("--me").trim() || this.colors[0], css.getPropertyValue("--them").trim() || this.colors[1]];
    const tick = (t: number) => {
      this.sample(t);
      this.draw();
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  stop(): void {
    this.running = false;
    cancelAnimationFrame(this.raf);
    this.ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
  }

  private barCount(): number {
    return Math.max(8, Math.floor(this.canvas.width / (5 * devicePixelRatio)));
  }

  private sample(t: number): void {
    // A square root, so quiet speech still shows; a little smoothing between frames.
    this.smooth[0] = this.smooth[0] * 0.55 + Math.sqrt(Math.min(1, this.levels.me)) * 0.45;
    this.smooth[1] = this.smooth[1] * 0.55 + Math.sqrt(Math.min(1, this.levels.them)) * 0.45;
    if (t - this.lastPush > 50) {
      this.lastPush = t;
      this.me.push(this.smooth[0]);
      this.them.push(this.smooth[1]);
      const n = this.barCount();
      if (this.me.length > n) this.me.splice(0, this.me.length - n);
      if (this.them.length > n) this.them.splice(0, this.them.length - n);
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
    if (w < 8 || h < 8) return;
    const n = this.barCount();
    const gap = 2 * dpr;
    const bw = Math.max(1, w / n - gap);
    const mid = h / 2;
    const half = mid - 1.5 * dpr; // room for the centre gap
    const min = 1.5 * dpr;
    const offset = n - this.me.length;
    for (const [lane, values] of [[0, this.me], [1, this.them]] as const) {
      ctx.fillStyle = this.colors[lane];
      for (let i = 0; i < n; i++) {
        const lv = i < offset ? 0 : values[i - offset];
        const bh = Math.max(min, lv * half);
        const x = i * (bw + gap) + gap / 2;
        // Me grows up from the centre, Them grows down from it.
        const y = lane === 0 ? mid - 0.75 * dpr - bh : mid + 0.75 * dpr;
        ctx.globalAlpha = i < offset ? 0.18 : 0.3 + 0.7 * (i / n);
        roundRect(ctx, x, y, bw, bh, Math.max(0, Math.min(bw, bh) / 2));
      }
    }
    ctx.globalAlpha = 1;
  }
}
