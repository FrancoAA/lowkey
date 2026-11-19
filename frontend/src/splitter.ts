/**
 * The meeting page's panels can be resized: drag the line between two panels (or focus it and
 * use the arrow keys); a double click puts it back. The sizes are kept on this computer.
 *
 * Without a screen video, the line between the notes and the chat sets the chat's width. With
 * one, it sets the width of the notes (on the right), and a second line between the video and
 * the chat sets the video's height. The page's CSS reads the sizes from --chat-w, --notes-w and
 * --video-h on .meeting-body; with no size set it uses its own.
 *
 * On a wide window the Meetings list is a column on the left: the line on its right sets its
 * width (--list-w on the section).
 */

type PanelSize = "chat-w" | "notes-w" | "video-h";
type Size = PanelSize | "list-w";

/** The smallest size of each panel, in px. */
const MIN_SIDE = 320; // the left column (notes or video and chat)
const MIN_CHAT = 260;
const MIN_NOTES = 300;
const MIN_VIDEO = 120;
const MIN_CHAT_H = 200; // the chat under the video
const STEP = 24; // an arrow key
/** The Meetings list: its normal width, its limits, and what it leaves to the open meeting. */
const LIST_W = 340;
const MIN_LIST = 260;
const MAX_LIST = 560;
const MIN_MEETING = 560;
const STEP_BIG = 96; // with Shift

function load(key: Size): number | null {
  try {
    const v = Number(localStorage.getItem(`lowkey.meeting.${key}`));
    return Number.isFinite(v) && v > 0 ? v : null;
  } catch {
    return null;
  }
}

function save(key: Size, v: number | null): void {
  try {
    if (v === null) localStorage.removeItem(`lowkey.meeting.${key}`);
    else localStorage.setItem(`lowkey.meeting.${key}`, String(Math.round(v)));
  } catch {
    // private mode: only for now
  }
}

export function resizablePanels(body: HTMLElement, opts: {
  /** The line between the left and the right column. */
  cols: HTMLElement;
  /** The line between the video and the chat (with a video). */
  rows: HTMLElement;
  video: HTMLVideoElement;
  chat: HTMLElement;
  notes: HTMLElement;
}): void {
  const { cols, rows, video, chat, notes } = opts;
  /** What the user chose (null: the page's own size). */
  const wanted: Record<PanelSize, number | null> = { "chat-w": load("chat-w"), "notes-w": load("notes-w"), "video-h": load("video-h") };

  const withVideo = (): boolean => body.dataset.video !== undefined;
  /** The size `cols` sets now. */
  const colKey = (): PanelSize => (withVideo() ? "notes-w" : "chat-w");
  /** The width of the line's track (the gap between the two columns). */
  const track = (el: HTMLElement): number => el.getBoundingClientRect()[el === cols ? "width" : "height"] || 20;

  function limits(key: PanelSize): [number, number] {
    if (key === "video-h") {
      const head = video.parentElement?.previousElementSibling?.getBoundingClientRect().height ?? 30;
      let max = body.clientHeight - track(rows) - MIN_CHAT_H - head - 8;
      // Not taller than the video at this width (it keeps its shape: more is only black).
      if (video.videoWidth > 0) max = Math.min(max, (video.clientWidth * video.videoHeight) / video.videoWidth);
      return [MIN_VIDEO, Math.max(MIN_VIDEO, max)];
    }
    const max = body.clientWidth - track(cols) - MIN_SIDE;
    const min = key === "chat-w" ? MIN_CHAT : MIN_NOTES;
    return [min, Math.max(min, max)];
  }

  /** The size `key` has on screen now. */
  function current(key: PanelSize): number {
    if (key === "chat-w") return chat.getBoundingClientRect().width;
    if (key === "notes-w") return notes.getBoundingClientRect().width;
    return video.getBoundingClientRect().height;
  }

  /** Puts the chosen sizes on the page, inside what the window allows now. */
  function apply(): void {
    for (const key of Object.keys(wanted) as PanelSize[]) {
      const v = wanted[key];
      if (v === null) {
        body.style.removeProperty(`--${key}`);
        continue;
      }
      const [min, max] = limits(key);
      body.style.setProperty(`--${key}`, `${Math.round(Math.min(max, Math.max(min, v)))}px`);
    }
    describe();
  }

  /** The lines' values for a screen reader. */
  function describe(): void {
    for (const [el, key] of [[cols, colKey()], [rows, "video-h"]] as [HTMLElement, PanelSize][]) {
      const [min, max] = limits(key);
      el.setAttribute("aria-valuemin", String(Math.round(min)));
      el.setAttribute("aria-valuemax", String(Math.round(max)));
      el.setAttribute("aria-valuenow", String(Math.round(current(key))));
    }
    cols.setAttribute("aria-label", withVideo() ? "Width of the notes" : "Width of the chat");
  }

  function set(key: PanelSize, v: number | null): void {
    if (v !== null) {
      const [min, max] = limits(key);
      v = Math.min(max, Math.max(min, v));
    }
    wanted[key] = v;
    save(key, v);
    apply();
  }

  // Drag: the right column (and the chat) grows to the left, the video grows down.
  function drag(el: HTMLElement, key: () => PanelSize): void {
    el.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const k = key();
      const start = current(k);
      const x0 = e.clientX;
      const y0 = e.clientY;
      el.setPointerCapture(e.pointerId);
      body.dataset.resizing = k === "video-h" ? "rows" : "cols";
      const move = (ev: PointerEvent): void => {
        set(k, k === "video-h" ? start + (ev.clientY - y0) : start - (ev.clientX - x0));
      };
      const end = (): void => {
        el.removeEventListener("pointermove", move);
        el.removeEventListener("pointerup", end);
        el.removeEventListener("pointercancel", end);
        delete body.dataset.resizing;
      };
      el.addEventListener("pointermove", move);
      el.addEventListener("pointerup", end);
      el.addEventListener("pointercancel", end);
    });
    el.addEventListener("dblclick", () => set(key(), null));
    el.addEventListener("keydown", (e) => {
      const k = key();
      const step = e.shiftKey ? STEP_BIG : STEP;
      const grow: Record<string, number> = k === "video-h" ? { ArrowDown: step, ArrowUp: -step } : { ArrowLeft: step, ArrowRight: -step };
      const d = grow[e.key];
      if (d !== undefined) set(k, current(k) + d);
      else if (e.key === "Home") set(k, limits(k)[0]);
      else if (e.key === "End") set(k, limits(k)[1]);
      else return;
      e.preventDefault();
    });
  }
  drag(cols, colKey);
  drag(rows, () => "video-h");

  // A smaller window, a video shown or gone, its size known: the sizes fit again.
  new ResizeObserver(apply).observe(body);
  new MutationObserver(apply).observe(body, { attributes: true, attributeFilter: ["data-video"] });
  video.addEventListener("loadedmetadata", apply);
  apply();
}

