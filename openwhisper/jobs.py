"""In-memory job queue. One worker thread runs jobs in order so the CPU is not oversubscribed."""

from __future__ import annotations

import logging
import os
import queue
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Any, Protocol

from . import formats

log = logging.getLogger("openwhisper")

KEEP_FINISHED_SECONDS = 3600
MAX_PENDING = 8


class QueueFull(RuntimeError):
    pass


class Transcriber(Protocol):
    def transcribe(self, path: str, language: str | None, model: str, on_update: Any) -> dict[str, Any]: ...


@dataclass
class Job:
    id: str
    filename: str
    path: str
    language: str | None
    model: str
    state: str = "queued"  # queued | preparing | transcribing | done | error
    progress: float = 0.0
    message: str = "Waiting in queue"
    detected_language: str | None = None
    language_probability: float | None = None
    segments: list[dict[str, Any]] = field(default_factory=list)
    result: dict[str, Any] | None = None
    error: str | None = None
    version: int = 0
    finished_at: float | None = None
    cond: threading.Condition = field(default_factory=threading.Condition, repr=False)

    def snapshot(self, since_segment: int = 0) -> dict[str, Any]:
        return {
            "id": self.id,
            "filename": self.filename,
            "state": self.state,
            "progress": round(self.progress, 4),
            "message": self.message,
            "language": self.detected_language,
            "language_probability": self.language_probability,
            "model": self.model,
            "new_segments": self.segments[since_segment:],
            "segment_count": len(self.segments),
            "result": self.result,
            "error": self.error,
            "version": self.version,
        }


class JobManager:
    def __init__(self, engine: Transcriber) -> None:
        self.engine = engine
        self.jobs: dict[str, Job] = {}
        self._queue: queue.Queue[str] = queue.Queue()
        self._lock = threading.Lock()
        self._worker = threading.Thread(target=self._run, name="transcriber", daemon=True)
        self._worker.start()

    def submit(self, filename: str, path: str, language: str | None, model: str) -> Job:
        job = Job(id=uuid.uuid4().hex, filename=filename, path=path, language=language, model=model)
        with self._lock:
            self._gc()
            pending = sum(j.state in ("queued", "preparing", "transcribing") for j in self.jobs.values())
            if pending >= MAX_PENDING:
                raise QueueFull(f"{pending} transcriptions are already waiting. Try again when they finish.")
            self.jobs[job.id] = job
        self._queue.put(job.id)
        return job

    def get(self, job_id: str) -> Job | None:
        return self.jobs.get(job_id)

    def _gc(self) -> None:
        now = time.time()
        for jid in [j.id for j in self.jobs.values() if j.finished_at and now - j.finished_at > KEEP_FINISHED_SECONDS]:
            del self.jobs[jid]

    @staticmethod
    def _apply(job: Job, update: dict[str, Any]) -> None:
        with job.cond:
            if "state" in update:
                job.state = update["state"]
            if "progress" in update:
                job.progress = update["progress"]
            if "message" in update:
                job.message = update["message"]
            if "language" in update:
                job.detected_language = update["language"]
                job.language_probability = update.get("language_probability")
            if "segment" in update:
                job.segments.append(update["segment"])
            job.version += 1
            job.cond.notify_all()

    def _run(self) -> None:
        while True:
            job = self.jobs.get(self._queue.get())
            if job is None:
                continue
            try:
                out = self.engine.transcribe(job.path, job.language, job.model, lambda u, j=job: self._apply(j, u))
                segs = out["segments"]
                result = {
                    "language": out.get("language"),
                    "duration": out.get("duration"),
                    "segments": segs,
                    "text": formats.to_txt(segs).strip(),
                    "srt": formats.to_srt(segs),
                    "vtt": formats.to_vtt(segs),
                }
                with job.cond:
                    job.result = result
                self._apply(job, {"state": "done", "progress": 1.0, "message": "Done"})
            except Exception as exc:
                log.exception("job %s failed", job.id)
                with job.cond:
                    job.error = str(exc) or exc.__class__.__name__
                self._apply(job, {"state": "error", "message": "Failed"})
            finally:
                job.finished_at = time.time()
                try:
                    os.unlink(job.path)
                except OSError:
                    pass
