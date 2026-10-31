"""`python -m openwhisper` — start the app and open it in the browser. No configuration needed."""

from __future__ import annotations

import argparse
import logging
import socket
import threading
import time
import webbrowser

import uvicorn

from . import config


def free_port(host: str, start: int) -> int:
    for port in range(start, start + 50):
        with socket.socket() as s:
            try:
                s.bind((host, port))
                return port
            except OSError:
                continue
    raise SystemExit(f"No free port found from {start} to {start + 49}.")


def main() -> None:
    ap = argparse.ArgumentParser(prog="openwhisper", description="Local speech-to-text in your browser.")
    ap.add_argument("--port", type=int, default=None, help=f"default: first free port from {config.PORT}")
    ap.add_argument("--no-browser", action="store_true", help="do not open a browser tab")
    args = ap.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(message)s")
    port = args.port or free_port(config.HOST, config.PORT)
    url = f"http://{config.HOST}:{port}"

    from .server import create_app

    app = create_app()
    engine = app.state.engine
    print(f"\n  open-whisper  →  {url}")
    print(f"  engine: faster-whisper on {engine.device} ({engine.compute_type}), model: {config.DEFAULT_MODEL}")
    if engine.model_state(config.DEFAULT_MODEL)["state"] == "missing":
        print(f"  First run: downloading the {config.DEFAULT_MODEL} model (~{config.MODELS[config.DEFAULT_MODEL].approx_mb} MB).")
    print("  Press Ctrl+C to stop.\n")

    server = uvicorn.Server(uvicorn.Config(app, host=config.HOST, port=port, log_level="warning"))

    if not args.no_browser:

        def open_when_ready() -> None:
            while not server.started:
                time.sleep(0.1)
            webbrowser.open(url)

        threading.Thread(target=open_when_ready, daemon=True).start()
    server.run()


if __name__ == "__main__":
    main()
