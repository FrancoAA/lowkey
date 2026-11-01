"""`python -m openwhisper` — start the app and open it in the browser. No configuration needed."""

from __future__ import annotations

import argparse
import logging
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
    to the running session (no config file is edited; they go away with the app)."""
    import json
    import os

    from . import hotkey as hk

    rd = hk.runtime_dir()
    rd.mkdir(parents=True, exist_ok=True)
    (rd / "server.json").write_text(json.dumps({"url": url, "pid": os.getpid()}))
    app.state.dictation.overlay = hk.Overlay(url)

    if args.no_hotkey:
        print("  Global dictation key: off (--no-hotkey). scripts/ow-dictate still works.")
        return None
    if not hk.hyprland_running():
        print("  Global dictation key: not on Hyprland; bind scripts/ow-dictate in your desktop (see README).")
        return None
    missing = [t for t in ("wl-copy", "wtype") if not shutil.which(t)] + ([] if hk.MicRecorder.available() else ["pw-record"])
    binding = hk.HyprlandHotkey(args.hotkey or hk.DEFAULT_KEY)
    try:
        print(f"  Global dictation key: {binding.install()}")
    except Exception as exc:  # never block the web app on the desktop integration
        print(f"  Global dictation key: could not add it ({exc}). See README for the config snippet.")
        return None
    if missing:
        print(f"  Note: install {', '.join(missing)} for the global key to record/copy/type.")
    return binding


def teardown_global_dictation(app, binding) -> None:
    import json
    import os

    from . import hotkey as hk

    try:
        if binding:
            binding.uninstall()
        overlay = app.state.dictation.overlay
        if overlay:
            overlay.close()
        f = hk.runtime_dir() / "server.json"
        if f.exists() and json.loads(f.read_text()).get("pid") == os.getpid():
            f.unlink()
    except Exception:
        pass


def main() -> None:
    ap = argparse.ArgumentParser(prog="openwhisper", description="Local speech-to-text in your browser.")
    ap.add_argument("--port", type=int, default=None, help=f"default: first free port from {config.PORT}")
    ap.add_argument("--no-browser", action="store_true", help="do not open a browser tab")
    ap.add_argument("--hotkey", default=None, help='global dictation key on Hyprland (default: "SUPER + CTRL + X")')
    ap.add_argument("--no-hotkey", action="store_true", help="do not add the global dictation key")
    args = ap.parse_args()

    quiet_logs()
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

    if not args.no_browser:

        def open_when_ready() -> None:
            while not server.started:
                time.sleep(0.1)
            webbrowser.open(url)

        threading.Thread(target=open_when_ready, daemon=True).start()
    server.run()


if __name__ == "__main__":
    main()
