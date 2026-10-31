from __future__ import annotations

import json

from openwhisper import config

from .conftest import FakeEngine, tone_wav, wait_done


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
