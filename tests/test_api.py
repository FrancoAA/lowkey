from __future__ import annotations

import json

from openwhisper import config

from .conftest import TEST_HOSTS, FakeEngine, tone_wav, wait_done


def upload(client, data: bytes, name: str = "tone.wav", **form):
    return client.post("/api/transcribe", files={"file": (name, data, "audio/wav")}, data=form)


def test_status_reports_default_model(client):
    body = client.get("/api/status").json()
    assert body["default_model"] == config.DEFAULT_MODEL
    assert {m["name"] for m in body["models"]} == set(config.MODELS)


def test_languages_include_english_and_spanish(client):
    langs = client.get("/api/languages").json()
    assert "en" in langs and "es" in langs


def test_transcribe_generated_audio_end_to_end(client, fake_engine: FakeEngine):
    res = upload(client, tone_wav())
    assert res.status_code == 202, res.text
    body = res.json()
    assert abs(body["duration"] - 1.0) < 0.05
    job = wait_done(client, body["job_id"])
    assert job["state"] == "done"
    r = job["result"]
    assert r["text"] == "Hello there. General test."
    assert r["srt"].startswith("1\n00:00:00,000 --> 00:00:00,500\nHello there.\n")
    assert r["vtt"].startswith("WEBVTT\n\n00:00:00.000 --> 00:00:00.500\nHello there.")
    # Zero config: no language and no model means auto-detect + the default model.
    assert fake_engine.calls == [(None, config.DEFAULT_MODEL)]


def test_language_and_model_override(client, fake_engine: FakeEngine):
    job_id = upload(client, tone_wav(), language="es", model="small").json()["job_id"]
    assert wait_done(client, job_id)["state"] == "done"
    assert fake_engine.calls == [("es", "small")]


def test_progress_stream_ends_with_result(client):
    job_id = upload(client, tone_wav()).json()["job_id"]
    events = []
    with client.stream("GET", f"/api/jobs/{job_id}/events") as stream:
        for line in stream.iter_lines():
            if line.startswith("data: "):
                events.append(json.loads(line[6:]))
    assert events[-1]["state"] == "done"
    assert events[-1]["result"]["text"] == "Hello there. General test."
    segs = []
    for e in events:  # the client-side merge rule from frontend/src/main.ts
        if "new_segments" in e:
            segs = segs[: e["segment_count"] - len(e["new_segments"])] + e["new_segments"]
    assert [s["text"] for s in segs] == ["Hello there.", "General test."]


def test_rejects_non_audio_file_with_clear_message(client):
    res = upload(client, b"%PDF-1.4 not audio at all" * 50, name="notes.pdf")
    assert res.status_code == 415
    assert "notes.pdf" in res.json()["detail"]
    assert "MP3" in res.json()["detail"]


def test_rejects_empty_file(client):
    assert upload(client, b"", name="empty.wav").status_code == 400


def test_rejects_unknown_language_and_model(client):
    assert upload(client, tone_wav(), language="xx").status_code == 400
    assert upload(client, tone_wav(), model="gpt-9").status_code == 400


def test_unknown_job_is_404(client):
    assert client.get("/api/jobs/nope").status_code == 404
    assert client.get("/api/jobs/nope/events").status_code == 404


def test_upload_size_limit(client, monkeypatch):
    monkeypatch.setattr(config, "MAX_UPLOAD_BYTES", 1000)
    # Small enough for the Content-Length precheck, too big for the streaming copy.
    res = upload(client, tone_wav(1.0))
    assert res.status_code == 413
    # Clearly too big: rejected from Content-Length before the body is parsed.
    res = upload(client, b"\0" * (3 * 1024 * 1024))
    assert res.status_code == 413


def test_cross_origin_post_and_foreign_host_are_refused(client):
    res = client.post(
        "/api/transcribe",
        files={"file": ("t.wav", tone_wav(), "audio/wav")},
        headers={"Origin": "https://evil.example"},
    )
    assert res.status_code == 403
    assert client.get("/api/status", headers={"Host": "evil.example"}).status_code == 400


def test_vite_dev_proxy_post_is_allowed(client):
    # Vite's proxy sets Host to the backend while Origin stays the dev server.
    res = client.post(
        "/api/transcribe",
        files={"file": ("t.wav", tone_wav(), "audio/wav")},
        headers={"Origin": "http://127.0.0.1:5173", "Host": "127.0.0.1:8000"},
    )
    assert res.status_code == 202, res.text


def test_unknown_api_path_is_json_404(client):
    res = client.get("/api/nope")
    assert res.status_code == 404


def test_missing_static_file_is_404_not_html(client):
    from openwhisper.server import DIST

    if not DIST.is_dir():
        import pytest

        pytest.skip("frontend not built")
    assert client.get("/favicon.ico").status_code == 404
    assert client.get("/").headers["content-type"].startswith("text/html")


class DownloadingEngine(FakeEngine):
    """Reports a model download in progress for a short while, like a first run."""

    def __init__(self) -> None:
        super().__init__()
        self.ticks = 0
        self.downloading = True

    def model_state(self, name: str) -> dict:
        if not self.downloading:
            return super().model_state(name)
        self.ticks += 1
        return {"name": name, "state": "downloading", "downloaded": self.ticks, "total": 100,
                "progress": self.ticks / 100, "error": None}

    def transcribe(self, path, language, model, on_update):
        import time

        on_update({"state": "preparing", "message": "Preparing model"})
        time.sleep(1.0)
        self.downloading = False
        return super().transcribe(path, language, model, on_update)


def test_progress_stream_reports_model_download():
    from fastapi.testclient import TestClient

    from openwhisper.server import create_app

    with TestClient(create_app(engine=DownloadingEngine(), preload=False, allowed_hosts=TEST_HOSTS)) as c:
        job_id = upload(c, tone_wav()).json()["job_id"]
        events = []
        with c.stream("GET", f"/api/jobs/{job_id}/events") as stream:
            for line in stream.iter_lines():
                if line.startswith("data: "):
                    events.append(json.loads(line[6:]))
    downloads = [e for e in events if e.get("model_state", {}).get("state") == "downloading"]
    assert len(downloads) >= 2, events
    assert downloads[-1]["model_state"]["progress"] > downloads[0]["model_state"]["progress"]
    assert events[-1]["state"] == "done"


def test_sweep_removes_only_our_stale_uploads(tmp_path):
    import os
    import time

    from openwhisper.server import sweep_stale_uploads

    old = time.time() - 7 * 3600
    ours_old = tmp_path / "openwhisper-ab12cd34.wav"
    ours_new = tmp_path / "openwhisper-ef56gh78.mp3"
    foreign = tmp_path / "openwhisper-notes.txt"
    for f in (ours_old, ours_new, foreign):
        f.write_bytes(b"x")
    os.utime(ours_old, (old, old))
    os.utime(foreign, (old, old))
    assert sweep_stale_uploads(str(tmp_path)) == 1
    assert not ours_old.exists() and ours_new.exists() and foreign.exists()
