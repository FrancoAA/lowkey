/**
 * Settings → General → Microphone: which input to record (desktop/src-tauri/src/microphones.rs),
 * which one is in use and why, a 4-second test with a level meter, and (Linux) the switch that
 * keeps a Bluetooth headset out of call mode.
 */
import type { Backend, Microphones } from "./backend";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export function initMicrophone(backend: Backend, onError: (message: string) => void): { refresh(): void } {
  const select = $<HTMLSelectElement>("mic-select");
  const note = $<HTMLElement>("mic-note");
  const test = $<HTMLButtonElement>("mic-test");
  const meter = $<HTMLDivElement>("mic-meter");
  const fill = $<HTMLDivElement>("mic-meter-fill");
  const btRow = $<HTMLDivElement>("mic-bt-row");
  const bt = $<HTMLInputElement>("mic-bt");

  function render(m: Microphones): void {
    const def = m.mics.find((x) => x.default);
    const options = [new Option(`System default${def ? ` — ${def.name}` : ""}`, "")];
    for (const x of m.mics) options.push(new Option(`${x.name}${x.bluetooth ? " · Bluetooth" : ""}${x.hardware ? "" : " · virtual"}`, x.id));
    // A saved microphone that is not connected stays in the menu, marked.
    if (m.saved && !m.mics.some((x) => x.id === m.saved)) options.push(new Option(`${m.saved} · not connected`, m.saved));
    const signature = options.map((o) => o.value + o.text).join("|");
    if (select.dataset.signature !== signature) {
      select.replaceChildren(...options);
      select.dataset.signature = signature;
    }
    select.value = m.selectable ? m.saved : "";
    select.disabled = !m.selectable;
    note.textContent = m.using.note ?? `Using ${m.using.name}.`;
    btRow.hidden = !m.bluetooth_aware;
    bt.checked = m.avoid_bluetooth;
  }

  async function refresh(): Promise<void> {
    try {
      render(await backend.microphones());
    } catch (err) {
      onError(String(err));
    }
  }

  select.addEventListener("change", () => {
    void backend.saveOptions({ microphone: select.value }).then(refresh, (err) => onError(String(err)));
  });
  bt.addEventListener("change", () => {
    void backend.saveOptions({ avoid_bluetooth_mic: bt.checked }).then(refresh, (err) => onError(String(err)));
  });
  // Devices come and go: the list is read again when the menu gets the focus.
  select.addEventListener("focus", () => void refresh());
  test.addEventListener("click", async () => {
    test.disabled = true;
    test.textContent = "Listening…";
    meter.hidden = false;
    fill.style.width = "0%";
    try {
      await backend.micTest();
    } catch (err) {
      onError(String(err));
      test.disabled = false;
      test.textContent = "Test";
      meter.hidden = true;
    }
  });
  backend.onMicLevel((lv) => {
    if (lv < 0) {
      test.disabled = false;
      test.textContent = "Test";
      meter.hidden = true;
      return;
    }
    fill.style.width = `${Math.round(Math.min(1, Math.sqrt(lv)) * 100)}%`;
  });
  void refresh();
  return { refresh: () => void refresh() };
}
