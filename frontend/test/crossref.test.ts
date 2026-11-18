import { describe, expect, it } from "vitest";
import { excerpt, SourceIndex } from "../src/crossref";

const paras = [
  { start: 0, end: 8, text: "Good morning everyone, let's start with the weekly numbers." },
  { start: 9, end: 20, text: "The Acme contract renews in March at forty thousand dollars." },
  { start: 21, end: 30, text: "Ana sends the contracts to legal by Friday." },
  { start: 31, end: 40, text: "We should also hire a second designer this quarter." },
];

describe("notes → transcript", () => {
  const index = new SourceIndex(paras);
  it("finds the paragraph with the point's rare words", () => {
    const found = index.find("**Ana** sends the contract to legal by Friday");
    expect(found[0].index).toBe(2);
    expect(found[0].ranges.length).toBeGreaterThan(0);
  });
  it("compares words by their first letters, without case or accents", () => {
    expect(index.find("ACME CONTRATO renews")[0].index).toBe(1);
  });
  it("a time in brackets picks its paragraph first; a time of day does not", () => {
    expect(index.find("Hiring (00:33)")[0].index).toBe(3);
    expect(index.find("Hire a second designer, interviews at 10:30")[0].index).toBe(3);
    expect(index.find("Interviews at 00:10")).toEqual([]);
  });
  it("finds nothing for a point the transcript does not have", () => {
    expect(index.find("Quantum entanglement roadmap")).toEqual([]);
  });
});

describe("excerpt", () => {
  it("keeps short texts whole and cuts long ones around the matches", () => {
    expect(excerpt("short", [[0, 5]]).text).toBe("short");
    const long = `${"filler ".repeat(80)}needle here ${"tail ".repeat(80)}`;
    const at = long.indexOf("needle");
    const e = excerpt(long, [[at, at + 6]], 100);
    expect(e.text.startsWith("…") && e.text.endsWith("…")).toBe(true);
    const [a, b] = e.ranges[0];
    expect(e.text.slice(a, b)).toBe("needle");
  });
});
