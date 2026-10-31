from __future__ import annotations

import io
import math
import struct
import time
import wave
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from openwhisper import config
from openwhisper.server import create_app

FIXTURES = Path(__file__).parent / "fixtures"
TEST_HOSTS = ["127.0.0.1", "localhost", "testserver"]  # TestClient sends Host: testserver


def tone_wav(seconds: float = 1.0, rate: int = 16000) -> bytes:
    """A tiny generated audio sample (440 Hz sine), valid WAV."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        n = int(seconds * rate)
        w.writeframes(b"".join(struct.pack("<h", int(8000 * math.sin(2 * math.pi * 440 * i / rate))) for i in range(n)))
    return buf.getvalue()


class FakeEngine:
    """Stands in for faster-whisper so API tests run in milliseconds."""

    device, compute_type = "cpu", "int8"

    def __init__(self) -> None:
        self.calls: list[tuple[str | None, str]] = []

    def model_state(self, name: str) -> dict:
        return {"name": name, "state": "ready", "downloaded": 1, "total": 1, "progress": 1.0, "error": None}

    def status(self) -> dict:
        return {
            "device": "cpu",
            "compute_type": "int8",
            "default_model": config.DEFAULT_MODEL,
            "models": [{"label": m.label, "approx_mb": m.approx_mb, **self.model_state(m.name)} for m in config.MODELS.values()],
        }

    def transcribe(self, path: str, language: str | None, model: str, on_update) -> dict:
        assert Path(path).exists()
        self.calls.append((language, model))
        on_update({"state": "transcribing", "progress": 0.0})
        on_update({"language": language or "en", "language_probability": 0.98})
        segs = [
            {"start": 0.0, "end": 0.5, "text": "Hello there."},
            {"start": 0.5, "end": 1.0, "text": "General test."},
        ]
        for s in segs:
            on_update({"segment": s, "progress": s["end"]})
        return {"language": language or "en", "duration": 1.0, "segments": segs}


@pytest.fixture
def fake_engine() -> FakeEngine:
    return FakeEngine()


@pytest.fixture
def client(fake_engine: FakeEngine):
    with TestClient(create_app(engine=fake_engine, preload=False, allowed_hosts=TEST_HOSTS)) as c:
        yield c


def wait_done(client: TestClient, job_id: str, timeout: float = 600) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        job = client.get(f"/api/jobs/{job_id}").json()
        if job["state"] in ("done", "error"):
            return job
        time.sleep(0.05)
    raise AssertionError("job did not finish in time")
