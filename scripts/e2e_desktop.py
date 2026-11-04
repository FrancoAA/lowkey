"""End-to-end check of the Tauri desktop app, in a nested Hyprland (your session is not touched).

The nested Hyprland sits on a temporary headless output of your session; it and the app get
their own XDG_RUNTIME_DIR, XDG_STATE_HOME, XDG_CONFIG_HOME, XDG_DATA_HOME and a private D-Bus
session bus, so nothing reaches your desktop, your config or your notifications.

The app runs with three test seams: OPENWHISPER_FAKE_MIC (a speech file instead of the mic),
OPENWHISPER_FAKE_SYSTEM (another speech file instead of the computer's sound, for Meetings) and
OPENWHISPER_TEST_LOG (UI events appended to a file, because the webview has no WebDriver here).
Hyprland ignores binds from virtual keyboards, so the key bind's command is run through
Hyprland's own exec dispatcher, exactly as a key press/release would.

Usage: python3 scripts/e2e_desktop.py [--first-run] [--config-bind] [--strip-only]   (standard library only)
  --strip-only   only the floating strip's drag and collapse (a quick run)
  OW_E2E_HOLD=<dir> with --strip-only: keep the nested session open (env in <dir>/env.json)
                 until <dir>/stop exists, to try things by hand
  --first-run    empty model folder: the key-started app downloads the model (~550 MB)
  --config-bind  the nested config has Franco's bindings.lua block (it runs
                 scripts/ow-dictate): the shim must hand the key to the desktop app, and the
                 app must not add a second bind
"""

from __future__ import annotations

import json
import re
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP = Path(os.environ.get("OW_E2E_APP") or ROOT / "desktop" / "src-tauri" / "target" / "release" / "lowkey")
# OW_E2E_HYPRBARS=<hyprbars.so>: load that title-bar plugin in the nested Hyprland (bars in pure red) and
# check that the overlay has no bar. Omarchy's window controls use it.
HYPRBARS = os.environ.get("OW_E2E_HYPRBARS")
# OW_E2E_PREVIEW=whistle|parakeet: the live preview engine (default "auto": Parakeet when cached).
# Parakeet needs its cached model (bench/.work/parakeet-tdt-0.6b-v3-int8); without it the run
# uses Whistle (17 MB) instead of downloading 670 MB.
PREVIEW = os.environ.get("OW_E2E_PREVIEW", "auto")
SHOTS = ROOT / "docs" / "screenshots"
SPEECH = ROOT / "tests" / "fixtures" / "hello.ogg"
# "Them" in the meeting test (the computer's sound): other words than the mic, so no echo rule applies.
# JFK's inaugural address (1961, public domain), 11 s.
THEM_SPEECH = ROOT / "tests" / "fixtures" / "jfk.ogg"
OUTPUT = "OWDESK"
BENCH_MODELS = ROOT / "bench" / ".work" / "ggml"
# Live preview engine + model. Filled after the first run that downloads them (17 MB);
# later runs link them, so they work offline like the ggml models above.
BENCH_WHISTLE = ROOT / "bench" / ".work" / "whistle"
BENCH_PARAKEET = ROOT / "bench" / ".work" / "parakeet-tdt-0.6b-v3-int8"  # Parakeet files (670 MB), if cached
# AI notes: Qwen3.5 4B (2.7 GB) and the llama.cpp runner, if cached (bench/summary). Without
# them the AI notes step is skipped (the test never downloads 2.7 GB).
BENCH_LLM = ROOT / "bench" / ".work" / "llm"
if PREVIEW == "parakeet" and not BENCH_PARAKEET.is_dir():
    print(f"  note: OW_E2E_PREVIEW=parakeet but no {BENCH_PARAKEET}: using whistle")
    PREVIEW = "whistle"
RUNTIME_DESC = "Dictate (Lowkey, added by the app)"
MEETING_DESC = "Meeting recording (Lowkey, added by the app)"
PASTE_DESC = "Paste last dictation (Lowkey, added by the app)"
NESTED_CONFIG = """
hl.monitor({ output = "", mode = "preferred", position = "auto", scale = 1 })
-- A taken key: Options must refuse it for the dictation key.
hl.bind("SUPER + ALT + Q", hl.dsp.exec_cmd("true"), { description = "e2e: a taken key" })
hl.config({ misc = { disable_hyprland_logo = true, disable_splash_rendering = true, disable_watchdog_warning = true },
            ecosystem = { no_donation_nag = true }, decoration = { rounding = 12 },
            -- The focus stays where the test puts it, wherever the pointer is.
            input = { follow_mouse = 0 } })
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
        # The same lines as Franco's block in bindings.lua (markers included: the app can take
        # the key over by removing exactly this block). It is from before the rename (the app was
        # "open-whisper"): the app must still know it as its own.
        shim = ROOT / "scripts" / "ow-dictate"
        snippet = ("-- BEGIN open-whisper.dictation\n"
                   "-- open-whisper: global dictation (hold = talk, tap = start/stop)\n"
                   f'hl.bind("SUPER + CTRL + X", hl.dsp.exec_cmd("{shim} press"), {{ description = "Dictate (open-whisper)" }})\n'
                   f'hl.bind("SUPER + CTRL + X", hl.dsp.exec_cmd("{shim} release"), {{ release = true }})\n'
                   "if _G.open_whisper_rule then _G.open_whisper_rule:set_enabled(false) end\n"
                   '_G.open_whisper_rule = hl.window_rule({ name = "open-whisper-overlay", match = { class = "^chrome-.*open-whisper-overlay.*$" }, '
                   'float = true, pin = true, no_initial_focus = true, no_focus = true, size = { 380, 76 }, '
                   'move = { "(monitor_w/2-190)", "(monitor_h-150)" }, border_size = 0, no_shadow = true, no_anim = true })\n'
                   "-- END open-whisper.dictation\n")
    (tmp / "hyprland.lua").write_text(NESTED_CONFIG + snippet)
    models = tmp / "models"
    models.mkdir()
    if not first_run:
        for f in ("ggml-large-v3-turbo-q5_0.bin", "ggml-small.bin"):
            if (BENCH_MODELS / f).exists():
                (models / f).symlink_to(BENCH_MODELS / f)
        if BENCH_PARAKEET.is_dir():
            (models / BENCH_PARAKEET.name).symlink_to(BENCH_PARAKEET)
        if (BENCH_LLM / "Qwen3.5-4B-Q4_K_M.gguf").exists() and (BENCH_LLM / "llama-b12047").is_dir():
            (models / "llm").mkdir()
            for f in ("Qwen3.5-4B-Q4_K_M.gguf", "llama-b12047"):
                (models / "llm" / f).symlink_to(BENCH_LLM / f)
        if BENCH_WHISTLE.is_dir():
            (models / "whistle").mkdir()
            for f in BENCH_WHISTLE.iterdir():
                (models / "whistle" / f.name).symlink_to(f)
    mic = tmp / "mic.wav"
    run(["ffmpeg", "-loglevel", "error", "-y", "-i", SPEECH, "-af", "apad=pad_dur=4", "-ar", "16000", "-ac", "1", mic])
    them = tmp / "them.wav"
    run(["ffmpeg", "-loglevel", "error", "-y", "-i", THEM_SPEECH, "-af", "apad=pad_dur=3", "-ar", "16000", "-ac", "1", them])
    # A fake pactl (OPENWHISPER_PACTL): a Bluetooth headset is the default input, a built-in
    # microphone exists; source-outputs (who uses the microphone) come from a file the test writes.
    pactl = tmp / "pactl"
    outputs = tmp / "source-outputs.json"
    outputs.write_text("[]")
    sources = tmp / "sources.json"
    sources.write_text(json.dumps([
        {"name": "bluez_input.e2e", "description": "WH-1000XM4", "properties": {"device.api": "bluez5", "device.bus": "bluetooth"}},
        {"name": "alsa_input.pci.e2e", "description": "Built-in Microphone", "properties": {"device.api": "alsa", "device.bus": "pci", "device.class": "sound"}},
        {"name": "alsa_output.pci.e2e.monitor", "description": "Monitor of Speakers", "properties": {"device.class": "monitor"}},
    ]))
    pactl.write_text(f"""#!/bin/sh
