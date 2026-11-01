"""End-to-end check of the Tauri desktop app, in a nested Hyprland (your session is not touched).

The nested Hyprland sits on a temporary headless output of your session; it and the app get
their own XDG_RUNTIME_DIR, XDG_STATE_HOME, XDG_CONFIG_HOME, XDG_DATA_HOME and a private D-Bus
session bus, so nothing reaches your desktop, your config or your notifications.

The app runs with two test seams: OPENWHISPER_FAKE_MIC (a speech file instead of the mic) and
OPENWHISPER_TEST_LOG (UI events appended to a file, because the webview has no WebDriver here).
Hyprland ignores binds from virtual keyboards, so the key bind's command is run through
Hyprland's own exec dispatcher, exactly as a key press/release would.

Usage: python3 scripts/e2e_desktop.py [--first-run] [--config-bind]   (standard library only)
  --first-run    empty model folder: the key-started app downloads the model (~550 MB)
  --config-bind  the nested config has Franco's bindings.lua block (it runs
                 scripts/ow-dictate): the shim must hand the key to the desktop app, and the
                 app must not add a second bind
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = Path(os.environ.get("OW_E2E_APP") or ROOT / "desktop" / "src-tauri" / "target" / "release" / "open-whisper")
# OW_E2E_HYPRBARS=<hyprbars.so>: load that title-bar plugin in the nested Hyprland (bars in pure red) and
# check that the overlay has no bar. Omarchy's window controls use it.
HYPRBARS = os.environ.get("OW_E2E_HYPRBARS")
SHOTS = ROOT / "docs" / "screenshots"
SPEECH = ROOT / "tests" / "fixtures" / "hello.ogg"
OUTPUT = "OWDESK"
BENCH_MODELS = ROOT / "bench" / ".work" / "ggml"
# Live preview engine + model. Filled after the first run that downloads them (17 MB);
# later runs link them, so they work offline like the ggml models above.
BENCH_WHISTLE = ROOT / "bench" / ".work" / "whistle"
RUNTIME_DESC = "Dictate (open-whisper, added by the app)"
NESTED_CONFIG = """
hl.monitor({ output = "", mode = "preferred", position = "auto", scale = 1 })
hl.config({ misc = { disable_hyprland_logo = true, disable_splash_rendering = true, disable_watchdog_warning = true },
            ecosystem = { no_donation_nag = true }, decoration = { rounding = 12 } })
