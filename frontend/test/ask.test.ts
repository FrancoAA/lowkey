import { describe, expect, it, vi } from "vitest";
import { linkCitations } from "../src/ask";
import { paragraphs, speakerLabel } from "../src/meetings";

const sources = [
  { n: 1, meeting_id: "m-1", title: "Launch planning", started_at: 0, at: 9, text: "The page is ready." },
  { n: 2, meeting_id: "m-2", title: "Hiring sync", started_at: 0, at: null, text: "- hire" },
];

describe("citations", () => {
  it("turns [n] into links to their sources and drops numbers that are not sources", () => {
    const el = document.createElement("p");
    el.textContent = "Tuesday [1], and hiring [2, 7].";
    const open = vi.fn();
    linkCitations(el, sources, open);
    const links = [...el.querySelectorAll<HTMLButtonElement>("button.cite")];
    expect(links.map((l) => l.textContent)).toEqual(["1", "2"]);
    expect(links[0].title).toBe("Launch planning · 00:09");
    expect(links[1].title).toBe("Hiring sync · summary");
    expect(el.textContent).toBe("Tuesday 1, and hiring 2.");
    links[1].click();
    expect(open).toHaveBeenCalledWith(sources[1]);
  });
});

describe("transcript paragraphs", () => {
  it("joins one speaker's close lines and names the others", () => {
    const lines = [
      { speaker: "me" as const, start: 0, end: 2, text: "Hello" },
      { speaker: "me" as const, start: 2.5, end: 4, text: "everyone." },
      { speaker: "them" as const, start: 5, end: 7, text: "Hi.", person: 2 },
      { speaker: "me" as const, start: 30, end: 31, text: "Later." },
    ];
    const p = paragraphs(lines);
    expect(p.map((x) => [x.text, x.from, x.to])).toEqual([
      ["Hello everyone.", 0, 1],
      ["Hi.", 2, 2],
      ["Later.", 3, 3],
    ]);
    expect(speakerLabel({ people: ["Kara", ""] }, "them", 2)).toBe("Speaker 2");
    expect(speakerLabel({ people: ["Kara", ""] }, "them", 1)).toBe("Kara");
    expect(speakerLabel({ people: [] }, "them")).toBe("Them");
    expect(speakerLabel({}, "me")).toBe("Me");
  });
});
