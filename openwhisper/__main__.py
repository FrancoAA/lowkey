"""`python -m openwhisper` — start the app and open it in the browser. No configuration needed."""

from __future__ import annotations

import argparse
import logging
import os
import shutil
import socket
import sys
import threading
import time
import webbrowser

import uvicorn

from . import config


def free_port(host: str, start: int) -> int:
    for port in range(start, start + 50):
        with socket.socket() as s:
            # Same option uvicorn uses, so a just-restarted app gets its old port back
            # (TIME_WAIT); a port with a live listener is still refused.
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                s.bind((host, port))
                return port
            except OSError:
                continue
    raise SystemExit(f"No free port found from {start} to {start + 49}.")


def quiet_logs() -> None:
    """Keep the terminal readable: our own messages only, no HTTP or library chatter."""
    logging.basicConfig(level=logging.WARNING, format="  %(message)s")
    logging.getLogger("openwhisper").setLevel(logging.INFO)
    for name in ("httpx", "faster_whisper"):
        logging.getLogger(name).setLevel(logging.ERROR)
    # The hub installs its own handler and resets its level on import, so use its API.
    from huggingface_hub.utils import logging as hf_logging

    hf_logging.set_verbosity_error()


def report_model(engine, name: str) -> None:
    """Terminal progress for the first-run model download, then a 'ready' line."""
    tty = sys.stdout.isatty()
    last = -1
    while True:
        st = engine.model_state(name)
        if st["state"] == "downloading" and st["total"]:
            pct = int(st["progress"] * 100)
            if tty:
                bar = "#" * (pct // 4)
                print(f"\r  Downloading model [{bar:<25}] {pct:3d}%  {st['downloaded'] >> 20}/{st['total'] >> 20} MB", end="", flush=True)
            elif pct // 10 != last // 10:
                print(f"  Downloading model… {pct}%", flush=True)
            last = pct
        elif st["state"] == "ready":
            print(("\n" if tty and last >= 0 else "") + "  Model ready. Drop a file or hold Space in the browser.", flush=True)
            return
        elif st["state"] == "error":
            print(("\n" if tty and last >= 0 else "") + f"  {st['error']}", flush=True)
            return
        time.sleep(0.5)


def setup_global_dictation(app, url: str, args) -> object | None:
    """Publish our URL for scripts/ow-dictate and, on Hyprland, add the key bind + overlay rule
    to the running session (no config file is edited). The overlay browser starts later, in
    after_start(), when the server can answer it."""
    import json
    import os

    from . import hotkey as hk

    try:
        rd = hk.private_runtime_dir()
        f = rd / "server.json"
        f.write_text(json.dumps({"url": url, "pid": os.getpid(), "token": app.state.dictation_token}))
        os.chmod(f, 0o600)
    except OSError as exc:  # the web app must start anyway
        print(f"  Global dictation: cannot use {hk.runtime_dir()} ({exc}); the key will not reach this app.")
    app.state.dictation.overlay = hk.Overlay(url)

    if args.no_hotkey:
        removed = hk.remove_app_binds() if hk.hyprland_running() else []
        print("  Global dictation key: not added (--no-hotkey)" + (f"; removed {', '.join(removed)}." if removed else "."))
        if hk.hyprland_running() and hk.CONFIG_DESC in [b.get("description") for b in hk.key_binds(args.hotkey or hk.DEFAULT_KEY)]:
            print("  Note: the key is in your Hyprland config (open-whisper.dictation block in bindings.lua) and still")
            print("  works. Delete that block to turn it off.")
        return None
    if not hk.hyprland_running():
        print("  Global dictation key: not on Hyprland; bind scripts/ow-dictate in your desktop (see README).")
        return None
    missing = [t for t in ("wl-copy", "wtype") if not shutil.which(t)] + ([] if hk.MicRecorder.available() else ["pw-record"])
    binding = hk.HyprlandHotkey(args.hotkey or hk.DEFAULT_KEY)
    try:
        print(f"  Global dictation key: {binding.install(keep_existing=args.dictate_on_start)}")
    except Exception as exc:  # never block the web app on the desktop integration
        print(f"  Global dictation key: could not add it ({exc}). See README for the config snippet.")
        return None
    binding.watch_reloads()
    app.state.dictation.overlay.enabled = binding.rule  # without our window rule the overlay could take focus
    if missing:
        print(f"  Note: install {', '.join(missing)} for the global key to record/copy/type.")
    return binding


def after_start(app, args) -> None:
    """Runs once the server answers: start the overlay browser (hidden, so the first press
    shows it at once) and, if the key started us, begin that dictation when the model is ready."""
    import shutil as _shutil

    from . import config
    from . import hotkey as hk

    # ow-dictate's start lock: the server answers now, so a key press reaches us.
    _shutil.rmtree(hk.runtime_dir() / "starting", ignore_errors=True)
    dictation = app.state.dictation
    if dictation.overlay and dictation.overlay.available():
        dictation.overlay.start()
    if args.dictate_on_start:
        engine = app.state.engine
        dictation.start_when_ready(lambda: engine.model_state(config.DEFAULT_MODEL))


def teardown_global_dictation(app, binding) -> None:
    import json
    import os

    from . import hotkey as hk

    if getattr(app.state, "torn_down", False):
        return
    app.state.torn_down = True
    try:
        if binding:
            binding.uninstall(keep_bind=True)  # the key can start the app again
        overlay = app.state.dictation.overlay
        if overlay:
            overlay.close()
        f = hk.runtime_dir() / "server.json"
        if f.exists() and json.loads(f.read_text()).get("pid") == os.getpid():
            f.unlink()
    except Exception:
        pass


def running_instance() -> str | None:
    """URL of an open-whisper that this user already runs and that answers, else None."""
    import json
    import urllib.request

    from . import hotkey as hk

    try:
        info = json.loads((hk.runtime_dir() / "server.json").read_text())
        os.kill(int(info["pid"]), 0)
        with urllib.request.urlopen(info["url"] + "/api/status", timeout=2):
            return info["url"]
    except (OSError, ValueError, KeyError):
        return None


def main() -> None:
    ap = argparse.ArgumentParser(prog="openwhisper", description="Local speech-to-text in your browser.")
    ap.add_argument("--port", type=int, default=None, help=f"default: first free port from {config.PORT}")
    ap.add_argument("--no-browser", action="store_true", help="do not open a browser tab")
    ap.add_argument("--hotkey", default=None, help='global dictation key on Hyprland (default: "SUPER + CTRL + X")')
    ap.add_argument("--no-hotkey", action="store_true", help="do not add the global dictation key (removes one the app added)")
    ap.add_argument("--dictate-on-start", action="store_true", help=argparse.SUPPRESS)  # used by scripts/ow-dictate
    args = ap.parse_args()

    quiet_logs()
    if (running := running_instance()) and not args.port:
        # One app per user: e.g. the dictation key already started one in the background.
        print(f"\n  open-whisper is already running at {running}")
        if not args.no_browser and not args.dictate_on_start:
            webbrowser.open(running)
        return
    port = args.port or free_port(config.HOST, config.PORT)
    url = f"http://{config.HOST}:{port}"

    from .server import create_app

    app = create_app()
    engine = app.state.engine
    print(f"\n  open-whisper  →  {url}")
    print(f"  engine: faster-whisper on {engine.device} ({engine.compute_type}), model: {config.DEFAULT_MODEL}")
    if engine.model_state(config.DEFAULT_MODEL)["state"] == "missing":
        print(f"  First run: downloading the {config.DEFAULT_MODEL} model (~{config.MODELS[config.DEFAULT_MODEL].approx_mb} MB).")
    hotkey = setup_global_dictation(app, url, args)
    app.state.shutdown_hooks = [lambda: teardown_global_dictation(app, hotkey)]
    print("  Press Ctrl+C to stop.\n")

    threading.Thread(target=report_model, args=(engine, config.DEFAULT_MODEL), daemon=True).start()
    # Open SSE streams (the overlay keeps one) would otherwise hold shutdown forever.
    server = uvicorn.Server(
        uvicorn.Config(app, host=config.HOST, port=port, log_level="warning", timeout_graceful_shutdown=2)
    )

    def quit_server() -> None:
        server.should_exit = True

    app.state.quit = quit_server

    def when_started() -> None:
        while not server.started:
            if server.should_exit:
                return
            time.sleep(0.05)
        after_start(app, args)
        if not args.no_browser:
            webbrowser.open(url)

    threading.Thread(target=when_started, daemon=True).start()
    try:
        server.run()
    finally:
        # Normally the lifespan hook already ran (Ctrl+C, SIGTERM). This covers a failed start,
        # e.g. the port taken between the check and the bind.
        teardown_global_dictation(app, hotkey)


if __name__ == "__main__":
    main()
