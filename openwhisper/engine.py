"""Speech-to-text engine: model download (with progress), loading and transcription.

faster-whisper (CTranslate2) runs on CUDA float16 when an NVIDIA GPU is usable,
otherwise on CPU with int8 quantization.
"""

from __future__ import annotations

import logging
import threading
import time
from collections.abc import Callable
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from tqdm.auto import tqdm

from . import config

log = logging.getLogger("openwhisper")

UpdateFn = Callable[[dict[str, Any]], None]


class UnsupportedMedia(ValueError):
    """The upload has no decodable audio stream."""


def probe_audio(path: str | Path) -> float:
    """Return the duration in seconds, or raise UnsupportedMedia with a readable message."""
    import av

    try:
        with av.open(str(path), mode="r") as container:
            streams = container.streams.audio
            if not streams:
                raise UnsupportedMedia("This file has no audio track.")
            if container.duration:
                return container.duration / 1_000_000
            s = streams[0]
            if s.duration and s.time_base:
                return float(s.duration * s.time_base)
            return 0.0
    except UnsupportedMedia:
        raise
    except Exception as exc:  # PyAV raises many error types for garbage input
        raise UnsupportedMedia("This file is not an audio or video format that can be decoded.") from exc


@dataclass
class ModelState:
    name: str
    state: str = "missing"  # missing | downloading | downloaded | loading | ready | error
    downloaded: int = 0
    total: int = 0
    error: str | None = None
    updated: float = field(default_factory=time.time)

    def public(self) -> dict[str, Any]:
        d = asdict(self)
        d["progress"] = (self.downloaded / self.total) if self.total else (1.0 if self.state == "ready" else 0.0)
        return d


def detect_device() -> tuple[str, str]:
    try:
        import ctranslate2

        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda", "float16"
    except Exception:  # pragma: no cover - depends on drivers
        pass
    return "cpu", "int8"


