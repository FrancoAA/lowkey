<img src="assets/icon.svg" width="64" height="64" alt="">

# open-whisper

Local speech to text as a desktop app. Drop an audio or video file and get the text, or hold
a key and talk into any app. Everything runs on your computer with open-source software and
open model weights. No account, no API key, no cloud.

It is one native program (Rust + Tauri v2) with whisper.cpp inside: on the GPU through Vulkan
(Linux, Windows) or Metal (macOS), else on the CPU.

![Desktop app](docs/screenshots/10-desktop-transcript.png)

## Install

**You need** Rust (`rustup`), Node.js 20+, and on Linux the WebKitGTK 4.1, GTK 3,
libappindicator, Vulkan loader and `glslc` packages (Omarchy has them). `make desktop` fetches
the Vulkan and SPIR-V headers into `desktop/.deps` if your system does not have them.

```sh
make install-desktop     # builds the app and copies it to ~/.local/bin/open-whisper
open-whisper             # start it
```

The first start downloads the speech model (0.55 GB) and the live preview model (17 MB), once.
The window shows the progress. Other builds:

```sh
make desktop             # only build: desktop/src-tauri/target/release/open-whisper
                         # (also fetches the checked sherpa-onnx libraries into desktop/.deps)
make desktop-bundle      # AppImage / .deb / .rpm in desktop/src-tauri/target/release/bundle/
```

## Use

- **File:** drop a file on the window, or click to choose one. It starts at once. MP3, WAV,
  M4A, OGG, FLAC, WEBM, MP4, MOV, MKV and most other audio/video formats work (with `ffmpeg`
  installed for Opus, WebM and most video). Other files get a clear error message.
- **Dictation in the window:** hold <kbd>Space</kbd> while you talk, then release. Or tap
  <kbd>Space</kbd> once to start and again to stop. The mic button does the same.
  <kbd>Esc</kbd> cancels. A live waveform shows that the app is listening.
- **Result:** the text appears while it is transcribed. **Copy** puts it on the clipboard.
  **.txt**, **.srt** and **.vtt** save the transcript or subtitles.
- **Settings** (the ⚙ button or <kbd>Ctrl</kbd>+<kbd>,</kbd>; never required): sections on the left —
  General (language), Model (model, free memory when idle), Dictation (the key, start at login),
  Live preview (on/off, engine), About.
- Files given on the command line open in the app. **Closing the window keeps the app
  running** in the tray, so the dictation key stays instant. Quit from the tray or with
  `open-whisper --dictate quit`.

## Dictate into any app

The global key is **<kbd>Super</kbd>+<kbd>Ctrl</kbd>+<kbd>X</kbd>** on Linux and
**<kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Space</kbd>** on macOS and Windows.

- **Hold** the key while you talk, then release. Or **tap** it to start and tap again to stop.
- A small window at the bottom of the screen shows a live waveform and, while you talk, the
  words it hears (**live preview**). It never takes the keyboard focus.
- The text is **typed into the app that has focus** and is also **copied to the clipboard**.
- Dictations follow on: if you start talking again soon after the last text was typed (the
  new recording ends at most **15 seconds** after it) and the same window has focus, the new
  text is typed with a space in front ("…a test. And then…"), unless it starts with `.`, `,`,
  `;`, `:`, `!`, `?`, `…`, `%` or a closing bracket or quote. After a longer pause, or in
  another window, no space is added. The clipboard always gets the text alone.
- **The key also works when the app is closed:** it starts the app in the background (no
  window), the overlay shows "Getting ready…", and the dictation begins when the model is
  ready. Keep holding the key for push-to-talk, or let go and tap it again to finish.
- While a dictation is still being transcribed, a new press shows "Still transcribing".
  Dictation goes ahead of files in the queue.

![Overlay with live preview](docs/screenshots/18-desktop-overlay-live-preview.png)
![Text typed into the focused app](docs/screenshots/14-desktop-text-typed.png)
![Changing the dictation key in Settings](docs/screenshots/19-desktop-options-key.png)

