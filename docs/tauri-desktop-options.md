# open-whisper as a Tauri v2 desktop app — decisions

Status: **decided by Franco on 2026-11-01 and built** on branch `feat/tauri-desktop`:
engine **C** (whisper.cpp in Rust by default, faster-whisper as an optional engine through the
Python helper, picked automatically without a GPU), hotkey **"Hyprland as today + per-OS
native"**, packaging **Linux first** with Rust from rustup in the home folder.

**Update 2026-11-01:** after parity was verified, Franco chose to remove Python completely:
the Python web app and the optional faster-whisper engine are gone. The app is whisper.cpp
only; without a GPU it runs on the CPU (`small` is the fast choice there). The live preview
(Cactus Whistle) was added: [live-preview.md](live-preview.md).

What the first version does differently from the recommendation below: the global key on
X11, macOS and Windows uses the Tauri global-shortcut plugin (as recommended), but the GNOME/KDE
Wayland portal path (GlobalShortcuts + RemoteDesktop typing) is **not built**: on those desktops
the app tells the user to bind `open-whisper --dictate …` in the keyboard settings. macOS,
Windows and X11 code paths are written but not built or tested on the development machine.
The text below is the decision document as it was presented.

What does not need a decision (it will be done in any option): the current web UI becomes the
Tauri webview UI; one app instance per user; first-run model download with progress; the
overlay with live waveform and "Getting ready…"; clipboard + typing; the 15 s leading-space
rule; the existing `open-whisper.dictation` block in `bindings.lua` keeps working; a model
setting with `--model <name>`, `~/.config/open-whisper/config.toml` and an Options selector that
shows RAM, download size, speed and WER per model, plus an idle-unload setting; zero
configuration by default.

---

## Decision 1 — Speech engine

New measurement on this machine (Ryzen 7 H 255, Radeon 780M iGPU), same 5 clips and WER
method as `docs/benchmark.md`. whisper.cpp was built with Vulkan (`GGML_VULKAN=ON`); the GPU
was used ("AMD Radeon 780M Graphics" in the log). Reproduce with
`OW_WCPP_BIN=bench/.work/whisper.cpp/build-vk/bin/whisper-cli OW_WCPP_LABEL=wcpp-vulkan make bench`.

| Engine on this machine | Model | Speed (× real time) | WER all | 11 s clip | Download |
|---|---|---|---|---|---|
| faster-whisper, CPU int8 (today) | large-v3-turbo | 4.0× | 3.9–4.2% | 3.4 s (encoder once) | 1.5 GB |
| whisper.cpp, CPU only | large-v3-turbo-q5_0 | 1.4× | 3.6% | 14.1 s | 0.55 GB |
| **whisper.cpp, Vulkan (780M)** | **large-v3-turbo-q5_0** | **10.9×** | **3.2%** | **1.6 s** | **0.55 GB** |
| whisper.cpp, Vulkan (780M) | large-v3-turbo | 8.9× | 3.9% | 1.8 s | 1.6 GB |
| whisper.cpp, Vulkan (780M) | small | 17.6× | 12.7% | 0.6 s | 0.47 GB |

Memory: faster-whisper turbo peaks at 1.7 GB RAM. whisper.cpp keeps the model in GPU memory;
the process RSS (120–210 MB) does not show it, and on an iGPU that memory is shared system RAM
(about the model size plus buffers, so roughly 0.8–2 GB). Not measured separately.

### Options