/** The Meetings list's width on a wide window: drag `line` (or focus it and use the arrow keys;
 * Shift: bigger steps; Home and End: smallest and largest); a double click puts it back. */
export function resizableList(section: HTMLElement, line: HTMLElement): void {
  let wanted = load("list-w");

  function limits(): [number, number] {
    return [MIN_LIST, Math.max(MIN_LIST, Math.min(MAX_LIST, section.clientWidth - MIN_MEETING))];
  }
  function width(): number {
    const [min, max] = limits();
    return Math.round(Math.min(max, Math.max(min, wanted ?? LIST_W)));
  }
  function apply(): void {
    if (!section.clientWidth) return; // another page: no width to go by
    section.style.setProperty("--list-w", `${width()}px`);
    const [min, max] = limits();
    line.setAttribute("aria-valuemin", String(Math.round(min)));
    line.setAttribute("aria-valuemax", String(Math.round(max)));
    line.setAttribute("aria-valuenow", String(width()));
  }
  function set(v: number | null): void {
    if (v !== null) {
      const [min, max] = limits();
      v = Math.min(max, Math.max(min, v));
    }
    wanted = v;
    save("list-w", v);
    apply();
  }

  // Drag: the list grows to the right.
  line.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const start = width();
    const x0 = e.clientX;
    line.setPointerCapture(e.pointerId);
    section.dataset.resizing = "";
    const move = (ev: PointerEvent): void => set(start + (ev.clientX - x0));
    const end = (): void => {
      line.removeEventListener("pointermove", move);
      line.removeEventListener("pointerup", end);
      line.removeEventListener("pointercancel", end);
      delete section.dataset.resizing;
    };
    line.addEventListener("pointermove", move);
    line.addEventListener("pointerup", end);
    line.addEventListener("pointercancel", end);
  });
  line.addEventListener("dblclick", () => set(null));
  line.addEventListener("keydown", (e) => {
    const step = e.shiftKey ? STEP_BIG : STEP;
    if (e.key === "ArrowRight") set(width() + step);
    else if (e.key === "ArrowLeft") set(width() - step);
    else if (e.key === "Home") set(limits()[0]);
    else if (e.key === "End") set(limits()[1]);
    else return;
    e.preventDefault();
  });

  // A smaller window, or the page shown again: the width fits again.
  new ResizeObserver(apply).observe(section);
  apply();
}
