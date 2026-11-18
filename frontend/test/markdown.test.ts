import { describe, expect, it } from "vitest";
import { renderMarkdown } from "../src/markdown";
import { highlighted, searchWords } from "../src/highlight";

describe("markdown", () => {
  it("builds headings, nested lists and bold from text nodes only", () => {
    const el = document.createElement("div");
    renderMarkdown(el, "## Launch\n- **Ana** ships Friday (00:41)\n  - after QA\n- <img src=x onerror=alert(1)>");
    expect(el.querySelector("h4")?.textContent).toBe("Launch");
    const items = el.querySelectorAll(":scope > ul > li");
    expect(items.length).toBe(2);
    expect(items[0].querySelector("strong")?.textContent).toBe("Ana");
    expect(items[0].querySelector("ul li")?.textContent).toBe("after QA");
    expect(el.querySelector("img")).toBeNull();
    expect(items[1].textContent).toBe("<img src=x onerror=alert(1)>");
  });
});

describe("search marks", () => {
  it("marks every word, any case, also inside a word", () => {
    const words = searchWords("  frid SHIP ");
    expect(words).toEqual(["frid", "ship"]);
    const box = document.createElement("p");
    box.append(...highlighted("Ship the new version on Friday", words));
    expect([...box.querySelectorAll("mark")].map((m) => m.textContent)).toEqual(["Ship", "Frid"]);
    expect(box.textContent).toBe("Ship the new version on Friday");
  });
});
