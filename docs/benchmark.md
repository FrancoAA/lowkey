# Speech-to-text benchmark on this machine

Goal: choose the default model for open-whisper on the machine it runs on, with no GPU.

**Result: `large-v3-turbo` on faster-whisper, CPU int8.** It has the best accuracy for
its speed. It runs at 4× real time, needs 1.7 GB of RAM and a 1.5 GB download.
Its WER is 4.2% overall and 8.2% on noisy English. `large-v3` is more accurate
(2.3%), but it is 1.8× slower and needs 2× the RAM and 2× the download, so it is
an option in the UI and not the default.

## Machine

- CPU: AMD Ryzen 7 H 255 (8 cores / 16 threads, Zen 5), 30 GB RAM
- GPU: Radeon 780M iGPU only; no CUDA, so every candidate runs on the CPU
- faster-whisper 1.2.1, CTranslate2 4.8.4 (int8); whisper.cpp 482279b (2026-10-30),
  CPU build with `GGML_NATIVE=ON`, no BLAS/Vulkan
- Settings for all: 8 compute threads, beam size 5, **language auto-detect** (the app
  default), no VAD, default `condition_on_previous_text`. These are the same for both
  engines, so the engines compare on equal terms. The app's own decode settings were then
  tuned separately (see "App decode settings" below).
- faster-whisper: one fresh process per candidate, one untimed warm-up, then all clips.
  whisper.cpp: one `whisper-cli` process per clip; its own timings give load and compute.
- Peak RAM: `ru_maxrss` from `wait4()` on the worker process. On Linux this includes
  children that the worker waited for, so the whisper.cpp numbers are `whisper-cli`'s
  (the Python wrapper alone is about 20 MB).

## Samples (with known reference text)

| id | length | source |
|---|---|---|
| en_tts | 23 s | English paragraph, Piper TTS voice `en_US-ljspeech-high` |
| es_tts | 22 s | Spanish paragraph, Piper TTS voice `es_ES-davefx-medium` |
| en_tts_noisy / es_tts_noisy | same | the TTS clips + seeded pink noise at **0 dB SNR**, cut to telephone band (300–3400 Hz, 8 kHz) |
| en_jfk | 11 s | real 1961 recording (JFK inaugural, US public domain; `samples/jfk.wav` from whisper.cpp) |

WER uses jiwer after lower-casing and removing punctuation. "WER all" pools the
word errors of all five clips (about 300 reference words).

## Results

| Candidate | Load s | RTF (lower=faster) | x real-time | Peak RAM MB | WER EN clean | WER EN real (JFK) | WER ES clean | WER EN noisy | WER ES noisy | WER all | Lang-ID ok |
|---|---|---|---|---|---|---|---|---|---|---|---|
| fw:small | 0.9 | 0.112 | 8.9x | 672 | 2.7% | 0.0% | 7.1% | 31.5% | 17.1% | 13.6% | 5/5 |
| fw:medium | 1.9 | 0.303 | 3.3x | 1604 | 0.0% | 0.0% | 1.4% | 11.0% | 5.7% | 4.2% | 5/5 |
| fw:large-v3-turbo | 1.8 | 0.253 | 4.0x | 1673 | 1.4% | 0.0% | 2.9% | 8.2% | 5.7% | 4.2% | 5/5 |
| fw:distil-large-v3 | 1.7 | 0.232 | 4.3x | 1566 | 4.1% | 0.0% | 104.3% | 19.2% | 98.6% | 51.6% | 3/5 |
| fw:large-v3 | 2.8 | 0.454 | 2.2x | 3067 | 0.0% | 0.0% | 4.3% | 2.7% | 2.9% | 2.3% | 5/5 |
| wcpp:small | 0.1 | 0.178 | 5.6x | 758 | 2.7% | 0.0% | 7.1% | 27.4% | 14.3% | 12.0% | 5/5 |
| wcpp:medium | 0.4 | 0.583 | 1.7x | 2081 | 0.0% | 0.0% | 1.4% | 11.0% | 5.7% | 4.2% | 5/5 |
| wcpp:large-v3-turbo | 0.2 | 0.780 | 1.3x | 1813 | 1.4% | 0.0% | 0.0% | 6.8% | 5.7% | 3.2% | 5/5 |
| wcpp:large-v3-turbo-q5_0 | 0.1 | 0.724 | 1.4x | 803 | 0.0% | 0.0% | 2.9% | 6.8% | 5.7% | 3.6% | 5/5 |

RTF = compute seconds / audio seconds (summed over the clips). Peak RAM = peak RSS of the
worker process. Load = model load time.

### Short-clip latency (dictation case)

Whisper always encodes a full 30 s window, so short clips cost almost the same as 30 s.
With auto-detect, stock faster-whisper also runs the encoder twice: once to detect the
language and once to transcribe. open-whisper detects the language from the exact window it
transcribes first and keeps that encoder output (`openwhisper/fastlang.py`), so the encoder
runs once. Same run, same machine, app settings, `large-v3-turbo`:

| clip | stock faster-whisper | open-whisper (encoder once) | same text and language |
|---|---|---|---|
| en_jfk (11 s) | 5.79 s | 3.42 s | yes |
| es_tts (22 s) | 5.68 s | 3.64 s | yes |
| en_tts (23 s) | 5.41 s | 3.50 s | yes |
| en_tts_noisy | 5.43 s | 3.66 s | yes |
| es_tts_noisy | 6.01 s | 3.72 s | yes |