class Engine:
    """Owns models. One model stays loaded at a time to bound memory use."""

    def __init__(self) -> None:
        self.device, self.compute_type = detect_device()
        self._lock = threading.RLock()
        self._prepare_locks: dict[str, threading.Lock] = {}
        self._load_lock = threading.Lock()  # one load at a time: two large models at once can exhaust RAM
        self._states: dict[str, ModelState] = {n: ModelState(n) for n in config.MODELS}
        self._loaded_name: str | None = None
        self._loaded: Any = None
        for name in config.MODELS:
            if self._local_path(name, offline=True):
                self._states[name].state = "downloaded"

    # ------------------------------------------------------------ status
    def model_state(self, name: str) -> dict[str, Any]:
        return self._states[name].public()

    def status(self) -> dict[str, Any]:
        return {
            "device": self.device,
            "compute_type": self.compute_type,
            "default_model": config.DEFAULT_MODEL,
            "models": [
                {"name": m.name, "label": m.label, "approx_mb": m.approx_mb, **self.model_state(m.name)}
                for m in config.MODELS.values()
            ],
        }

    # ------------------------------------------------------------ download
    def _local_path(self, name: str, offline: bool) -> str | None:
        from huggingface_hub import snapshot_download

        try:
            path = snapshot_download(
                config.MODELS[name].repo, allow_patterns=list(config.MODEL_FILES), local_files_only=offline
            )
        except Exception:
            return None
        # A snapshot dir exists as soon as the first small file lands; require the weights too.
        ok = all((Path(path) / f).is_file() for f in ("config.json", "model.bin", "tokenizer.json"))
        return path if ok else None

    def _download(self, name: str) -> str:
        from huggingface_hub import HfApi, hf_hub_download

        spec = config.MODELS[name]
        st = self._states[name]
        st.state, st.error, st.downloaded = "downloading", None, 0
        info = HfApi().model_info(spec.repo, files_metadata=True)
        files = [(s.rfilename, s.size or 0) for s in info.siblings if s.rfilename in config.MODEL_FILES]
        st.total = sum(size for _, size in files) or spec.approx_mb * 1024**2
        done_before = 0
        engine_state = st
        file_size = [0]
        written = [0]  # bytes written to disk for the current file
        received = [0]  # bytes received from the network (xet only; arrives smoothly)

        def publish() -> None:
            this_file = min(file_size[0], max(written[0], received[0]))
            # Never step backwards (retries, file switches); never pass the total.
            engine_state.downloaded = max(engine_state.downloaded, min(engine_state.total, done_before + this_file))
            engine_state.updated = time.time()

        class Progress(tqdm):
            """Counts bytes for the UI. Our own counters: the hub may hand us a disabled bar,
            and a disabled tqdm does not advance `self.n`. The launcher prints the terminal line."""

            def __init__(self, *args: Any, **kwargs: Any) -> None:
                kwargs["disable"] = True
                super().__init__(*args, **kwargs)

            # With hf_xet the hub would otherwise open a second "transfer" bar of this class and
            # count every byte twice. With update_transfer it reports both on this one bar.
            # Disk writes come in large flushes, so progress shows the larger of the two counts.
            def update_transfer(self, n: float | None = 1) -> None:
                received[0] += int(n or 0)
                publish()

            def set_transfer_postfix_str(self, *args: Any, **kwargs: Any) -> None:
                pass

            def update(self, n: float | None = 1) -> bool | None:
                written[0] += int(n or 0)
                publish()
                return super().update(n)

        path = None
        for fname, size in files:
            written[0] = received[0] = 0
            file_size[0] = size
            path = hf_hub_download(spec.repo, fname, tqdm_class=Progress)
            done_before += size
            st.downloaded = max(st.downloaded, done_before)
        if path is None:
            raise RuntimeError(f"{spec.repo} has none of the expected model files {config.MODEL_FILES}")
        return str(Path(path).parent)

    # ------------------------------------------------------------ load
    def prepare(self, name: str) -> Any:
        """Download (if needed) and load `name`. Blocking; safe to call from many threads."""
        if name not in config.MODELS:
            raise ValueError(f"Unknown model {name!r}")
        with self._lock:
            plock = self._prepare_locks.setdefault(name, threading.Lock())
        with plock:
            with self._lock:
                if self._loaded_name == name:
                    return self._loaded
            st = self._states[name]
            try:
                path = self._local_path(name, offline=True) or self._download(name)
                st.state = "loading"  # also while waiting for another model's load to finish
                with self._load_lock:
                    with self._lock:
                        if self._loaded_name == name:
                            st.state = "ready"
                            return self._loaded
                    st.state, st.downloaded = "loading", st.total
                    from .fastlang import FastLangWhisperModel as WhisperModel

                    with self._lock:
                        # Free the previous model before loading the next one.
                        if self._loaded_name and self._loaded_name != name:
                            self._states[self._loaded_name].state = "downloaded"
                        self._loaded, self._loaded_name = None, None
                    model = WhisperModel(
                        path, device=self.device, compute_type=self.compute_type, cpu_threads=config.compute_threads()
                    )
                    with self._lock:
                        self._loaded, self._loaded_name = model, name
                    st.state = "ready"
                    return model
            except Exception as exc:
                log.exception("preparing model %s failed", name)
                st.state = "error"
                text = f"{type(exc).__name__} {exc}".lower()
                offline = any(
                    k in text
                    for k in ("connect", "resolv", "resolution", "offline", "temporary failure", "timeout",
                              "network", "localentrynotfound")
                )
                st.error = (
                    "No internet connection: the model must be downloaded once. Connect and restart open-whisper."
                    if offline
                    else f"Model failed to load: {exc}"
                )
                raise

    # ------------------------------------------------------------ transcribe
    def transcribe(self, path: str, language: str | None, model: str, on_update: UpdateFn) -> dict[str, Any]:
        on_update({"state": "preparing", "message": "Preparing model"})
        whisper = self.prepare(model)
        on_update({"state": "transcribing", "progress": 0.0, "message": "Transcribing"})
        segments_iter, info = whisper.transcribe(
            path,
            language=language or None,
            beam_size=5,
            # VAD drops silence (Whisper invents "Thank you." on silent audio). The default 0.5
            # threshold also cut speech in noisy audio; 0.3 measured best (docs/benchmark.md).
            vad_filter=True,
            vad_parameters={"threshold": 0.3},
            condition_on_previous_text=False,  # avoids repetition loops on long audio
        )
        on_update({"language": info.language, "language_probability": round(info.language_probability, 3)})
        duration = info.duration or 0.0
        segments = []
        for seg in segments_iter:
            item = {"start": round(seg.start, 3), "end": round(seg.end, 3), "text": seg.text.strip()}
            segments.append(item)
            on_update({"segment": item, "progress": min(0.99, seg.end / duration) if duration else 0.5})
        return {"language": info.language, "duration": duration, "segments": segments}
