# Parakeet v3 as a second engine

Status: added on 2026-11-01 as an option in Options (`parakeet-v3`). Franco chose it to try as
"a good middle point between speed and accuracy". The default stays whisper.cpp
`large-v3-turbo-q5_0` on a GPU (`small` without one).

## What runs

- Model: [NVIDIA Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
  (CC-BY-4.0), the int8 sherpa-onnx export
  `csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` at revision `2bda32ec`. Four files,
  670 MB, each checked with SHA-256 after the download (`models.rs`, `PARAKEET_FILES`).
- 25 European languages, English and Spanish included. It finds the language itself; it has no
  language setting, so Options disables the language list while it is selected.
- Runtime: [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) 2.0.1 (Apache-2.0) with
  onnxruntime (MIT), linked statically into the app through the `sherpa-onnx` crate. The
  archive (22 MB) is fetched by `desktop/scripts/sherpa-onnx.sh` and checked with the SHA-256
  that GitHub publishes for the release asset; the crate's build script then takes it from
  `desktop/.deps/sherpa-onnx` (`SHERPA_ONNX_ARCHIVE_DIR`, set in `src-tauri/.cargo/config.toml`,
  so it applies to builds started from `desktop/src-tauri`, as the Makefile does) instead of
  downloading it unchecked. The crate is pinned to `=2.0.1`, the release of the archive. Note:
  the crate reuses an archive already unpacked in `target/sherpa-onnx-prebuilt/` without a
  check; the checked one applies to a clean `target/`. The app binary grows from 55 MB to 84 MB.
- Built only on Linux x86-64 (`cfg(parakeet)` from `src-tauri/build.rs`), where an archive is
  pinned. On other systems the app has the Whisper models only, and Options does not list it.
- The four files are checked by size at every start; `tokens.txt` (a wrong one can end the
  process inside sherpa-onnx) is checked with SHA-256 before every load; if loading still
  fails, all files are checked and the wrong ones downloaded again, once.
- CPU only: onnxruntime has no Vulkan backend for the Radeon iGPU. 8 threads (the physical
  cores), about 1.1 GB of memory with the model loaded.
- Long audio: Silero VAD decides only whether there is speech. The pieces cover all the audio
  and are at most 60 s, cut at the quietest 100 ms of their last quarter. (An earlier version
  dropped what the VAD did not mark as speech: in heavy noise that lost most words, 84% WER on
  the noisy Spanish clip.) Each piece's tokens and times become one segment per sentence, for
  .srt and .vtt.

## Measurements (Ryzen 7 H 255; whisper.cpp on the Radeon 780M through Vulkan)

Through the app (`open-whisper --transcribe … --model …`; "seconds" is the transcription with
the model already loaded, as in normal use). Clips: the 5 benchmark clips, plus English and
Spanish with pink noise at 10 and 5 dB SNR (full band), plus a 3 s dictation.

| Clip | Parakeet v3 (CPU) | Whisper large-v3-turbo-q5_0 (GPU) |
|---|---|---|
| **3 s dictation (time)** | **0.14 s** | 1.56 s |
| 11 s (JFK), time | 0.38 s | 1.74 s |
| 22 s clips, time | 0.8–1.0 s | 2.0–2.2 s |
| WER, all 9 clips | 10.4% | 3.2% |
| English clean / 10 dB / 5 dB | 0.0% / 0.0% / 2.7% | 0.0% / 1.4% / 4.1% |
| Spanish clean / 10 dB / 5 dB | 5.7% / 11.4% / 11.4% | 2.9% / 2.9% / 2.9% |
| Extreme noise (0 dB + telephone band), English / Spanish | 49.3% / 5.7% | 6.8% / 5.7% |
| JFK (real speech) | 0.0% | 0.0% |

In short: about 10× faster for a dictation, as accurate in English with normal noise, and
2–4× more errors in Spanish (dropped "a" before an object, "abrida" for "abría",
"bicocho"). It fails on the extreme-noise English clip. The Options WER (14.3%) is the
5-clip benchmark figure, the same set as the other models.

Other engines measured with sherpa-onnx 2.0.1 on the CPU, same clips (decode time only):

| Model | WER, 9 clips | 3 s dictation | Download | Notes |
|---|---|---|---|---|
| Cohere Transcribe (14 languages, Apache-2.0) | 2.9% | 0.47 s | 1.6 GB | as accurate as Whisper turbo; 3.5 GB RAM; needs the language |
| Qwen3-ASR 0.6B | 8.1% | 0.60 s | 0.84 GB | slow on long clips (5–6 s for 22 s) |
| Canary 180M flash (en/es/de/fr) | 18.7% | 0.16 s | 0.15 GB | fails in heavy noise; needs the language |

Not tried in the app: Whisper with a shorter encoder window (`audio_ctx`) for short audio.
Without VAD it kept the WER and cut a 3 s dictation from 1.6 s to 1.0 s, but with the app's
VAD it doubled the WER (3.6% → 7.5%), so it was dropped.

## Tests

- Unit: pieces (joined up to the limit, long speech cut at a pause, exactly at the limit, a
  tiny limit), Parakeet tokens to sentences with times (no punctuation, times past the piece,
  no times), a wrong file is not "present" and is deleted, the catalog entry.
- Real model: a clean models folder; the app downloads and checks the four files, then
  transcribes (the JFK clip comes back exact, as two sentences).
- `make e2e-desktop`: Options switches to Parakeet, it loads, a global-key dictation into the
  terminal is typed with it, and Options switches back. Only when the model is cached in
  `bench/.work/parakeet-tdt-0.6b-v3-int8` (else it picks Small and prints SKIP).
