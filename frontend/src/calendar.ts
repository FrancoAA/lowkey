/**
 * Settings → Meetings → Calendar: link a private iCal address, or remove it
 * (desktop/src-tauri/src/calendar.rs). The address goes to the app once and never comes back to
 * the page; the status says which calendar is linked and whether it could be read.
 */
import type { Status } from "./api";
import type { Backend } from "./backend";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export function renderCalendar(backend: Backend, status: Status, onError: (m: string) => void, changed: () => void): void {
  const add = $<HTMLDivElement>("calendar-add");
  const linked = $<HTMLDivElement>("calendar-linked");
  const input = $<HTMLInputElement>("calendar-url");
  const link = $<HTMLButtonElement>("calendar-link");
  const unlink = $<HTMLButtonElement>("calendar-unlink");
  const name = $<HTMLElement>("calendar-name");
  const c = status.calendar;
  add.hidden = !!c?.linked;
  linked.hidden = !c?.linked;
  if (c?.linked) {
    name.textContent = c.error ? `${c.name || "Calendar"}: ${c.error}` : c.name || "Linked";
    name.toggleAttribute("data-error", !!c.error);
  }
  if (link.dataset.wired) return;
  link.dataset.wired = "1";
  const submit = async () => {
    const url = input.value.trim();
    if (!url) return input.focus();
    link.disabled = true;
    link.textContent = "Checking…";
    try {
      await backend.calendar.link(url);
      input.value = "";
      changed();
    } catch (err) {
      onError(String(err));
    } finally {
      link.disabled = false;
      link.textContent = "Link";
    }
  };
  link.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      void submit();
    }
  });
  unlink.addEventListener("click", async () => {
    try {
      await backend.calendar.unlink();
      changed();
    } catch (err) {
      onError(String(err));
    }
  });
}
