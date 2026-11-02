"""Meeting summary benchmark: small local LLMs through llama.cpp's server (OpenAI-compatible API).

Each model gets the same meeting (Markdown as open-whisper exports it: notes, then the
transcript) and the same instructions; the answer, the speed and the memory are saved.
The meeting has known decisions, tasks, an open question and numbers (expected.json); the
scores are counted by hand from the answers (results.md).

Usage: python3 bench/summary/run.py [model.gguf ...]   (standard library only)
  LLAMA_SERVER=<path to llama-server>   default bench/.work/llama.cpp/build/bin/llama-server
  OW_LLM_GPU=0                          CPU only (default: all layers on the GPU, Vulkan)
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORK = HERE.parent / ".work"
SERVER = Path(os.environ.get("LLAMA_SERVER") or WORK / "llama.cpp" / "build" / "bin" / "llama-server")
PORT = 18734
GPU = os.environ.get("OW_LLM_GPU", "1") != "0"
# The meeting's language, as the app knows it (Whisper detects it; else the app's language).
LANG = os.environ.get("OW_LLM_LANG", "Spanish")

SYSTEM = """You write meeting notes from a transcript. "Me" is the person who recorded the meeting; \
"Them" is everyone else (several people: tell them apart by the names they use). The notes \
written by "Me" during the meeting show what mattered to them.

Write in {lang}. Use only facts from the transcript; never invent names, \
dates or numbers. Markdown, with these sections:

## Summary
Three to five sentences.

## Decisions
One line per decision.

## Action items
One line per task: **who** — what — when (if said).

## Open questions
Things left without a decision, with the next date if one was set.

## Numbers
Figures that were said (money, dates, metrics), one per line, each with what it is."""


def rss_mb(pid: int) -> float:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) / 1024
    except OSError:
        pass
    return 0.0


def post(path: str, body: dict, timeout: float = 900) -> dict:
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}{path}", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def run(model: Path, meeting: str) -> dict:
    args = [SERVER, "-m", model, "--port", str(PORT), "-c", "32768", "--jinja", "-fa", "on", "--no-webui"]
    args += ["-ngl", "99"] if GPU else ["-ngl", "0"]
    log = open(HERE / f"server-{model.stem}.log", "w")
    t_load = time.monotonic()
    srv = subprocess.Popen([str(a) for a in args], stdout=log, stderr=subprocess.STDOUT)
    try:
        while True:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{PORT}/health", timeout=2) as r:
                    if json.loads(r.read()).get("status") == "ok":
                        break
            except Exception:
                pass
            if srv.poll() is not None:
                raise RuntimeError(f"llama-server ended; see {log.name}")
            time.sleep(0.3)
        load_s = time.monotonic() - t_load
        t0 = time.monotonic()
        out = post("/v1/chat/completions", {
            "messages": [{"role": "system", "content": SYSTEM.replace("{lang}", LANG)}, {"role": "user", "content": meeting}],
            "temperature": 0.2,
            "max_tokens": 1200,
            "chat_template_kwargs": {"enable_thinking": False},
        })
        wall = time.monotonic() - t0
        t = out.get("timings", {})
        return {
            "model": model.name,
            "device": "GPU (Vulkan)" if GPU else "CPU",
            "load_s": round(load_s, 1),
            "prompt_tokens": t.get("prompt_n"),
            "prompt_tok_s": round(t.get("prompt_per_second", 0), 1),
            "gen_tokens": t.get("predicted_n"),
            "gen_tok_s": round(t.get("predicted_per_second", 0), 1),
            "total_s": round(wall, 1),
            "rss_mb": round(rss_mb(srv.pid)),
            "answer": out["choices"][0]["message"]["content"],
        }
    finally:
        srv.terminate()
        srv.wait(20)


def main() -> None:
    models = [Path(a) for a in sys.argv[1:]] or sorted((WORK / "llm").glob("*.gguf"))
    meeting = (HERE / "meeting-es.md").read_text()
    results = []
    for m in models:
        print(f"== {m.name} ({'GPU' if GPU else 'CPU'})", flush=True)
        r = run(m, meeting)
        print(json.dumps({k: v for k, v in r.items() if k != "answer"}), flush=True)
        (HERE / f"answer-{m.stem}{'' if GPU else '-cpu'}.md").write_text(r["answer"])
        results.append(r)
    out = HERE / ("results.json" if GPU else "results-cpu.json")
    out.write_text(json.dumps(results, indent=1, ensure_ascii=False))
    print(f"saved {out}")


if __name__ == "__main__":
    main()
