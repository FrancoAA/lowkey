"""End-to-end check of the global dictation hotkey, in a nested Hyprland.

Your desktop is not touched: the test starts a second (nested) Hyprland whose window sits on a
temporary headless output of your session, so it is never on your screen and never takes your
keyboard focus. Keyboard, focus and clipboard inside it are separate from yours.

Inside the nested Hyprland it:
- starts open-whisper with a fake microphone (a speech file played in real time); the app adds
  its SUPER + CTRL + X bind and overlay window rule to that Hyprland, as it would to yours;
- opens a terminal and checks that the press and release binds exist; then it runs the bound
  command through Hyprland's own exec dispatcher, exactly as a key press/release would
  (Hyprland ignores binds from virtual keyboards such as wtype, so a physical key press cannot
  be simulated); hold = push-to-talk, then tap mode;
- checks: the overlay appears without taking focus, it shows a live level, the transcript is
  typed (wtype) into the terminal and is on the clipboard (wl-copy);
- saves screenshots (grim) to docs/screenshots/.
Then it stops everything and removes the headless output and the temporary window rule.

Usage: uv run python scripts/e2e_hotkey.py
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SHOTS = ROOT / "docs" / "screenshots"
SPEECH = ROOT / "tests" / "fixtures" / "hello.ogg"
OUTPUT = "OWE2E"
CLI = ROOT / "scripts" / "ow-dictate"
NESTED_CONFIG = """
-- Minimal nested Hyprland for the open-whisper hotkey test.
hl.config({ misc = { disable_hyprland_logo = true, disable_splash_rendering = true, disable_watchdog_warning = true },
            ecosystem = { no_donation_nag = true }, decoration = { rounding = 12 } })
