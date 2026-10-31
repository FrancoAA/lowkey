"""Built-in defaults. Normal use needs no configuration at all.

The default model was chosen by benchmarking on the development machine
(see docs/benchmark.md). Users can switch models from the UI ("Options").
"""

from __future__ import annotations

import os
import platform
from pathlib import Path
from dataclasses import dataclass


@dataclass(frozen=True)
class ModelSpec:
    name: str
    repo: str
    label: str
    approx_mb: int


MODELS: dict[str, ModelSpec] = {
    m.name: m
    for m in [
        ModelSpec("small", "Systran/faster-whisper-small", "Small · fastest", 464),
        ModelSpec("medium", "Systran/faster-whisper-medium", "Medium", 1459),
        ModelSpec("large-v3-turbo", "mobiuslabsgmbh/faster-whisper-large-v3-turbo", "Large v3 Turbo · best balance", 1546),
        ModelSpec("large-v3", "Systran/faster-whisper-large-v3", "Large v3 · most accurate, slowest", 2947),
    ]
}

# Benchmark winner on CPU (docs/benchmark.md): WER 4.2% at 4x real time, 1.7 GB RAM.
DEFAULT_MODEL = "large-v3-turbo"

# Files faster-whisper needs; README/.gitattributes are skipped.
MODEL_FILES = ("config.json", "model.bin", "tokenizer.json", "vocabulary.txt", "vocabulary.json", "preprocessor_config.json")

# Upload limit: generous for long recordings, but bounded so a bad drop cannot fill the disk.
MAX_UPLOAD_BYTES = 2 * 1024**3

HOST = "127.0.0.1"
PORT = 8000


def compute_threads() -> int:
    """Physical-core estimate; CTranslate2 scales poorly onto SMT siblings."""
    try:
        cpus = len(os.sched_getaffinity(0))
    except AttributeError:  # not Linux
        cpus = os.cpu_count() or 2
    try:
        smt = Path("/sys/devices/system/cpu/smt/active").read_text().strip() == "1"
    except OSError:
        # No Linux SMT info: x86 desktop CPUs usually have SMT, ARM chips do not.
        smt = platform.machine().lower() in ("x86_64", "amd64", "i686", "x86") and cpus >= 4
    return max(1, cpus // 2 if smt else cpus)
