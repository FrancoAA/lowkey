"""Global-hotkey dictation: state machine, API, Hyprland Lua snippets. No mic, no desktop."""

from __future__ import annotations

import tempfile
import time

import pytest

from openwhisper import hotkey as hk
from openwhisper.jobs import JobManager

from .conftest import FakeEngine


class FakeRecorder:
    def __init__(self, on_level, on_end=None, seconds: float = 1.0) -> None:
        self.on_level, self.on_end, self.seconds, self.started, self.stopped = on_level, on_end, seconds, False, False

    def start(self) -> None:
        self.started = True
        self.on_level(0.7)

    def stop(self) -> bytes:
        self.stopped = True
        return b"\0\0" * int(hk.RATE * self.seconds)


class Clock:
    def __init__(self) -> None:
        self.t = 100.0

    def __call__(self) -> float:
        return self.t


def make(seconds: float = 1.0, clock: Clock | None = None, deliver_result=("copied", "typed"), engine=None):
    delivered: list[str] = []
    recorders: list[FakeRecorder] = []
    notes: list[str] = []

    def factory(on_level, on_end=None):
        r = FakeRecorder(on_level, on_end, seconds)
        recorders.append(r)
        return r

    d = hk.GlobalDictation(
        JobManager(engine or FakeEngine()), model=lambda: "large-v3-turbo",
        deliver=lambda text: delivered.append(text) or list(deliver_result),
        recorder_factory=factory, clock=clock or Clock(), notifier=notes.append,
    )
    d.notes = notes
    return d, delivered, recorders


