# Meetings

The Meetings section records a call or a talk and shows it as text next to your notes, like
the AI note-takers that join no call (Granola and others). Here everything runs on the
computer: no account, no cloud, and the audio is not kept.

This is phase 1: recording, live transcript, notes, saving and export, on Linux. Phase 2 adds
notes written by a language model (a local server such as Ollama or LM Studio by default; a
cloud API only when you choose it, with a clear warning). Phase 3 adds the computer's sound on
macOS and Windows, a prompt when a call starts, and questions over past meetings.

## The meeting key and the strip

- Settings → Shortcuts → **Meeting key**: Super+Ctrl+M on Linux, Ctrl+Alt+M on macOS and Windows
  (`meeting_hotkey` in config.toml). It runs `lowkey --meeting toggle`: a tap starts a
  meeting, the next tap stops and saves it. A tap while it starts is remembered (it stops
  right after the start). On Hyprland a closed app starts in the background for it; on X11,
  macOS and Windows the key lives in the running app.
- Hyprland: one bind (no release bind), described "Meeting recording (Lowkey, added by
  the app)". The dictation key and the meeting key never share a combination: each counts the
  other's bind as someone else's, and the app refuses the dictation key as the meeting key.
  X11, macOS, Windows: the global-shortcut plugin, as for dictation.
