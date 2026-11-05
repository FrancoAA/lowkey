# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["faster-whisper>=1.2.1", "jiwer>=4.0.0"]
# ///
"""Benchmark open-source speech-to-text candidates on this machine.

Measures, per (engine, model):
  - load time, transcription time, real-time factor (RTF = compute seconds / audio seconds)
  - peak RSS of the worker process (each candidate runs in its own fresh process)
  - WER against the known reference text, plus whether auto language detection was right

Engines:
  - faster-whisper (CTranslate2), CPU int8 (or CUDA float16 if available)
  - whisper.cpp (ggml), CPU build at bench/.work/whisper.cpp (optional; skipped if not built)

All runs use language auto-detection (the app default), beam size 5, no VAD.

Usage:
  uv run bench/make_samples.py      # once
  uv run bench/benchmark.py         # all candidates
  uv run bench/benchmark.py --only fw:small fw:large-v3-turbo

A research tool, not part of the app (which uses only whisper.cpp). The faster-whisper rows are
kept to show how the default was chosen (docs/benchmark.md).
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
import time
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent
WORK = ROOT / ".work"
SAMPLES = WORK / "samples"
# OW_WCPP_BIN / OW_WCPP_LABEL: benchmark another whisper.cpp build, e.g. a Vulkan one:
#   OW_WCPP_BIN=bench/.work/whisper.cpp/build-vk/bin/whisper-cli OW_WCPP_LABEL=wcpp-vulkan
WCPP = Path(os.environ.get("OW_WCPP_BIN") or WORK / "whisper.cpp" / "build" / "bin" / "whisper-cli").resolve()
WCPP_LABEL = os.environ.get("OW_WCPP_LABEL", "wcpp")
GGML = WORK / "ggml"
THREADS = max(1, (os.cpu_count() or 2) // 2)  # physical cores on SMT machines
BEAM = 5

CANDIDATES = [
    "fw:small",
    "fw:medium",
    "fw:large-v3-turbo",
    "fw:distil-large-v3",
    "fw:large-v3",
    "wcpp:small",
    "wcpp:medium",
    "wcpp:large-v3-turbo",
    "wcpp:large-v3-turbo-q5_0",
]


# ---------------------------------------------------------------- scoring
def normalize(text: str) -> str:
    text = unicodedata.normalize("NFKC", text).lower()
    text = re.sub(r"[^\w\s']", " ", text)
    text = text.replace("'", "")
    return re.sub(r"\s+", " ", text).strip()


def word_errors(ref: str, hyp: str) -> tuple[int, int]:
    import jiwer

    r, h = normalize(ref), normalize(hyp)
    out = jiwer.process_words(r, h or "<empty>")
    return out.substitutions + out.deletions + out.insertions, len(r.split())


# ---------------------------------------------------------------- workers
def worker_fw(model: str, samples: list[str]) -> None:
    import ctranslate2
    from faster_whisper import WhisperModel

    cuda = ctranslate2.get_cuda_device_count() > 0
    t0 = time.perf_counter()
    m = WhisperModel(
        model,
        device="cuda" if cuda else "cpu",
        compute_type="float16" if cuda else "int8",
        cpu_threads=THREADS,
    )
    load = time.perf_counter() - t0
    # Warm-up so the first timed sample does not pay one-off init costs.
    list(m.transcribe(str(SAMPLES / "en_jfk.wav"), beam_size=1)[0])
    for s in samples:
        t0 = time.perf_counter()
        segments, info = m.transcribe(str(SAMPLES / f"{s}.wav"), beam_size=BEAM, vad_filter=False)
        text = " ".join(seg.text.strip() for seg in segments)
        dt = time.perf_counter() - t0
        print(json.dumps({"sample": s, "load_s": load, "compute_s": dt, "text": text, "lang": info.language}), flush=True)


def run_wcpp(model: str, sample: str) -> dict:
    """One whisper-cli process per sample; timings come from whisper.cpp's own report."""
    with tempfile.TemporaryDirectory() as td:
        prefix = Path(td) / "out"
        cmd = [
            str(WCPP), "-m", str(GGML / f"ggml-{model}.bin"), "-f", str(SAMPLES / f"{sample}.wav"),
            "-t", str(THREADS), "-bs", str(BEAM), "-l", "auto", "-oj", "-of", str(prefix), "-nt",
        ]
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        out, err = proc.communicate()
        if proc.returncode:
            raise RuntimeError(err[-2000:])
        data = json.loads((prefix.with_suffix(".json")).read_text())
    text = " ".join(x["text"].strip() for x in data["transcription"])
    lang = data.get("result", {}).get("language", "?")
    load = float(re.search(r"load time =\s*([\d.]+) ms", err).group(1)) / 1000
    total = float(re.search(r"total time =\s*([\d.]+) ms", err).group(1)) / 1000
    return {"sample": sample, "load_s": load, "compute_s": total - load, "text": text, "lang": lang}


