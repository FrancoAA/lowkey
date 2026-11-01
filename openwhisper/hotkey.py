"""System-wide dictation: a Hyprland key bind -> mic capture here -> transcript to the
clipboard (wl-copy) and typed into the focused app (wtype), with a small always-on-top
overlay window that shows the live waveform.

Flow: the key bind runs `scripts/ow-dictate press|release`, which POSTs to this server.
Hold the key = push-to-talk; tap it = start, tap again = stop. The server records the mic
itself (PipeWire `pw-record`), so recording starts at the key press and the overlay is only
a display. Nothing here edits Hyprland config files: the bind and the overlay window rule
are added to the running Hyprland with `hyprctl eval` and removed on exit.
"""

from __future__ import annotations

import json
import logging
import math
import os
import shlex
import shutil
import subprocess
import tempfile
import threading
import time
import wave
from collections.abc import Callable
from pathlib import Path
from typing import Any

log = logging.getLogger("openwhisper")

RATE = 16000
HOLD_S = 0.35  # a press longer than this is push-to-talk; shorter is tap-to-toggle
MIN_RECORDING_S = 0.4
MAX_RECORDING_S = 10 * 60
RESULT_SHOWN_S = 2.0
DEFAULT_KEY = "SUPER + CTRL + X"  # Omarchy's dictation key (voxtype uses it when installed)
OVERLAY_PATH = "/open-whisper-overlay"
# Chromium on Wayland ignores --class; an --app window's app_id is built from its URL.
OVERLAY_CLASS_RE = "^chrome-.*open-whisper-overlay.*$"
CLI = Path(__file__).resolve().parent.parent / "scripts" / "ow-dictate"


def runtime_dir() -> Path:
    base = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    return Path(base) / "open-whisper"


def level_from_pcm(chunk: bytes) -> float:
    """0..1 level from s16le samples, same curve as the web waveform."""
    n = len(chunk) // 2
    if not n:
        return 0.0
    import array

    a = array.array("h", chunk[: n * 2])
    rms = math.sqrt(sum(x * x for x in a) / n) / 32768
    return min(1.0, max(0.0, (20 * math.log10(rms + 1e-6) + 55) / 45))


