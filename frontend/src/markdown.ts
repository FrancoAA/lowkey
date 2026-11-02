/**
 * The small part of Markdown that the AI notes use: headings, bullet and numbered lists,
 * paragraphs, **bold** and `code`. Built from DOM nodes and text nodes only (never innerHTML):
 * whatever the model writes stays text.
 */

function inline(parent: HTMLElement, text: string): void {
  // **bold** and `code`; everything else is plain text.
  const parts = text.split(/(\*\*[^*]+\*\*|`[^`]+`)/g);
  for (const part of parts) {
    if (!part) continue;
    if (part.startsWith("**") && part.endsWith("**") && part.length > 4) {
      const b = document.createElement("strong");
      b.textContent = part.slice(2, -2);
      parent.append(b);
    } else if (part.startsWith("`") && part.endsWith("`") && part.length > 2) {
      const c = document.createElement("code");
      c.textContent = part.slice(1, -1);
      parent.append(c);
    } else {
      parent.append(document.createTextNode(part));
    }
  }
}

export function renderMarkdown(el: HTMLElement, md: string): void {
  el.replaceChildren();
  let list: HTMLUListElement | HTMLOListElement | null = null;
  let para: HTMLParagraphElement | null = null;
  for (const raw of md.replace(/\r/g, "").split("\n")) {
    const line = raw.trimEnd();
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    const bullet = /^\s*[-*•]\s+(.*)$/.exec(line);
    const numbered = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (!line.trim()) {
      list = null;
      para = null;
    } else if (heading) {
      list = null;
      para = null;
      const h = document.createElement(heading[1].length <= 2 ? "h4" : "h5");
      inline(h, heading[2]);
      el.append(h);
    } else if (bullet || numbered) {
      para = null;
      const want = bullet ? "UL" : "OL";
      if (!list || list.tagName !== want) {
        list = document.createElement(bullet ? "ul" : "ol");
        el.append(list);
      }
      const li = document.createElement("li");
      inline(li, (bullet ?? numbered)![1]);
      list.append(li);
    } else {
      list = null;
      if (!para) {
        para = document.createElement("p");
        el.append(para);
      } else {
        para.append(document.createElement("br"));
      }
      inline(para, line.trim());
    }
  }
}
