/**
 * Settings → General → Offer to record calls: offer to record when a call app uses the microphone (Linux), and each call's
 * choice: ask, always record, or never (desktop/src-tauri/src/calls.rs).
 */
import type { Status } from "./api";
import type { Backend } from "./backend";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export function renderCalls(backend: Backend, status: Status, onError: (m: string) => void): void {
  const card = $<HTMLElement>("calls-card");
  const detect = $<HTMLInputElement>("calls-detect");
  const apps = $<HTMLDivElement>("calls-apps");
  const c = status.calls;
  card.hidden = !c?.available;
  if (!c?.available) return;
  detect.checked = c.detect;
  if (!detect.dataset.wired) {
    detect.dataset.wired = "1";
    detect.addEventListener("change", () => {
      void backend.saveOptions({ call_detection: detect.checked }).catch((err) => onError(String(err)));
    });
  }
  apps.hidden = !c.detect || !c.apps.length;
  const signature = c.apps.map((a) => `${a.key}=${a.policy}`).join("|");
  if (apps.dataset.signature === signature) return;
  apps.dataset.signature = signature;
  apps.replaceChildren(
    ...c.apps.map((a) => {
      const row = document.createElement("label");
      row.className = "calls-app";
      const name = document.createElement("span");
      name.textContent = a.label;
      const select = document.createElement("select");
      select.className = "select";
      for (const [value, text] of [["ask", "Ask"], ["always", "Always record"], ["never", "Never"]]) select.add(new Option(text, value));
      select.value = a.policy;
      select.addEventListener("change", () => {
        void backend.saveOptions({ call_policy: { key: a.key, policy: select.value } }).catch((err) => onError(String(err)));
      });
      row.append(name, select);
      return row;
    }),
  );
}
