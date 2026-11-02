# Meetings

The Meetings section records a call or a talk and shows it as text next to your notes, like
the AI note-takers that join no call (Granola and others). Here everything runs on the
computer: no account, no cloud, and the audio is not kept.

This is phase 1: recording, live transcript, notes, saving and export, on Linux. Phase 2 adds
notes written by a language model (a local server such as Ollama or LM Studio by default; a
cloud API only when you choose it, with a clear warning). Phase 3 adds the computer's sound on
macOS and Windows, a prompt when a call starts, and questions over past meetings.

## The meeting key and the strip

- Settings → Keys → **Meeting key**: Super+Ctrl+M on Linux, Ctrl+Alt+M on macOS and Windows
  (`meeting_hotkey` in config.toml). It runs `open-whisper --meeting toggle`: a tap starts a
  meeting, the next tap stops and saves it. A closed app starts in the background for it.
- Hyprland: one bind (no release bind), described "Meeting recording (open-whisper, added by
  the app)". The dictation key and the meeting key never share a combination: each counts the
  other's bind as someone else's, and the app refuses the dictation key as the meeting key.
  X11, macOS, Windows: the global-shortcut plugin, as for dictation.
- The **strip** is the dictation overlay (same window, same Hyprland rule: floating, pinned,
  never focused). While a meeting records it shows a red dot, the time, the Me/Them levels and
  the newest line ("Them: …", the speaker always shows). At Stop it says "Saving the
  meeting…", then "Meeting saved" for 3 s. A dictation during a meeting takes the strip and
  gives it back when it is done. A meeting started in the window shows the strip too.
- A meeting started with the key gets the title "Meeting"; the list shows its date.

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

One thread sends the pieces, in order, to the model chosen in Settings → Model (Parakeet or
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
- Files: `~/.local/share/open-whisper/meetings/m-<unix ms>.json` (the folder is 0700). The id
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

## Limits now

- **Stop waits for the pieces that are not transcribed yet.** A model slower than the speech
  (Whisper turbo on a CPU: two sources need 2× real time) falls behind; the meeting then shows
  a notice, and Stop can take long. Parakeet (36× real time on the CPU) and Small keep up.
- The list reads every meeting file, and each live update carries the whole meeting. Fine for
  tens of meetings and calls of a few hours; a summary index comes when it is needed.
- If the sound server hangs in a read, stopping does not wait for it more than 1 s; that
  thread ends when the read returns.

Review 14 (`docs/reviews/14-30b299c.md`) lists what was checked and fixed.
