# Meetings

The Meetings section records a call or a talk and shows it as text next to your notes, like
the AI note-takers that join no call (Granola and others). Here everything runs on the
computer: no account, no cloud, and the audio is not kept.

This is phase 1: recording, live transcript, notes, saving and export, on Linux. Phase 2 adds
notes written by a language model (a local server such as Ollama or LM Studio by default; a
cloud API only when you choose it, with a clear warning). Phase 3 adds the computer's sound on
macOS and Windows, a prompt when a call starts, and questions over past meetings.

## Two sources

| Source | Label | How |
|---|---|---|
| Microphone | **Me** | cpal, the same as dictation (`mic::new_tap`: no samples kept, no 10-minute limit) |
| Computer's sound | **Them** | Linux: the monitor of the default output (`@DEFAULT_MONITOR@`) through `libpulse-simple.so.0`, loaded at run time. PipeWire serves it through pipewire-pulse. 16 kHz mono float, 100 ms blocks |

Without the library (or without a sound server), the meeting records the microphone only and
says so. macOS (ScreenCaptureKit) and Windows (WASAPI loopback) are not built yet.

## From audio to lines

`meeting.rs`, `Chunker`, one per source:

- Frames of 100 ms. "Loud" is the level that 80% of the frames stay under.
- A piece ends at a **pause** (6 frames under 20% of "loud") once it has **4 s**; long speech
  is cut at **20 s**, at the quietest frame of the last quarter.
- **Silence** (loud under RMS 0.003, e.g. nothing plays) is dropped and never transcribed.
- Each block carries its time by the clock: when a source stops sending for over a second, its
  next audio starts at the right time.

One thread sends the pieces, in order, to the model chosen in Settings → Model (Parakeet or
Whisper; the VAD drops pieces with no speech). `Engines` runs one transcription at a time, so
the meeting, dictation and files share the model safely. Lines are kept in time order; sound
labels such as `[Music]` are dropped.

**Echo.** Without headphones, the microphone also hears the others. A "Me" line of 3 or more
words is removed when 60% of its words are in "Them" lines within 3 s of it.

## Saving

- A meeting is saved when it starts, after each piece, and when your notes change (0.6 s after
  you stop typing). A crash keeps what was transcribed; the list then says "interrupted".
- Files: `~/.local/share/open-whisper/meetings/m-<unix ms>.json` (the folder is 0700). The id
  is `m-` plus digits only, so a command never names a path outside the folder.
- Quitting the app during a recording keeps what is transcribed and does not wait for the rest.
- **Export .md** and **Copy**: the title, the date in your time zone, the length, your notes,
  then the transcript in paragraphs (`**Me** (00:12): …`).

## Measured

The e2e test (`scripts/e2e_desktop.py`, nested Hyprland, fake sources: `hello.ogg` as the mic,
`bench/.work/samples/en_jfk.wav` as the computer's sound, Whisper large-v3-turbo-q5_0 on the
Radeon 780M): text from both sources 13 s after the start; Stop saves at once. Screenshots:
`docs/screenshots/21-desktop-meeting-live.png`, `22-desktop-meeting-saved.png`.

The real monitor read is checked by an ignored unit test (it reads 1 s, plays nothing, keeps
nothing): `cd desktop/src-tauri && cargo test --lib system_audio -- --ignored`.
