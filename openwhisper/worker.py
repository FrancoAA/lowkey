"""faster-whisper helper for the open-whisper desktop app (JSON lines over stdin/stdout).

The desktop app starts it with `uv run python -m openwhisper.worker` when a faster-whisper
model is chosen (or picked automatically on a machine without a GPU). It uses the same Engine
as the web app: CPU int8 or CUDA, encoder reuse for auto language detection, VAD 0.3.

Requests (one JSON object per line):
  {"id": 1, "cmd": "prepare", "model": "large-v3-turbo"}
  {"id": 2, "cmd": "transcribe", "model": "large-v3-turbo", "path": "/a.mp3", "language": null}
Events (one per line): {"id": N, "event": "state"|"progress"|"language"|"segment"|"done"|"error", ...}
and, without id, {"event": "model_state", "model": name, "state": {...}} during downloads.
"""

from __future__ import annotations

import json
import logging
import sys
import threading
import time
from typing import Any

import openwhisper  # noqa: F401  (sets HF_HUB_DISABLE_XET before huggingface_hub loads)

from .engine import Engine, UnsupportedMedia, probe_audio

_out_lock = threading.Lock()


def emit(obj: dict[str, Any]) -> None:
    with _out_lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


def watch_model(engine: Engine, name: str, stop: threading.Event) -> None:
    """Forward download/load progress while a model is prepared."""
    last = None
    while not stop.is_set():
        st = engine.model_state(name)
        key = (st["state"], st["downloaded"], st.get("error"))
        if key != last:
            last = key
            emit({"event": "model_state", "model": name, "state": st})
        stop.wait(0.25)
    emit({"event": "model_state", "model": name, "state": engine.model_state(name)})


def handle(engine: Engine, req: dict[str, Any]) -> None:
    rid, cmd, model = req.get("id"), req.get("cmd"), req.get("model")
    stop = threading.Event()
    watcher = threading.Thread(target=watch_model, args=(engine, model, stop), daemon=True) if model else None
    try:
        if watcher:
            watcher.start()
        if cmd == "prepare":
            engine.prepare(model)
            emit({"id": rid, "event": "done"})
        elif cmd == "transcribe":
            path = req["path"]
            try:
                duration = probe_audio(path)
            except UnsupportedMedia as exc:
                raise RuntimeError(f"{path.rsplit('/', 1)[-1]}: {exc} Try MP3, WAV, M4A, OGG, FLAC, WEBM or a video file with sound.") from exc

            def on_update(u: dict[str, Any]) -> None:
                if "state" in u:
                    emit({"id": rid, "event": "state", "state": u["state"], "message": u.get("message", "")})
                if "language" in u:
                    emit({"id": rid, "event": "language", "language": u["language"], "probability": u.get("language_probability")})
                if "segment" in u:
                    emit({"id": rid, "event": "segment", "segment": u["segment"]})
                if "progress" in u:
                    emit({"id": rid, "event": "progress", "progress": u["progress"]})

            out = engine.transcribe(path, req.get("language"), model, on_update)
            emit({"id": rid, "event": "done", "language": out["language"], "duration": out["duration"] or duration,
                  "segments": out["segments"]})
        else:
            emit({"id": rid, "event": "error", "error": f"unknown command {cmd!r}"})
    except Exception as exc:  # report, keep serving
        emit({"id": rid, "event": "error", "error": str(exc) or exc.__class__.__name__})
    finally:
        stop.set()
        if watcher:
            watcher.join(2)


def main() -> None:
    logging.basicConfig(level=logging.WARNING, stream=sys.stderr, format="  %(message)s")
    for name in ("faster_whisper", "httpx"):
        logging.getLogger(name).setLevel(logging.ERROR)
    from huggingface_hub.utils import logging as hf_logging

    hf_logging.set_verbosity_error()
    engine = Engine()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            emit({"event": "error", "error": "bad request"})
            continue
        handle(engine, req)  # one at a time: the desktop app queues jobs
        time.sleep(0)


if __name__ == "__main__":
    main()