"""


def run(args, env=None, check=True) -> str:
    out = subprocess.run([str(a) for a in args], capture_output=True, text=True, env=env)
    if check and out.returncode:
        raise RuntimeError(f"{args}: {out.stderr or out.stdout}")
    return out.stdout


def wait(cond, timeout: float, what: str, step: float = 0.1):
    end = time.time() + timeout
    while time.time() < end:
        v = cond()
        if v:
            return v
        time.sleep(step)
    raise AssertionError(f"timed out waiting for {what}")


def check(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionError(msg)
    print(f"  ok  {msg}")


def test_pids(log: Path) -> list[int]:
    """App processes started by this test (their environment carries this run's log path)."""
    out = subprocess.run(["pgrep", "-f", f"^{APP}"], capture_output=True, text=True).stdout
    mine = []
    for p in out.split():
        try:
            if f"OPENWHISPER_TEST_LOG={log}".encode() in Path(f"/proc/{p}/environ").read_bytes():
                mine.append(int(p))
        except OSError:
            pass
    return mine


def main() -> None:
    first_run = "--first-run" in sys.argv
    config_bind = "--config-bind" in sys.argv
    for tool in ("hyprctl", "Hyprland", "wtype", "wl-paste", "grim", "foot", "ffmpeg", "dbus-daemon"):
        if not shutil.which(tool):
            sys.exit(f"needs {tool}")
    if not APP.exists():
        sys.exit(f"build the app first: cd desktop/src-tauri && cargo build --release ({APP})")
    outer_runtime, outer_display = os.environ.get("XDG_RUNTIME_DIR", ""), os.environ.get("WAYLAND_DISPLAY", "")
    if not (os.environ.get("HYPRLAND_INSTANCE_SIGNATURE") and outer_runtime and outer_display):
        sys.exit("needs a running Hyprland session")

    tmp = Path(tempfile.mkdtemp(prefix="ow-desktop-e2e-"))
    runtime = Path(tempfile.mkdtemp(prefix="ow", dir="/tmp"))  # short: socket paths max 108 bytes
    snippet = ""
    if config_bind:  # exactly what Franco's bindings.lua block contains
        shim = ROOT / "scripts" / "ow-dictate"
        snippet = (f'hl.bind("SUPER + CTRL + X", hl.dsp.exec_cmd("{shim} press"), {{ description = "Dictate (open-whisper)" }})\n'
                   f'hl.bind("SUPER + CTRL + X", hl.dsp.exec_cmd("{shim} release"), {{ release = true }})\n')
    (tmp / "hyprland.lua").write_text(NESTED_CONFIG + snippet)
    models = tmp / "models"
    models.mkdir()
    if not first_run:
        for f in ("ggml-large-v3-turbo-q5_0.bin", "ggml-small.bin"):
            if (BENCH_MODELS / f).exists():
                (models / f).symlink_to(BENCH_MODELS / f)
        if BENCH_WHISTLE.is_dir():
            (models / "whistle").mkdir()
            for f in BENCH_WHISTLE.iterdir():
                (models / "whistle" / f.name).symlink_to(f)
    mic = tmp / "mic.wav"
    run(["ffmpeg", "-loglevel", "error", "-y", "-i", SPEECH, "-af", "apad=pad_dur=4", "-ar", "16000", "-ac", "1", mic])
    log = tmp / "events.jsonl"
    typed = tmp / "typed.txt"
    bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address=1", f"--address=unix:path={runtime}/bus"],
                           stdout=subprocess.PIPE, text=True)
    bus_addr = bus.stdout.readline().strip()

    nested_env = {
        **os.environ,
        "XDG_RUNTIME_DIR": str(runtime),
        "XDG_STATE_HOME": str(tmp / "state"),
        "XDG_CONFIG_HOME": str(tmp / "config"),
        "XDG_DATA_HOME": str(tmp / "data"),
        "WAYLAND_DISPLAY": str(Path(outer_runtime) / outer_display),
        "DBUS_SESSION_BUS_ADDRESS": bus_addr,
        "OPENWHISPER_FAKE_MIC": str(mic),
        "OPENWHISPER_TEST_LOG": str(log),
        "OPENWHISPER_MODELS_DIR": str(models),
    }
    if config_bind:
        nested_env["OPENWHISPER_DESKTOP_BIN"] = str(APP)  # "installed": the shim may start it
    nested_env.pop("HYPRLAND_INSTANCE_SIGNATURE", None)

    nested = app = None
    try:
        run(["hyprctl", "output", "create", "headless", OUTPUT])
        run(["hyprctl", "eval",
             '_G.ow_desk_rule = hl.window_rule({ name = "ow-desktop-e2e", match = { class = "^(aquamarine)$" }, '
             f'monitor = "{OUTPUT}", float = true, size = {{ 1280, 800 }}, no_initial_focus = true }})'])
        nested = subprocess.Popen(["Hyprland", "-c", tmp / "hyprland.lua"], env=nested_env,
                                  stdout=open(tmp / "nested.log", "w"), stderr=subprocess.STDOUT)

        def instances():
            try:
                return json.loads(run(["hyprctl", "instances", "-j"], nested_env, check=False) or "[]")
            except ValueError:
                return []

        inst = wait(instances, 20, "nested Hyprland")[0]
        env = {**nested_env, "HYPRLAND_INSTANCE_SIGNATURE": inst["instance"], "WAYLAND_DISPLAY": inst["wl_socket"]}
        wait(lambda: run(["hyprctl", "eval", "return true"], env, check=False).strip() == "ok", 10, "nested Lua")
        if HYPRBARS:
            run(["hyprctl", "plugin", "load", HYPRBARS], env)
            wait(lambda: "hyprbars" in run(["hyprctl", "plugin", "list"], env, check=False), 10, "hyprbars loaded")
            run(["hyprctl", "eval", 'hl.config({ plugin = { hyprbars = { enabled = true, bar_height = 34, '
                                    'bar_color = "rgb(ff0000)", bar_title_enabled = true } } })'], env)
            print(f"  hyprbars loaded in the nested Hyprland ({HYPRBARS}); bars are pure red")

        def hj(*a):
            return json.loads(run(["hyprctl", *a, "-j"], env) or "null")

        def win(title: str):
            return next((c for c in hj("clients") if c["title"] == title and c["workspace"]["id"] >= 0), None)

        def x_binds():
            return [b for b in hj("binds") if b.get("modmask") == 68 and b.get("key", "").upper() == "X"]

        def events(kind: str) -> list:
            if not log.exists():
                return []
            out = []
            for line in log.read_text().splitlines():
                try:
                    e = json.loads(line)
                except ValueError:
                    continue
                if e["event"] == kind:
                    out.append(e["payload"])
            return out

        def job(pred):
            return next((j for j in reversed(events("job")) if pred(j) and j["state"] in ("done", "error")), None)

        def dict_state():
            d = events("dictation")
            return d[-1] if d else {"state": "idle"}

        key_cmd = f"{ROOT / 'scripts' / 'ow-dictate'}" if config_bind else f"{APP} --dictate"

        def key(action: str) -> None:
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{key_cmd} {action}")'], env)

        def shot(name: str) -> None:
            SHOTS.mkdir(parents=True, exist_ok=True)
            run(["grim", SHOTS / name], env)
            print(f"  screenshot docs/screenshots/{name}")

        def red_pixels_around(w) -> int:
            """Pure-red pixels (the nested hyprbars colour) in and just above a window."""
            (x, y), (wd, ht) = w["at"], w["size"]
            png = tmp / "around.png"
            run(["grim", "-g", f"{x - 10},{max(0, y - 60)} {wd + 20}x{ht + 70}", png], env)
            raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", str(png), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
                                 capture_output=True, check=True).stdout
            return sum(1 for i in range(0, len(raw) - 2, 3) if raw[i] > 220 and raw[i + 1] < 30 and raw[i + 2] < 30)

        def app_pids() -> list[int]:
            return test_pids(log)

        def focus(addr: str) -> None:
            run(["hyprctl", "dispatch", f'hl.dsp.focus({{ window = "address:{addr}" }})'], env)
            wait(lambda: (hj("activewindow") or {}).get("address") == addr, 5, "focus")

        print(f"nested Hyprland {inst['wl_socket']} on {OUTPUT}; private runtime, config, data and D-Bus")

        # ---------------------------------------------------------------- start
        (tmp / "config" / "open-whisper").mkdir(parents=True)
        (tmp / "config" / "open-whisper" / "config.toml").write_text('idle_unload_minutes = 5\n')
        app = subprocess.Popen([APP], env=env, stdout=open(tmp / "app.log", "w"), stderr=subprocess.STDOUT)
        main_win = wait(lambda: win("open-whisper"), 30, "main window")
        print("  ok  main window opened")
        if first_run:  # empty model folder: the pill shows the download
            wait(lambda: any(m["state"] == "downloading" and m["progress"] >= 0.1
                             for st in events("status")[-3:] for m in st["models"]), 300, "first-run download in the window")
            time.sleep(0.8)
            shot("17-desktop-first-run-download.png")
        status = wait(lambda: next((s for s in reversed(events("status"))
                                    if any(m["name"] == s["default_model"] and m["state"] == "ready" for m in s["models"])), None),
                      600 if first_run else 120, "model ready")
        check(status["gpu"] and status["default_model"] == "large-v3-turbo-q5_0",
              f"zero config: {status['device']}, default model {status['default_model']}")
        check(status["options"]["idle_unload_minutes"] == 5, "config.toml is read (idle_unload_minutes = 5)")
        b = wait(lambda: x_binds() or None, 10, "binds")
        if config_bind:
            time.sleep(1.0)
            b = x_binds()
            check(len(b) == 2 and any(x["description"] == "Dictate (open-whisper)" for x in b),
                  "the key comes from the config block; the app added no duplicate")
            check("from your Hyprland config" in (tmp / "app.log").read_text(), "the app reports the config key")
        else:
            check(len(b) == 2 and any(x["description"] == RUNTIME_DESC for x in b), "SUPER+CTRL+X added at runtime")
        check(win("open-whisper-overlay") is None, "overlay hidden at start")
        time.sleep(1.0)
        shot("09-desktop-main.png")

        # ---------------------------------------------------------------- files (single instance)
        second = subprocess.run([APP, SPEECH], env=env, capture_output=True, timeout=20)
        check(second.returncode == 0 and len(app_pids()) == 1, "a second launch hands the file over and exits (single instance)")
        j = wait(lambda: job(lambda j: j["filename"] == "hello.ogg"), 60, "file job")
        check(j["state"] == "done" and "hello" in j["result"]["text"].lower() and j["language"] == "en",
              f"file transcribed: {j['result']['text']!r} ({j['language']})")
        check("-->" in j["result"]["srt"] and j["result"]["vtt"].startswith("WEBVTT"), ".srt and .vtt produced")
        time.sleep(1.0)
        shot("10-desktop-transcript.png")

        bogus = tmp / "song.mp3"
        bogus.write_bytes(b"this is not audio\n" * 200)
        run([APP, bogus], env)
        jb = wait(lambda: job(lambda j: j["filename"] == "song.mp3"), 30, "bad file job")
        check(jb["state"] == "error" and "not an audio or video format" in jb["error"], f"clear error: {jb['error']!r}")
        time.sleep(0.8)
        shot("11-desktop-error.png")

        # ---------------------------------------------------------------- in-window dictation (Space)
        # A real key through Hyprland's keymap (wtype's virtual keymap reaches GTK as Escape).
        def space(state: str) -> None:
            run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "", key = "space", state = "{state}" }})'], env)

        focus(main_win["address"])
        space("down")
        time.sleep(2.5)
        shot("12-desktop-window-dictation.png")
        time.sleep(1.0)
        space("up")
        jd = wait(lambda: job(lambda j: j["filename"] == "Dictation"), 60, "window dictation job")
        check(jd["state"] == "done" and "hello" in jd["result"]["text"].lower(), f"Space dictation in the window: {jd['result']['text']!r}")

        # ---------------------------------------------------------------- global key
        subprocess.Popen(["foot", "--app-id=ow-e2e-target", "-o", "font=monospace:size=15", "sh", "-c",
                          f"stty -icanon; printf 'Text from the global hotkey lands here:\\n\\n'; cat > {typed}"],
                         env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        target = wait(lambda: next((c for c in hj("clients") if c["class"] == "ow-e2e-target"), None), 10, "terminal")
        focus(target["address"])
        # Live preview (Whistle): on by default; the engine is fetched once (17 MB) at start.
        pv = wait(lambda: next((s["preview"] for s in reversed(events("status")) if s.get("preview", {}).get("state") in ("ready", "unavailable")), None),
                  180, "live preview engine")
        preview_ok = pv["state"] == "ready"
        if not preview_ok:  # e.g. offline with no cached copy: dictation must work without it
            print(f"  SKIP live preview checks: {pv['error']}")
        else:
            print("  ok  live preview engine ready")
            if not BENCH_WHISTLE.is_dir():
                shutil.copytree(models / "whistle", BENCH_WHISTLE)
        n_key = len(events("dictation"))
        key("press")
        wait(lambda: dict_state()["state"] == "listening", 10, "listening")
        ov = wait(lambda: win("open-whisper-overlay"), 10, "overlay window")
        check(ov["floating"] and ov["pinned"], f"overlay floating and pinned (size {ov['size']}, at {ov['at']})")
        check((hj("activewindow") or {}).get("address") == target["address"], "overlay did not take keyboard focus")
        if HYPRBARS:
            main_red = red_pixels_around(wait(lambda: win("open-whisper"), 10, "main window"))
            check(main_red > 1000, f"hyprbars draws its bar on normal windows (main window: {main_red} red pixels)")
            red = red_pixels_around(ov)
            check(red == 0, f"no title bar on the overlay ({red} red pixels around it)")
        if preview_ok:
            live = wait(lambda: next((d["preview"] for d in reversed(events("dictation")[n_key:])
                                      if d["state"] == "listening" and len(d.get("preview", "").split()) >= 3), None), 8, "preview words")
            check(True, f"words reach the overlay while the key is held: {live!r}")
            time.sleep(0.4)
            shot("18-desktop-overlay-live-preview.png")
        else:
            time.sleep(2.0)
        lv = max(d.get("level", 0) for d in events("dictation")[-30:])
        check(lv > 0.2, f"live mic level reaches the overlay (level={lv:.2f})")
        shot("13-desktop-overlay-listening.png")
        time.sleep(1.0)
        key("release")
        d1 = wait(lambda: (s := dict_state())["state"] in ("done", "error") and s, 60, "global result")
        check(d1["state"] == "done" and d1["delivered"] == ["copied", "typed"], f"typed and copied: {d1['text']!r}")
        tail = [d["preview"] for d in events("dictation")[n_key:] if d["state"] == "transcribing" and d.get("preview")]
        print(f"  info  preview while transcribing: {tail[-1] if tail else '(the main model was first)'!r}; typed: {d1['text']!r}")
        check(not d1.get("preview"), "the preview is gone once the text is typed")
        wait(lambda: typed.exists() and typed.read_text().strip() == d1["text"], 10, "typed text")
        check(run(["wl-paste", "--no-newline"], env) == d1["text"], "clipboard has the transcript")
        time.sleep(0.3)
        shot("14-desktop-text-typed.png")

        key("press")
        time.sleep(0.1)
        key("release")
        time.sleep(1.0)
        check(dict_state()["state"] == "listening", "tap mode keeps listening")
        time.sleep(2.5)
        key("press")
        time.sleep(0.1)
        key("release")
        d2 = wait(lambda: (s := dict_state())["state"] in ("done", "error") and s, 60, "tap result")
        wait(lambda: typed.read_text().strip() == f"{d1['text']} {d2['text']}", 10, "joined text")
        print("  ok  second dictation within 15 s joined with a space")

        # ---------------------------------------------------------------- Options (keyboard)
        def press_key(k: str) -> None:
            for st in ("down", "up"):
                run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "", key = "{k}", state = "{st}" }})'], env)
                time.sleep(0.05)

        cfg_file = tmp / "config" / "open-whisper" / "config.toml"
        main_win = wait(lambda: win("open-whisper"), 10, "main window")
        focus(main_win["address"])
        for _ in range(3):
            press_key("Tab")  # dropzone, mic button, Options
        press_key("Return")
        for _ in range(2):
            press_key("Tab")  # language, model
        press_key("End")  # last model in the list: Small
        wait(lambda: cfg_file.exists() and 'model = "small"' in cfg_file.read_text(), 15, "model saved to config.toml")
        wait(lambda: any(s["default_model"] == "small" for s in events("status")[-5:]), 15, "status follows the choice")
        check(True, "Options: choosing a model saves it to config.toml and the app switches to it")
        time.sleep(1.0)
        shot("16-desktop-options.png")
        press_key("Home")  # back to the recommended model
        wait(lambda: 'model = "large-v3-turbo-q5_0"' in cfg_file.read_text(), 15, "model back")
        print("  ok  back to large-v3-turbo-q5_0")
        press_key("Escape")

        # ---------------------------------------------------------------- close, quit, key starts the app
        run(["hyprctl", "dispatch", f'hl.dsp.window.close({{ window = "address:{main_win["address"]}" }})'], env)
        time.sleep(1.0)
        check(app.poll() is None and win("open-whisper") is None, "closing the window keeps the app running (for the key)")
        key("quit")
        app.wait(20)
        app = None
        check(len(x_binds()) == 2, "the key stays after quitting")
        check(not (runtime / "open-whisper" / "desktop.json").exists(), "runtime file removed on quit")

        if first_run:  # make the key-started app download again, to show it in the overlay
            for f in models.glob("ggml-large-v3-turbo-q5_0*"):
                f.unlink()
        n_before = len(events("dictation"))
        key("press")
        wait(lambda: any(d["state"] == "starting" for d in events("dictation")[n_before:]), 60, "starting state")
        print("  ok  the key started the closed app (background, no main window)")
        starting = [d for d in events("dictation")[n_before:] if d["state"] == "starting"]
        wait(lambda: win("open-whisper-overlay"), 30, "overlay during start-up")
        if first_run:
            def pct(d):
                try:
                    return int(d["detail"].rstrip("%").rsplit(" ", 1)[-1]) if d["detail"].endswith("%") else -1
                except ValueError:
                    return -1

            wait(lambda: any(pct(d) >= 10 for d in events("dictation")[n_before:]), 300, "download progress")
            time.sleep(1.0)
        shot("15-desktop-key-starting.png")
        check(any(d["detail"] for d in starting), f"start-up detail shown: {starting[-1]['detail']!r}")
        check(win("open-whisper") is None, "main window stays closed")
        wait(lambda: dict_state()["state"] == "listening", 600 if first_run else 60, "listening after start")
        time.sleep(3.0)
        key("release")
        d3 = wait(lambda: (s := dict_state())["state"] in ("done", "error") and s, 60, "cold-start result")
        check(d3["state"] == "done" and "typed" in d3["delivered"], f"typed after a cold start: {d3['text']!r}")
        check(len(app_pids()) == 1, "one app process")
        key("quit")
        wait(lambda: not app_pids(), 20, "quit")

        # ---------------------------------------------------------------- --model flag
        out = json.loads(run([APP, "--transcribe", SPEECH, "--model", "small"], env).strip().splitlines()[-1])
        check(out["model"] == "small" and "short test" in out["text"].lower(), f"--model small: {out['text']!r}")
        print("E2E DESKTOP OK")
    finally:
        if app and app.poll() is None:
            app.terminate()
        for pid in test_pids(log):  # only processes of this test run, never yours
            try:
                os.kill(pid, signal.SIGTERM)
            except OSError:
                pass
        if nested:
            nested.terminate()
            try:
                nested.wait(10)
            except subprocess.TimeoutExpired:
                nested.kill()
        bus.terminate()
        subprocess.run(["hyprctl", "eval", "if _G.ow_desk_rule then _G.ow_desk_rule:set_enabled(false); _G.ow_desk_rule = nil end"],
                       capture_output=True)
        subprocess.run(["hyprctl", "output", "remove", OUTPUT], capture_output=True)
        shutil.rmtree(runtime, ignore_errors=True)
        print(f"  cleanup done; logs in {tmp}")


if __name__ == "__main__":
    main()