| | A. Python sidecar (faster-whisper) | B. Rust-native whisper.cpp (`whisper-rs`) | C. B by default + faster-whisper as optional engine | D. CTranslate2 from Rust (`ct2rs`) |
|---|---|---|---|---|
| What runs | Tauri app + bundled Python process (today's engine code) | One Rust binary with whisper.cpp inside | B, plus the Python engine fetched with uv only when chosen or when there is no GPU | One Rust binary with CTranslate2 inside |
| Speed, this machine | 4.0× (CPU) | 10.9× (Vulkan) | 10.9× (Vulkan); 4.0× when the sidecar is used | ~4.0× (CPU), same as A |
| CPU-only machines | best (4.0×) | slow (1.4×; dictation of 11 s takes ~14 s) | best of both: sidecar on CPU-only | like A |
| GPU | NVIDIA (CUDA) only; no AMD/Intel, no Apple | Vulkan (AMD/Intel/NVIDIA, Linux+Windows), Metal (macOS), CUDA | as B | CUDA only |
| macOS (Apple Silicon) | CPU only | Metal GPU: fast | Metal GPU | CPU only |
| Installer size (est.) | +150–200 MB compressed per OS (Python ~45 MB, CTranslate2 ~145 MB, PyAV ~115 MB, onnxruntime ~65 MB, numpy ~60 MB unpacked ≈ 450 MB) | ~15–30 MB (+ WebKit on Linux AppImage) | as B; sidecar ~450 MB downloaded only if used | ~40–80 MB |
| Model download | 1.5 GB (CT2) | 0.55 GB (turbo q5_0) | 0.55 GB (+1.5 GB if sidecar) | 1.5 GB |
| Features to rebuild | none (same engine) | VAD (whisper.cpp has Silero VAD), language ID, segments, progress callbacks: built in; re-benchmark defaults | as B + engine switch | VAD (onnx), timestamps, mel, language ID: mostly by hand |
| Risk | Python packaging per OS/arch (PyInstaller or uv), two processes | Vulkan driver quirks (fallback to CPU needed), C++ build per OS | most code and tests | young bindings, heavy C++/BLAS build per OS |
| Effort (relative) | Medium | Medium–High | High | High |

**Recommendation: C, built in two steps.** Step 1 is B (whisper.cpp in Rust, GPU via Vulkan
on Linux/Windows and Metal on macOS, CPU fallback), which makes this machine 2.7× faster,
cuts the installer and the model download, and gives macOS a GPU. Step 2 adds faster-whisper
as an optional engine (run by uv, not bundled), so the current behaviour stays available and
CPU-only machines keep the faster engine; the app picks it automatically when no GPU works.
New default model on a GPU: `large-v3-turbo-q5_0` (best WER and speed in the table above).
If you prefer less work: **B alone**, accepting slow CPU-only machines.

---

## Decision 2 — Global key, typing and overlay per OS

Today (Hyprland): runtime bind or the `bindings.lua` block → `scripts/ow-dictate` → HTTP + token.
In Tauri this becomes: the bind runs the app binary (`open-whisper --dictate press|release`);
the single-instance plugin hands the command to the running app (or starts it), so the token,
`server.json` and curl go away (the hand-off uses a private local socket).

| OS / desktop | Global key | Hold (press + release) | Typing into the focused app | Overlay that never takes focus |
|---|---|---|---|---|
| **Hyprland** | runtime bind via `hyprctl eval` + the existing config block (as today) | yes (bind + release bind) | `wtype` (virtual keyboard) | Tauri window + Hyprland window rule (as today) |
| sway / other wlroots | `swaymsg bindsym` at runtime, or the XDG GlobalShortcuts portal | yes | `wtype` | no position control; window rule per compositor |
| GNOME ≥ 48 / KDE (Wayland) | XDG GlobalShortcuts portal (system dialog, once) | yes (Activated/Deactivated) | no virtual keyboard: RemoteDesktop portal / libei (one permission dialog), or clipboard only | always-on-top not allowed by GNOME; KDE allows |
| X11 | `global-hotkey` crate | yes | XTest (`enigo`) | always-on-top + no-focus window |
| macOS | `global-hotkey` (Carbon hot keys) | yes | `enigo` (CGEvent); needs Accessibility permission once | non-activating panel (`tauri-nspanel`) |
| Windows | `global-hotkey` (RegisterHotKey) | yes | `enigo` (SendInput) | topmost + `WS_EX_NOACTIVATE` window |

Default key per OS (proposal): Linux Super+Ctrl+X (as today); macOS Ctrl+Option+Space;
Windows Ctrl+Alt+Space (Win+Ctrl+X is close to the system Win+X menu).

Microphone: `cpal` on all OS (WASAPI, CoreAudio, ALSA/PipeWire). The app still records the
mic itself, as you decided; only the library changes from `pw-record` to `cpal`.

**Recommendation:** Hyprland exactly as today (it is tested and in your config); X11, macOS and
Windows through `global-hotkey` + `enigo`; GNOME/KDE through the GlobalShortcuts portal with
typing through the RemoteDesktop portal, and clipboard-only (with a clear message) where that
is refused. Build and test Linux/Hyprland first (nested Hyprland only); macOS and Windows code
is written to compile and run but cannot be tested on this machine.

---

## Decision 3 — Packaging scope for the first version

| Option | What you get | Cost |
|---|---|---|
| a. Linux only now (AppImage + .deb + .rpm, plus `cargo tauri dev`) | runs here; others later | lowest |
| b. Linux + unsigned macOS/Windows builds | builds exist, but cannot be tested here; macOS Gatekeeper and Windows SmartScreen warn | CI or another machine needed to build them |
| c. Signed macOS/Windows releases | normal install experience | Apple Developer account (USD 99/year) and a Windows code-signing certificate |

**Recommendation: a** now, with the code kept cross-platform; b when you have a Mac or
Windows machine (or CI) to test on.

---

## Also needed (no decision, for your information)

- **Rust toolchain**: not installed on this machine (`cargo`/`rustc` missing). Building needs
  `rustup` (user install in `~/.rustup`, no system change) or `mise use rust`. Tell me if you
  prefer one; I will not install it before you answer.
- Linux build dependencies are already here: WebKitGTK 4.1, libsoup 3, GTK 3, AppIndicator,
  librsvg, Vulkan loader + RADV driver, `glslc`. whisper.cpp's Vulkan build also needs
  Vulkan-Headers and SPIRV-Headers; for the measurement they were fetched into `bench/.work`
  (no system package was installed). For the app, the build would fetch them the same way, or
  you install `vulkan-headers spirv-headers` with pacman.
- Hyprland work is tested only in a nested Hyprland, never in your session.
