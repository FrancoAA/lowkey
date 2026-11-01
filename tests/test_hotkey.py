"""Global-hotkey dictation: state machine, API, Hyprland Lua snippets. No mic, no desktop."""

from __future__ import annotations

import time

import pytest

from openwhisper import hotkey as hk
from openwhisper.jobs import JobManager

from .conftest import FakeEngine


class FakeRecorder:
    def __init__(self, on_level, seconds: float = 1.0) -> None:
        self.on_level, self.seconds, self.started, self.stopped = on_level, seconds, False, False

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


def make(seconds: float = 1.0, clock: Clock | None = None):
    delivered: list[str] = []
    recorders: list[FakeRecorder] = []

    def factory(on_level):
        r = FakeRecorder(on_level, seconds)
        recorders.append(r)
        return r

    d = hk.GlobalDictation(
        JobManager(FakeEngine()), model=lambda: "large-v3-turbo",
        deliver=lambda text: delivered.append(text) or ["copied", "typed"],
        recorder_factory=factory, clock=clock or Clock(),
    )
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
    def broken(_):
        raise FileNotFoundError("pw-record")

    d = hk.GlobalDictation(JobManager(FakeEngine()), model=lambda: "small", deliver=lambda t: [], recorder_factory=broken)
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
    res = client.post("/api/dictation/toggle", headers={"Origin": "https://evil.example"})
    assert res.status_code == 403


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