# --------------------------------------------------------------------------- mic
class MicRecorder:
    """Raw 16 kHz mono s16 from the default PipeWire source.

    OPENWHISPER_FAKE_MIC=<audio file> replaces the mic with that file played in real time
    (looped). It exists for automated tests only.
    """

    def __init__(self, on_level: Callable[[float], None]) -> None:
        self.on_level = on_level
        self.proc: subprocess.Popen | None = None
        self.buf = bytearray()
        self.thread: threading.Thread | None = None

    @staticmethod
    def command() -> list[str]:
        fake = os.environ.get("OPENWHISPER_FAKE_MIC")
        if fake:
            return ["ffmpeg", "-loglevel", "error", "-re", "-stream_loop", "-1", "-i", fake,
                    "-f", "s16le", "-ac", "1", "-ar", str(RATE), "-"]
        return ["pw-record", "--raw", "--rate", str(RATE), "--channels", "1", "--format", "s16", "--latency", "50ms", "-"]

    @staticmethod
    def available() -> bool:
        return shutil.which(MicRecorder.command()[0]) is not None

    def start(self) -> None:
        self.buf = bytearray()
        self.proc = subprocess.Popen(self.command(), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self.thread = threading.Thread(target=self._read, name="mic", daemon=True)
        self.thread.start()

    def _read(self) -> None:
        assert self.proc and self.proc.stdout
        chunk_bytes = RATE // 20 * 2  # 50 ms
        while True:
            chunk = self.proc.stdout.read(chunk_bytes)
            if not chunk:
                return
            self.buf += chunk
            self.on_level(level_from_pcm(chunk))
            if len(self.buf) > MAX_RECORDING_S * RATE * 2:
                return

    def stop(self) -> bytes:
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.thread:
            self.thread.join(2)
        self.proc = None
        return bytes(self.buf)


def write_wav(pcm: bytes) -> str:
    fd, path = tempfile.mkstemp(prefix="openwhisper-", suffix=".wav")
    with os.fdopen(fd, "wb") as f, wave.open(f, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(pcm)
    return path


# --------------------------------------------------------------------------- hyprland
def hyprland_running() -> bool:
    return bool(os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")) and shutil.which("hyprctl") is not None


def hyprctl(*args: str, check: bool = False) -> str:
    out = subprocess.run(["hyprctl", *args], capture_output=True, text=True, timeout=5)
    if check and (out.returncode or out.stdout.strip() not in ("ok", "")):
        raise RuntimeError(f"hyprctl {args[0]}: {out.stdout.strip() or out.stderr.strip()}")
    return out.stdout


def dispatch(lua_dispatcher: str) -> str:
    """Hyprland 0.56 Lua dispatch, e.g. 'hl.dsp.window.pin({ window = "address:0x1" })'."""
    return hyprctl("dispatch", lua_dispatcher)


def lua_config() -> bool:
    """True when the running Hyprland takes Lua (`hyprctl eval`), as Hyprland 0.56 with a .lua config does."""
    try:
        return hyprctl("eval", "return true").strip() == "ok"
    except (subprocess.SubprocessError, OSError):
        return False


def lua_str(s: str) -> str:
    return json.dumps(s)  # a JSON string is a valid Lua string literal for our inputs


def overlay_rule_lua() -> str:
    """Floating, pinned, never focused, bottom-centre. Kept in a Lua global so a restart replaces it."""
    return (
        "if _G.open_whisper_rule then _G.open_whisper_rule:set_enabled(false) end\n"
        "_G.open_whisper_rule = hl.window_rule({ name = \"open-whisper-overlay\", "
        f"match = {{ class = {lua_str(OVERLAY_CLASS_RE)} }}, "
        "float = true, pin = true, no_initial_focus = true, no_focus = true, "
        "size = { 380, 76 }, move = { \"(monitor_w/2-190)\", \"(monitor_h-150)\" }, "
        "border_size = 0, no_shadow = true, no_anim = true })"
    )


def bind_lua(key: str, cli: Path) -> str:
    press = lua_str(f"{shlex.quote(str(cli))} press")
    release = lua_str(f"{shlex.quote(str(cli))} release")
    k = lua_str(key)
    return (
        f"hl.bind({k}, hl.dsp.exec_cmd({press}), {{ description = \"Dictate (open-whisper)\" }})\n"
        f"hl.bind({k}, hl.dsp.exec_cmd({release}), {{ release = true }})"
    )


def permanent_snippet(key: str = DEFAULT_KEY, cli: Path = CLI) -> str:
    """What to add to ~/.config/hypr/bindings.lua to keep the hotkey without the app adding it."""
    return "-- open-whisper: global dictation (hold = talk, tap = start/stop)\n" + bind_lua(key, cli) + "\n" + overlay_rule_lua()


def _mods_mask(key: str) -> tuple[int, str]:
    masks = {"SHIFT": 1, "CTRL": 4, "CONTROL": 4, "ALT": 8, "SUPER": 64, "WIN": 64, "LOGO": 64}
    parts = [p.strip().upper() for p in key.split("+") if p.strip()]
    return sum(masks.get(p, 0) for p in parts[:-1]), parts[-1]


def bind_owner(key: str) -> str | None:
    """Description of the existing bind on `key`, '' if bound without one, None if free."""
    mask, k = _mods_mask(key)
    try:
        binds = json.loads(hyprctl("binds", "-j") or "[]")
    except (json.JSONDecodeError, subprocess.SubprocessError, OSError):
        return None
    for b in binds:
        if b.get("modmask") == mask and str(b.get("key", "")).upper() == k:
            return b.get("description") or ""
    return None


class HyprlandHotkey:
    """Adds the bind + overlay window rule to the running Hyprland; removes the bind on exit."""

    def __init__(self, key: str = DEFAULT_KEY, cli: Path = CLI) -> None:
        self.key, self.cli, self.added = key, cli, False

    def install(self) -> str:
        if not lua_config():
            return "not added: this Hyprland has no Lua config (hyprctl eval). Add the README snippet instead."
        owner = bind_owner(self.key)
        hyprctl("eval", overlay_rule_lua(), check=True)
        if owner is not None:
            if "open-whisper" in owner:
                return f"{self.key} (from your Hyprland config)"
            return f"not added: {self.key} is already bound ({owner or 'no description'}). Use --hotkey."
        hyprctl("eval", bind_lua(self.key, self.cli), check=True)
        self.added = True
        return f"{self.key} — hold to talk, tap to start/stop (active while open-whisper runs)"

    def uninstall(self) -> None:
        if self.added:
            hyprctl("eval", f"hl.unbind({lua_str(self.key)})")
            self.added = False


class Overlay:
    """Chromium --app window that shows the waveform. Started once, then hidden on a special
    workspace between uses so it appears instantly."""

    HIDDEN = "special:open-whisper"

    def __init__(self, url: str) -> None:
        self.url = url
        self.proc: subprocess.Popen | None = None
        self.profile = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "open-whisper" / "overlay-browser"

    @staticmethod
    def browser() -> str | None:
        for name in ("chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "brave"):
            if path := shutil.which(name):
                return path
        return None

    def _address(self) -> str | None:
        try:
            for c in json.loads(hyprctl("clients", "-j") or "[]"):
                if "open-whisper-overlay" in c.get("class", ""):
                    return c["address"]
        except (json.JSONDecodeError, subprocess.SubprocessError, OSError, KeyError):
            pass
        return None

    def show(self) -> None:
        if not hyprland_running():
            return
        addr = self._address()
        if not addr:
            exe = self.browser()
            if not exe or (self.proc and self.proc.poll() is None):
                return  # no browser, or still starting
            self.profile.mkdir(parents=True, exist_ok=True)
            self.proc = subprocess.Popen(
                [exe, f"--user-data-dir={self.profile}", "--no-first-run", "--no-default-browser-check",
                 "--disable-features=Translate", f"--app={self.url}{OVERLAY_PATH}"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
            )
            return  # the window rule places it; it is pinned, so it shows on every workspace
        ws = json.loads(hyprctl("activeworkspace", "-j") or "{}").get("id")
        if ws is not None:
            target = lua_str(f"address:{addr}")
            dispatch(f"hl.dsp.window.move({{ workspace = {lua_str(str(ws))}, follow = false, window = {target} }})")
            if not self._pinned(addr):
                dispatch(f"hl.dsp.window.pin({{ window = {target} }})")

    def _pinned(self, addr: str) -> bool:
        try:
            return any(c["address"] == addr and c.get("pinned") for c in json.loads(hyprctl("clients", "-j")))
        except (json.JSONDecodeError, subprocess.SubprocessError, OSError, KeyError):
            return False

    def hide(self) -> None:
        if not hyprland_running():
            return
        addr = self._address()
        if addr:
            target = lua_str(f"address:{addr}")
            if self._pinned(addr):
                dispatch(f"hl.dsp.window.pin({{ window = {target} }})")  # unpin: a pinned window cannot move
            dispatch(f"hl.dsp.window.move({{ workspace = {lua_str(self.HIDDEN)}, follow = false, window = {target} }})")

    def close(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()


# --------------------------------------------------------------------------- output
def deliver_text(text: str) -> list[str]:
    """Clipboard + type into the focused window. Returns what worked, for the overlay."""
    done = []
    if shutil.which("wl-copy"):
        if subprocess.run(["wl-copy", "--", text], timeout=5).returncode == 0:
            done.append("copied")
    if shutil.which("wtype"):
        if subprocess.run(["wtype", "--", text], timeout=60).returncode == 0:
            done.append("typed")
    return done


def notify(message: str) -> None:
    if shutil.which("notify-send"):
        subprocess.Popen(["notify-send", "-a", "open-whisper", "open-whisper", message],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# --------------------------------------------------------------------------- state machine
class GlobalDictation:
    """idle -> listening -> transcribing -> (result shown) -> idle."""

    def __init__(self, jobs: Any, model: Callable[[], str], overlay: Overlay | None = None,
                 deliver: Callable[[str], list[str]] = deliver_text,
                 recorder_factory: Callable[..., Any] = MicRecorder, clock: Callable[[], float] = time.monotonic) -> None:
        self.jobs, self.model, self.overlay, self.deliver = jobs, model, overlay, deliver
        self.recorder_factory, self.clock = recorder_factory, clock
        self.state = "idle"
        self.level = 0.0
        self.text = ""
        self.error: str | None = None
        self.delivered: list[str] = []
        self.version = 0
        self.pressed_at = 0.0
        self.started_at = 0.0
        self.recorder: Any = None
        self._lock = threading.RLock()
        self._hide_timer: threading.Timer | None = None

    # -- public events (from the key bind) --
    def press(self) -> dict[str, Any]:
        with self._lock:
            self.pressed_at = self.clock()
            if self.state == "listening":
                self._stop()  # second tap in toggle mode
            elif self.state in ("idle", "done", "error"):
                self._start()
            return self.snapshot()

    def release(self) -> dict[str, Any]:
        with self._lock:
            # A long press is push-to-talk. A short one (tap) leaves it listening until the next press.
            if self.state == "listening" and self.clock() - self.pressed_at >= HOLD_S:
                self._stop()
            return self.snapshot()

    def toggle(self) -> dict[str, Any]:
        with self._lock:
            if self.state == "listening":
                self._stop()
            elif self.state in ("idle", "done", "error"):
                self._start()
            return self.snapshot()

    def cancel(self) -> dict[str, Any]:
        with self._lock:
            if self.state == "listening" and self.recorder:
                self.recorder.stop()
                self.recorder = None
                self._set("idle")
                self._schedule_hide(0)
            return self.snapshot()

    def snapshot(self) -> dict[str, Any]:
        return {"state": self.state, "level": round(self.level, 3), "text": self.text, "error": self.error,
                "delivered": self.delivered, "version": self.version,
                "elapsed": round(self.clock() - self.started_at, 1) if self.state == "listening" else 0}

    # -- internals --
    def _set(self, state: str, **fields: Any) -> None:
        self.state = state
        for k, v in fields.items():
            setattr(self, k, v)
        self.version += 1

    def _on_level(self, level: float) -> None:
        self.level = level
        self.version += 1

    def _start(self) -> None:
        if self._hide_timer:
            self._hide_timer.cancel()
        self.started_at = self.clock()
        self._set("listening", text="", error=None, delivered=[], level=0.0)
        try:
            self.recorder = self.recorder_factory(self._on_level)
            self.recorder.start()
        except OSError as exc:
            self.recorder = None
            self._fail(f"Could not open the microphone: {exc}")
            return
        if self.overlay:
            threading.Thread(target=self.overlay.show, daemon=True).start()

    def _stop(self) -> None:
        recorder, self.recorder = self.recorder, None
        pcm = recorder.stop() if recorder else b""
        if len(pcm) < MIN_RECORDING_S * RATE * 2:
            self._fail("Too short. Hold the key while you talk, or tap it once to start and again to stop.")
            return
        self._set("transcribing", level=0.0)
        threading.Thread(target=self._transcribe, args=(pcm,), daemon=True).start()

    def _transcribe(self, pcm: bytes) -> None:
        path = write_wav(pcm)
        try:
            job = self.jobs.submit("dictation.wav", path, None, self.model())
        except Exception as exc:  # queue full
            os.unlink(path)
            self._fail(str(exc))
            return
        with job.cond:
            while job.state not in ("done", "error"):
                job.cond.wait(1)
        if job.state == "error" or not job.result:
            self._fail(job.error or "Transcription failed.")
            return
        text = job.result["text"].strip()
        if not text:
            self._fail("No speech was heard.")
            return
        delivered = self.deliver(text)
        with self._lock:
            self._set("done", text=text, delivered=delivered)
        self._schedule_hide(RESULT_SHOWN_S)

    def _fail(self, message: str) -> None:
        with self._lock:
            self._set("error", error=message, level=0.0)
        if not (self.overlay and Overlay.browser() and hyprland_running()):
            notify(message)  # no overlay to show it
        self._schedule_hide(RESULT_SHOWN_S)

    def _schedule_hide(self, delay: float) -> None:
        if not self.overlay:
            return
        if self._hide_timer:
            self._hide_timer.cancel()

        def hide() -> None:
            with self._lock:
                if self.state in ("listening", "transcribing"):
                    return
            self.overlay.hide()

        self._hide_timer = threading.Timer(delay, hide)
        self._hide_timer.daemon = True
        self._hide_timer.start()


if __name__ == "__main__":
    # `python -m openwhisper.hotkey` prints the snippet for ~/.config/hypr/bindings.lua.
    print(permanent_snippet())