Absolute times vary with machine load (an earlier run measured stock at 4.5 s for JFK);
the saving is the cost of one encoder pass, about 2 s on this CPU. With the language set in
Options it was 2.4 s; `small` with auto-detect, 1.5 s. More threads (16 instead of 8) and
greedy decoding (beam 1) did not change the time.

### App decode settings (large-v3-turbo)

The app runs Silero VAD before Whisper and sets `condition_on_previous_text=False`.
Measured on the same five clips, plus 3 s of pure silence:

| setting | WER all | noisy EN | noisy ES | output for 3 s of silence |
|---|---|---|---|---|
| no VAD | 4.2% | 8.2% | 5.7% | "Thank you." (hallucination) |
| VAD, default threshold 0.5 | 5.5% | 12.3% | 7.1% | "" |
| **VAD, threshold 0.3 (shipped)** | **3.9%** | **6.8%** | **5.7%** | "" |

The default VAD threshold cut real speech in the 0 dB clips. A threshold of 0.3 keeps that speech
and still removes silence. `condition_on_previous_text` made no difference on these clips;
it is off to avoid repetition loops on long recordings. Speed was the same in all rows.

## Why large-v3-turbo

- **Accuracy:** it ties with `medium` for overall WER (4.2%) and is better on the
  hardest clip (noisy English: 8.2% vs 11.0%). It is much better than `small` (13.6%)
  and `small` fails on noisy audio (31.5% noisy English).
- **Speed:** it is faster than `medium` (4.0× vs 3.3× real time) with the same RAM
  (1.7 vs 1.6 GB). `large-v3` is more accurate (2.3%) but runs at 2.2× real time with
  3.1 GB of RAM and a 3 GB first download. That is too much for a zero-config default
  on a CPU-only machine.
- **Multilingual:** it has the large-v3 encoder, which helps languages beyond the two we
  measured. `distil-large-v3` is English-only in practice: Spanish became an English
  translation (98–104% WER) and its language ID failed on 2 of 5 clips.
- **Engine:** on this CPU, faster-whisper (CTranslate2 int8) is 3× faster than
  whisper.cpp for the same model (turbo: 4.0× vs 1.3× real time) with the same accuracy.
  So the app keeps faster-whisper. whisper.cpp q5_0 uses much less RAM (0.8 GB), which can
  help small machines.

## Limits of this benchmark

- The samples are small: about 300 words, mostly TTS. Differences of 1–2 WER points
  (a few words) are within noise. The large gaps (small, distil on Spanish, noisy audio)
  are real.
- Piper adds some randomness, so a new run of `make_samples.py` can move clean-clip WER
  by about one word.
- We measured no GPU paths. A Vulkan build of whisper.cpp could use the 780M iGPU; that
  is not measured.

## Reproduce

```sh
mkdir -p bench/.work && cd bench/.work && git clone --depth 1 https://github.com/ggml-org/whisper.cpp.git \
  && cmake -S whisper.cpp -B whisper.cpp/build -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=ON \
  && cmake --build whisper.cpp/build -j --target whisper-cli    # optional
# ggml models (optional): ggml-{small,medium,large-v3-turbo,large-v3-turbo-q5_0}.bin
#   from huggingface.co/ggerganov/whisper.cpp into bench/.work/ggml/
cd ../.. && make bench    # samples + all candidates; writes bench/.work/results.md
```

The whisper.cpp candidates are skipped when the binary or models are missing.

## whisper.cpp with Vulkan on the Radeon 780M (2026-11-01)

Measured for the Tauri desktop decision (`docs/tauri-desktop-options.md`). Same clips, same
settings (8 threads, beam 5, auto language, no VAD); whisper.cpp 482279b built with
`GGML_VULKAN=ON` (Vulkan-Headers and SPIRV-Headers fetched into `bench/.work`, no system
package installed). The log confirms the device "AMD Radeon 780M Graphics".

| Candidate | RTF | x real-time | WER all | WER EN noisy | WER ES noisy | 11 s JFK clip |
|---|---|---|---|---|---|---|
| wcpp-vulkan:large-v3-turbo-q5_0 | 0.092 | 10.9x | 3.2% | 5.5% | 5.7% | 1.6 s |
| wcpp-vulkan:large-v3-turbo | 0.112 | 8.9x | 3.9% | 6.8% | 5.7% | 1.8 s |
| wcpp-vulkan:medium | 0.152 | 6.6x | 3.2% | 6.8% | 5.7% | 1.7 s |
| wcpp-vulkan:small | 0.057 | 17.6x | 12.7% | 30.1% | 14.3% | 0.6 s |

For comparison, faster-whisper large-v3-turbo on the CPU: 4.0x, WER 4.2% (3.9% with the
app's VAD), 3.4 s for the JFK clip with the encoder reuse. "Peak RAM" is not comparable here:
whisper.cpp keeps the model in GPU memory, which the process RSS (120-210 MB) does not include;
on an iGPU that memory is shared system RAM.

Reproduce:

```sh
OW_WCPP_BIN=bench/.work/whisper.cpp/build-vk/bin/whisper-cli OW_WCPP_LABEL=wcpp-vulkan \
  uv run --group bench python bench/benchmark.py --only wcpp:large-v3-turbo-q5_0 wcpp:large-v3-turbo wcpp:medium wcpp:small
```
