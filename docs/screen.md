# Screen video with meetings

During a meeting you can also record the screen, when you want and as often as you want. The
videos are saved with the meeting, and a moment of the transcript shows what was on the screen
then. Linux only for now.

## Use

- During a meeting, **Record screen**: in the meeting's live panel (next to Stop), or the
  screen button on the strip (before the mute button). The audio goes on as it was. The first
  time, your desktop's screen-sharing dialog asks what to share (a screen or one window); the
  video starts when you choose. The next times use the same choice without the dialog.
- **Stop screen** (the same button, red while the screen records) stops the video; the meeting
  goes on. **Record screen** again starts one more video of the same meeting.
- While the screen records, the live panel says "your microphone, the computer's sound and the
  screen"; the strip shows a small screen before the time.
- Closing the dialog, or a missing encoder, does not stop the meeting: a notice says why. If
  the sharing is stopped from the desktop, the video so far is kept, with a notice, and
  **Record screen** can start a new one.
- Settings → Meetings → **Screen video** says what this computer has ("Ready: VP8 on the CPU", or
  what to install, with **Check again**). **Choose the screen again** makes the next Record
  screen ask again.
- In a saved meeting the video is on the left, the chat under it, and the notes and the
  transcript on the right. Under 900 pixels of width: one column, the video on top (it stays in
  view while the rest scrolls), then the notes and the transcript, then the chat. **Screen
  video** collapses it (the choice is kept). A meeting with more than one video has a button for
  each, with its time; at the end of one, the next one plays.
- **The transcript and the video follow each other.** A click on a paragraph moves the video to
  its moment (playing or not, as it was); a double click (or Enter) fixes the text. Each time in
  the transcript plays the video from there. Under a point of the notes, each place in the
  transcript has **Play**, and **Show in transcript** shows the video at that moment too. While
  the video plays, or when you move it, the paragraph said at that moment is marked and kept in
  view in the Transcript tab (not while you scroll the transcript yourself). Paragraphs said
  while the screen was not recorded have no video: a click on them fixes the text.
- The note-app file has `videos:` (their paths) in its front matter (the videos stay in the
  app's folder; they are not copied). AI agents get `videos` (path, offset, length) from
  `get_meeting`.

## How it works

- **The screen** comes from the desktop's ScreenCast portal (xdg-desktop-portal, through the
  `ashpd` crate): monitor or window, the cursor in the picture, "persist until revoked". The
  portal's restore token is saved as `screen_token` in `config.toml`; each start gives a new
  one, which replaces it.
- **The encoder** runs in its own process: `gst-launch-1.0 -e pipewiresrc fd=3 path=<node> !
  videorate ! 10 fps ! videoconvert ! videoscale ! <size> ! queue ! <encoder> ! <muxer> !
  filesink`. The portal's PipeWire connection is its file descriptor 3. A crash in a GPU driver
  or a plugin cannot close the app (as llama-server for the notes). `keepalive-time` sends the
  last frame again when the screen does not change, so the video has no holes; the queue drops
  old frames when the encoder is slower than the screen.
- **Encoder**: the first one that works, best first. Each is checked with two test frames
  through the same pipeline (a GPU encoder can be installed and still fail), and the player
  must have a decoder for its codec (AV1: dav1d, VP9/VP8: libvpx, H.264: libav or OpenH264, or
  the VA one):

  | Encoder | File | Where | Package (Arch) |
  |---|---|---|---|
  | `vaav1enc` | WebM (AV1) | GPU | gst-plugin-va |
  | `vavp9enc` | WebM (VP9) | GPU | gst-plugin-va |
  | `vah264enc` | fragmented MP4 (H.264) | GPU | gst-plugin-va |
  | `vp8enc` | WebM (VP8) | CPU, realtime settings | gst-plugins-good |
  | `x264enc` | fragmented MP4 (H.264) | CPU | gst-plugins-ugly |
  | `openh264enc` | fragmented MP4 (H.264) | CPU | gst-plugins-bad + openh264 |

  Only containers that can be read after a kill. `pipewiresrc` comes with PipeWire's GStreamer
  plugin (`gst-plugin-pipewire`).
