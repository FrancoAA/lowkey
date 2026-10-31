"""Real faster-whisper run on a committed 3-second TTS clip. Downloads the default model if needed."""

import pytest
from fastapi.testclient import TestClient

from openwhisper.server import create_app

from .conftest import FIXTURES, wait_done


@pytest.mark.slow
def test_default_model_transcribes_speech():
    with TestClient(create_app(preload=False)) as client:
        data = (FIXTURES / "hello.ogg").read_bytes()
        res = client.post("/api/transcribe", files={"file": ("hello.ogg", data, "audio/ogg")})
        assert res.status_code == 202, res.text
        job = wait_done(client, res.json()["job_id"])
        assert job["state"] == "done", job["error"]
        text = job["result"]["text"].lower()
        assert "hello" in text and "test" in text
        assert job["result"]["language"] == "en"
