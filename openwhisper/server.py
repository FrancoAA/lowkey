"""FastAPI app: upload -> job -> live progress over Server-Sent Events."""

from __future__ import annotations

import asyncio
import json
import logging
import os
import tempfile
import threading
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from fastapi.responses import FileResponse, HTMLResponse, StreamingResponse
from fastapi.staticfiles import StaticFiles

from . import config
from .engine import UnsupportedMedia, probe_audio
from .jobs import JobManager

log = logging.getLogger("openwhisper")

DIST = Path(__file__).resolve().parent.parent / "frontend" / "dist"
CHUNK = 1024 * 1024


def _languages() -> dict[str, str]:
    from faster_whisper.tokenizer import _LANGUAGE_CODES

    try:
        from faster_whisper.tokenizer import LANGUAGES  # type: ignore[attr-defined]
    except ImportError:  # names are optional; codes are enough
        LANGUAGES = {}
    return {code: LANGUAGES.get(code, code) for code in _LANGUAGE_CODES}


def create_app(engine: Any = None, preload: bool = True) -> FastAPI:
    if engine is None:
        from .engine import Engine

        engine = Engine()
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

    app = FastAPI(title="open-whisper", lifespan=lifespan)
    app.state.jobs = jobs
    app.state.engine = engine

    @app.get("/api/status")
    def status() -> dict[str, Any]:
        return engine.status()

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
        fd, tmp = tempfile.mkstemp(prefix="openwhisper-", suffix=Path(name).suffix[:16])
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
        job = jobs.submit(name, tmp, None if language == "auto" else language, model)
        return {"job_id": job.id, "duration": duration}

    @app.get("/api/jobs/{job_id}")
    def get_job(job_id: str) -> dict[str, Any]:
        job = jobs.get(job_id)
        if not job:
            raise HTTPException(404, "Job not found.")
        return job.snapshot()

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

    if DIST.is_dir():
        app.mount("/assets", StaticFiles(directory=DIST / "assets"), name="assets")

        @app.get("/{path:path}", include_in_schema=False)
        def spa(path: str) -> FileResponse:
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
