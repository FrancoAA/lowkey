"""Real faster-whisper run on a committed 3-second TTS clip. Downloads the default model if needed."""

import pytest
from fastapi.testclient import TestClient

from openwhisper.server import create_app

from .conftest import FIXTURES, TEST_HOSTS, wait_done


@pytest.mark.slow
def test_default_model_transcribes_speech():
    with TestClient(create_app(preload=False, allowed_hosts=TEST_HOSTS)) as client:
        data = (FIXTURES / "hello.ogg").read_bytes()
        res = client.post("/api/transcribe", files={"file": ("hello.ogg", data, "audio/ogg")})
        assert res.status_code == 202, res.text
        job = wait_done(client, res.json()["job_id"])
        assert job["state"] == "done", job["error"]
        text = job["result"]["text"].lower()
        assert "hello" in text and "test" in text
        assert job["result"]["language"] == "en"


@pytest.mark.slow
def test_auto_detect_runs_the_encoder_once_with_the_same_result():
    from faster_whisper import WhisperModel

    from openwhisper.fastlang import FastLangWhisperModel

    audio = str(FIXTURES / "hello.ogg")
    kw = dict(beam_size=5, vad_filter=True, vad_parameters={"threshold": 0.3}, condition_on_previous_text=False)
    stock = WhisperModel("large-v3-turbo", device="cpu", compute_type="int8")
    fast = FastLangWhisperModel("large-v3-turbo", device="cpu", compute_type="int8")
    s_segs, s_info = stock.transcribe(audio, **kw)
    f_segs, f_info = fast.transcribe(audio, **kw)
    assert " ".join(s.text for s in s_segs) == " ".join(s.text for s in f_segs)
    assert s_info.language == f_info.language == "en"
    assert fast.encode_calls == 1  # stock faster-whisper runs it twice for a short clip


@pytest.mark.slow
def test_encoder_reuse_on_audio_longer_than_one_window():
    import numpy as np
    from faster_whisper import WhisperModel
    from faster_whisper.audio import decode_audio

    from openwhisper.fastlang import FastLangWhisperModel

    clip = decode_audio(str(FIXTURES / "hello.ogg"))
    gap = np.zeros(16000, dtype=clip.dtype)
    audio = np.concatenate([np.concatenate([clip, gap])] * 12)  # ~45 s: two 30 s windows
    kw = dict(beam_size=5, condition_on_previous_text=False)  # no VAD: keep both windows
    stock = WhisperModel("large-v3-turbo", device="cpu", compute_type="int8")
    fast = FastLangWhisperModel("large-v3-turbo", device="cpu", compute_type="int8")
    s_segs, s_info = stock.transcribe(audio, **kw)
    s_text = " ".join(s.text for s in s_segs)
    fast.encode_calls = 0
    f_segs, f_info = fast.transcribe(audio, **kw)
    f_text = " ".join(s.text for s in f_segs)
    assert s_text == f_text and s_info.language == f_info.language == "en"
    windows = fast.encode_calls
    assert windows >= 2  # later windows still run the encoder; only the duplicate is skipped