**Hyprland (0.56+, Lua config): no setup.** The app adds the key and the overlay window rule
to the running Hyprland with `hyprctl eval`, and again when Hyprland reloads its config. No
config file is changed. To keep the key after a logout (and to start the closed app), put it in
your config: `open-whisper --print-snippet lua` prints the lines; add them to
`~/.config/hypr/bindings.lua` between `-- BEGIN open-whisper.dictation` and
`-- END open-whisper.dictation`. A block that runs `scripts/ow-dictate` (from earlier versions)
keeps working: the script hands the key to the app, or starts `~/.local/bin/open-whisper`
(else this folder's build; `OPENWHISPER_DESKTOP_BIN` overrides) with its output in
`$XDG_RUNTIME_DIR/open-whisper/app.log`. When the key is in your config, the app
sees it and does not add it again. Classic `hyprland.conf`: `--print-snippet conf`.

- **Change the key in Settings → Dictation → Change**, then press the new combination
  (Super, Ctrl or Alt with a letter, digit, F1-F24 or Space; Esc cancels). It works at once,
  and the app refuses a combination that another shortcut uses. When the key comes from a
  block in your Hyprland config, Settings offer **"Let the app manage it"**: the block is
  removed (a backup is kept next to the file), Hyprland reloads, and the app checks that its
  key works; if not, your config is put back. It also turns on "Start at login" (the config's
  bind worked after a logout; the app's bind needs the app).
- **Settings → Dictation → Start when you log in**: the app starts hidden at login, so the key
  works all the time (Linux: `~/.config/autostart/open-whisper.desktop`).
- Command line: `--hotkey "…"` (this run), `hotkey = "…"` in the settings, `--no-hotkey` adds
  none; `--remove-hotkey` removes a key that the app added (never yours).
- Title-bar plugins (hyprbars, as in Omarchy's window controls) skip the overlay: its rule
  turns the bar off when the plugin is loaded.
- It needs `wl-copy` and `wtype` (typing on Wayland). The microphone is recorded by the app
  itself, and only while the overlay shows "Listening".
- **X11, macOS, Windows:** the app registers the key while it runs. Typing uses xdotool (X11)
  or the system API (macOS asks once for Accessibility permission). Not tested on this
  machine.
- **GNOME, KDE, sway (Wayland):** apps cannot add keys there. Bind `open-whisper --dictate
  press` / `--dictate release` (or `--dictate toggle`) in the desktop's keyboard settings.

### Live preview

While you hold the key, the overlay shows the words as you say them; the main model still
writes the text that is typed. Two engines (Settings → Live preview → Preview engine):

- **Parakeet** (the default when its model is on disk): more accurate, 25 languages, about
  1.1 GB of memory. When Parakeet is also the main model, both share one copy.
- **Whistle** ([Cactus](https://huggingface.co/Cactus-Compute/whistle), 17 MB, CPU): light;
  English, German, French, Spanish, Italian, Dutch and Polish.

Turn the preview off in Settings (`live_preview = false`). Details and measurements:
[docs/live-preview.md](docs/live-preview.md).

## Settings (all optional)

Settings in the window save them to `~/.config/open-whisper/config.toml`:

```toml
model = "large-v3-turbo-q5_0"   # empty or missing: the default for this machine
language = "auto"
idle_unload_minutes = 15        # free the model's memory after 15 idle minutes; 0 = never
hotkey = "SUPER + CTRL + X"
live_preview = true             # words in the overlay while you talk
preview_engine = "auto"         # auto (Parakeet when on disk, else Whistle) | whistle | parakeet
```

A model that the app no longer has (the faster-whisper `fw:` models) falls back to the
default. Command line: `--model <id>` (overrides the file), `--language <code>`, `--hotkey "…"`,
`--no-hotkey`, `--background`, `--transcribe <file>` (prints JSON, no window),
`--print-snippet [lua|conf]`, `--remove-hotkey`, `--dictate press|release|toggle|cancel|quit`.

## Models

The Whisper models run in whisper.cpp, Parakeet in sherpa-onnx (onnxruntime, CPU). Numbers are from the development machine (Ryzen 7 H 255,
Radeon 780M; [docs/tauri-desktop-options.md](docs/tauri-desktop-options.md)):

| Model | Download | GPU (Vulkan) | CPU only | WER | Notes |
|---|---|---|---|---|---|
| large-v3-turbo-q5_0 | 0.55 GB | 10.9× real time | 1.4× | 3.2% | **default with a GPU** |
| large-v3-turbo | 1.6 GB | 8.9× | 1.3× | 3.9% | |
| medium | 1.5 GB | 6.6× | 1.7× | 3.2% | |
| small | 0.47 GB | 17.6× | 5.6× | 12.7% | **default without a GPU** |
| parakeet-v3 | 0.67 GB | — | **35.9×** | 14.3%* | NVIDIA Parakeet (CPU, 25 languages): a 3 s dictation in 0.14 s instead of 1.56 s |

\*Parakeet: 0–5.7% on four of the five clips, 49% on the extreme-noise English one; in Spanish
it makes 2–4× more errors than the default. Details: [docs/parakeet.md](docs/parakeet.md).

A model that is not on disk downloads the first time you use it, into
`~/.local/share/open-whisper/models`. Silero VAD drops silence, so silent audio gives no text.

## How it works

`desktop/src-tauri/src/` (Rust):

- `engine.rs` loads one model at a time (download with progress, idle unload) and runs
  whisper.cpp with VAD (threshold 0.3) and beam size 5. `jobs.rs` is a queue with one worker;
  dictation goes first.
- `mic.rs` records the microphone (cpal); `dictation.rs` is the global key's state machine;
  `output.rs` types and copies the text; `hyprland.rs` adds the key and the overlay rule.
- `preview.rs` runs the live preview (Whistle) on its own thread.
- `app.rs` has the windows, the tray, the commands for the page and the single instance (a
  second launch hands its command or file to the running app).

`frontend/` (Vite + TypeScript) has the window's page (`index.html`, `src/main.ts`) and the
overlay (`overlay.html`, `src/overlay.ts`).

## Develop

```sh
make desktop-dev   # the app with hot reload (Vite + tauri dev)
make test          # UI typecheck + build, Rust unit tests
make e2e-desktop   # the app in a nested Hyprland (your session is not touched): files,
                   # errors, dictation, the global key, live preview, Settings; screenshots 09-20.
                   # python3 scripts/e2e_desktop.py --first-run: empty model folder
                   # OW_E2E_HYPRBARS=<hyprbars.so>: also checks that title-bar plugins skip the overlay
make bench         # research only: the model benchmark (docs/benchmark.md), Python via uv
```

The real preview engine test: `cd desktop/src-tauri && cargo test --lib real_whistle -- --ignored`.
Dependencies are pinned in `desktop/src-tauri/Cargo.lock` and `frontend/package-lock.json`.

## Privacy

Audio and transcripts stay on your computer. The only network access is the one-time model
download from Hugging Face (the live preview engine and model come from pinned revisions,
checked with SHA-256; that engine has no network code).

## Licenses

- [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT), Whisper model weights by
  OpenAI (MIT), Silero VAD (MIT).
- Parakeet: [NVIDIA Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
  weights (CC-BY-4.0), run with [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx)
  (Apache-2.0) and onnxruntime (MIT).
- Live preview: [Cactus Whistle and the needle engine](https://huggingface.co/Cactus-Compute/whistle)
  (Apache-2.0).
- Tauri, Vite and TypeScript are open source (MIT/Apache).
- Benchmark only: [faster-whisper](https://github.com/SYSTRAN/faster-whisper) (MIT) and
  [Piper](https://github.com/rhasspy/piper) TTS (MIT).
