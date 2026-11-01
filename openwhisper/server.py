"""FastAPI app: upload -> job -> live progress over Server-Sent Events."""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import tempfile
import threading
import time
from urllib.parse import urlsplit
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from fastapi.responses import FileResponse, HTMLResponse, JSONResponse, StreamingResponse
from starlette.middleware.trustedhost import TrustedHostMiddleware
from fastapi.staticfiles import StaticFiles

from . import config
from .engine import UnsupportedMedia, probe_audio
from .jobs import MAX_PENDING, JobManager, QueueFull

log = logging.getLogger("openwhisper")

DIST = (Path(__file__).resolve().parent.parent / "frontend" / "dist").resolve()
CHUNK = 1024 * 1024
LOCAL_HOSTS = ["127.0.0.1", "localhost"]


def _languages() -> dict[str, str]:
    from faster_whisper.tokenizer import _LANGUAGE_CODES

    try:
        from faster_whisper.tokenizer import LANGUAGES  # type: ignore[attr-defined]
    except ImportError:  # names are optional; codes are enough
        LANGUAGES = {}
    return {code: LANGUAGES.get(code, code) for code in _LANGUAGE_CODES}


UPLOAD_PREFIX = "openwhisper-"
_UPLOAD_NAME = re.compile(r"^openwhisper-[a-z0-9_]{8}(\.[^/]{0,16})?$")  # tempfile.mkstemp shape


def sweep_stale_uploads(directory: str | None = None, max_age_s: float = 6 * 3600) -> int:
    """Remove our own temp uploads left behind by a killed process. Returns how many were removed."""
    now, removed, uid = time.time(), 0, os.getuid()
    for f in Path(directory or tempfile.gettempdir()).glob(f"{UPLOAD_PREFIX}*"):
        try:
            st = f.stat()
            if _UPLOAD_NAME.match(f.name) and f.is_file() and st.st_uid == uid and now - st.st_mtime > max_age_s:
                f.unlink()
                removed += 1
        except OSError:
            pass
    return removed


