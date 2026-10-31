"""Generate benchmark audio samples with known reference text.

Sources (all open):
- Piper TTS (MIT) with open voices: en_US-ljspeech-high (LJSpeech, public domain data)
  and es_ES-davefx-medium.
- jfk.wav from the whisper.cpp repo (JFK 1961 inaugural address, US public domain).
- "noisy" variants: the TTS clips mixed with pink noise (0 dB SNR) and band-limited to
  telephone quality (300-3400 Hz, 8 kHz), to separate models that only work on clean audio.

Output: bench/.work/samples/<id>.wav (16 kHz mono) and bench/.work/samples/references.json.
Usage: uv run --group bench python bench/make_samples.py
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import wave
from pathlib import Path

from huggingface_hub import hf_hub_download

ROOT = Path(__file__).resolve().parent
WORK = ROOT / ".work"
OUT = WORK / "samples"

VOICES = {
    "en": "en/en_US/ljspeech/high/en_US-ljspeech-high",
    "es": "es/es_ES/davefx/medium/es_ES-davefx-medium",
}

TEXTS = {
    "en": (
        "The old lighthouse keeper climbed the spiral stairs every evening at dusk. "
        "He checked the lamp, wiped the salt from the glass, and wrote a short note in his logbook. "
        "Most nights were quiet, but in winter the storms came from the north and the waves "
        "crashed against the rocks below. Ships passing the point would flash their lights twice "
        "to say thank you, and he always answered with a single long beam."
    ),
    "es": (
        "Cada mañana, la panadera del barrio abría la tienda antes de que saliera el sol. "
        "Encendía el horno de leña, preparaba la masa con harina, agua y sal, y esperaba con paciencia "
        "a que el pan creciera. Los vecinos llegaban temprano para comprar barras calientes y medialunas. "
        "Ella conocía a todos por su nombre y siempre guardaba un pedazo de bizcocho para los niños "
        "que pasaban camino a la escuela."
    ),
}

JFK_TEXT = (
    "And so my fellow Americans, ask not what your country can do for you, "
    "ask what you can do for your country."
)


def run(cmd: list[str]) -> None:
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def duration(path: Path) -> float:
    with wave.open(str(path)) as w:
        return w.getnframes() / w.getframerate()


def tts(lang: str, dest: Path) -> None:
    voice = VOICES[lang]
    onnx = hf_hub_download("rhasspy/piper-voices", f"{voice}.onnx")
    hf_hub_download("rhasspy/piper-voices", f"{voice}.onnx.json")
    raw = dest.with_suffix(".raw.wav")
    subprocess.run(
        [sys.executable, "-m", "piper", "-m", onnx, "-f", str(raw)],
        input=TEXTS[lang].encode(),
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    run(["ffmpeg", "-y", "-i", str(raw), "-ac", "1", "-ar", "16000", str(dest)])
    raw.unlink()


def rms(path: Path) -> float:
    import array
    import math

    with wave.open(str(path)) as w:
        pcm = array.array("h", w.readframes(w.getnframes()))
    return math.sqrt(sum(x * x for x in pcm) / len(pcm)) / 32768


def noisy(src: Path, dest: Path, snr_db: float = 0.0) -> None:
    # Seeded pink noise at `snr_db` relative to the speech RMS, then telephone band + 8 kHz.
    # Pink noise RMS from anoisesrc is ~amplitude/5.4 (measured with volumedetect); calibrated so the target SNR holds within ~1 dB.
    amplitude = min(1.0, rms(src) * 10 ** (-snr_db / 20) * 5.4)
    run([
        "ffmpeg", "-y", "-i", str(src),
        "-f", "lavfi", "-i", f"anoisesrc=color=pink:seed=42:amplitude={amplitude:.4f}:duration={duration(src):.2f}:sample_rate=16000",
        "-filter_complex",
        "[0:a][1:a]amix=inputs=2:duration=first:normalize=0,highpass=f=300,lowpass=f=3400,"
        "aresample=8000,aresample=16000",
        "-ac", "1", "-ar", "16000", str(dest),
    ])


def main() -> None:
    if not shutil.which("ffmpeg"):
        sys.exit("ffmpeg is required to build benchmark samples")
    OUT.mkdir(parents=True, exist_ok=True)
    refs: dict[str, dict] = {}

    for lang in ("en", "es"):
        clean = OUT / f"{lang}_tts.wav"
        tts(lang, clean)
        refs[clean.stem] = {"lang": lang, "text": TEXTS[lang], "kind": "TTS (clean)"}
        dirty = OUT / f"{lang}_tts_noisy.wav"
        noisy(clean, dirty)
        refs[dirty.stem] = {"lang": lang, "text": TEXTS[lang], "kind": "TTS + pink noise at 0 dB SNR, phone band"}

    jfk_src = WORK / "whisper.cpp" / "samples" / "jfk.wav"
    if not jfk_src.exists():
        jfk_src = WORK / "jfk.wav"
        if not jfk_src.exists():
            import urllib.request

            urllib.request.urlretrieve(
                "https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav", jfk_src
            )
    jfk = OUT / "en_jfk.wav"
    run(["ffmpeg", "-y", "-i", str(jfk_src), "-ac", "1", "-ar", "16000", str(jfk)])
    refs[jfk.stem] = {"lang": "en", "text": JFK_TEXT, "kind": "real speech (1961 recording)"}

    for k, v in refs.items():
        v["seconds"] = round(duration(OUT / f"{k}.wav"), 2)
    (OUT / "references.json").write_text(json.dumps(refs, indent=2, ensure_ascii=False))
    for k, v in refs.items():
        print(f"{k:16s} {v['seconds']:6.2f}s  {v['kind']}")


if __name__ == "__main__":
    main()
