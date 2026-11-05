/**
 * Notes → transcript: the transcript paragraphs that a point of the notes most likely comes from.
 * The model writes the notes from the transcript, so a point shares its rare words (names,
 * figures, terms) with the place it comes from. Each word counts by how rare it is in the
 * meeting (IDF): "budget" said twice counts more than "the" said everywhere. Words are compared
 * without case and accents, by their first 5 letters ("contrato" and "contratos", "send" and
 * "sends"). A time in the point, "(04:19)", picks its paragraph first.
 * No model and no network: it runs in the page, in a few milliseconds, also for old notes.
 */

/** What this needs of a transcript paragraph. */
export interface SourceText {
  start: number;
  /** Seconds; a time after the last paragraph's end points nowhere. */
  end?: number;
  text: string;
}

export interface Source {
  /** The paragraph's index in the list given. */
  index: number;
  /** 0..1: how much of the point's weight this paragraph has. */
  score: number;
  /** Where the matched words are in the paragraph's text: [from, to) character offsets. */
  ranges: [number, number][];
}

const WORD = /[\p{L}\p{N}]+/gu;
const STEM = 5;
/** Words that say nothing about where a point comes from (English, Spanish; without accents).
 * Other languages: their common words count little anyway (IDF), but they are marked. */
const STOP = new Set(
  (
    "the and for with that this these those from are was were been being have has had will would " +
    "can could should you your yours our ours they them their its his her she him not but into " +
    "about then than also just very there here what which who when where why how all any some " +
    "los las del por para con que una uno unos unas como mas pero sus nos esta este esto estos estas " +
    "ese esa eso esos esas hay muy sin sobre entre cuando donde porque tambien todo todos toda todas " +
    "ser son fue era han hemos tiene tienen ella ellos ellas les lo al se su mi tu yo"
  ).split(" "),
);

function fold(word: string): string {
  return word.normalize("NFKD").replace(/\p{M}/gu, "").toLowerCase();
}

/** The key a word is compared by; null for a word too short to tell anything. */
function key(word: string): string | null {
  const w = fold(word);
  if (/^\p{N}+$/u.test(w)) return w; // numbers in full: "15" is not "150"
  if (w.length < 3 || STOP.has(w)) return null;
  return w.slice(0, STEM);
}

function keys(text: string): Set<string> {
  const out = new Set<string>();
  for (const m of text.matchAll(WORD)) {
    const k = key(m[0]);
    if (k) out.add(k);
  }
  return out;
}

/** "(04:19)", "(1:04:19)" → seconds. Only in brackets, as the transcript's times are given:
 * "at 10:30" in a point is a time of day, not a moment of the meeting. */
function times(text: string): number[] {
  const out: number[] = [];
  for (const m of text.matchAll(/\((?:(\d{1,2}):)?(\d{1,2}):(\d{2})\)/g)) {
    out.push(Number(m[1] ?? 0) * 3600 + Number(m[2]) * 60 + Number(m[3]));
  }
  return out;
}

/** The transcript, read once for many points. */
export class SourceIndex {
  private readonly paras: SourceText[];
  private readonly paraKeys: Set<string>[];
  private readonly idf = new Map<string, number>();

  constructor(paras: SourceText[]) {
    this.paras = paras;
    this.paraKeys = paras.map((p) => keys(p.text));
    const df = new Map<string, number>();
    for (const ks of this.paraKeys) for (const k of ks) df.set(k, (df.get(k) ?? 0) + 1);
    const n = paras.length;
    for (const [k, d] of df) this.idf.set(k, Math.log((n + 1) / (d + 0.5)));
  }

