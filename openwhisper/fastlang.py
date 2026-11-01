"""faster-whisper model that runs the encoder once for auto-detected language.

With `language=None`, faster-whisper encodes the first 30 s window to detect the language,
then encodes the same window again to transcribe it. On a CPU the encoder is most of the work
for a short clip, so a dictation took ~2x longer with auto-detect.

This subclass (1) detects the language from exactly the window that transcription encodes
first, and (2) keeps the last encoder output, so the second encode of that window is a cache
hit. The result is the same; only the duplicate encoder pass goes away.
"""

from __future__ import annotations

import hashlib
import threading
from typing import Any

import numpy as np
from faster_whisper import WhisperModel
from faster_whisper.audio import pad_or_trim


class FastLangWhisperModel(WhisperModel):
    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._enc_lock = threading.Lock()
        self._enc_cache: tuple[tuple, Any] | None = None
        self.encode_calls = 0  # real encoder runs (for tests and benchmarks)

    @staticmethod
    def _key(features: np.ndarray) -> tuple:
        arr = np.ascontiguousarray(features)
        return arr.shape, arr.dtype.str, hashlib.blake2b(arr.tobytes(), digest_size=16).digest()

    def encode(self, features: np.ndarray):  # type: ignore[override]
        key = self._key(features)
        with self._enc_lock:
            if self._enc_cache and self._enc_cache[0] == key:
                return self._enc_cache[1]
        out = super().encode(features)
        self.encode_calls += 1
        with self._enc_lock:
            self._enc_cache = (key, out)
        return out

    def detect_language(  # type: ignore[override]
        self,
        audio: np.ndarray | None = None,
        features: np.ndarray | None = None,
        vad_filter: bool = False,
        vad_parameters: Any = None,
        language_detection_segments: int = 1,
        language_detection_threshold: float = 0.5,
    ):
        if features is None or language_detection_segments != 1 or features.ndim != 2:
            return super().detect_language(
                audio, features, vad_filter, vad_parameters, language_detection_segments, language_detection_threshold
            )
        # Same slice as the first window in generate_segments(): it uses shape[-1] - 1 frames.
        content_frames = features.shape[-1] - 1
        window = pad_or_trim(features[:, : min(self.feature_extractor.nb_max_frames, content_frames)])
        encoder_output = self.encode(window)
        results = self.model.detect_language(encoder_output)[0]
        all_language_probs = [(token[2:-2], prob) for (token, prob) in results]
        language, language_probability = all_language_probs[0]
        return language, language_probability, all_language_probs
