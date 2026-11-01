"""System-wide dictation: a Hyprland key bind -> mic capture here -> transcript to the
clipboard (wl-copy) and typed into the focused app (wtype), with a small always-on-top
overlay window that shows the live waveform.

Flow: the key bind runs `scripts/ow-dictate press|release`, which POSTs to this server.
Hold the key = push-to-talk; tap it = start, tap again = stop. The server records the mic
itself (PipeWire `pw-record`), so recording starts at the key press and the overlay is only
a display. Nothing here edits Hyprland config files: the bind and the overlay window rule
are added to the running Hyprland with `hyprctl eval` and added again after a config reload.
The bind stays when the app stops, so the key can start the app again (ow-dictate does that,
and the dictation begins when the app is ready); a Hyprland reload or a new login drops it.
"""

from __future__ import annotations

import array
import json
import logging
import math
import os
import shlex
import shutil
import socket
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
ERROR_SHOWN_S = 5.0
DEFAULT_KEY = "SUPER + CTRL + X"  # Omarchy's dictation key (voxtype uses it when installed)
OVERLAY_PATH = "/open-whisper-overlay"
# Chromium on Wayland ignores --class; an --app window's app_id is built from its URL.
OVERLAY_CLASS_RE = "^chrome-.*open-whisper-overlay.*$"
CLI = Path(__file__).resolve().parent.parent / "scripts" / "ow-dictate"
# Binds added by the app say so; the permanent snippet says just "Dictate (open-whisper)".
RUNTIME_DESC = "Dictate (open-whisper, added by the app)"
CONFIG_DESC = "Dictate (open-whisper)"


def runtime_dir() -> Path:
    base = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    return Path(base) / "open-whisper"


def private_runtime_dir() -> Path:
    """runtime_dir(), created 0700; refuses one that another user owns (shared /tmp)."""
    rd = runtime_dir()
    rd.mkdir(mode=0o700, parents=True, exist_ok=True)
    st = rd.lstat()
    if rd.is_symlink() or st.st_uid != os.getuid():
        raise PermissionError(f"{rd} belongs to another user")
    os.chmod(rd, 0o700)
    return rd


def level_from_pcm(chunk: bytes) -> float:
    """0..1 level from s16le samples, same curve as the web waveform."""
    n = len(chunk) // 2
    if not n:
        return 0.0
    a = array.array("h", chunk[: n * 2])
    rms = math.sqrt(sum(x * x for x in a) / n) / 32768
    return min(1.0, max(0.0, (20 * math.log10(rms + 1e-6) + 55) / 45))