- The **strip** is the dictation overlay (same window, same Hyprland rule: floating, pinned,
  never focused). While a meeting records it shows a red dot, the time, a two-colour wave (Me
  above the line in light grey, Them below it in steel blue) and the newest line, its speaker in its
  colour. At Stop it says "Saving the
  meeting…", then "Meeting saved" for 3 s. A dictation during a meeting takes the strip and
  gives it back when it is done. A meeting started in the window shows the strip too (with no
  keys at all, the app adds the strip's window rule itself). The time is not in the
  screen-reader region, so it is not read out every second.
- If the meeting key cannot be added (e.g. `config.toml` gives it the dictation key's
  combination), Settings → Shortcuts shows it "not active" with the reason; moving the dictation key
  adds it again.
- A meeting started with the key gets the title "Meeting"; the list shows its date.

## The Meetings section

- The list is grouped by day (Today, Yesterday, then dates), newest first, with the time, the
  length, the first words, and marks for "Recording", "Interrupted" and "AI notes". With no
  meetings, it says how to start, and names the meeting key.
- One meeting: **AI notes | Transcript**, side by side and the full height of the window,
  each scrolling on its own (the section has no width limit; lines stay under ~75 characters);
  under 720 px one above the other. While it records, a panel shows the clock, the two-colour
  wave (the same colours as the transcript's Me and Them) and Stop (it has the focus after
  Start), and the AI notes column says the notes come when it stops. Esc goes back to the list.
- There is no notes field: the AI notes take its place. Notes typed in older versions stay in
  the file, in the export and in what the model reads.

## Two sources

| Source | Label | How |
|---|---|---|
| Microphone | **Me** | cpal, the same as dictation (`mic::new_tap`: no samples kept, no 10-minute limit) |
| Computer's sound | **Them** | Linux: the monitor of the default output (`@DEFAULT_MONITOR@`) through `libpulse-simple.so.0`, loaded at run time. PipeWire serves it through pipewire-pulse. 16 kHz mono float, 100 ms blocks |

Without the library (or without a sound server), the meeting records the microphone only and
says so. macOS (ScreenCaptureKit) and Windows (WASAPI loopback) are not built yet.

## From audio to lines

`meeting.rs`, `Chunker`, one per source:

- Frames of 100 ms. A frame with an RMS level under 0.003 is **silence** (nothing plays gives 0).
- Silence in front of speech is dropped (0.3 s stays), so a short word after a long quiet
  time is kept. Audio with no frame above the silence level is never transcribed.
- "Loud" is the level that 80% of the non-silent frames stay under. A piece ends at a
  **pause** (6 frames under 20% of "loud") once it has **4 s**; long speech is cut at **20 s**,
  at the quietest frame of the last quarter.
- Each block carries its time by the clock: when a source stops sending for over a second, its
  next audio starts at the right time.

One thread sends the pieces, in order, to the model chosen in Settings → Models (Parakeet or
Whisper; the VAD drops pieces with no speech). `Engines` runs one transcription at a time, so
the meeting, dictation and files share the model safely. Lines are kept in time order; sound
labels such as `[Music]` are dropped.

**Echo.** Without headphones, the microphone also hears the others, at the same time. A "Me"
line of 3 or more words is removed when it lies inside the time of the "Them" lines (1.5 s of
slack) and 80% of its words are in them, in the same order. Words that you repeat after the
others spoke stay. Chinese and Japanese count each character as a word.

## Saving

- A meeting is saved when it starts, after each piece, and when your notes change (0.6 s after
  you stop typing), with its length so far. One write at a time; each file is synced before it
  replaces the old one. A crash keeps what was transcribed; the list then says "interrupted".
  When a save of your notes fails, the page keeps them and tries again at the next change.
- Files: `~/.local/share/lowkey/meetings/m-<unix ms>.json` (the folder is 0700). The id
  is `m-` plus digits only, so a command never names a path outside the folder.
- Quitting the app during a recording keeps what is transcribed and does not wait for the rest.
- **Export .md** and **Copy**: the title, the date in your time zone, the length, your notes,
  then the transcript in paragraphs (`**Me** (00:12): …`).

## Measured

The e2e test (`scripts/e2e_desktop.py`, nested Hyprland, fake sources: `hello.ogg` as the mic,
`tests/fixtures/jfk.ogg` (JFK's inaugural address, public domain) as the computer's sound,
Whisper large-v3-turbo-q5_0 on the Radeon 780M): text from both sources 13 s after the start;
Stop saved in under a second (the model kept up, so nothing waited). Screenshots:
`docs/screenshots/21-desktop-meeting-live.png`, `22-desktop-meeting-saved.png`.

The real monitor read is checked by an ignored unit test (it reads 1 s, plays nothing, keeps
nothing): `cd desktop/src-tauri && cargo test --lib system_audio -- --ignored`.

## AI notes

`llm.rs`. A language model reads the meeting as Markdown (title, date, your notes, the
transcript; never older AI notes) and writes: Summary, Decisions, Action items (**who** — what —
when), Open questions, Numbers. The language is named in the instructions (Settings → General,
else found in the transcript with whatlang): with "the language of the meeting" two of three
models wrote English notes for a Spanish meeting (bench/summary/results.md).

- **Built in**: Qwen3.5 4B Q4_K_M (Apache-2.0, 2.74 GB, SHA-256 pinned, HF revision e87f176) and
  llama.cpp's `llama-server` from the pinned release b12047 (Vulkan on Linux and Windows, Metal
  on macOS; SHA-256 as GitHub publishes it). Both download on the first **Write notes** with
  progress in the meeting, into `~/.local/share/lowkey/models/llm`. The server runs in its
  own process on 127.0.0.1 with a random 192-bit API key from the system's random generator,
  given through its environment (`LLAMA_API_KEY`; the command line is readable by other users,
  the environment is not), so no web page and no other user can use it. 64K context (about four
  hours of meeting), all layers on the GPU when the app found one. It stops 2 minutes after the
  last use (never during a job), on Cancel, and when the app quits; a pid file lets the next
  start stop one that a crash or a kill left behind (only if that pid is our llama-server).
- **The built-in model** (Settings → Models → AI notes → Notes model, `ai_builtin_model`; its status, "Ready · Qwen3.5 4B
  on GPU" or "downloads on first use", is next to the heading): Qwen3.5 4B
  (`qwen3.5-4b`, 2.7 GB, recommended), Qwen3.5 2B (`qwen3.5-2b`, 1.3 GB) or Gemma 4 E4B
  (`gemma-4-e4b`, 5.0 GB), each pinned to a Hugging Face commit and its SHA-256, with the
  note of its test in the menu. A model downloads the first time it writes notes; the runner
  restarts with the new model when the choice changes.
- **Own server**: any OpenAI-compatible API (`ai_url`, `ai_model`, `ai_key`). Ollama:
  `http://localhost:11434/v1`, LM Studio: `http://localhost:1234/v1`. Settings warns when the
  address is not on this computer (the address is parsed as the HTTP client parses it). The key
  stays in config.toml (written 0600 from its first byte) and never goes to the page.
- The text streams into the meeting while it is written. **Cancel** works at every step: the
  download (the partial file stays and resumes), the start, the model's reading, the writing.
  One job at a time; deleting the meeting cancels its job. Notes cut at the length limit say so.
- The model reads the title, the length, your notes and the transcript, but not the date (the
  app does not always know your time zone, and a wrong date would end up in the notes).
- **Automatic**: when a meeting stops (`ai_auto`, on by default). The first time, the built-in
  model downloads first (2.7 GB, progress in the meeting; Cancel stops it).
- The notes are saved in the meeting file (`ai_notes`, `ai_model`, `ai_at`); Copy and Export put
  them first, as "AI notes (model, a draft)", their headings one level down.
- The e2e test (cached model): notes written 7–9 s after Stop for a 16-s test meeting, streamed,
  saved; screenshot `docs/screenshots/25-desktop-meeting-ai-notes.png`.

## Limits now

- **Stop waits for the pieces that are not transcribed yet.** A model slower than the speech
  (Whisper turbo on a CPU: two sources need 2× real time) falls behind; the meeting then shows
  a notice, and Stop can take long. Parakeet (36× real time on the CPU) and Small keep up.
- The list reads every meeting file, and each live update carries the whole meeting. Fine for
  tens of meetings and calls of a few hours; a summary index comes when it is needed.
- If the sound server hangs in a read, stopping does not wait for it more than 1 s; that
  thread ends when the read returns.

Review 14 (`docs/reviews/14-30b299c.md`) lists what was checked and fixed.