def create_app(engine: Any = None, preload: bool = True, allowed_hosts: list[str] | None = None) -> FastAPI:
    if engine is None:
        from .engine import Engine

        engine = Engine()
    sweep_stale_uploads()
    jobs = JobManager(engine)
    languages = _languages()

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        if preload and hasattr(engine, "prepare"):
            # Fetch + load the default model right away so the first transcription is quick.
            def warm() -> None:
                try:
                    engine.prepare(config.DEFAULT_MODEL)
                except Exception:
                    pass  # state + message are exposed via /api/status

            threading.Thread(target=warm, name="preload", daemon=True).start()
        yield
        # Runs on Ctrl+C and SIGTERM. uvicorn re-raises the signal after shutdown, so code
        # after server.run() would never run; desktop cleanup must happen here.
        for hook in getattr(app.state, "shutdown_hooks", []):
            try:
                hook()
            except Exception:
                log.exception("shutdown hook failed")

    app = FastAPI(title="open-whisper", lifespan=lifespan)
    # Local-only app: refuse DNS-rebinding hosts and cross-site form posts from other web pages.
    hosts = allowed_hosts or LOCAL_HOSTS
    app.add_middleware(TrustedHostMiddleware, allowed_hosts=hosts)

    @app.middleware("http")
    async def guard(request: Request, call_next):
        if request.url.path.startswith("/api/") and request.method not in ("GET", "HEAD"):
            # Compare host names only: the Vite dev proxy rewrites Host to the backend port.
            origin = request.headers.get("origin")
            if origin and urlsplit(origin).hostname not in hosts:
                return JSONResponse({"detail": "Cross-origin requests are not allowed."}, status_code=403)
            if request.url.path == "/api/transcribe":
                if jobs.pending() >= MAX_PENDING:
                    return JSONResponse({"detail": "Too many transcriptions are waiting. Try again soon."}, status_code=429)
                # Reject oversized uploads before the multipart parser spools them to disk.
                # Without Content-Length (chunked), the streaming cap in the handler applies.
                length = request.headers.get("content-length")
                if length is not None and not length.isdigit():
                    return JSONResponse({"detail": "Bad Content-Length."}, status_code=400)
                if length and int(length) > config.MAX_UPLOAD_BYTES + CHUNK:
                    limit = config.MAX_UPLOAD_BYTES // 1024**3
                    return JSONResponse({"detail": f"File is larger than {limit} GB."}, status_code=413)
        return await call_next(request)

    app.state.jobs = jobs
    app.state.engine = engine
    from .hotkey import OVERLAY_PATH, GlobalDictation

    dictation = GlobalDictation(jobs, model=lambda: config.DEFAULT_MODEL)
    app.state.dictation = dictation

    @app.get("/api/dictation")
    def dictation_state() -> dict[str, Any]:
        return dictation.snapshot()

    @app.post("/api/dictation/{action}")
    def dictation_action(action: str) -> dict[str, Any]:
        handlers = {"press": dictation.press, "release": dictation.release,
                    "toggle": dictation.toggle, "cancel": dictation.cancel}
        if action not in handlers:
            raise HTTPException(404, "Unknown dictation action.")
        return handlers[action]()

    @app.get("/api/dictation/events")
    async def dictation_events(request: Request) -> StreamingResponse:
        async def stream():
            seen = -1
            while not await request.is_disconnected():
                if dictation.version != seen:
                    snap = dictation.snapshot()
                    seen = snap["version"]
                    yield f"data: {json.dumps(snap)}\n\n"
                await asyncio.sleep(0.05)  # ~20 updates/s for the waveform

        return StreamingResponse(
            stream(), media_type="text/event-stream", headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"}
        )

    @app.get("/api/status")
    def status() -> dict[str, Any]:
        return {**engine.status(), "max_upload_bytes": config.MAX_UPLOAD_BYTES}

    @app.get("/api/languages")
    def get_languages() -> dict[str, str]:
        return languages

    @app.post("/api/transcribe", status_code=202)
    async def transcribe(
        file: UploadFile = File(...),
        language: str = Form("auto"),
        model: str = Form(""),
    ) -> dict[str, Any]:
        language = (language or "auto").strip().lower()
        if language != "auto" and language not in languages:
            raise HTTPException(400, f"Unknown language code {language!r}.")
        model = model.strip() or config.DEFAULT_MODEL
        if model not in config.MODELS:
            raise HTTPException(400, f"Unknown model {model!r}.")

        name = Path(file.filename or "audio").name
        fd, tmp = tempfile.mkstemp(prefix=UPLOAD_PREFIX, suffix=Path(name).suffix[:16])
        size = 0
        try:
            with os.fdopen(fd, "wb") as out:
                while chunk := await file.read(CHUNK):
                    size += len(chunk)
                    if size > config.MAX_UPLOAD_BYTES:
                        raise HTTPException(413, f"File is larger than {config.MAX_UPLOAD_BYTES // 1024**3} GB.")
                    out.write(chunk)
            if size == 0:
                raise HTTPException(400, "The file is empty.")
            try:
                duration = await asyncio.to_thread(probe_audio, tmp)
            except UnsupportedMedia as exc:
                raise HTTPException(
                    415, f"{name}: {exc} Try MP3, WAV, M4A, OGG, FLAC, WEBM or a video file with sound."
                ) from exc
        except BaseException:
            os.unlink(tmp)
            raise
        try:
            job = jobs.submit(name, tmp, None if language == "auto" else language, model)
        except QueueFull as exc:
            os.unlink(tmp)
            raise HTTPException(429, str(exc)) from exc
        return {"job_id": job.id, "duration": duration}

    @app.get("/api/jobs/{job_id}")
    def get_job(job_id: str) -> dict[str, Any]:
        job = jobs.get(job_id)
        if not job:
            raise HTTPException(404, "Job not found.")
        snap = job.snapshot()
        if snap["state"] in ("queued", "preparing"):
            snap["model_state"] = engine.model_state(job.model)
        return snap

    @app.get("/api/jobs/{job_id}/events")
    async def job_events(job_id: str, request: Request) -> StreamingResponse:
        job = jobs.get(job_id)
        if not job:
            raise HTTPException(404, "Job not found.")

        async def stream():
            seen_version, seen_segments, seen_model = -1, 0, None
            while True:
                if job.version != seen_version:
                    snap = job.snapshot(since_segment=seen_segments)
                    seen_version, seen_segments = snap["version"], snap["segment_count"]
                    # Model download progress is part of "preparing"; include it for the UI.
                    if snap["state"] in ("queued", "preparing"):
                        snap["model_state"] = engine.model_state(job.model)
                    yield f"data: {json.dumps(snap)}\n\n"
                    if snap["state"] in ("done", "error"):
                        return
                elif job.state in ("queued", "preparing"):
                    # Version does not move while a model downloads; push download progress when it changes.
                    ms = engine.model_state(job.model)
                    key = (ms["state"], ms["downloaded"])
                    if key != seen_model:
                        seen_model = key
                        yield f"data: {json.dumps({'id': job.id, 'state': job.state, 'model_state': ms})}\n\n"
                if await request.is_disconnected():
                    return
                await asyncio.sleep(0.25)

        return StreamingResponse(
            stream(), media_type="text/event-stream", headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"}
        )

    def candidate_is_file(path: str) -> bool:
        c = (DIST / path).resolve()
        return c.is_file() and DIST in c.parents

    if DIST.is_dir():
        app.mount("/assets", StaticFiles(directory=DIST / "assets"), name="assets")

        @app.get(OVERLAY_PATH, include_in_schema=False)
        def overlay_page() -> FileResponse:
            return FileResponse(DIST / "overlay.html")

        @app.get("/{path:path}", include_in_schema=False)
        def spa(path: str) -> FileResponse:
            if path.startswith("api/"):
                raise HTTPException(404, "Not found.")
            if Path(path).suffix and not candidate_is_file(path):
                raise HTTPException(404, "Not found.")  # e.g. /favicon.ico: no HTML for missing files
            candidate = (DIST / path).resolve()
            if path and candidate.is_file() and DIST in candidate.parents:
                return FileResponse(candidate)
            return FileResponse(DIST / "index.html")
    else:

        @app.get("/", include_in_schema=False)
        def no_frontend() -> HTMLResponse:
            return HTMLResponse(
                "<h1>open-whisper</h1><p>The web UI is not built yet. Start the app with <code>./run</code>.</p>",
                status_code=503,
            )

    return app