"""


def run(args: list[str], env: dict | None = None, check: bool = True) -> str:
    out = subprocess.run(args, capture_output=True, text=True, env=env)
    if check and out.returncode:
        raise RuntimeError(f"{args}: {out.stderr or out.stdout}")
    return out.stdout


def wait(cond, timeout: float, what: str, step: float = 0.1):
    end = time.time() + timeout
    while time.time() < end:
        value = cond()
        if value:
            return value
        time.sleep(step)
    raise AssertionError(f"timed out waiting for {what}")


def check(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionError(msg)
    print(f"  ok  {msg}")


def get(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=3) as r:
        return json.loads(r.read())


def up(url: str) -> bool:
    try:
        get(url + "/api/status")
        return True
    except Exception:
        return False


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def instances() -> list[dict]:
    return json.loads(run(["hyprctl", "instances", "-j"]) or "[]")


def main() -> None:
    for tool in ("hyprctl", "Hyprland", "wtype", "wl-paste", "grim", "foot", "ffmpeg"):
        if not shutil.which(tool):
            sys.exit(f"needs {tool}")
    if not os.environ.get("HYPRLAND_INSTANCE_SIGNATURE"):
        sys.exit("needs a running Hyprland session")

    tmp = Path(tempfile.mkdtemp(prefix="ow-hotkey-e2e-"))
    (tmp / "hyprland.lua").write_text(NESTED_CONFIG)
    mic = tmp / "mic.wav"  # speech + 4 s of silence, so the loop does not repeat inside one take
    run(["ffmpeg", "-loglevel", "error", "-y", "-i", str(SPEECH), "-af", "apad=pad_dur=4", "-ar", "16000", "-ac", "1", str(mic)])
    typed = tmp / "typed.txt"

    nested = server = None
    try:
        # ---- nested Hyprland, parked on a headless output of the real session
        run(["hyprctl", "output", "create", "headless", OUTPUT])
        run(["hyprctl", "eval",
             '_G.ow_e2e_rule = hl.window_rule({ name = "ow-e2e-nested", match = { class = "^(aquamarine)$" }, '
             f'monitor = "{OUTPUT}", float = true, size = {{ 1280, 720 }}, no_initial_focus = true }})'])
        before = {i["instance"] for i in instances()}
        nested = subprocess.Popen(["Hyprland", "-c", str(tmp / "hyprland.lua")],
                                  stdout=open(tmp / "nested.log", "w"), stderr=subprocess.STDOUT)
        inst = wait(lambda: next((i for i in instances() if i["instance"] not in before), None), 20, "nested Hyprland")
        env = {**os.environ, "HYPRLAND_INSTANCE_SIGNATURE": inst["instance"], "WAYLAND_DISPLAY": inst["wl_socket"]}
        wait(lambda: run(["hyprctl", "eval", "return true"], env, check=False).strip() == "ok", 10, "nested Lua")

        def hj(*a: str):
            return json.loads(run(["hyprctl", *a, "-j"], env) or "null")

        print(f"nested Hyprland {inst['wl_socket']} on headless output {OUTPUT}")

        # ---- the app, as a user would start it (but with a fake mic)
        port = free_port()
        url = f"http://127.0.0.1:{port}"
        server = subprocess.Popen(
            [sys.executable, "-m", "openwhisper", "--no-browser", "--port", str(port)],
            cwd=ROOT, env={**env, "OPENWHISPER_FAKE_MIC": str(mic)},
            stdout=open(tmp / "server.log", "w"), stderr=subprocess.STDOUT,
        )
        wait(lambda: up(url), 60, "server")
        wait(lambda: any(m["name"] == "large-v3-turbo" and m["state"] == "ready" for m in get(url + "/api/status")["models"]),
             180, "model ready")
        binds = wait(lambda: [b for b in hj("binds") if b.get("modmask") == 68 and b.get("key", "").upper() == "X"] or None,
                     10, "binds")
        check(len(binds) == 2 and any(b["release"] for b in binds), "SUPER+CTRL+X press and release binds added at runtime")

        # ---- a target app with keyboard focus
        subprocess.Popen(
            ["foot", "--app-id=ow-e2e-target", "-o", "font=monospace:size=15", "sh", "-c",
             f"stty -icanon; printf 'Text from the global hotkey lands here:\\n\\n'; cat > {typed}"],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        target = wait(lambda: next((c for c in hj("clients") if c["class"] == "ow-e2e-target"), None), 10, "terminal")
        wait(lambda: (hj("activewindow") or {}).get("address") == target["address"], 5, "terminal focus")

        def key(action: str) -> None:
            # What the bind runs on press/release, through Hyprland's exec (its env and cwd).
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{CLI} {action}")'], env)

        print("hold the key (push-to-talk)")
        key("press")
        wait(lambda: get(url + "/api/dictation")["state"] == "listening", 5, "listening")
        overlay = wait(lambda: next((c for c in hj("clients") if "open-whisper-overlay" in c["class"]), None), 20, "overlay window")
        check(overlay["floating"] and overlay["pinned"], "overlay window is floating and pinned")
        check((hj("activewindow") or {}).get("address") == target["address"], "overlay did not take keyboard focus")
        time.sleep(1.5)
        level = 0.0
        for _ in range(10):
            level = max(level, get(url + "/api/dictation")["level"])
            time.sleep(0.05)
        check(level > 0.2, f"live mic level reaches the overlay (level={level:.2f})")
        SHOTS.mkdir(parents=True, exist_ok=True)
        run(["grim", str(SHOTS / "06-hotkey-overlay-listening.png")], env)
        print("  screenshot docs/screenshots/06-hotkey-overlay-listening.png")
        time.sleep(1.5)
        key("release")

        done = wait(lambda: (s := get(url + "/api/dictation"))["state"] in ("done", "error") and s, 60, "transcript")
        check(done["state"] == "done", f"transcribed: {done['text']!r} ({done['error']})")
        check(done["delivered"] == ["copied", "typed"], "text copied and typed")
        wait(lambda: typed.exists() and typed.read_text().strip() == done["text"], 10, "typed text")
        check(True, f"text typed into the focused app: {typed.read_text().strip()!r}")
        check(run(["wl-paste", "--no-newline"], env) == done["text"], "clipboard has the transcript")
        time.sleep(0.4)
        run(["grim", str(SHOTS / "07-hotkey-text-pasted.png")], env)
        print("  screenshot docs/screenshots/07-hotkey-text-pasted.png")
        wait(lambda: all(c["workspace"]["name"] == "special:open-whisper"
                         for c in hj("clients") if "open-whisper-overlay" in c["class"]), 10, "overlay hidden")
        print("  ok  overlay hides after the result")

        print("tap mode: tap to start, tap again to stop")
        key("press")
        time.sleep(0.1)
        key("release")
        wait(lambda: get(url + "/api/dictation")["state"] == "listening", 5, "listening after tap")
        time.sleep(1.0)
        check(get(url + "/api/dictation")["state"] == "listening", "still listening after the key is released")
        shown = wait(lambda: next((c for c in hj("clients") if "open-whisper-overlay" in c["class"]
                                   and c["workspace"]["name"] != "special:open-whisper"), None), 5, "overlay shown again")
        check(shown["address"] == overlay["address"], "the same overlay window is reused (instant show)")
        time.sleep(2.5)
        key("press")
        time.sleep(0.1)
        key("release")
        done2 = wait(lambda: (s := get(url + "/api/dictation"))["state"] in ("done", "error") and s, 60, "second transcript")
        check(done2["state"] == "done" and len(done2["text"]) > 3, f"tap-mode transcript: {done2['text']!r}")

        print("stop the app: the bind goes away")
        server.terminate()
        server.wait(15)
        server = None
        left = [b for b in hj("binds") if b.get("modmask") == 68 and b.get("key", "").upper() == "X"]
        check(not left, "bind removed when the app stops")
        print("E2E HOTKEY OK")
    finally:
        if server:
            server.terminate()
            try:
                server.wait(10)
            except subprocess.TimeoutExpired:
                server.kill()
        if nested:
            nested.terminate()
            try:
                nested.wait(10)
            except subprocess.TimeoutExpired:
                nested.kill()
        subprocess.run(["hyprctl", "eval", "if _G.ow_e2e_rule then _G.ow_e2e_rule:set_enabled(false); _G.ow_e2e_rule = nil end"],
                       capture_output=True)
        subprocess.run(["hyprctl", "output", "remove", OUTPUT], capture_output=True)
        print(f"  cleanup done; logs in {tmp}")


if __name__ == "__main__":
    main()
