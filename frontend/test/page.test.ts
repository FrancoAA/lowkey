/**
 * The whole page on the demo stand-in (src/demo.ts): it opens on Meetings, a meeting has its
 * tabs and menus, My notes are kept, Ctrl+K searches, Settings is one page, Ask cites sources.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const settle = (ms = 50) => new Promise((r) => setTimeout(r, ms));
async function until(check: () => boolean, ms = 2000): Promise<void> {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error("timed out");
    await settle(20);
  }
}
const visible = (el: HTMLElement) => !el.closest("[hidden]");
const key = (k: string, o: KeyboardEventInit = {}) => document.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, ...o }));

beforeAll(async () => {
  const html = readFileSync(resolve(__dirname, "../index.html"), "utf8");
  document.body.innerHTML = html.slice(html.indexOf("<body>") + 6, html.indexOf('<script type="module"'));
  // happy-dom has no canvas drawing: the waveforms get a stand-in context.
  HTMLCanvasElement.prototype.getContext = (() => new Proxy({}, { get: () => () => {} })) as never;
  window.history.replaceState(null, "", "/?crash&gpuoff"); // the demo's run before stopped; safe mode
  await import("../src/main");
  await until(() => document.querySelectorAll(".meeting-item").length === 3);
});

describe("the window", () => {
  it("opens on Meetings, grouped by day", () => {
    expect(visible($("section-meetings"))).toBe(true);
    expect(document.querySelector('.nav-item[data-section="meetings"]')?.getAttribute("aria-current")).toBe("page");
    expect([...document.querySelectorAll(".meeting-day h3")].map((h) => h.textContent)).toContain("Today");
  });

  it("a meeting opens on its summary, with chips, tabs and menus", async () => {
    document.querySelector<HTMLButtonElement>(".meeting-item-open")!.click();
    await until(() => visible($("meeting-view")));
    expect($<HTMLInputElement>("meeting-title").value).toBe("Launch planning");
    expect([...document.querySelectorAll(".meta-chip")].map((c) => c.textContent)).toContain("Kara, Lincoln");
    expect($("tab-notes").getAttribute("aria-selected")).toBe("true");
    expect($("ai-body").querySelector("h4")?.textContent).toBe("Launch plan");
    $("share-button").click();
    expect($("share-menu").hidden).toBe(false);
    expect($("copy-notes").hidden).toBe(false);
    document.body.click();
    expect($("share-menu").hidden).toBe(true);
  });

  it("My notes are kept as you type", async () => {
    $("tab-mine").click();
    expect(visible($("mine-panel"))).toBe(true);
    const input = $<HTMLTextAreaElement>("mine-input");
    expect(input.value).toContain("asset list");
    input.value = "- check the links";
    input.dispatchEvent(new Event("input"));
    await settle(700); // saved after a short pause
    $("meeting-back").click();
    await until(() => visible($("meetings-home")));
    document.querySelector<HTMLButtonElement>(".meeting-item-open")!.click();
    await until(() => visible($("meeting-view")));
    $("tab-mine").click();
    expect($<HTMLTextAreaElement>("mine-input").value).toBe("- check the links");
  });

  // (Ctrl+J does the same in the app; happy-dom has no layout, so the page's "on screen" check fails here.)
  it("the Ask button opens the questions about the meeting, and closes them", () => {
    $("chat-toggle").click();
    expect($("meeting-body").hasAttribute("data-chat")).toBe(true);
    expect($("chat-toggle").getAttribute("aria-expanded")).toBe("true");
    $("chat-close").click();
    expect($("meeting-body").hasAttribute("data-chat")).toBe(false);
  });

  it("Ctrl+K searches the meetings", async () => {
    key("k", { ctrlKey: true });
    await until(() => visible($("meetings-home")));
    expect(document.activeElement).toBe($("meetings-search"));
  });

  it("Settings is one page with tabs; Esc goes back", () => {
    key(",", { ctrlKey: true });
    expect(visible($("settings-page"))).toBe(true);
    expect(document.activeElement?.getAttribute("data-section")).toBe("general"); // the focus on its first tab
    expect(visible($("section-general"))).toBe(true);
    document.querySelector<HTMLButtonElement>('#settings-tabs .tab[data-section="meeting-settings"]')!.click();
    expect(visible($("section-meeting-settings"))).toBe(true);
    expect($("nav-settings").getAttribute("aria-current")).toBe("page");
    key("Escape");
    expect(visible($("settings-page"))).toBe(false);
    expect(visible($("section-meetings"))).toBe(true);
  });

  it("after a crash, a notice offers a report, which also goes to the clipboard", async () => {
    await until(() => !$("crash-notice").hidden);
    expect($("crash-reason").textContent).toBe("illegal instruction");
    $("crash-copy").click();
    await until(() => $("crash-notice").hidden);
    expect($("problem-note").textContent).toBe("The report is on the clipboard.");
    expect($("problem-note").hidden).toBe(false);
  });

  it("in safe mode, a notice says why the CPU is used, and closes", async () => {
    await until(() => !$("gpu-notice").hidden);
    expect($("gpu-reason").textContent).toBe("The GPU check stopped (access violation).");
    $("gpu-close").click();
    expect($("gpu-notice").hidden).toBe(true);
  });

  it("Ask answers with links to its sources", async () => {
    document.querySelector<HTMLButtonElement>('.nav-item[data-section="ask"]')!.click();
    const input = $<HTMLTextAreaElement>("ask-input");
    input.value = "When does the page go live?";
    input.dispatchEvent(new Event("input"));
    $<HTMLFormElement>("ask-form").requestSubmit();
    await until(() => document.querySelectorAll("#ask-log .cite").length === 2);
    expect(document.querySelectorAll(".ask-source").length).toBe(2);
    expect($("ask-new").hidden).toBe(false);
  });
});