# ---------------------------------------------------------------- driver
def measure(cand: str, samples: list[str]) -> dict:
    engine, model = cand.split(":", 1)
    rows, peak_kb = [], 0
    if engine == "fw":
        proc = subprocess.Popen(
            [sys.executable, __file__, "--worker", model, *samples],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        out = proc.stdout.read()
        err = proc.stderr.read()
        _, status, ru = os.wait4(proc.pid, 0)
        if status != 0:
            raise RuntimeError(err[-2000:])
        peak_kb = ru.ru_maxrss
        rows = [json.loads(line) for line in out.splitlines() if line.startswith("{")]
    else:
        for s in samples:
            # Measure RSS by wrapping in a child we can wait4() on.
            proc = subprocess.Popen(
                [sys.executable, __file__, "--wcpp-worker", model, s],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            out = proc.stdout.read()
            err = proc.stderr.read()
            _, status, ru = os.wait4(proc.pid, 0)
            if status != 0:
                raise RuntimeError(err[-2000:])
            peak_kb = max(peak_kb, ru.ru_maxrss)
            rows.append(json.loads(out.strip().splitlines()[-1]))
    if engine == "wcpp":
        cand = f"{WCPP_LABEL}:{model}"
    return {"candidate": cand, "peak_rss_mb": peak_kb / 1024, "rows": rows}


def cpu_name() -> str:
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or platform.machine()


def summarize(results: list[dict], refs: dict) -> str:
    lines = [
        f"Machine: {cpu_name()}, {os.cpu_count()} threads, "
        f"{THREADS} compute threads, beam {BEAM}, auto language detection, no VAD.",
        "",
        "| Candidate | Load s | RTF (lower=faster) | x real-time | Peak RAM MB | WER EN clean | WER EN real (JFK) | WER ES clean | WER EN noisy | WER ES noisy | WER all | Lang-ID ok |",
        "|---|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for r in results:
        by = {row["sample"]: row for row in r["rows"]}
        audio = sum(refs[s]["seconds"] for s in by)
        compute = sum(row["compute_s"] for row in by.values())
        rtf = compute / audio

        def wer(keys: list[str]) -> str:
            e = n = 0
            for k in keys:
                if k in by:
                    de, dn = word_errors(refs[k]["text"], by[k]["text"])
                    e, n = e + de, n + dn
            return f"{100 * e / n:.1f}%" if n else "-"

        lang_ok = sum(by[k]["lang"] == refs[k]["lang"] for k in by)
        lines.append(
            f"| {r['candidate']} | {by[next(iter(by))]['load_s']:.1f} | {rtf:.3f} | {1 / rtf:.1f}x | "
            f"{r['peak_rss_mb']:.0f} | {wer(['en_tts'])} | {wer(['en_jfk'])} | {wer(['es_tts'])} | "
            f"{wer(['en_tts_noisy'])} | {wer(['es_tts_noisy'])} | {wer(list(by))} | {lang_ok}/{len(by)} |"
        )
    return "\n".join(lines)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--worker", nargs="+", help=argparse.SUPPRESS)
    ap.add_argument("--wcpp-worker", nargs=2, help=argparse.SUPPRESS)
    ap.add_argument("--only", nargs="+", default=None, help="subset of candidates, e.g. fw:small")
    args = ap.parse_args()

    if args.worker:
        return worker_fw(args.worker[0], args.worker[1:])
    if args.wcpp_worker:
        print(json.dumps(run_wcpp(*args.wcpp_worker)))
        return

    refs = json.loads((SAMPLES / "references.json").read_text())
    samples = list(refs)
    results = []
    for cand in args.only or CANDIDATES:
        engine, model = cand.split(":", 1)
        if engine == "wcpp" and not (WCPP.exists() and (GGML / f"ggml-{model}.bin").exists()):
            print(f"skip {cand}: whisper.cpp binary or model missing", file=sys.stderr)
            continue
        print(f"running {cand} ...", file=sys.stderr, flush=True)
        t0 = time.perf_counter()
        res = measure(cand, samples)
        print(f"  done in {time.perf_counter() - t0:.0f}s", file=sys.stderr, flush=True)
        results.append(res)
        (WORK / "results.json").write_text(json.dumps(results, indent=2, ensure_ascii=False))
    table = summarize(results, refs)
    (WORK / "results.md").write_text(table + "\n")
    print(table)


if __name__ == "__main__":
    main()
