"""End-to-end check of the global dictation hotkey, in a nested Hyprland.

Your desktop is not touched: the test starts a second (nested) Hyprland whose window sits on a
temporary headless output of your session, so it is never on your screen and never takes your
keyboard focus. The nested Hyprland and every app in it get their own XDG_RUNTIME_DIR and
XDG_STATE_HOME, so keyboard, focus, clipboard, runtime files and the overlay browser profile
are all separate from yours.

Part A (app running): open-whisper adds its SUPER + CTRL + X bind; hold-to-talk and tap mode
type the text into a terminal and copy it; a config reload keeps the key; stopping the app
keeps the key.
Part B (app closed): the key press starts open-whisper in the background (no browser tab),
the overlay shows start-up (and, with --first-run, the model download), the dictation begins
when the model is ready, and the text is typed.

Hyprland ignores binds from virtual keyboards (wtype), so a physical key press cannot be
simulated: the test checks the binds exist and runs the bound command through Hyprland's own
exec dispatcher, exactly as a key press/release would.

Usage: uv run python scripts/e2e_hotkey.py [--first-run] [--config-bind]
  --first-run    part B uses an empty model cache (downloads ~1.5 GB) to show download progress
  --config-bind  the nested config contains the permanent bindings.lua snippet; checks that the
                 app uses it and never adds a duplicate bind
"""

from __future__ import annotations

import json
import os
import shutil
import signal
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
CLI = ROOT / "scripts" / "ow-dictate"
OUTPUT = "OWE2E"
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