- **Size**: 10 frames a second, at most 1920 pixels wide (the same shape, even numbers), about
  1 Mbit/s: about 450 MB for one hour. A window that changes size keeps the video's size, with
  borders.
- **Time**: the moment GStreamer starts to play is the video's time 0. Its distance from the
  meeting's start (the transcript's time 0) is saved as its `offset`: a moment `t` of the
  transcript is `t - offset` in that video (about ±0.3 s). Its `duration` is from then to the
  end of its stream (0 after a crash: the next video's start, or the meeting's end, is used).
- **One at a time**: Record screen opens the picker only when no video records or is being
  saved. Each start gets a new number, never used again in the meeting: an answer from a picker
  that was cancelled (Stop screen while it was open) is discarded, never taken for the next one.
  The meeting's Stop waits for a Stop screen still on its way.
- **Stop** (Stop screen, the meeting's Stop, or the sharing stopped from the desktop): SIGINT,
  GStreamer ends the stream (EOS) and closes the file correctly; after 5 s it is killed. The
  portal session is closed (the desktop's "sharing" sign goes).
- **Files**: each Record screen is one file, `m-<id>-<n>` (n = 1, 2…). While it records,
  `.m-<id>-<n>.webm.part` (or `.mp4.part`), mode 0600, in the meetings folder; at Stop screen
  (or the meeting's Stop), `m-<id>-<n>.webm`. The meeting's JSON has a list,
  `videos: [{ file, offset, duration, width, height, codec, bytes }]`. Deleting the meeting
  deletes them.
- **A crash**: GStreamer gets SIGINT from the kernel when the app ends (`PR_SET_PDEATHSIG`; it
  ignores SIGPIPE, because its output then goes nowhere), so its file is closed. At the next start a `.part` file left behind is written again (remuxed,
  no new encoding, 60 s at most) so that it has its length and can be seeked, gets its final name and its
  size in the meeting. If the crash came before it joined, it is added to the list with
  `offset: -1` (its time is not known): its own button plays it, the transcript does not move
  to it. A `.part` without its meeting is deleted.
- **Playback**: WebKitGTK plays media only from http(s) and file addresses (through a custom
  scheme such as Tauri's asset protocol its player fails: "source not supported"). So the app
  serves the videos itself (`src/video_server.rs`): on 127.0.0.1 only, a random port, a random
  128-bit key in every address, GET and HEAD of `m-<id>-<n>.webm|mp4` in the meetings folder only,
  with byte ranges for the seeks. It starts when the first video is played. The CSP allows
  `media-src http://127.0.0.1:*`. WebKitGTK plays the file with the same GStreamer that wrote it.

## Tests

- Unit tests (`cargo test --lib`): the encoder choice, the pipeline text (the path quoted for
  gst-launch), the size, the files left by a crash, a real 2 s recording of a test picture
  stopped with SIGINT, and one killed and then repaired, the video names; in `meeting.rs`, a
  meeting with two videos: Record screen (one at a time), the first joining (its offset), Stop
  screen (its final name and length; the meeting goes on), a closed picker (a notice), Stop
  screen while the picker is open (its answer discarded), a second video saved at Stop, a
  screen chosen after Stop (discarded), a file that is not the meeting's own (never played),
  and the delete.
- `OPENWHISPER_FAKE_SCREEN=1` records a test picture (`videotestsrc`) instead of the screen,
  with no portal. The desktop end-to-end test (`scripts/e2e_desktop.py`) uses it: the strip's
  screen button records, stops and records again; the two videos are saved with the meeting;
  the window loads the first beside the transcript; a click on a paragraph moves the video to
  its moment and marks it; a click on a transcript time plays it (the page reports its player's
  events to the test log).
- The real portal opens the desktop's picker, so it is never in the automated tests:
  `cargo test --lib real_portal -- --ignored --nocapture` (by hand; `OW_SCREEN_TOKEN=<token>`
  from its output checks that the next start needs no picker).

## Limits now

- Linux only. macOS (ScreenCaptureKit) and Windows (Windows.Graphics.Capture) are not done.
- One screen or one window, no sound in the video (the meeting's audio is the transcript's).
- About ±0.3 s between the video and the transcript.
- The videos are not in the note-app folder, only their paths.
