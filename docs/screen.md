# Screen video with meetings

A meeting can also record the screen. The video is saved with the meeting, and a moment of
the transcript plays what was on the screen then. Linux only for now; off by default
(Settings → General → Record the screen with meetings).

## Use

- Turn it on in Settings → General. Settings says what it found: "Ready: VP8 on the CPU", or
  what to install.
- Start a meeting as usual. The audio starts at once. The first time, your desktop's
  screen-sharing dialog asks what to share (a screen or one window); the video joins the
  meeting when you choose. The next meetings use the same choice without the dialog.
  **Choose the screen again** (in Settings) makes the next meeting ask again.
- While it records, the live panel says "your microphone, the computer's sound and the screen"
  and shows a screen mark; the strip shows a small screen before the time.
- Closing the dialog, or a missing encoder, does not stop the meeting: it records the audio
  only, and a notice says why. If the sharing is stopped during the meeting (from the desktop),
  the video so far is kept, with a notice.
- In a saved meeting the player is above the notes and the transcript. **Screen video**
  collapses it (the choice is kept). Each transcript time plays the video from there; under a
  point of the notes, each place in the transcript has **Play**. While the video plays, the
  paragraph said at that moment is marked (and kept in view in the Transcript tab).
- The note-app file has `video: <path>` in its front matter (the video stays in the app's
  folder; it is not copied). AI agents get `video_path` from `get_meeting`.

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
  through the same pipeline (a GPU encoder can be installed and still fail):

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
  meeting's start (the transcript's time 0) is saved as `video.offset`: a moment `t` of the
  transcript is `t - offset` in the video (about ±0.3 s).
- **Stop**: SIGINT, GStreamer ends the stream (EOS) and closes the file correctly; after 5 s
  it is killed. The portal session is closed (the desktop's "sharing" sign goes).
- **Files**: while it records, `.m-<id>.webm.part` (or `.mp4.part`), mode 0600, in the meetings
  folder; at Stop, `m-<id>.webm`. The meeting's JSON has
  `video: { file, offset, width, height, codec, bytes }`. Deleting the meeting deletes the
  video.
- **A crash**: GStreamer gets SIGINT from the kernel when the app ends (`PR_SET_PDEATHSIG`), so
  its file is closed. At the next start a `.part` file left behind is written again (remuxed,
  no new encoding) so that it has its length and can be seeked, gets its final name and its
  size in the meeting. A `.part` without its meeting is deleted.
- **Playback**: WebKitGTK plays media only from http(s) and file addresses (through a custom
  scheme such as Tauri's asset protocol its player fails: "source not supported"). So the app
  serves the videos itself (`src/video_server.rs`): on 127.0.0.1 only, a random port, a random
  128-bit key in every address, GET and HEAD of `m-<id>.webm|mp4` in the meetings folder only,
  with byte ranges for the seeks. It starts when the first video is played. The CSP allows
  `media-src http://127.0.0.1:*`. WebKitGTK plays the file with the same GStreamer that wrote it.

## Tests

- Unit tests (`cargo test --lib`): the encoder choice, the pipeline text (the path quoted for
  gst-launch), the size, the files left by a crash, a real 2 s recording of a test picture
  stopped with SIGINT, and one killed and then repaired; in `meeting.rs`, the video joining a
  meeting (its offset), its final name at Stop, a screen chosen after Stop (discarded), a file
  that is not the meeting's own (never played), and the delete.
- `OPENWHISPER_FAKE_SCREEN=1` records a test picture (`videotestsrc`) instead of the screen,
  with no portal. The desktop end-to-end test (`scripts/e2e_desktop.py`) uses it: the video is
  saved with the meeting, the window loads it, and a click on a transcript time seeks it (the
  page reports its player's events to the test log).
- The real portal opens the desktop's picker, so it is never in the automated tests:
  `cargo test --lib real_portal -- --ignored --nocapture` (by hand; `OW_SCREEN_TOKEN=<token>`
  from its output checks that the next start needs no picker).

## Limits now

- Linux only. macOS (ScreenCaptureKit) and Windows (Windows.Graphics.Capture) are not done.
- One screen or one window, no sound in the video (the meeting's audio is the transcript's).
- About ±0.3 s between the video and the transcript.
- The video is not in the note-app folder, only its path.
