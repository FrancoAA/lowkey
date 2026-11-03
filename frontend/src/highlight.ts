/**
 * Search results: the searched words marked (<mark>) in a text, any case, also inside a word,
 * the same way the app matches them (desktop/src-tauri/src/history.rs, meeting.rs).
 */
export function searchWords(query: string): string[] {
  return query.split(/\s+/).filter(Boolean).map((w) => w.toLowerCase());
}

/** `text` as nodes, with each match of `words` in a <mark>. */
export function highlighted(text: string, words: string[]): Node[] {
  if (!words.length) return [document.createTextNode(text)];
  const escaped = [...words].sort((a, b) => b.length - a.length).map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  const re = new RegExp(`(${escaped.join("|")})`, "giu");
  const out: Node[] = [];
  let last = 0;
  for (const m of text.matchAll(re)) {
    const at = m.index ?? 0;
    if (at > last) out.push(document.createTextNode(text.slice(last, at)));
    const mark = document.createElement("mark");
    mark.textContent = m[0];
    out.push(mark);
    last = at + m[0].length;
  }
  if (last < text.length) out.push(document.createTextNode(text.slice(last)));
  return out;
}