case "$*" in
  "get-default-source") echo bluez_input.e2e ;;
  "-f json list sources") cat "{sources}" ;;
  "-f json list source-outputs") cat "{outputs}" ;;
  *) exit 1 ;;
esac
""")
    pactl.chmod(0o755)
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
        "OPENWHISPER_FAKE_SYSTEM": str(them),  # never your real sound
        "OPENWHISPER_PACTL": str(pactl),
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
            for attempt in range(6):  # grim sometimes fails with "failed to create buffer"; it passes on a retry
                try:
                    run(["grim", SHOTS / name], env)
                    break
                except RuntimeError:
                    if attempt == 5:
                        raise
                    time.sleep(2.0)
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

        # A test pointer (wlr virtual pointer) for the strip's drag and clicks: in the nested
        # Hyprland only. Built from scripts/e2e_pointer (needs gcc and wayland-scanner).
        pointer_tool = tmp / "e2e_pointer"
        src = ROOT / "scripts" / "e2e_pointer"
        try:
            xml = src / "wlr-virtual-pointer-unstable-v1.xml"
            run(["wayland-scanner", "client-header", xml, tmp / "wlr-virtual-pointer-unstable-v1-client-protocol.h"])
            run(["wayland-scanner", "private-code", xml, tmp / "vptr-protocol.c"])
            run(["gcc", "-O2", f"-I{tmp}", "-o", pointer_tool, src / "pointer.c", tmp / "vptr-protocol.c", "-lwayland-client"])
        except (RuntimeError, OSError) as e:
            print(f"  note: no test pointer ({e}); the strip's drag and collapse are not checked")
            pointer_tool = None

        def pointer(*cmds: str) -> None:
            mon = hj("monitors")[0]
            w, h = mon["width"], mon["height"]
            run([pointer_tool, *[c.format(W=w, H=h) for c in cmds]], env)

        def strip_step() -> None:
            """Drag the strip, collapse it, see it come back collapsed where it was left, open it."""
            cfg = tmp / "config" / "lowkey" / "config.toml"
            n0 = len(events("meeting"))
            focus_before = (hj("activewindow") or {}).get("address")
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{APP} --meeting toggle")'], env)
            s0 = wait(lambda: win("lowkey-overlay"), 15, "the strip for the drag")
            wait(lambda: next((m for m in events("meeting")[n0:] if m["state"] == "recording"), None), 30, "recording")
            time.sleep(0.8)
            check(tuple(s0["size"]) == (380, 76), f"the strip opens at 380x76: {s0['size']}")
            (x, y), (sw, sh) = s0["at"], s0["size"]
            if os.environ.get("OW_E2E_HOLD"):  # debugging: keep the session for commands from outside
                (Path(os.environ["OW_E2E_HOLD"]) / "env.json").write_text(json.dumps({**env, "OW_TMP": str(tmp), "OW_PTR": str(pointer_tool)}))
                wait(lambda: (Path(os.environ["OW_E2E_HOLD"]) / "stop").exists(), 1800, "the stop file")
                return
            # Drag from its left part (not the collapse button), up and to the left.
            steps = [f"rel:{-30},{-20}"] * 10
            pointer(f"abs:{x + 60},{y + 38},{{W}},{{H}}", "sleep:150", "down", "sleep:80", *[c for st in steps for c in (st, "sleep:20")], "sleep:150", "up")
            s1 = wait(lambda: (w_ := win("lowkey-overlay")) and abs(w_["at"][0] - (x - 300)) <= 12 and abs(w_["at"][1] - (y - 200)) <= 12 and w_,
                      5, "the strip moved by the drag")
            time.sleep(0.5)
            check((hj("activewindow") or {}).get("address") == focus_before,
                  f"dragging the strip moves it ({s0['at']} -> {s1['at']}); the focus stays on the app")
            # Mute: the microphone button (first row, right): silence goes in its place, the
            # computer's sound goes on; the strip says so. Then unmute.
            (x1, y1) = s1["at"]
            n_m = len(events("meeting"))
            pointer(f"abs:{x1 + 330},{y1 + 27},{{W}},{{H}}", "sleep:250", "click")
            wait(lambda: any(m.get("muted") for m in events("meeting")[n_m:]), 5, "muted")
            time.sleep(0.8)
            check((hj("activewindow") or {}).get("address") == focus_before, "the mute button mutes your microphone; no focus taken")
            run(["grim", "-g", f"{x1 - 20},{y1 - 20} 420x116", SHOTS / "38-desktop-strip-muted.png"], env)
            print("  screenshot docs/screenshots/38-desktop-strip-muted.png")
            n_u = len(events("meeting"))
            pointer(f"abs:{x1 + 330},{y1 + 27},{{W}},{{H}}", "sleep:250", "click")
            wait(lambda: any(m.get("muted") is False and m["state"] == "recording" for m in events("meeting")[n_u:]), 5, "unmuted")
            print("  ok  a second click unmutes it")
            # Collapse: its button in the top-right corner (it shows on hover).
            pointer(f"abs:{x1 + sw - 17},{y1 + 17},{{W}},{{H}}", "sleep:250", "click")
            s2 = wait(lambda: (w_ := win("lowkey-overlay")) and tuple(w_["size"]) == (150, 44) and w_, 5, "the strip collapsed")
            centre = lambda w_: (w_["at"][0] + w_["size"][0] / 2, w_["at"][1] + w_["size"][1] / 2)
            c1, c2 = centre(s1), centre(s2)
            check(abs(c1[0] - c2[0]) <= 2 and abs(c1[1] - c2[1]) <= 2, f"collapsed to a 150x44 pill around the same centre ({c1} -> {c2})")
            wait(lambda: "overlay_collapsed = true" in cfg.read_text(), 5, "collapsed state saved")
            time.sleep(0.6)
            run(["grim", "-g", f"{int(s2['at'][0]) - 40},{int(s2['at'][1]) - 30} 212x104", SHOTS / "37-desktop-strip-collapsed.png"], env)
            print("  screenshot docs/screenshots/37-desktop-strip-collapsed.png")
            # Stop: the strip goes; its place is saved; the next recording shows it there, collapsed.
            n1 = len(events("meeting"))
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{APP} --meeting toggle")'], env)
            wait(lambda: next((m for m in events("meeting")[n1:] if m["state"] == "done"), None), 120, "saved")
            wait(lambda: win("lowkey-overlay") is None, 15, "the strip goes")
            check(re.search(r"^overlay_at = \[", cfg.read_text(), re.M), "the place is saved when the strip goes")
            n2 = len(events("meeting"))
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{APP} --meeting toggle")'], env)
            s3 = wait(lambda: win("lowkey-overlay"), 15, "the strip again")
            wait(lambda: next((m for m in events("meeting")[n2:] if m["state"] == "recording"), None), 30, "recording again")
            time.sleep(0.5)
            s3 = win("lowkey-overlay")
            check(tuple(s3["size"]) == (150, 44) and abs(s3["at"][0] - s2["at"][0]) <= 3 and abs(s3["at"][1] - s2["at"][1]) <= 3,
                  f"the next recording shows it collapsed where it was left ({s2['at']} / {s3['at']})")
            # A click on the pill (no move) opens it again.
            pointer(f"abs:{s3['at'][0] + 40},{s3['at'][1] + 22},{{W}},{{H}}", "sleep:150", "click")
            s4 = wait(lambda: (w_ := win("lowkey-overlay")) and tuple(w_["size"]) == (380, 76) and w_, 5, "the strip opened")
            wait(lambda: "overlay_collapsed = false" in cfg.read_text(), 5, "open state saved")
            time.sleep(0.5)
            check((hj("activewindow") or {}).get("address") == focus_before, f"a click on the pill opens the strip ({s4['at']}); no focus taken")
            n3 = len(events("meeting"))
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{APP} --meeting toggle")'], env)
            wait(lambda: next((m for m in events("meeting")[n3:] if m["state"] == "done"), None), 120, "saved again")
            wait(lambda: win("lowkey-overlay") is None, 15, "the strip goes again")

        print(f"nested Hyprland {inst['wl_socket']} on {OUTPUT}; private runtime, config, data and D-Bus")

        # ---------------------------------------------------------------- start
        # Settings, data and the login entry where the app had them before the rename
        # ("open-whisper"): it moves them to "lowkey" at start.
        (tmp / "config" / "open-whisper").mkdir(parents=True)
        (tmp / "data" / "open-whisper").mkdir(parents=True)
        (tmp / "data" / "open-whisper" / "history.json").write_text("[]")
        old_autostart = tmp / "config" / "autostart" / "open-whisper.desktop"
        if not config_bind:  # that run checks that taking the key over turns it on
            old_autostart.parent.mkdir(parents=True)
            old_autostart.write_text(f'[Desktop Entry]\nType=Application\nName=open-whisper\nExec="{APP.with_name("open-whisper")}" --background\n')
        # AI notes after Stop only with the cached model: the test never downloads 2.7 GB.
        ai_auto = "true" if (models / "llm").is_dir() else "false"
        # A vocabulary term: "open whisper" (as Whisper hears it) must come out as "OpenWhisper".
        (tmp / "config" / "open-whisper" / "config.toml").write_text(
            f'idle_unload_minutes = 5\npreview_engine = "{PREVIEW}"\nai_auto = {ai_auto}\n\n'
            '[[vocabulary]]\nterm = "OpenWhisper"\nsounds_like = ["open whisper", "Open Wish"]\n')
        app = subprocess.Popen([APP], env=env, stdout=open(tmp / "app.log", "w"), stderr=subprocess.STDOUT)
        main_win = wait(lambda: win("Lowkey"), 30, "main window")
        print("  ok  main window opened")
        check((tmp / "config" / "lowkey" / "config.toml").is_file() and not (tmp / "config" / "open-whisper").exists()
              and (tmp / "data" / "lowkey" / "history.json").is_file() and not (tmp / "data" / "open-whisper").exists(),
              "the settings and data moved from open-whisper to lowkey")
        if not config_bind:
            new_autostart = tmp / "config" / "autostart" / "lowkey.desktop"
            check(not old_autostart.exists() and new_autostart.exists() and f'Exec="{APP}"' in new_autostart.read_text(),
                  "the old login entry became lowkey.desktop, for this program")
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
        check(win("lowkey-overlay") is None, "overlay hidden at start")
        if "--strip-only" in sys.argv:  # a quick run: only the strip's move and collapse
            if pointer_tool:
                strip_step()
            print("E2E STRIP OK")
            return
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
            want = {"auto": "parakeet" if BENCH_PARAKEET.is_dir() else "whistle"}.get(PREVIEW, PREVIEW)
            check(pv.get("engine") == want, f"live preview engine ready: {pv.get('engine')} (asked {PREVIEW})")
            if not BENCH_WHISTLE.is_dir() and (models / "whistle").is_dir():
                shutil.copytree(models / "whistle", BENCH_WHISTLE)
        n_key = len(events("dictation"))
        key("press")
        wait(lambda: dict_state()["state"] == "listening", 10, "listening")
        ov = wait(lambda: win("lowkey-overlay"), 10, "overlay window")
        check(ov["floating"] and ov["pinned"], f"overlay floating and pinned (size {ov['size']}, at {ov['at']})")
        check((hj("activewindow") or {}).get("address") == target["address"], "overlay did not take keyboard focus")
        if HYPRBARS:
            main_red = red_pixels_around(wait(lambda: win("Lowkey"), 10, "main window"))
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
        check("OpenWhisper" in d1["text"] and "open whisper" not in d1["text"].lower(), "the vocabulary writes the term (OpenWhisper), not what it sounds like")
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

        def press_key(k: str) -> None:
            for st in ("down", "up"):
                run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "", key = "{k}", state = "{st}" }})'], env)
                time.sleep(0.05)

        # ---------------------------------------------------------------- history and paste last
        hist_file = tmp / "data" / "lowkey" / "history.json"
        kept = wait(lambda: hist_file.exists() and len(json.loads(hist_file.read_text())) >= 3 and json.loads(hist_file.read_text()), 10, "history entries")
        check([e["text"] for e in kept][-2:] == [d1["text"], d2["text"]] and oct(hist_file.stat().st_mode & 0o777) == "0o600",
              f"the history keeps every dictation ({len(kept)}: window and key), in a private file")
        pk = [b for b in hj("binds") if b.get("modmask") == 72 and b.get("key", "").upper() == "V"]
        check(len(pk) == 1 and pk[0]["description"] == PASTE_DESC, "the paste-last key SUPER+ALT+V is added at start (one bind)")
        before = typed.read_text()
        run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{APP} --paste-last")'], env)
        wait(lambda: typed.read_text() == before + d2["text"], 10, "the last dictation typed again")
        check(True, f"the paste-last key types the last dictation again: {d2['text']!r}")
        focus(main_win["address"])
        for st in ("down", "up"):  # Ctrl+, -> General, then Up -> History
            run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "CTRL", key = "comma", state = "{st}" }})'], env)
            time.sleep(0.05)
        press_key("Up")
        time.sleep(0.8)
        shot("29-desktop-history.png")

        # ---------------------------------------------------------------- tray menu (D-Bus, as a status bar uses it)
        cfg_file = tmp / "config" / "lowkey" / "config.toml"

        def busctl(*a) -> str:
            return run(["busctl", f"--address={bus_addr}", "--json=short", *a], env)

        def tray_target():
            """The app's connection and object path that hold the tray menu (None: not yet)."""
            names = [x["name"] for x in json.loads(busctl("list")) if x.get("pid") == app.pid and x["name"].startswith(":")]
            for name in names:
                tree = run(["busctl", f"--address={bus_addr}", "tree", "--list", name], env, check=False).split()
                path = next((p for p in tree if p.endswith("/Menu")), None)
                if path:
                    return name, path
            return None

        def tray_menu() -> list:
            """The menu as nested {id, label, enabled, checked, items}."""
            target = tray_target()
            if not target:
                return []
            name, path = target
            root = json.loads(busctl("call", name, path, "com.canonical.dbusmenu", "GetLayout", "iias", "0", "--", "-1", "0"))["data"][1]

            def node(n):
                i, props, kids = n
                p = {k: v["data"] for k, v in props.items()}
                return {"id": i, "label": p.get("label", "").replace("_", ""), "enabled": p.get("enabled", True),
                        "checked": p.get("toggle-state") == 1, "sep": p.get("type") == "separator",
                        "items": [node(k["data"]) for k in kids]}
            return node(root)["items"]

        def tray_find(items, *labels):
            for lab in labels:
                items = next(x for x in items if x["label"].startswith(lab))
                if lab != labels[-1]:
                    items = items["items"]
            return items

        def tray_click(*labels) -> None:
            # The menu is built again after each click: an id read just before can be gone.
            for attempt in range(5):
                name, path = tray_target()
                try:
                    busctl("call", name, path, "com.canonical.dbusmenu", "Event", "isvu", str(tray_find(tray_menu(), *labels)["id"]), "clicked", "s", "", "0")
                    return
                except StopIteration:  # read while the app built the menu again
                    if attempt == 4:
                        raise RuntimeError(f"no tray item {labels}: {tray_labels()}")
                    time.sleep(0.3)
                except RuntimeError as e:
                    if "does not refer to a menu item" not in str(e) or attempt == 4:
                        raise
                    time.sleep(0.3)

        def tray_labels() -> list:
            return [x["label"] for x in tray_menu() if not x["sep"]]

        menu = wait(lambda: tray_menu() or None, 10, "the tray menu on D-Bus")
        check(tray_labels() == ["Open", "Start dictation", "Start meeting recording", "Microphone", "Speech-to-text model", "Exit"]
              and menu[1]["sep"], f"tray menu: Open first, a divider under it, Exit last: {tray_labels()}")
        mics = [(m["label"], m["checked"]) for m in tray_find(menu, "Microphone")["items"]]
        check(mics == [("System default — WH-1000XM4", True), ("WH-1000XM4 · Bluetooth", False), ("Built-in Microphone", False)],
              f"tray microphones: {mics}")
        models_menu = [(m["label"], m["checked"]) for m in tray_find(menu, "Speech-to-text model")["items"]]
        check(models_menu[0] == ("Automatic — Large v3 Turbo (q5)", True) and ("Small", False) in models_menu,
              f"tray models, Automatic chosen: {models_menu[:3]}…")
        # Microphone and model from the tray: saved, and the window follows.
        tray_click("Microphone", "Built-in Microphone")
        wait(lambda: 'microphone = "alsa_input.pci.e2e"' in cfg_file.read_text(), 10, "microphone saved from the tray")
        wait(lambda: tray_find(tray_menu(), "Microphone", "Built-in Microphone")["checked"], 10, "the tray ticks the microphone")
        tray_click("Speech-to-text model", "Small")
        wait(lambda: 'model = "small"' in cfg_file.read_text(), 10, "model saved from the tray")
        wait(lambda: any(s["default_model"] == "small" for s in events("status")[-5:]), 15, "status follows the tray's model")
        check(tray_find(tray_menu(), "Speech-to-text model", "Small")["checked"], "the tray ticks the model; the microphone and model are saved")
        tray_click("Speech-to-text model", "Automatic")
        tray_click("Microphone", "System default")
        wait(lambda: 'model = ""' in cfg_file.read_text() and 'microphone = ""' in cfg_file.read_text(), 10, "back to automatic choices")
        # A dictation from the tray: typed into the app that has the focus.
        focus(target["address"])
        before = typed.read_text()
        n_t = len(events("dictation"))
        tray_click("Start dictation")
        wait(lambda: dict_state()["state"] == "listening", 10, "listening (tray)")
        wait(lambda: tray_labels()[1] == "Stop dictation", 5, "the tray says Stop dictation")
        time.sleep(2.5)
        tray_click("Stop dictation")
        d3 = wait(lambda: next((d for d in events("dictation")[n_t:] if d["state"] in ("done", "error")), None), 60, "tray dictation result")
        check(d3["state"] == "done" and "hello" in d3["text"].lower(), f"the tray starts and stops a dictation: {d3['text']!r}")
        wait(lambda: typed.read_text().startswith(before) and d3["text"] in typed.read_text()[len(before):], 10, "tray dictation typed")
        wait(lambda: tray_labels()[1] == "Start dictation", 5, "the tray says Start dictation again")
        # A meeting recording from the tray.
        n_m = len(events("meeting"))
        tray_click("Start meeting recording")
        wait(lambda: next((m for m in events("meeting")[n_m:] if m["state"] == "recording"), None), 30, "meeting from the tray")
        wait(lambda: tray_labels()[2] == "Stop meeting recording", 5, "the tray says Stop meeting recording")
        time.sleep(3)
        tray_click("Stop meeting recording")
        saved = wait(lambda: next((m for m in events("meeting")[n_m:] if m["state"] == "done"), None), 60, "tray meeting saved")
        check(bool(saved["meeting"]["id"]), "the tray starts and stops a meeting recording")
        if (models / "llm").is_dir() and saved["meeting"]["lines"]:
            # Its AI notes start by themselves: done before the next meeting asks for its own.
            wait(lambda: next((e for e in events("ai-notes") if e["id"] == saved["meeting"]["id"] and e["state"] in ("done", "error")), None),
                 240, "the tray meeting's AI notes")
        wait(lambda: tray_labels()[2] == "Start meeting recording", 10, "the tray says Start meeting recording again")
        wait(lambda: win("lowkey-overlay") is None, 15, "the strip goes after the tray's meeting")
        # Open from the tray: the window comes back.
        run(["hyprctl", "dispatch", f'hl.dsp.window.close({{ window = "address:{main_win["address"]}" }})'], env)
        wait(lambda: win("Lowkey") is None, 10, "window closed")
        tray_click("Open")
        main_win = wait(lambda: win("Lowkey"), 10, "window opened from the tray")
        print("  ok  Open in the tray shows the window again")

        # ---------------------------------------------------------------- Options (keyboard)
        cfg_file = tmp / "config" / "lowkey" / "config.toml"
        main_win = wait(lambda: win("Lowkey"), 10, "main window")
        focus(main_win["address"])
        # Settings: Ctrl+, opens them with the focus on the section list; Down moves to Model,
        # Tab to its first control (the model list).
        for st in ("down", "up"):
            run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "CTRL", key = "comma", state = "{st}" }})'], env)
            time.sleep(0.05)
        time.sleep(0.3)
        shot("20-desktop-settings-general.png")
        press_key("Down")  # Model
        press_key("Tab")  # the model list
        # Parakeet (670 MB) only when it is cached, so the run stays offline-capable; else Small.
        with_pk = BENCH_PARAKEET.is_dir()
        last = "parakeet-v3" if with_pk else "small"
        press_key("End" if with_pk else "s")  # Parakeet is last; "s" picks Small by its first letter
        wait(lambda: cfg_file.exists() and f'model = "{last}"' in cfg_file.read_text(), 15, "model saved to config.toml")
        wait(lambda: any(s["default_model"] == last for s in events("status")[-5:]), 15, "status follows the choice")
        check(True, f"Options: choosing a model ({last}) saves it to config.toml and the app switches to it")
        if not with_pk:
            print(f"  SKIP Parakeet dictation: no {BENCH_PARAKEET} (it would download 670 MB)")
        ready = lambda: any(m["name"] == "parakeet-v3" and m["state"] == "ready" for s in events("status")[-3:] for m in s["models"])
        if with_pk:
            wait(ready, 60, "Parakeet loaded")
        time.sleep(1.0)
        shot("16-desktop-options.png")
        # A global-key dictation with Parakeet, into the terminal.
        if with_pk:
            focus(target["address"])
            n_pk = len(events("dictation"))
            key("press")
            wait(lambda: dict_state()["state"] == "listening", 10, "listening (Parakeet)")
            time.sleep(3.5)
            key("release")
            t_rel = time.monotonic()
            d_pk = wait(lambda: next((d for d in events("dictation")[n_pk:] if d["state"] in ("done", "error")), None), 60, "Parakeet result")
            took = time.monotonic() - t_rel
            check(d_pk["state"] == "done" and "short test" in d_pk["text"].lower(), f"Parakeet dictation typed: {d_pk['text']!r} ({took:.2f} s after release)")
            wait(lambda: typed.read_text().rstrip().endswith(d_pk["text"]), 10, f"Parakeet text typed (terminal has {typed.read_text()[-60:]!r})")
            focus(main_win["address"])
            time.sleep(0.3)
        press_key("Home")  # back to the recommended model
        wait(lambda: 'model = "large-v3-turbo-q5_0"' in cfg_file.read_text(), 15, "model back")
        print("  ok  back to large-v3-turbo-q5_0")

        # ---------------------------------------------------------------- Options: dictation key
        def key_state(k: str, st: str, mods: str = "") -> None:
            run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "{mods}", key = "{k}", state = "{st}" }})'], env)
            time.sleep(0.05)

        def press_combo(mods: str, k: str) -> None:
            # WebKitGTK reports Ctrl/Alt/Shift with the key but never Super: the page follows the
            # Super key itself. So Super is pressed as a key (as on a real keyboard) and the other
            # modifiers come with the key event (send_key_state does not update them otherwise).
            others = " ".join(m for m in mods.split() if m != "SUPER")
            if "SUPER" in mods.split():
                key_state("Super_L", "down")
            key_state(k, "down", others)
            key_state(k, "up", others)
            if "SUPER" in mods.split():
                key_state("Super_L", "up")

        def binds_on(mask: int, k: str) -> list:
            return [b for b in hj("binds") if b.get("modmask") == mask and b.get("key", "").upper() == k]

        def latest_status() -> dict:
            return events("status")[-1] if events("status") else {}

        for st in ("down", "up"):  # Shift+Tab: back to the section list (on Model)
            run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "SHIFT", key = "Tab", state = "{st}" }})'], env)
            time.sleep(0.05)
        press_key("Down")  # Dictation
        press_key("Tab")  # its first control: the key button (Change, or "Let the app manage it")
        if config_bind:
            # The key comes from the bindings.lua block: Options offers to take it over.
            check(latest_status().get("hotkey_source") == "config", "Options: the key comes from the config block")
            block_file = tmp / "hyprland.lua"
            press_key("Return")  # "Let the app manage it"
            wait(lambda: latest_status().get("hotkey_source") == "app", 20, "key managed by the app")
            check("open-whisper.dictation" not in block_file.read_text(), "the block is gone from the Hyprland config")
            backups = list(tmp.glob("hyprland.lua.bak.lowkey.*"))
            check(len(backups) == 1 and "open-whisper.dictation" in backups[0].read_text(), f"a backup of the old config: {backups[0].name if backups else None}")
            check(len(x_binds()) == 2 and all(b["description"] in (RUNTIME_DESC, "") for b in x_binds()), "SUPER+CTRL+X now comes from the app")
            check((tmp / "config" / "autostart" / "lowkey.desktop").exists(), "taking the key over turned on start at login")
        # A combination that another shortcut uses is refused; the key stays.
        n_ev = len(events("hotkey-change"))
        press_key("Return")  # Change
        time.sleep(0.3)
        press_combo("SUPER ALT", "Q")
        refused = wait(lambda: events("hotkey-change")[n_ev:] or None, 10, "taken key answer")[0]
        check("Err" in refused["result"] and "e2e: a taken key" in refused["result"]["Err"] and len(x_binds()) == 2,
              f"a taken key is refused and the key stays: {refused['result']}")
        press_key("Return")  # Change
        time.sleep(0.3)
        press_combo("SUPER ALT", "D")
        time.sleep(0.5)
        print(f"  info  key events: {events('hotkey-change') + events('hotkey-adopt')}")
        wait(lambda: 'hotkey = "SUPER + ALT + D"' in cfg_file.read_text(), 15, "new key saved to config.toml")
        wait(lambda: binds_on(72, "D") and not x_binds(), 10, "the key moved")
        check(len(binds_on(72, "D")) == 2 and not x_binds(), "Options: the key moved to SUPER+ALT+D at once (press and release binds); SUPER+CTRL+X is free")
        run(["hyprctl", "reload"], env)  # a config reload: the app adds its key again, once
        time.sleep(2.0)
        check(len(binds_on(72, "D")) == 2 and not x_binds(), "after a Hyprland reload the new key is back, not doubled")
        time.sleep(0.5)
        shot("19-desktop-options-key.png")
        press_key("Return")  # Change again: back to the default
        time.sleep(0.3)
        press_combo("SUPER CTRL", "X")
        wait(lambda: 'hotkey = "SUPER + CTRL + X"' in cfg_file.read_text() and len(x_binds()) == 2 and not binds_on(72, "D"), 15, "key back")
        print("  ok  back to SUPER+CTRL+X")
        # The meeting key: added at start (one bind); it cannot take the dictation key; it moves.
        mk = binds_on(68, "M")
        check(len(mk) == 1 and mk[0]["description"] == MEETING_DESC, "the meeting key SUPER+CTRL+M is added at start (one bind)")
        press_key("Tab")  # Meeting key → Change
        n_mk = len(events("meeting-key-change"))
        press_key("Return")
        time.sleep(0.3)
        press_combo("SUPER CTRL", "X")
        r = wait(lambda: events("meeting-key-change")[n_mk:] or None, 10, "meeting key answer")[0]
        check("Err" in r["result"] and "dictation key" in r["result"]["Err"], f"the dictation key is refused as the meeting key: {r['result']}")
        press_key("Return")
        time.sleep(0.3)
        press_combo("SUPER ALT", "M")
        wait(lambda: 'meeting_hotkey = "SUPER + ALT + M"' in cfg_file.read_text(), 15, "meeting key saved to config.toml")
        wait(lambda: binds_on(72, "M") and not binds_on(68, "M"), 10, "meeting key moved")
        check(len(binds_on(72, "M")) == 1 and len(x_binds()) == 2, "Options: the meeting key moved to SUPER+ALT+M; the dictation key stays")
        time.sleep(0.4)
        shot("24-desktop-settings-keys.png")
        press_key("Return")
        time.sleep(0.3)
        press_combo("SUPER CTRL", "M")
        wait(lambda: 'meeting_hotkey = "SUPER + CTRL + M"' in cfg_file.read_text() and binds_on(68, "M") and not binds_on(72, "M"), 15, "meeting key back")
        print("  ok  meeting key back to SUPER+CTRL+M")
        press_key("Tab")  # past the paste-last key's Change, to the login switch
        # Start at login: the autostart entry appears and goes (in the test's own XDG_CONFIG_HOME).
        autostart = tmp / "config" / "autostart" / "lowkey.desktop"
        # Franco's real entries (old and new name): never touched.
        real_autostart = [Path.home() / ".config" / "autostart" / n for n in ("open-whisper.desktop", "lowkey.desktop")]
        real_before = [f.exists() for f in real_autostart]
        was = autostart.exists()  # on after taking the key over (--config-bind)
        press_key("Tab")
        press_key("space")
        wait(lambda: autostart.exists() != was, 10, "autostart entry toggled")
        press_key("space")
        wait(lambda: autostart.exists() == was, 10, "autostart entry back")
        if not autostart.exists():
            press_key("space")
            wait(lambda: autostart.exists(), 10, "autostart entry")
        check("--background" in autostart.read_text(), "start at login: the entry starts the app in the background")
        press_key("space")
        wait(lambda: not autostart.exists(), 10, "autostart entry removed")
        check([f.exists() for f in real_autostart] == real_before, "your real ~/.config/autostart was not touched")
        print("  ok  start at login turned off again")
        press_key("Escape")
        # Settings → Models → AI notes (General, down to Models, then Tab past the speech model
        # and the idle menu to the notes model: the view scrolls to it).
        for st in ("down", "up"):
            run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "CTRL", key = "comma", state = "{st}" }})'], env)
            time.sleep(0.05)
        press_key("Down")
        for _ in range(3):
            press_key("Tab")
        time.sleep(0.6)
        shot("28-desktop-settings-ai-notes.png")
        press_key("Escape")

        # ---------------------------------------------------------------- Meetings
        def ctrl_key(k: str) -> None:
            for st in ("down", "up"):
                run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "CTRL", key = "{k}", state = "{st}" }})'], env)
                time.sleep(0.05)

        meetings_dir = tmp / "data" / "lowkey" / "meetings"
        press_key("Down")  # Meetings (from the Transcribe tab)
        press_key("Tab")  # the search field
        press_key("Tab")  # Record
        n_m = len(events("meeting"))
        press_key("Return")
        started = wait(lambda: events("meeting-start")[-1:] or None, 15, "meeting started")[0]
        check("Ok" in started["result"], f"Meetings: recording starts ({started['result']})")
        mid = started["result"]["Ok"]

        def lines_of(m: dict, who: str) -> list[str]:
            return [x["text"] for x in m["meeting"]["lines"] if x["speaker"] == who]

        def both():
            ms = [m for m in events("meeting")[n_m:] if m["meeting"]["id"] == mid]
            last = ms[-1] if ms else None
            return last if last and lines_of(last, "me") and any("country" in t.lower() for t in lines_of(last, "them")) else None

        t0 = time.monotonic()
        live = wait(both, 90, "live text from the mic and from the computer's sound")
        check(live["meeting"]["system_audio"] and live["state"] == "recording",
              f"live transcript from both sources after {time.monotonic() - t0:.1f} s: me {lines_of(live, 'me')[:1]}, them {lines_of(live, 'them')[:1]}")
        time.sleep(0.8)
        shot("21-desktop-meeting-live.png")
        focus(main_win["address"])
        press_key("Return")  # Stop: it has the focus after Start
        wait(lambda: next((m for m in events("meeting")[n_m:] if m["meeting"]["id"] == mid and m["state"] == "done"), None), 60, "meeting saved")
        saved = json.loads((meetings_dir / f"{mid}.json").read_text())
        me_text = " ".join(x["text"] for x in saved["lines"] if x["speaker"] == "me").lower()
        them_text = " ".join(x["text"] for x in saved["lines"] if x["speaker"] == "them").lower()
        check(saved["state"] == "done" and saved["duration"] > 5,
              f"Stop saves the meeting ({saved['duration']:.0f} s, {len(saved['lines'])} lines)")
        check("test" in me_text and "country" in them_text and "country" not in me_text,
              "Me = the microphone, Them = the computer's sound")
        check(saved["lines"] == sorted(saved["lines"], key=lambda x: x["start"]), "lines in time order")
        check(oct(meetings_dir.stat().st_mode & 0o777) == "0o700", "the meetings folder is private (0700)")
        time.sleep(1.0)
        shot("22-desktop-meeting-saved.png")

        # ---------------------------------------------------------------- AI notes (written when it stops)
        if (models / "llm").is_dir():
            t_ai = time.monotonic()
            ai = wait(lambda: next((e for e in events("ai-notes") if e["id"] == mid and e["state"] in ("done", "error")), None), 240, "AI notes")
            check(ai["state"] == "done", f"AI notes written by themselves after Stop in {time.monotonic() - t_ai:.0f} s ({ai['detail']}): {ai['text'][:80]!r}")
            check(any(e["state"] == "writing" and e["text"] for e in events("ai-notes") if e["id"] == mid), "the notes stream in while they are written")
            saved = json.loads((meetings_dir / f"{mid}.json").read_text())
            check(saved["ai_notes"] == ai["text"] and saved["ai_model"] == "Qwen3.5 4B", "the AI notes are saved with the meeting")
            low = saved["ai_notes"].lower()
            # Words of the test speech (the live transcript differs a little in each run, and so
            # do the words the model picks): Me says "Hello from OpenWhisper. This is a short
            # test."; Them is JFK's "ask not what your country…".
            check("## " in low and any(w in low for w in ("openwhisper", "open whisper", "test", "country")),
                  "the notes have topic sections and use the transcript")
            check(not any(h in saved["ai_notes"] for h in ("## Summary", "## Action items", "## Open questions")),
                  "the notes follow the topics, not fixed sections (summary, action items…)")
            time.sleep(1.0)
            shot("25-desktop-meeting-ai-notes.png")
            # Ask about this meeting: Ctrl+K, a question, Enter; the answer is saved with it.
            focus(main_win["address"])
            ctrl_key("k")
            for ch in "what did they say about the country":
                press_key("space" if ch == " " else ch)
            n_c = len(events("ai-chat"))
            t_c = time.monotonic()
            press_key("Return")
            ans = wait(lambda: next((e for e in events("ai-chat")[n_c:] if e["id"] == mid and e["state"] in ("done", "error", "cancelled")), None), 180, "an answer")
            check(ans["state"] == "done" and "country" in ans["text"].lower(),
                  f"Ask about this meeting: answered in {time.monotonic() - t_c:.0f} s from the transcript: {ans['text'][:80]!r}")
            check(any(e["state"] == "writing" and e["text"] for e in events("ai-chat")[n_c:] if e["id"] == mid), "the answer streams in")
            chat = json.loads((meetings_dir / f"{mid}.json").read_text())["chat"]
            check([c["role"] for c in chat] == ["user", "assistant"] and chat[0]["text"] == "what did they say about the country" and chat[1]["text"] == ans["text"],
                  "the question and the answer are saved with the meeting")
            time.sleep(1.0)
            shot("34-desktop-meeting-chat.png")
        else:
            print(f"  SKIP AI notes: no cached Qwen3.5 4B and runner in {BENCH_LLM}")
        # A wide window: the AI notes and the chat side by side (the nested window, full screen).
        def fullscreen() -> bool:
            before = win("Lowkey")["size"]
            run(["hyprctl", "dispatch", f'hl.dsp.window.fullscreen({{ window = "address:{main_win["address"]}" }})'], env, check=False)
            time.sleep(1.0)
            return (win("Lowkey") or {}).get("size") != before

        if fullscreen():
            shot("27-desktop-meeting-wide.png")
            press_key("Escape")
            time.sleep(0.8)
            shot("26-desktop-meetings-list.png")
            fullscreen()  # back
        else:
            print("  SKIP the wide screenshots: full screen did not work in the nested Hyprland")
            press_key("Escape")  # back to the list (grouped by day)
            time.sleep(0.8)
            shot("26-desktop-meetings-list.png")

        # ---------------------------------------------------------------- Meetings: search, and delete from the list
        def meeting_files() -> set:
            return {p.name for p in meetings_dir.glob("m-*.json")}

        before_files = meeting_files()
        focus(main_win["address"])
        ctrl_key("f")  # Ctrl+F: the search field
        for ch in "country":
            press_key(ch)
        time.sleep(1.0)
        shot("32-desktop-meetings-search.png")
        press_key("Tab")  # Record
        press_key("Tab")  # the first result (the newest meeting)
        press_key("Tab")  # its Delete button
        press_key("Return")
        time.sleep(0.6)
        shot("33-desktop-meeting-delete-confirm.png")
        press_key("Escape")  # no
        time.sleep(0.6)
        check(meeting_files() == before_files, "Esc in the confirmation deletes nothing")
        press_key("Return")  # Delete again: the focus is back on it
        time.sleep(0.6)
        press_key("Tab")  # Cancel -> Delete meeting
        press_key("Return")
        gone = wait(lambda: (before_files - meeting_files()) or None, 10, "a meeting deleted from the list")
        check(gone == {f"{mid}.json"}, f"Delete in the list deletes that meeting only, after the confirmation ({sorted(gone)})")
        ctrl_key("f")
        press_key("Escape")  # clears the search
        time.sleep(0.5)

        # ---------------------------------------------------------------- Meetings: the key, from another app
        focus(target["address"])  # the terminal has the focus
        n_k = len(events("meeting"))

        def meeting_key() -> None:  # what SUPER+CTRL+M runs
            run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{APP} --meeting toggle")'], env)

        meeting_key()
        strip = wait(lambda: win("lowkey-overlay"), 15, "the recording strip")
        rec = wait(lambda: next((m for m in reversed(events("meeting")[n_k:]) if m["state"] == "recording" and m["meeting"]["lines"]), None),
                   90, "lines from the key's meeting")
        check(strip["pinned"] and (hj("activewindow") or {}).get("address") == target["address"],
              f"the meeting key records from another app; the strip shows it and takes no focus ({len(rec['meeting']['lines'])} lines)")
        time.sleep(0.8)
        shot("23-desktop-meeting-strip.png")
        meeting_key()
        done = wait(lambda: next((m for m in events("meeting")[n_k:] if m["state"] == "done"), None), 60, "the key's meeting saved")
        check(done["meeting"]["lines"] and (meetings_dir / f"{done['meeting']['id']}.json").exists(), "the key again stops and saves it")
        time.sleep(0.5)
        check(win("lowkey-overlay") is not None, "the strip says it is saved")
        wait(lambda: win("lowkey-overlay") is None, 10, "the strip goes after the save")
        print("  ok  the strip goes a moment after the save")

        # ---------------------------------------------------------------- calls: Zoom on the microphone
        n_c = len(events("call"))
        outputs.write_text(json.dumps([{"properties": {"application.name": "ZOOM VoiceEngine", "application.process.binary": "zoom",
                                                        "application.process.id": "4242"}}]))
        offer = wait(lambda: next((e for e in events("call")[n_c:] if e["state"] == "offer"), None), 15, "call offer")
        # The event comes first; the strip window can take a moment to show.
        wait(lambda: win("lowkey-overlay"), 5, "the strip with the offer")
        check(offer["label"] == "Zoom", "Zoom on the microphone for 2 s: the strip offers to record it")
        time.sleep(0.6)
        shot("30-desktop-call-offer.png")
        n_z = len(events("meeting"))
        meeting_key()  # record it
        zrec = wait(lambda: next((m for m in events("meeting")[n_z:] if m["state"] == "recording"), None), 15, "the call's recording")
        check(zrec["meeting"]["title"] == "Zoom call", f"the meeting key records the offered call, named after it: {zrec['meeting']['title']!r}")
        outputs.write_text("[]")  # Zoom leaves the microphone
        ended = wait(lambda: next((e for e in events("call")[n_c:] if e["state"] == "ended"), None), 15, "call ended")
        check(ended["auto"] is False and any(m["state"] == "recording" for m in events("meeting")[-2:]),
              "the call ended: the strip says so; a recording you started goes on until you stop it")
        time.sleep(0.6)
        shot("31-desktop-call-ended.png")
        meeting_key()
        wait(lambda: next((m for m in events("meeting")[n_z:] if m["state"] == "done"), None), 60, "the call's meeting saved")
        print("  ok  the call's meeting saved with the key")

        # ---------------------------------------------------------------- close, quit, key starts the app
        run(["hyprctl", "dispatch", f'hl.dsp.window.close({{ window = "address:{main_win["address"]}" }})'], env)
        time.sleep(1.0)
        check(app.poll() is None and win("Lowkey") is None, "closing the window keeps the app running (for the key)")
        key("quit")
        app.wait(20)
        app = None
        check(len(x_binds()) == 2, "the key stays after quitting")
        check(not (runtime / "lowkey" / "desktop.json").exists(), "runtime file removed on quit")

        if first_run:  # make the key-started app download again, to show it in the overlay
            for f in models.glob("ggml-large-v3-turbo-q5_0*"):
                f.unlink()
        n_before = len(events("dictation"))
        key("press")
        wait(lambda: any(d["state"] == "starting" for d in events("dictation")[n_before:]), 60, "starting state")
        print("  ok  the key started the closed app (background, no main window)")
        starting = [d for d in events("dictation")[n_before:] if d["state"] == "starting"]
        wait(lambda: win("lowkey-overlay"), 30, "overlay during start-up")
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
        check(win("Lowkey") is None, "main window stays closed")
        wait(lambda: dict_state()["state"] == "listening", 600 if first_run else 60, "listening after start")
        time.sleep(3.0)
        key("release")
        d3 = wait(lambda: (s := dict_state())["state"] in ("done", "error") and s, 60, "cold-start result")
        check(d3["state"] == "done" and "typed" in d3["delivered"], f"typed after a cold start: {d3['text']!r}")
        check(len(app_pids()) == 1, "one app process")
        if pointer_tool:
            strip_step()
        # The theme switch and Exit at the bottom of the side menu: a second launch opens the
        # window; Ctrl+, puts the focus on the section list, End goes to About; the switch and
        # Exit come after the page in the Tab order (after About's 4 links), never right after
        # the section list.
        run(["hyprctl", "dispatch", f'hl.dsp.exec_cmd("{APP}")'], env)
        qwin = wait(lambda: win("Lowkey"), 20, "the window from a second launch")
        focus(qwin["address"])
        time.sleep(0.5)
        for st in ("down", "up"):
            run(["hyprctl", "dispatch", f'hl.dsp.send_key_state({{ mods = "CTRL", key = "comma", state = "{st}" }})'], env)
            time.sleep(0.05)
        time.sleep(0.4)
        press_key("End")  # About
        for _ in range(5):
            press_key("Tab")
        theme_before = re.search(r'^theme = "(\w+)"', cfg_file.read_text(), re.M)
        press_key("Return")  # the theme switch: light <-> dark, saved
        flipped = wait(lambda: (m := re.search(r'^theme = "(light|dark)"', cfg_file.read_text(), re.M)) and m.group(1), 10, "theme saved from the switch")
        check(theme_before is None or theme_before.group(1) != flipped, f"the side menu's switch changes the theme ({theme_before and theme_before.group(1)} -> {flipped})")
        time.sleep(0.5)
        shot("36-desktop-theme-switch.png")
        press_key("Return")  # and back
        wait(lambda: re.search(r'^theme = "(light|dark)"', cfg_file.read_text(), re.M).group(1) != flipped, 10, "theme switched back")
        press_key("Tab")
        time.sleep(0.3)
        shot("35-desktop-exit-button.png")
        press_key("Return")
        wait(lambda: not app_pids(), 20, "quit from the side menu")
        check(not (runtime / "lowkey" / "desktop.json").exists(), "Exit in the side menu quits the app (runtime file removed)")

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