def main() -> None:
    first_run = "--first-run" in sys.argv
    config_bind = "--config-bind" in sys.argv  # the key comes from the config snippet, as in Franco's setup
    for tool in ("hyprctl", "Hyprland", "wtype", "wl-paste", "grim", "foot", "ffmpeg"):
        if not shutil.which(tool):
            sys.exit(f"needs {tool}")
    outer_runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    outer_display = os.environ.get("WAYLAND_DISPLAY", "")
    if not (os.environ.get("HYPRLAND_INSTANCE_SIGNATURE") and outer_runtime and outer_display):
        sys.exit("needs a running Hyprland session")

    tmp = Path(tempfile.mkdtemp(prefix="ow-hotkey-e2e-"))
    # Short path: Hyprland's sockets live under it and Unix socket paths max out at 108 bytes.
    runtime = Path(tempfile.mkdtemp(prefix="ow", dir="/tmp"))
    snippet = ""
    if config_bind:
        snippet = subprocess.run([sys.executable, "-m", "openwhisper.hotkey"], cwd=ROOT, capture_output=True,
                                 text=True, check=True).stdout
    (tmp / "hyprland.lua").write_text(NESTED_CONFIG + snippet)
    mic = tmp / "mic.wav"  # speech + 4 s of silence, so the loop does not repeat inside one take
    run(["ffmpeg", "-loglevel", "error", "-y", "-i", str(SPEECH), "-af", "apad=pad_dur=4", "-ar", "16000", "-ac", "1", str(mic)])
    typed = tmp / "typed.txt"

    # Everything inside the nested Hyprland inherits this.
    nested_env = {
        **os.environ,
        "XDG_RUNTIME_DIR": str(runtime),
        "XDG_STATE_HOME": str(tmp / "state"),
        "WAYLAND_DISPLAY": str(Path(outer_runtime) / outer_display),  # the nested one connects to yours
        "OPENWHISPER_FAKE_MIC": str(mic),
        "DBUS_SESSION_BUS_ADDRESS": "unix:path=/nonexistent",  # no desktop notifications on your screen
    }
    nested_env.pop("HYPRLAND_INSTANCE_SIGNATURE", None)
    if first_run:
        nested_env["HF_HOME"] = str(tmp / "hf")  # part B downloads the model like a first run

    nested = server = None
    try:
        run(["hyprctl", "output", "create", "headless", OUTPUT])
        run(["hyprctl", "eval",
             '_G.ow_e2e_rule = hl.window_rule({ name = "ow-e2e-nested", match = { class = "^(aquamarine)$" }, '
             f'monitor = "{OUTPUT}", float = true, size = {{ 1280, 720 }}, no_initial_focus = true }})'])
        nested = subprocess.Popen(["Hyprland", "-c", str(tmp / "hyprland.lua")], env=nested_env,
                                  stdout=open(tmp / "nested.log", "w"), stderr=subprocess.STDOUT)
        def nested_instances() -> list[dict]:
            try:
                return json.loads(run(["hyprctl", "instances", "-j"], nested_env, check=False) or "[]")
            except ValueError:  # prints a plain message while there is none yet
                return []

        inst = wait(nested_instances, 20, "nested Hyprland")[0]
        env = {**nested_env, "HYPRLAND_INSTANCE_SIGNATURE": inst["instance"], "WAYLAND_DISPLAY": inst["wl_socket"]}
        wait(lambda: run(["hyprctl", "eval", "return true"], env, check=False).strip() == "ok", 10, "nested Lua")

        def hj(*a: str):
            return json.loads(run(["hyprctl", *a, "-j"], env) or "null")

        def x_binds() -> list[dict]:
            return [b for b in hj("binds") if b.get("modmask") == 68 and b.get("key", "").upper() == "X"]

        def key(action: str) -> None:
            # What the bind runs on press/release, through Hyprland's exec (its env and cwd).
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{CLI} {action}")'], env)

        def overlay_win() -> dict | None:
            return next((c for c in hj("clients") if "open-whisper-overlay" in c["class"]
                         and c["workspace"]["name"] != "special:open-whisper"), None)

        def server_url() -> str | None:
            try:
                return json.loads((runtime / "open-whisper" / "server.json").read_text())["url"]
            except (OSError, ValueError, KeyError):
                return None

        print(f"nested Hyprland {inst['wl_socket']} on headless output {OUTPUT} (own runtime dir)")
        SHOTS.mkdir(parents=True, exist_ok=True)

        # ================= part A: app running
        port = free_port()
        url = f"http://127.0.0.1:{port}"
        app_env = {**env, "PYTHONUNBUFFERED": "1", "HF_HOME": os.environ.get("HF_HOME", str(Path.home() / ".cache" / "huggingface"))}
        server = subprocess.Popen([sys.executable, "-m", "openwhisper", "--no-browser", "--port", str(port)],
                                  cwd=ROOT, env=app_env, stdout=open(tmp / "server-a.log", "w"), stderr=subprocess.STDOUT)
        wait(lambda: up(url), 60, "server")
        wait(lambda: any(m["name"] == "large-v3-turbo" and m["state"] == "ready" for m in get(url + "/api/status")["models"]),
             180, "model ready")
        binds = wait(lambda: x_binds() or None, 10, "binds")
        check(len(binds) == 2 and any(b["release"] for b in binds), "SUPER+CTRL+X press and release binds added at runtime")
        if config_bind:
            check(len(binds) == 2 and any(b.get("description") == "Dictate (open-whisper)" for b in binds),
                  "the key comes from the config; the app added no duplicate")
            check("from your Hyprland config" in (tmp / "server-a.log").read_text(), "the app reports the config key")
        else:
            check(any(b.get("description") == "Dictate (open-whisper, added by the app)" for b in binds),
                  "bind is marked as added by the app")

        run(["hyprctl", "reload"], env)
        time.sleep(0.5)
        check(len(wait(lambda: x_binds() or None, 10, "binds after reload")) == 2, "binds back after `hyprctl reload`")

        subprocess.Popen(
            ["foot", "--app-id=ow-e2e-target", "-o", "font=monospace:size=15", "sh", "-c",
             f"stty -icanon; printf 'Text from the global hotkey lands here:\\n\\n'; cat > {typed}"],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        target = wait(lambda: next((c for c in hj("clients") if c["class"] == "ow-e2e-target"), None), 10, "terminal")
        wait(lambda: (hj("activewindow") or {}).get("address") == target["address"], 5, "terminal focus")

        print("A1: hold the key (push-to-talk)")
        key("press")
        wait(lambda: get(url + "/api/dictation")["state"] == "listening", 5, "listening")
        overlay = wait(overlay_win, 20, "overlay window")
        check(overlay["floating"] and overlay["pinned"], "overlay window is floating and pinned")
        check((hj("activewindow") or {}).get("address") == target["address"], "overlay did not take keyboard focus")
        time.sleep(1.5)
        level = 0.0
        for _ in range(10):
            level = max(level, get(url + "/api/dictation")["level"])
            time.sleep(0.05)
        check(level > 0.2, f"live mic level reaches the overlay (level={level:.2f})")
        run(["grim", str(SHOTS / "06-hotkey-overlay-listening.png")], env)
        time.sleep(1.5)
        key("release")
        done = wait(lambda: (s := get(url + "/api/dictation"))["state"] in ("done", "error") and s, 60, "transcript")
        check(done["state"] == "done" and done["delivered"] == ["copied", "typed"], f"typed and copied: {done['text']!r}")
        wait(lambda: typed.exists() and typed.read_text().strip() == done["text"], 10, "typed text")
        check(run(["wl-paste", "--no-newline"], env) == done["text"], "clipboard has the transcript")
        time.sleep(0.4)
        run(["grim", str(SHOTS / "07-hotkey-text-pasted.png")], env)
        wait(lambda: overlay_win() is None, 10, "overlay hidden")

        print("A2: tap to start, tap again to stop")
        key("press")
        time.sleep(0.1)
        key("release")
        time.sleep(1.0)
        check(get(url + "/api/dictation")["state"] == "listening", "tap mode keeps listening after the key is released")
        time.sleep(2.5)
        key("press")
        time.sleep(0.1)
        key("release")
        done2 = wait(lambda: (s := get(url + "/api/dictation"))["state"] in ("done", "error") and s, 60, "tap transcript")
        check(done2["state"] == "done", f"tap-mode transcript: {done2['text']!r}")
        # Within 15 s, same window: the second dictation is joined with one space.
        wait(lambda: typed.read_text().strip() == f"{done['text']} {done2['text']}", 10, "second typed text")
        check(True, "second dictation within 15 s joined with a space")

        print("A3: stop the app; the key stays")
        server.send_signal(signal.SIGTERM)
        server.wait(15)
        server = None
        check(len(x_binds()) == 2, "exactly two binds remain after the app stops (the key can start it again)")
        check(not (runtime / "open-whisper" / "server.json").exists(), "runtime file removed on exit")
        wait(lambda: not any("open-whisper-overlay" in c["class"] for c in hj("clients")), 10, "overlay browser closed")

        # ================= part B: app closed, the key starts it
        print("B: press the key with the app closed" + (" (empty model cache)" if first_run else ""))
        key("press")
        url_b = wait(server_url, 60, "app started by the key")
        wait(lambda: up(url_b), 30, "auto-started app answers")
        check(True, f"the key started open-whisper in the background at {url_b}")
        states = set()
        shot_taken = False
        end = time.time() + (900 if first_run else 120)
        while time.time() < end:
            st = get(url_b + "/api/dictation")
            states.add(st["state"])
            pct = int(st["detail"].rstrip("%").rsplit(" ", 1)[-1]) if st["detail"].endswith("%") else 0
            if st["state"] == "starting" and not shot_taken and overlay_win() and (not first_run or pct >= 10):
                time.sleep(0.8 if not first_run else 0.5)
                run(["grim", str(SHOTS / "08-hotkey-starting.png")], env)
                print(f"  screenshot docs/screenshots/08-hotkey-starting.png ({get(url_b + '/api/dictation')['detail']})")
                shot_taken = True
            if st["state"] == "listening":
                break
            time.sleep(0.1)
        check("starting" in states, f"overlay showed the start-up state ({', '.join(sorted(states))})")
        check(get(url_b + "/api/dictation")["state"] == "listening", "dictation began when the model was ready")
        check(overlay_win() is not None, "overlay visible while listening")
        check((hj("activewindow") or {}).get("address") == target["address"], "focus still on the terminal")
        time.sleep(3.0)
        key("release")  # the key was held since before the start: push-to-talk
        done3 = wait(lambda: (s := get(url_b + "/api/dictation"))["state"] in ("done", "error") and s, 60, "transcript B")
        check(done3["state"] == "done" and "typed" in done3["delivered"], f"typed after a cold start: {done3['text']!r}")
        wait(lambda: typed.read_text().count(done3["text"]) == 3, 10, "third typed text")
        check(len(x_binds()) == 2, "still exactly two binds after the key started the app")
        check(not (runtime / "open-whisper" / "starting").exists(), "start lock removed once the app answered")
        info = json.loads((runtime / "open-whisper" / "server.json").read_text())
        check(oct((runtime / "open-whisper").stat().st_mode & 0o777) == "0o700" and len(info.get("token", "")) > 20,
              "runtime dir is private and holds the dictation token")
        key("quit")
        wait(lambda: not (runtime / "open-whisper" / "server.json").exists(), 15, "auto-started app stopped")
        check(True, "`ow-dictate quit` stopped the app that the key started")
        print("E2E HOTKEY OK")
    finally:
        if server:
            server.terminate()
            try:
                server.wait(10)
            except subprocess.TimeoutExpired:
                server.kill()
        try:  # an app the key started
            pid = json.loads((runtime / "open-whisper" / "server.json").read_text())["pid"]
            os.kill(pid, signal.SIGTERM)
            time.sleep(2)
        except (OSError, ValueError, KeyError):
            pass
        if nested:
            nested.terminate()
            try:
                nested.wait(10)
            except subprocess.TimeoutExpired:
                nested.kill()
        subprocess.run(["hyprctl", "eval", "if _G.ow_e2e_rule then _G.ow_e2e_rule:set_enabled(false); _G.ow_e2e_rule = nil end"],
                       capture_output=True)
        subprocess.run(["hyprctl", "output", "remove", OUTPUT], capture_output=True)
        shutil.rmtree(runtime, ignore_errors=True)
        print(f"  cleanup done; logs in {tmp}")


if __name__ == "__main__":
    main()