  /**
   * The best paragraphs for `point` (most likely first; at most `max`; none when nothing fits).
   * `context`: the topic's heading and the point above a sub-point; they help, with less weight.
   */
  find(point: string, context: string[] = [], max = 3): Source[] {
    if (!this.paras.length) return [];
    const weight = new Map<string, number>();
    for (const c of context) for (const k of keys(c)) weight.set(k, 0.3);
    const own = keys(point);
    for (const k of own) weight.set(k, 1);
    let total = 0;
    for (const [k, w] of weight) total += w * (this.idf.get(k) ?? 0);

    const scored: { index: number; score: number; matched: Set<string> }[] = [];
    this.paraKeys.forEach((ks, index) => {
      let score = 0;
      const matched = new Set<string>();
      let ownHits = 0;
      for (const [k, w] of weight) {
        if (!ks.has(k)) continue;
        score += w * this.idf.get(k)!;
        matched.add(k);
        if (own.has(k)) ownHits++;
      }
      // One shared word is chance, unless the point is that short.
      if (ownHits >= Math.min(2, own.size) && ownHits > 0 && total > 0) scored.push({ index, score: score / total, matched });
    });
    scored.sort((a, b) => b.score - a.score || a.index - b.index);

    // A time in the point: its paragraph first, whatever its words.
    const pinned: number[] = [];
    const last = this.paras[this.paras.length - 1];
    const endsAt = (last.end ?? last.start + 60) + 5;
    for (const t of times(point)) {
      if (t > endsAt) continue;
      let at = -1;
      this.paras.forEach((p, i) => {
        if (Math.floor(p.start) <= t) at = i; // the transcript shows each start rounded down
      });
      if (at >= 0 && !pinned.includes(at)) pinned.push(at);
    }

    const best = scored[0]?.score ?? 0;
    const out: Source[] = [];
    for (const i of pinned) {
      const s = scored.find((x) => x.index === i);
      out.push({ index: i, score: s?.score ?? 1, ranges: this.ranges(i, s?.matched ?? new Set()) });
    }
    if (best >= 0.2) {
      for (const s of scored) {
        if (out.length >= max) break;
        if (s.score < best * 0.6) break;
        if (out.some((o) => o.index === s.index)) continue;
        out.push({ index: s.index, score: s.score, ranges: this.ranges(s.index, s.matched) });
      }
    }
    return out.slice(0, max);
  }

  private ranges(index: number, matched: Set<string>): [number, number][] {
    const out: [number, number][] = [];
    for (const m of this.paras[index].text.matchAll(WORD)) {
      const k = key(m[0]);
      if (k && matched.has(k)) out.push([m.index!, m.index! + m[0].length]);
    }
    return out;
  }
}

/**
 * A part of `text` around most of the `ranges` (about `size` characters), with "…" where it was
 * cut; the ranges moved to the part's offsets.
 */
export function excerpt(text: string, ranges: [number, number][], size = 240): { text: string; ranges: [number, number][] } {
  if (text.length <= size) return { text, ranges };
  // The window that shows the most matches: each starts a little before a match, and is counted
  // with the bounds it is shown with (less up to 30 characters, to end at a space).
  let from = 0;
  let most = -1;
  for (const [a] of ranges) {
    const w = Math.max(0, Math.min(a - 40, text.length - size));
    const n = ranges.filter(([b, e]) => b >= w && e <= w + size - 30).length;
    if (n > most) {
      most = n;
      from = w;
    }
  }
  // Start and end at a space, not inside a word.
  if (from > 0) {
    const sp = text.indexOf(" ", from);
    if (sp >= 0 && sp - from < 20) from = sp + 1;
  }
  let to = Math.min(text.length, from + size);
  if (to < text.length) {
    const sp = text.lastIndexOf(" ", to);
    if (sp > from + size - 30) to = sp;
  }
  const lead = from > 0 ? "…" : "";
  const tail = to < text.length ? "…" : "";
  const moved = ranges
    .filter(([a, b]) => a >= from && b <= to)
    .map(([a, b]) => [a - from + lead.length, b - from + lead.length] as [number, number]);
  return { text: lead + text.slice(from, to) + tail, ranges: moved };
}