# --------------------------------------------------------------------------- mic
class MicRecorder:
    """Raw 16 kHz mono s16 from the default PipeWire source.

    `on_end(reason)` is called if capture ends on its own: "limit" at MAX_RECORDING_S, or
    "failed: <stderr>" when the recorder exits (no PipeWire, no source, mic unplugged).
    OPENWHISPER_FAKE_MIC=<audio file> replaces the mic with that file played in real time
    (looped). It exists for automated tests only.
    """

    def __init__(self, on_level: Callable[[float], None], on_end: Callable[[str], None] | None = None) -> None:
        self.on_level, self.on_end = on_level, on_end
        self.proc: subprocess.Popen | None = None
        self.buf = bytearray()
        self.thread: threading.Thread | None = None
        self.stopping = False

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
        self.stopping = False
        self.proc = subprocess.Popen(self.command(), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.thread = threading.Thread(target=self._read, name="mic", daemon=True)
        self.thread.start()

    def _read(self) -> None:
        proc = self.proc
        assert proc and proc.stdout
        chunk_bytes = RATE // 20 * 2  # 50 ms
        while True:
            chunk = proc.stdout.read(chunk_bytes)
            if not chunk:
                break
            self.buf += chunk
            self.on_level(level_from_pcm(chunk))
            if len(self.buf) >= MAX_RECORDING_S * RATE * 2:
                if self.on_end and not self.stopping:
                    self.on_end("limit")
                return
        if self.on_end and not self.stopping:
            err = (proc.stderr.read() if proc.stderr else b"").decode(errors="replace").strip().splitlines()
            self.on_end("failed: " + (err[-1] if err else f"{self.command()[0]} stopped"))

    def stop(self) -> bytes:
        self.stopping = True
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.thread and self.thread is not threading.current_thread():
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
    """Lua string literal. (json.dumps is not enough: Lua has no \\uXXXX escape.)"""
    out = []
    for ch in s:
        if ch in ('"', "\\"):
            out.append("\\" + ch)
        elif ord(ch) < 32 or ord(ch) == 127:
            out.append(f"\\{ord(ch):03d}")
        else:
            out.append(ch)  # UTF-8 passes through Lua strings as bytes
    return '"' + "".join(out) + '"'


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


def bind_lua(key: str, cli: Path, description: str = CONFIG_DESC) -> str:
    press = lua_str(f"{shlex.quote(str(cli))} press")
    release = lua_str(f"{shlex.quote(str(cli))} release")
    k = lua_str(key)
    return (
        f"hl.bind({k}, hl.dsp.exec_cmd({press}), {{ description = {lua_str(description)} }})\n"
        f"hl.bind({k}, hl.dsp.exec_cmd({release}), {{ release = true }})"
    )


def permanent_snippet(key: str = DEFAULT_KEY, cli: Path = CLI) -> str:
    """What to add to ~/.config/hypr/bindings.lua to keep the hotkey without the app adding it."""
    return "-- open-whisper: global dictation (hold = talk, tap = start/stop)\n" + bind_lua(key, cli) + "\n" + overlay_rule_lua()


def conf_snippet(key: str = DEFAULT_KEY, cli: Path = CLI) -> str:
    """The same binds for a classic hyprland.conf (no overlay: see README)."""
    mods, k = key.rsplit("+", 1) if "+" in key else ("", key)
    mods = " ".join(m.strip() for m in mods.split("+") if m.strip())
    q = shlex.quote(str(cli))
    return (f"# open-whisper: global dictation (hold = talk, tap = start/stop)\n"
            f"bind = {mods}, {k.strip()}, exec, {q} press\n"
            f"bindr = {mods}, {k.strip()}, exec, {q} release")


def _mods_mask(key: str) -> tuple[int, str]:
    masks = {"SHIFT": 1, "CTRL": 4, "CONTROL": 4, "ALT": 8, "SUPER": 64, "WIN": 64, "LOGO": 64}
    parts = [p.strip().upper() for p in key.split("+") if p.strip()]
    return sum(masks.get(p, 0) for p in parts[:-1]), parts[-1]


def key_binds(key: str) -> list[dict]:
    """Keyboard binds on `key` in the default submap."""
    mask, k = _mods_mask(key)
    try:
        binds = json.loads(hyprctl("binds", "-j") or "[]")
    except (json.JSONDecodeError, subprocess.SubprocessError, OSError):
        return []
    return [b for b in binds if b.get("modmask") == mask and str(b.get("key", "")).upper() == k
            and not b.get("mouse") and not b.get("submap")]


class HyprlandHotkey:
    """Adds the bind + overlay window rule to the running Hyprland, again after each config
    reload (a reload drops runtime binds), and removes them on exit."""

    def __init__(self, key: str = DEFAULT_KEY, cli: Path = CLI) -> None:
        self.key, self.cli, self.added, self.rule = key, cli, False, False
        self._watch_stop = threading.Event()

    def install(self, keep_existing: bool = False) -> str:
        """keep_existing: the key just started us, so an app-added bind is known to work;
        leave it alone (re-adding it could drop the release of a key that is still held)."""
        if not lua_config():
            return ("not added: this Hyprland has no Lua config (hyprctl eval). "
                    "Run `uv run python -m openwhisper.hotkey --conf` and add the lines to hyprland.conf.")
        hyprctl("eval", overlay_rule_lua(), check=True)
        self.rule = True
        owners = [b.get("description") or "" for b in key_binds(self.key)]
        if CONFIG_DESC in owners:
            return f"{self.key} (from your Hyprland config)"
        if RUNTIME_DESC in owners and keep_existing:
            self.added = True
            return f"{self.key} — hold to talk, tap to start/stop (also starts open-whisper when it is closed)"
        if RUNTIME_DESC in owners:
            hyprctl("eval", f"hl.unbind({lua_str(self.key)})")  # from an earlier run; may point elsewhere
        elif owners:
            return f"not added: {self.key} is already bound ({owners[0] or 'no description'}). Use --hotkey."
        hyprctl("eval", bind_lua(self.key, self.cli, RUNTIME_DESC), check=True)
        self.added = True
        return f"{self.key} — hold to talk, tap to start/stop (also starts open-whisper when it is closed)"

    def watch_reloads(self) -> None:
        """Re-install after `hyprctl reload` or a config change, using the event socket."""
        sock_path = Path(os.environ.get("XDG_RUNTIME_DIR", "")) / "hypr" / os.environ.get("HYPRLAND_INSTANCE_SIGNATURE", "") / ".socket2.sock"
        if not sock_path.exists():
            return

        def loop() -> None:
            while not self._watch_stop.is_set():
                try:
                    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
                        s.connect(str(sock_path))
                        s.settimeout(1.0)
                        buf = b""
                        while not self._watch_stop.is_set():
                            try:
                                data = s.recv(4096)
                            except TimeoutError:
                                continue
                            if not data:
                                break
                            buf += data
                            *lines, buf = buf.split(b"\n")
                            if any(line.startswith(b"configreloaded>>") for line in lines):
                                time.sleep(0.3)
                                if self.added or self.rule:
                                    log.info("Hyprland config reloaded: adding the dictation key again")
                                    self.added = self.rule = False
                                    self.install(keep_existing=True)
                except OSError:
                    self._watch_stop.wait(2)

        threading.Thread(target=loop, name="hypr-events", daemon=True).start()

    def uninstall(self, keep_bind: bool = True) -> None:
        """On exit the bind stays (it starts the app again); the overlay rule goes."""
        self._watch_stop.set()
        if self.added and not keep_bind:
            hyprctl("eval", f"hl.unbind({lua_str(self.key)})")
            self.added = False
        if self.rule:
            hyprctl("eval", "if _G.open_whisper_rule then _G.open_whisper_rule:set_enabled(false); _G.open_whisper_rule = nil end")
            self.rule = False


def remove_app_binds() -> list[str]:
    """Remove binds that the app added (any key), e.g. for --no-hotkey.

    Hyprland can only unbind a whole key (hl.unbind), and Keybind:remove() crashed Hyprland
    0.56.2, so a key that also has a bind from the user's config is left alone: unbinding it
    would remove the config bind too. That case does not occur after a config reload (which
    drops app-added binds)."""
    try:
        binds = json.loads(hyprctl("binds", "-j") or "[]")
    except (json.JSONDecodeError, subprocess.SubprocessError, OSError):
        return []
    masks = {64: "SUPER", 4: "CTRL", 8: "ALT", 1: "SHIFT"}

    def combo(b: dict) -> tuple[int, str]:
        return b.get("modmask", 0), str(b.get("key", "")).upper()

    config_keys = {combo(b) for b in binds if b.get("description") == CONFIG_DESC}
    removed = []
    for b in binds:
        if b.get("description") == RUNTIME_DESC and combo(b) not in config_keys and not b.get("submap"):
            key = " + ".join([v for m, v in masks.items() if b["modmask"] & m] + [b["key"]])
            hyprctl("eval", f"hl.unbind({lua_str(key)})")
            removed.append(key)
    return removed


class Overlay:
    """Chromium --app window that shows the waveform. Started at app start (hidden), then
    moved between the active workspace and a hidden special workspace, so it shows instantly.
    Only used when our window rule is in place: without it the window could take focus, and
    the text would be typed into the overlay."""

    HIDDEN = "special:open-whisper"

    def __init__(self, url: str) -> None:
        self.url = url
        self.enabled = False
        self.proc: subprocess.Popen | None = None
        self.want_visible = False
        self._lock = threading.Lock()
        self.profile = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "open-whisper" / "overlay-browser"

    @staticmethod
    def browser() -> str | None:
        for name in ("chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "brave"):
            if path := shutil.which(name):
                return path
        return None

    def available(self) -> bool:
        return self.enabled and hyprland_running() and self.browser() is not None

    def _clients(self) -> list[dict]:
        try:
            return json.loads(hyprctl("clients", "-j") or "[]")
        except (json.JSONDecodeError, subprocess.SubprocessError, OSError):
            return []

    def _window(self) -> dict | None:
        return next((c for c in self._clients() if "open-whisper-overlay" in c.get("class", "")), None)

    def start(self) -> None:
        """Launch the browser window (hidden as soon as it maps) unless it already runs."""
        if not self.available() or (self.proc and self.proc.poll() is None) or self._window():
            return
        self.profile.mkdir(parents=True, exist_ok=True)
        self.proc = subprocess.Popen(
            [self.browser(), f"--user-data-dir={self.profile}", "--no-first-run", "--no-default-browser-check",
             "--disable-features=Translate", f"--app={self.url}{OVERLAY_PATH}"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
        )
        threading.Thread(target=self._place_when_mapped, daemon=True).start()

    def _place_when_mapped(self) -> None:
        for _ in range(150):  # up to 15 s for a cold browser start
            if self._window():
                self._apply()
                return
            time.sleep(0.1)

    def show(self) -> None:
        with self._lock:
            self.want_visible = True
        if self._window():
            self._apply()
        else:
            self.start()  # shows itself when mapped (want_visible)

    def hide(self) -> None:
        with self._lock:
            self.want_visible = False
        self._apply()

    def _apply(self) -> None:
        """Move the window to match want_visible. Serialized, and re-reads the wish at the end
        so a show() that lands during a slow hide() wins."""
        with self._lock:
            win = self._window()
            if not win:
                return
            target = lua_str(f"address:{win['address']}")
            hidden = win["workspace"]["name"] == self.HIDDEN
            if self.want_visible and (hidden or not win.get("pinned")):
                ws = json.loads(hyprctl("activeworkspace", "-j") or "{}").get("id")
                if ws is not None:
                    dispatch(f"hl.dsp.window.move({{ workspace = {lua_str(str(ws))}, follow = false, window = {target} }})")
                    if not win.get("pinned"):
                        dispatch(f"hl.dsp.window.pin({{ window = {target} }})")
            elif not self.want_visible and not hidden:
                if win.get("pinned"):
                    dispatch(f"hl.dsp.window.pin({{ window = {target} }})")  # unpin: a pinned window cannot move
                dispatch(f"hl.dsp.window.move({{ workspace = {lua_str(self.HIDDEN)}, follow = false, window = {target} }})")

    def visible(self) -> bool:
        win = self._window()
        return bool(win) and win["workspace"]["name"] != self.HIDDEN

    def close(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()


# --------------------------------------------------------------------------- output
# Two dictations within this many seconds, into the same window, are one flow of text: the
# second is typed with a leading space so "…test." + "Hello" does not become "test.Hello".
JOIN_WINDOW_S = 15.0
# No leading space before text that starts with punctuation or a closing bracket/quote.
NO_SPACE_BEFORE = tuple(".,;:!?)]}…%'\"»”’-")


def joining_space(text: str, now: float, last_at: float | None, same_window: bool) -> str:
    if last_at is None or now - last_at > JOIN_WINDOW_S or not same_window:
        return ""
    if not text or text[0].isspace() or text.startswith(NO_SPACE_BEFORE):
        return ""
    return " "


def focused_window() -> str | None:
    """Address of the focused window on Hyprland (None elsewhere or on error)."""
    if not hyprland_running():
        return None
    try:
        return json.loads(hyprctl("activewindow", "-j") or "{}").get("address")
    except (json.JSONDecodeError, subprocess.SubprocessError, OSError):
        return None


def deliver_text(text: str, typed_prefix: str = "") -> list[str]:
    """Clipboard + type into the focused window. Text goes over stdin, not argv, so other
    users and processes cannot read it in the process list. The clipboard gets the text
    alone; `typed_prefix` (a joining space) is only typed."""
    done = []
    if shutil.which("wl-copy"):
        if subprocess.run(["wl-copy"], input=text.encode(), timeout=5).returncode == 0:
            done.append("copied")
    if shutil.which("wtype"):
        if subprocess.run(["wtype", "-"], input=(typed_prefix + text).encode(), timeout=60).returncode == 0:
            done.append("typed")
    return done


def notify(message: str) -> None:
    if shutil.which("notify-send"):
        subprocess.Popen(["notify-send", "-a", "open-whisper", "open-whisper", message],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# --------------------------------------------------------------------------- state machine
class GlobalDictation:
    """(starting ->) idle -> listening -> transcribing -> done/error -> (overlay hides) ...

    "starting" is used when the key started the app: the overlay shows start-up and model
    download progress, and listening begins as soon as the model is ready.

    All state changes happen under one lock. `epoch` counts recordings, so late events from
    an old recording (its transcript, its hide timer) cannot touch a newer one.
    """

    def __init__(self, jobs: Any, model: Callable[[], str], overlay: Overlay | None = None,
                 deliver: Callable[..., list[str]] = deliver_text,
                 window: Callable[[], str | None] = focused_window,
                 recorder_factory: Callable[..., Any] = MicRecorder, clock: Callable[[], float] = time.monotonic,
                 notifier: Callable[[str], None] = notify) -> None:
        self.jobs, self.model, self.overlay, self.deliver = jobs, model, overlay, deliver
        self.recorder_factory, self.clock, self.notifier = recorder_factory, clock, notifier
        self.window = window
        self.last_typed_at: float | None = None  # end of the last typed dictation
        self.last_typed_window: str | None = None
        self.state = "idle"
        self.level = 0.0
        self.text = ""
        self.error: str | None = None
        self.detail = ""
        self.delivered: list[str] = []
        self.version = 0
        self.epoch = 0
        self.pressed_at = 0.0
        self.started_at = 0.0
        self.recorder: Any = None
        self.job: Any = None
        self._lock = threading.RLock()
        self._hide_timer: threading.Timer | None = None

    # -- public events (from the key bind) --
    def press(self) -> dict[str, Any]:
        with self._lock:
            if self.state == "starting":
                self._set("idle", detail="")  # tapped again before it was ready: never mind
                self.epoch += 1
                self._schedule_hide(0)
                return self.snapshot()
            self.pressed_at = self.clock()
            if self.state == "listening":
                self._stop()  # second tap in toggle mode
            elif self.state == "transcribing":
                self.detail = "Still transcribing the last one. Try again in a moment."
                self.version += 1
            else:
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
            elif self.state != "transcribing":
                self._start()
            return self.snapshot()

    def start_when_ready(self, model_state: Callable[[], dict[str, Any]], poll_s: float = 0.2) -> None:
        """The key started the app: show the overlay now, start listening when the model is
        ready. The key press happened about now, so a later release still counts as a hold."""
        with self._lock:
            if self.state != "idle":
                return  # a press reached us first and already started a dictation
            self.epoch += 1
            epoch = self.epoch
            self.pressed_at = self.clock()
            self._set("starting", detail="Starting open-whisper…", text="", error=None, delivered=[])
        if self.overlay:
            threading.Thread(target=self.overlay.show, daemon=True).start()

        def wait() -> None:
            last = None
            while True:
                ms = model_state()
                with self._lock:
                    if epoch != self.epoch or self.state != "starting":
                        return  # cancelled by a second tap or Esc
                    if ms["state"] == "ready":
                        self._start(keep_press=True)
                        return
                    if ms["state"] == "error":
                        self._fail(ms.get("error") or "The model could not be loaded.")
                        return
                    detail = (f"Downloading the model (first run)… {int(ms['progress'] * 100)}%"
                              if ms["state"] == "downloading" else "Loading the model…")
                    if detail != last:
                        last = detail
                        self.detail = detail
                        self.version += 1
                time.sleep(poll_s)

        threading.Thread(target=wait, name="dictate-on-start", daemon=True).start()

    def cancel(self) -> dict[str, Any]:
        with self._lock:
            if self.state == "listening" and self.recorder:
                self.recorder.stop()
                self.recorder = None
            if self.state in ("starting", "listening", "transcribing"):
                self.epoch += 1  # a running transcript is dropped, not typed
                self._set("idle", detail="")
                self._schedule_hide(0)
            return self.snapshot()

    def snapshot(self) -> dict[str, Any]:
        return {"state": self.state, "level": round(self.level, 3), "text": self.text, "error": self.error,
                "detail": self.detail, "delivered": self.delivered, "version": self.version,
                "elapsed": round(self.clock() - self.started_at, 1) if self.state == "listening" else 0}

    # -- internals (call with the lock held) --
    def _set(self, state: str, **fields: Any) -> None:
        self.state = state
        for k, v in fields.items():
            setattr(self, k, v)
        self.version += 1

    def _on_level(self, level: float) -> None:
        self.level = level
        self.version += 1

    def _on_end(self, epoch: int, reason: str) -> None:
        with self._lock:
            if epoch != self.epoch or self.state != "listening":
                return
            if reason == "limit":
                self._stop()
            else:
                self.recorder = None
                self._fail(f"The microphone stopped: {reason.removeprefix('failed: ')}")

    def _start(self, keep_press: bool = False) -> None:
        if self._hide_timer:
            self._hide_timer.cancel()
        if not keep_press:
            self.pressed_at = self.clock()
        self.epoch += 1
        epoch = self.epoch
        self.started_at = self.clock()
        self._set("listening", text="", error=None, detail="", delivered=[], level=0.0)
        try:
            self.recorder = self.recorder_factory(self._on_level, lambda reason: self._on_end(epoch, reason))
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
        self._set("transcribing", level=0.0, detail="")
        threading.Thread(target=self._transcribe, args=(pcm, self.epoch), daemon=True).start()

    def _transcribe(self, pcm: bytes, epoch: int) -> None:
        path = write_wav(pcm)
        try:
            job = self.jobs.submit("dictation.wav", path, None, self.model(), priority=0)
        except Exception as exc:  # queue full
            os.unlink(path)
            with self._lock:
                if epoch == self.epoch:
                    self._fail(str(exc))
            return
        last = None
        with job.cond:
            while job.state not in ("done", "error"):
                # Show why it waits: another job ahead, or the first-run model download.
                msg = ""
                if job.state == "queued":
                    msg = "Waiting for another transcription…"
                elif job.state == "preparing":
                    ms = self.jobs.engine.model_state(job.model)
                    msg = (f"Downloading the model (first run)… {int(ms['progress'] * 100)}%"
                           if ms["state"] == "downloading" else "Loading the model…")
                if msg != last:
                    last = msg
                    with self._lock:
                        if epoch == self.epoch and self.state == "transcribing":
                            self.detail = msg or ""
                            self.version += 1
                job.cond.wait(0.5)
        with self._lock:
            if epoch != self.epoch:
                return  # cancelled or superseded
            if job.state == "error" or not job.result:
                self._fail(job.error or "Transcription failed.")
                return
            text = job.result["text"].strip()
            if not text:
                self._fail("No speech was heard.")
                return
        win = self.window()
        same_window = win is None or win == self.last_typed_window  # unknown window: time alone decides
        prefix = joining_space(text, self.clock(), self.last_typed_at, same_window)
        delivered = self.deliver(text, prefix)
        with self._lock:
            if "typed" in delivered:
                self.last_typed_at, self.last_typed_window = self.clock(), win
            if epoch != self.epoch:
                return
            if not delivered:
                self._fail("Could not type or copy the text (install wtype and wl-copy). Text: " + text, text=text)
                return
            self._set("done", text=text, delivered=delivered, detail="")
            self._schedule_hide(RESULT_SHOWN_S)

    def _fail(self, message: str, text: str = "") -> None:
        self._set("error", error=message, level=0.0, text=text, detail="")
        self.notifier(message)
        self._schedule_hide(ERROR_SHOWN_S)

    def _schedule_hide(self, delay: float) -> None:
        if not self.overlay:
            return
        if self._hide_timer:
            self._hide_timer.cancel()
        epoch = self.epoch

        def hide() -> None:
            with self._lock:
                if epoch != self.epoch or self.state in ("starting", "listening", "transcribing"):
                    return  # a newer dictation owns the overlay now
                self.overlay.hide()

        self._hide_timer = threading.Timer(delay, hide)
        self._hide_timer.daemon = True
        self._hide_timer.start()


if __name__ == "__main__":
    import sys

    # `python -m openwhisper.hotkey` prints the snippet for ~/.config/hypr/bindings.lua;
    # `--conf` prints the binds for a classic hyprland.conf; `--remove` removes the key that
    # the app added to the running Hyprland.
    if "--remove" in sys.argv:
        print("removed: " + (", ".join(remove_app_binds()) or "nothing"))
    else:
        print(conf_snippet() if "--conf" in sys.argv else permanent_snippet())