def wait_state(d, *states, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        if d.state in states:
            return d.state
        time.sleep(0.01)
    raise AssertionError(f"state stayed {d.state}")


def test_hold_is_push_to_talk():
    clock = Clock()
    d, delivered, recs = make(clock=clock)
    assert d.press()["state"] == "listening"
    assert recs[0].started and d.level == 0.7
    clock.t += 2.0  # held
    assert d.release()["state"] == "transcribing"
    assert wait_state(d, "done") == "done"
    assert delivered == ["Hello there. General test."]
    assert d.snapshot()["delivered"] == ["copied", "typed"]


def test_tap_toggles():
    clock = Clock()
    d, delivered, _ = make(clock=clock)
    d.press()
    clock.t += 0.1  # quick tap
    assert d.release()["state"] == "listening"  # keeps listening
    clock.t += 3.0
    assert d.press()["state"] == "transcribing"  # second tap stops
    clock.t += 0.1
    assert d.release()["state"] in ("transcribing", "done")  # release after the stop is ignored
    wait_state(d, "done")
    assert len(delivered) == 1


def test_too_short_recording_is_an_error_not_a_transcript():
    clock = Clock()
    d, delivered, _ = make(seconds=0.1, clock=clock)
    d.press()
    clock.t += 1.0
    assert d.release()["state"] == "error"
    assert "Too short" in d.error and delivered == []


def test_cancel_discards_audio():
    d, delivered, recs = make()
    d.press()
    assert d.cancel()["state"] == "idle"
    assert recs[0].stopped and delivered == []


def test_toggle_action():
    d, delivered, _ = make()
    assert d.toggle()["state"] == "listening"
    assert d.toggle()["state"] == "transcribing"
    wait_state(d, "done")
    assert delivered


def test_mic_failure_is_reported():
    def broken(*_):
        raise FileNotFoundError("pw-record")

    d = hk.GlobalDictation(JobManager(FakeEngine()), model=lambda: "small", deliver=lambda t: [], recorder_factory=broken,
                           notifier=lambda m: None)
    assert d.press()["state"] == "error"
    assert "microphone" in d.error


def test_level_curve_matches_the_web_waveform():
    assert hk.level_from_pcm(b"\0\0" * 800) == 0.0
    loud = (8000).to_bytes(2, "little", signed=True) * 800
    assert 0.5 < hk.level_from_pcm(loud) <= 1.0


def test_api_routes(client):
    assert client.get("/api/dictation").json()["state"] == "idle"
    assert client.post("/api/dictation/nope").status_code == 404
    # A web page cannot trigger dictation (it would type into the focused app).
    for origin in ("https://evil.example", "http://localhost:3000", "http://127.0.0.1:8000"):
        res = client.post("/api/dictation/toggle", headers={"Origin": origin})
        assert res.status_code == 403, origin
    assert client.post("/api/dictation/toggle", headers={"Sec-Fetch-Site": "same-origin"}).status_code == 403
    assert client.get("/api/dictation").json()["state"] == "idle"  # none of those started the mic


def test_lua_snippets_are_well_formed():
    lua = hk.bind_lua("SUPER + CTRL + D", hk.CLI)
    assert 'hl.bind("SUPER + CTRL + D"' in lua and "release = true" in lua
    assert lua.count("hl.dsp.exec_cmd(") == 2 and "ow-dictate press" in lua and "ow-dictate release" in lua
    rule = hk.overlay_rule_lua()
    assert "no_focus = true" in rule and "no_initial_focus = true" in rule and "open-whisper-overlay" in rule
    assert "set_enabled(false)" in rule  # a restart replaces the rule instead of stacking it


@pytest.mark.parametrize("key,mask,k", [("SUPER + CTRL + D", 68, "D"), ("SUPER + SHIFT + SPACE", 65, "SPACE"), ("ALT + F9", 8, "F9")])
def test_key_parsing(key, mask, k):
    assert hk._mods_mask(key) == (mask, k)


def test_lua_strings_escape_quotes_and_keep_utf8(tmp_path):
    assert hk.lua_str('a"b\\c') == '"a\\"b\\\\c"'
    assert hk.lua_str("Año/ñ") == '"Año/ñ"'  # Lua has no \uXXXX escape; UTF-8 bytes are fine
    assert hk.lua_str("x\ny") == '"x\\010y"'


class SlowEngine(FakeEngine):
    def __init__(self) -> None:
        super().__init__()
        import threading

        self.gate = threading.Event()

    def transcribe(self, path, language, model, on_update):
        self.gate.wait(5)
        return super().transcribe(path, language, model, on_update)


def test_press_while_transcribing_says_busy():
    engine = SlowEngine()
    d, delivered, _ = make(engine=engine)
    d.toggle()
    d.toggle()
    assert d.state == "transcribing"
    snap = d.press()
    assert snap["state"] == "transcribing" and "Still transcribing" in snap["detail"]
    engine.gate.set()
    wait_state(d, "done")
    assert len(delivered) == 1


def test_cancel_while_transcribing_drops_the_text():
    engine = SlowEngine()
    d, delivered, _ = make(engine=engine)
    d.toggle()
    d.toggle()
    assert d.cancel()["state"] == "idle"
    engine.gate.set()
    time.sleep(0.3)
    assert delivered == [] and d.state == "idle"  # nothing typed into whatever has focus


def test_nothing_delivered_is_an_error_that_keeps_the_text():
    d, delivered, _ = make(deliver_result=())
    d.toggle()
    d.toggle()
    wait_state(d, "error")
    assert "wtype" in d.error and d.text == "Hello there. General test."
    assert d.notes and "wtype" in d.notes[-1]


def test_recorder_failure_and_limit():
    d, delivered, recs = make()
    d.toggle()
    recs[0].on_end("failed: no source")
    assert d.state == "error" and "microphone stopped" in d.error and "no source" in d.error
    d.toggle()
    recs[1].on_end("limit")  # the cap stops and transcribes instead of silently dropping audio
    assert d.state == "transcribing"
    wait_state(d, "done")
    assert len(delivered) == 1


def test_stale_end_event_is_ignored():
    d, _, recs = make()
    d.toggle()
    d.toggle()
    wait_state(d, "done")
    d.toggle()  # a new recording
    recs[0].on_end("failed: old recorder")  # late event from the first one
    assert d.state == "listening"


def test_dictation_jobs_go_ahead_of_uploads():
    order = []

    class Recording(FakeEngine):
        def transcribe(self, path, language, model, on_update):
            order.append(model)
            return super().transcribe(path, language, model, on_update)

    from .conftest import tone_wav

    gate = SlowEngine()
    jm = JobManager(gate)
    files = []
    for _ in range(3):
        p = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
        p.write(tone_wav())
        p.close()
        files.append(p.name)
    jm.submit("busy.wav", files[0], None, "busy")  # occupies the worker
    time.sleep(0.1)
    jm.engine = Recording()
    jm.submit("upload.wav", files[1], None, "upload")
    jm.submit("dictation.wav", files[2], None, "dictation", priority=0)
    gate.gate.set()
    end = time.time() + 5
    while len(order) < 2 and time.time() < end:
        time.sleep(0.02)
    assert order == ["dictation", "upload"]


def test_conf_snippet_for_classic_config():
    text = hk.conf_snippet("SUPER + CTRL + X", hk.CLI)
    assert "bind = SUPER CTRL, X, exec," in text and "bindr = SUPER CTRL, X, exec," in text
