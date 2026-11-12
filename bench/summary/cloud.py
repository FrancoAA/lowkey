"""Meeting notes through OpenRouter: the app's instructions, scored against expected.json
(bench/summary/results.md, "Cloud models"). Standard library only; costs a few cents.

Usage: python3 bench/summary/cloud.py <key file> bench/summary/meeting-es.md <instructions file> <out dir> <run> [models,…]
  The instructions: llm::system_prompt("Spanish", ["Martín"]) as the app sends them.
  <run> "rescore:N" scores the saved answers of run N again, with no request.
"""
import concurrent.futures as cf
import json
import re
import sys
import time
import urllib.request
from pathlib import Path

KEY = Path(sys.argv[1]).read_text().strip()
MEETING = Path(sys.argv[2]).read_text()
SYSTEM = Path(sys.argv[3]).read_text()
OUT = Path(sys.argv[4])
RUN = sys.argv[5] if len(sys.argv) > 5 else "1"
MODELS = sys.argv[6].split(",") if len(sys.argv) > 6 else [
    "anthropic/claude-haiku-5.5", "anthropic/claude-sonnet-5.5", "google/gemini-3.8-flash", "google/gemini-3.1-flash-lite",
    "openai/gpt-5-mini", "openai/gpt-4.1-mini", "mistralai/mistral-medium-3.1", "qwen/qwen3.8-flash", "moonshotai/kimi-k2-0905",
]


def ask(model):
    body = {"model": model, "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": MEETING}], "max_tokens": 32000, "usage": {"include": True}}
    req = urllib.request.Request("https://openrouter.ai/api/v1/chat/completions", json.dumps(body).encode(), {"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"})
    t = time.time()
    try:
        v = json.load(urllib.request.urlopen(req, timeout=400))
    except Exception as e:
        return {"model": model, "error": f"{e} {getattr(e, 'read', lambda: b'')()[:300]}"}
    u = v.get("usage", {})
    return {"model": model, "seconds": round(time.time() - t, 1), "text": v["choices"][0]["message"].get("content") or "", "finish": v["choices"][0].get("finish_reason"),
            "out_tokens": u.get("completion_tokens"), "reasoning_tokens": (u.get("completion_tokens_details") or {}).get("reasoning_tokens"), "cost": u.get("cost")}


def lines_with(text, *words):
    return [l for l in text.splitlines() if all(re.search(w, l, re.I) for w in words)]


def task(text, who, *what):
    return any(re.match(rf"\s*[-*] \*\*{who}\*\*", l) for l in lines_with(text, *what))


def sections(text, title):
    """The text of each "## " section whose heading matches."""
    parts = re.split(r"(?m)^## ", text)
    return [p for p in parts if re.match(rf"[^\n]*({title})", p, re.I)]


def score(t):
    checks = {
        "D 2.4 on Thursday 14": bool(lines_with(t, r"2\.4|lanzamiento|versión", r"jueves 14|14 de noviembre")),
        "D Salesforce in January": bool(lines_with(t, "salesforce|integraci", "enero")),
        "D Postgres, not Mongo": bool(lines_with(t, "postgres")),
        "D freelance designer 9000": bool(lines_with(t, "freelance|diseñador")) and bool(re.search(r"9[.,]?000|nueve mil", t, re.I)),
        "T Martín ticket 412": task(t, "Martín", "412|pagos duplicados"),
        "T Lucía release notes": task(t, "Lucía", "notas"),
        "T Me calls Acme": task(t, "Me", "acme"),
        "T Martín schema draft": task(t, "Martín", "esquema|migraci|borrador"),
        "T Sofía job offer": task(t, "Sofía", "oferta|publica"),
        "Q Team price open, 20 Nov": any(re.search(r"20 de noviembre|el 20\b", sec) and re.search(r"abiert|pendiente|no se decid|no se cambia|sin decidir|queda", sec, re.I) for sec in sections(t, "precio|team")),
        "Q tax data security open": bool(lines_with(t, "seguridad", "fiscal|identificaci|pendiente|abiert")),
        "N 41 customers": bool(re.search(r"\b41\b|cuarenta y un", t, re.I)),
        "N churn 3.2%": bool(re.search(r"3[.,]2|tres coma dos", t, re.I)),
        "no task for Them": not re.search(r"^\s*[-*] \*\*Them\*\*", t, re.M),
        "Acme call is Me's": not any(re.match(r"\s*[-*] \*\*(?!Me\*\*)", l) for l in lines_with(t, "acme", "llam|habla|avis")),
    }
    bullets = [l for l in t.splitlines() if re.match(r"\s*[-*] ", l)]
    timed = sum(bool(re.search(r"\(\d{1,2}:\d{2}", l)) for l in bullets)
    return checks, len(bullets), timed


results = []
if RUN.startswith("rescore:"):
    RUN = RUN.split(":", 1)[1]
    results = json.loads((OUT / f"results.run{RUN}.json").read_text())
else:
    with cf.ThreadPoolExecutor(len(MODELS)) as ex:
        for r in ex.map(ask, MODELS):
            results.append(r)
OUT.mkdir(parents=True, exist_ok=True)
print(f"{'model':32} {'s':>6} {'out':>6} {'think':>6} {'cost':>8} {'score':>6} {'timed':>8}  missed")
for r in results:
    name = r["model"].replace("/", "_")
    if "error" in r:
        print(f"{r['model']:32} ERROR {r['error'][:150]}")
        continue
    (OUT / f"{name}.run{RUN}.md").write_text(r["text"])
    checks, n, timed = score(r["text"])
    r["checks"] = checks
    missed = [k for k, ok in checks.items() if not ok]
    print(f"{r['model']:32} {r['seconds']:6} {r['out_tokens'] or 0:6} {r['reasoning_tokens'] or 0:6} {r['cost'] or 0:8.4f} {sum(checks.values()):3}/{len(checks)} {timed:3}/{n:<4}  {'; '.join(missed)}")
(OUT / f"results.run{RUN}.json").write_text(json.dumps(results, ensure_ascii=False, indent=1))
