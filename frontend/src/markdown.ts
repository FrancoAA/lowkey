/**
 * The small part of Markdown that the notes and answers use: headings, bullet and numbered
 * lists (indented ones go inside the item above them), paragraphs, **bold** and `code`. Built from DOM nodes and text nodes only (never innerHTML):
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
  /** The open lists, outermost first, with the indent of their items. */
  let lists: { indent: number; list: HTMLUListElement | HTMLOListElement }[] = [];
  let para: HTMLParagraphElement | null = null;
  for (const raw of md.replace(/\r/g, "").split("\n")) {
    const line = raw.trimEnd();
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    const bullet = /^\s*[-*•]\s+(.*)$/.exec(line);
    const numbered = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (!line.trim()) {
      para = null; // a blank line between items keeps the list
    } else if (heading) {
      lists = [];
      para = null;
      const h = document.createElement(heading[1].length <= 2 ? "h4" : "h5");
      inline(h, heading[2]);
      el.append(h);
    } else if (bullet || numbered) {
      para = null;
      const indent = /^\s*/.exec(line)![0].replace(/\t/g, "    ").length;
      const tag = bullet ? "UL" : "OL";
      while (lists.length && indent < lists[lists.length - 1].indent) lists.pop();
      let top = lists[lists.length - 1];
      if (top && indent === top.indent && top.list.tagName !== tag) {
        lists.pop(); // the same level, another kind of list: a new list after it
        top = lists[lists.length - 1];
      }
      if (!top || indent > top.indent || top.list.tagName !== tag) {
        const list = document.createElement(bullet ? "ul" : "ol");
        // Deeper than the open list: inside its last item.
        const parent = top ? (top.list.lastElementChild as HTMLElement | null) ?? top.list : el;
        parent.append(list);
        lists.push({ indent, list });
      }
      const li = document.createElement("li");
      inline(li, (bullet ?? numbered)![1]);
      lists[lists.length - 1].list.append(li);
    } else {
      lists = [];
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
