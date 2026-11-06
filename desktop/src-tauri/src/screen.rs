//! Screen video with meetings (Linux): the screen (or a window) as a video next to the meeting,
//! so a place in the transcript or in the notes plays what was on the screen then.
//!
//! The screen comes from the desktop's ScreenCast portal (xdg-desktop-portal): the first time it
//! shows its picker; its "restore token" (config `screen_token`) chooses the same screen the
//! next times, without the picker. The video is made by GStreamer in its own process
//! (`gst-launch-1.0 -e`), as the built-in notes model runs in llama-server: a crash in a GPU
//! driver or a plugin cannot close the app. The portal's PipeWire connection is that process's
//! file descriptor 3. Stop: SIGINT, GStreamer then ends the stream (EOS) and closes the file
//! correctly; after 5 s it is killed. If the app itself stops (a crash, a kill), the process
//! gets SIGINT from the kernel (PR_SET_PDEATHSIG) and closes its file too.
//!
//! 10 frames a second, at most 1920 pixels wide, about 1 Mbit/s: about 450 MB for one hour.
//! Encoder: the first that works of `ENCODERS` (GPU first). Only containers that can be read
//! after a kill: WebM, fragmented MP4. A file that was not closed correctly is written again
//! (remuxed, no new encoding) so that it gets its length and can be seeked.
//!
//! While it records the file is `.m-<id>.<ext>.part` (mode 0600) in the meetings folder; at Stop
//! it becomes `m-<id>.<ext>`. Files left by a crash are made whole at the next start
//! (`recover`).
//!
//! OPENWHISPER_FAKE_SCREEN=1 records a test picture (`videotestsrc`) instead of the screen, with
//! no portal: for the automated tests (they never open the real picker).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;

/// This build can record the screen (the portal and PipeWire are Linux only).
pub const SUPPORTED: bool = cfg!(target_os = "linux");

/// Frames a second: enough to read a slide or a document, small on disk.
pub const FPS: u32 = 10;
/// Wider screens are scaled down to this width.
pub const MAX_WIDTH: u32 = 1920;
/// The test picture's size (OPENWHISPER_FAKE_SCREEN).
const FAKE_SIZE: (u32, u32) = (1280, 720);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Container {
    Webm,
    /// Fragmented MP4: each second is readable on its own.
    Mp4,
}

impl Container {
    pub fn ext(self) -> &'static str {
        match self {
            Container::Webm => "webm",
            Container::Mp4 => "mp4",
        }
    }

    /// The muxer, as gst-launch writes it.
    fn muxer(self) -> &'static str {
        match self {
            Container::Webm => "webmmux",
            Container::Mp4 => "mp4mux fragment-duration=1000",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Encoder {
    /// The GStreamer element (also the first word of `launch`).
    pub element: &'static str,
    /// The encoder and what comes after it up to the muxer, as gst-launch writes it.
    pub launch: &'static str,
    /// The raw format the encoder takes ("" = any).
    pub format: &'static str,
    pub container: Container,
    /// For people: "AV1", "VP8"…
    pub codec: &'static str,
    pub gpu: bool,
    /// The elements it needs besides the common ones (parsers, the muxer).
    pub needs: &'static [&'static str],
    /// Decoders of its codec (one is enough): the player (WebKitGTK's GStreamer) must play it.
    pub plays: &'static [&'static str],
}

const AV1_DEC: &[&str] = &["dav1ddec", "av1dec", "vaav1dec"];
const VP9_DEC: &[&str] = &["vp9dec", "vavp9dec"];
const H264_DEC: &[&str] = &["avdec_h264", "openh264dec", "vah264dec"];

/// Best first. The GPU ones (VA-API) are in gst-plugins-bad; vp8enc in gst-plugins-good.
pub const ENCODERS: &[Encoder] = &[
    Encoder { element: "vaav1enc", launch: "vaav1enc bitrate=800 key-int-max=100 ! av1parse", format: "NV12", container: Container::Webm, codec: "AV1", gpu: true, needs: &["av1parse", "webmmux"], plays: AV1_DEC },
    Encoder { element: "vavp9enc", launch: "vavp9enc bitrate=1000 key-int-max=100", format: "NV12", container: Container::Webm, codec: "VP9", gpu: true, needs: &["webmmux"], plays: VP9_DEC },
    Encoder { element: "vah264enc", launch: "vah264enc bitrate=1000 key-int-max=100 ! h264parse", format: "NV12", container: Container::Mp4, codec: "H.264", gpu: true, needs: &["h264parse", "mp4mux"], plays: H264_DEC },
    Encoder {
        element: "vp8enc",
        launch: "vp8enc deadline=1 cpu-used=8 target-bitrate=1000000 keyframe-max-dist=100 threads=4",
        format: "I420",
        container: Container::Webm,
        codec: "VP8",
        gpu: false,
        needs: &["webmmux"],
        plays: &["vp8dec"],
    },
    Encoder { element: "x264enc", launch: "x264enc tune=zerolatency speed-preset=veryfast bitrate=1000 key-int-max=100 ! h264parse", format: "I420", container: Container::Mp4, codec: "H.264", gpu: false, needs: &["h264parse", "mp4mux"], plays: H264_DEC },
    Encoder { element: "openh264enc", launch: "openh264enc bitrate=1000000 complexity=low rate-control=bitrate ! h264parse", format: "I420", container: Container::Mp4, codec: "H.264", gpu: false, needs: &["h264parse", "mp4mux"], plays: H264_DEC },
];

/// Every pipeline has these (gst-plugins-base, and pipewiresrc of PipeWire).
const COMMON: &[&str] = &["videorate", "videoconvert", "videoscale", "filesink"];

impl Encoder {
    /// "VP8 on the CPU".
    pub fn label(&self) -> String {
        format!("{} on the {}", self.codec, if self.gpu { "GPU" } else { "CPU" })
    }
}

/// The first encoder of `ENCODERS` that `works` (each is asked in turn, best first).
pub fn choose(works: impl Fn(&Encoder) -> bool) -> Option<&'static Encoder> {
    ENCODERS.iter().find(|e| works(e))
}

// ------------------------------------------------------------------ pipelines
/// Where the picture comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// The portal's stream: its PipeWire node on the connection passed as fd 3.
    PipeWire { node: u32 },
    /// A test picture (OPENWHISPER_FAKE_SCREEN, the encoder check).
    Test { frames: Option<u32> },
}

/// The video's size: at most `MAX_WIDTH` wide, the same shape, even numbers (H.264 needs them).
pub fn video_size(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (width.max(2), height.max(2));
    let (w, h) = if w > MAX_WIDTH { (MAX_WIDTH, (h as u64 * MAX_WIDTH as u64 / w as u64) as u32) } else { (w, h) };
    (w & !1, (h & !1).max(2))
}

/// A path in a gst-launch pipeline: gst-launch joins its arguments with spaces and reads them
/// again, so a path is quoted.
fn quoted(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The arguments of `gst-launch-1.0 -e` (without the program): from `source` to the file `out`,
/// `size` pixels, `FPS` frames a second.
pub fn pipeline(source: &Source, enc: &Encoder, size: (u32, u32), out: &Path) -> Vec<String> {
    let (w, h) = size;
    let src = match source {
        // keepalive-time: a screen that does not change sends no frames; the last one is sent
        // again, so the video has no holes.
        Source::PipeWire { node } => format!("pipewiresrc fd=3 path={node} do-timestamp=true keepalive-time=1000"),
        Source::Test { frames } => {
            let n = frames.map(|n| format!(" num-buffers={n}")).unwrap_or_default();
            format!("videotestsrc is-live=true pattern=ball{n} ! video/x-raw,width={w},height={h},framerate={FPS}/1")
        }
    };
    let format = if enc.format.is_empty() { String::new() } else { format!(",format={}", enc.format) };
    // videorate first: fewer frames to convert. The fixed size after videoscale: a window that
    // changes size keeps the video's size (with borders), which the muxers need. The queue drops
    // old frames when the encoder is slower than the screen, so the video keeps to the clock.
    let text = format!(
        "{src} ! videorate ! video/x-raw,framerate={FPS}/1 ! videoconvert ! videoscale add-borders=true ! video/x-raw,width={w},height={h}{format},pixel-aspect-ratio=1/1 \
         ! queue max-size-buffers={FPS} max-size-bytes=0 max-size-time=0 leaky=downstream ! {} ! {} offset-to-zero=true \
         ! filesink buffer-mode=unbuffered location={}",
        enc.launch,
        enc.container.muxer(),
        quoted(out),
    );
    // mp4mux has no offset-to-zero (its file starts at 0 by itself).
    let text = if enc.container == Container::Mp4 { text.replace(" offset-to-zero=true", "") } else { text };
    text.split_whitespace().map(str::to_string).collect()
}

/// The arguments that write `from` again into `to` without a new encoding (a file left open by
/// a kill gets its length and its index).
pub fn repair_pipeline(container: Container, from: &Path, to: &Path) -> Vec<String> {
    format!("filesrc location={} ! parsebin ! {} ! filesink location={}", quoted(from), container.muxer(), quoted(to)).split_whitespace().map(str::to_string).collect()
}

// ------------------------------------------------------------------ files
// A meeting can record the screen more than once (Record screen, Stop screen, again): each time
// is one file, "<meeting id>-<n>" (n = 1, 2…): its stem.

/// The stem of video `n` of meeting `id`.
pub fn stem(id: &str, n: u32) -> String {
    format!("{id}-{n}")
}

/// "m-<digits>-<n>" → (meeting id, n).
pub fn parse_stem(stem: &str) -> Option<(&str, u32)> {
    let (id, n) = stem.rsplit_once('-')?;
    let n: u32 = n.parse().ok().filter(|n| *n > 0)?;
    (crate::meeting::valid_id(id) && stem == format!("{id}-{n}")).then_some((id, n))
}

/// The video with stem `stem` while it records.
pub fn part_path(dir: &Path, stem: &str, c: Container) -> PathBuf {
    dir.join(format!(".{stem}.{}.part", c.ext()))
}

/// The video's name in the meetings folder.
pub fn file_name(stem: &str, c: Container) -> String {
    format!("{stem}.{}", c.ext())
}

/// A video's name ("m-<digits>-<n>.webm|mp4") → its meeting's id and its container.
pub fn video_file(name: &str) -> Option<(&str, Container)> {
    let (stem, ext) = name.rsplit_once('.')?;
    Some((parse_stem(stem)?.0, container_of(ext)?))
}

/// The videos of meeting `id` (whole or not): deleted with the meeting.
pub fn delete_files(dir: &Path, id: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let whole = name.strip_prefix('.').and_then(|n| n.strip_suffix(".part")).unwrap_or(&name);
        if video_file(whole).is_some_and(|(of, _)| of == id) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A video file's container, from its name.
fn container_of(name: &str) -> Option<Container> {
    match name.rsplit('.').next()? {
        "webm" => Some(Container::Webm),
        "mp4" => Some(Container::Mp4),
        _ => None,
    }
}

/// A video that a crash left as `.part`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Leftover {
    pub id: String,
    pub stem: String,
    pub part: PathBuf,
    pub container: Container,
}

/// At start, the videos that a crash left as `.part`.
pub fn leftovers(dir: &Path) -> Vec<Leftover> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".part")) else { continue };
        let Some((stem, ext)) = rest.rsplit_once('.') else { continue };
        if let (Some((id, _)), Some(c)) = (parse_stem(stem), container_of(ext)) {
            out.push(Leftover { id: id.to_string(), stem: stem.to_string(), part: e.path(), container: c });
        }
    }
    out.sort();
    out
}

/// A `.part` file that a crash left: whole again under its final name (written again when it
/// can be read; as it is when not). Returns the final file and its size.
pub fn finish_part(dir: &Path, stem: &str, part: &Path, c: Container, closed: bool) -> anyhow::Result<(PathBuf, u64)> {
    let out = dir.join(file_name(stem, c));
    if !closed && repair(c, part, &out).is_ok() {
        let _ = std::fs::remove_file(part);
    } else {
        let _ = std::fs::remove_file(&out); // a half-written repair
        std::fs::rename(part, &out)?;
    }
    private(&out);
    let size = std::fs::metadata(&out)?.len();
    anyhow::ensure!(size > 0, "the video is empty");
    Ok((out, size))
}

fn private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// No new encoding: about 1 s for an hour of video. A damaged file that hangs GStreamer is kept
/// as it is after this.
const REPAIR_LIMIT: Duration = Duration::from_secs(60);

fn repair(c: Container, from: &Path, to: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(run_quiet(&repair_pipeline(c, from, to), REPAIR_LIMIT), "GStreamer could not read it again");
    anyhow::ensure!(std::fs::metadata(to).map(|m| m.len() > 0).unwrap_or(false), "nothing was written");
    Ok(())
}

/// An empty file only this user can read, for GStreamer to write into (filesink keeps the mode).
fn private_file(path: &Path) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path).map(drop)
}

// ------------------------------------------------------------------ what this computer has
const GST_LAUNCH: &str = "gst-launch-1.0";

/// Settings → Meetings: what the screen video would use.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Status {
    /// "checking" (the first look, a second or two) | "ready" | "missing".
    pub state: String,
    /// "VP8 on the CPU" when ready.
    pub encoder: String,
    /// What to install when it is missing.
    pub problem: String,
    /// The encoder's element (two have the same label).
    #[serde(skip)]
    pub element: String,
}

static STATUS: Mutex<Option<(Instant, Status)>> = Mutex::new(None);
static CHECKING: Mutex<()> = Mutex::new(());
/// A look is old after this (packages may be installed meanwhile).
const STATUS_AGE: Duration = Duration::from_secs(600);

/// The last look; "checking" before the first one ends (`check` makes it).
pub fn status() -> Status {
    STATUS.lock().as_ref().map(|s| s.1.clone()).unwrap_or_else(|| Status { state: "checking".into(), ..Default::default() })
}

/// Look again at what is installed (blocking: a second or two the first time). `force`: also
/// when the last look is new.
pub fn check(force: bool) -> Status {
    let _one = CHECKING.lock();
    if let Some((at, s)) = STATUS.lock().as_ref() {
        if !force && at.elapsed() < STATUS_AGE {
            return s.clone();
        }
    }
    let s = look();
    *STATUS.lock() = Some((Instant::now(), s.clone()));
    s
}

/// The encoder to record with (from the last look; a new look when there was none).
fn encoder() -> anyhow::Result<&'static Encoder> {
    let s = check(false);
    ENCODERS.iter().find(|e| e.element == s.element && s.state == "ready").ok_or_else(|| anyhow::anyhow!("{}", s.problem))
}

fn has(element: &str) -> bool {
    std::process::Command::new("gst-inspect-1.0")
        .args(["--exists", element])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// An encoder is installed and makes a file: two test frames through the same pipeline (a GPU
/// encoder can be installed and still fail, e.g. without a VA driver).
fn works(enc: &Encoder) -> bool {
    if !enc.needs.iter().chain(std::iter::once(&enc.element)).all(|e| has(e)) || !enc.plays.iter().any(|e| has(e)) {
        return false;
    }
    // A new private folder (not a name another user could have made first).
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lowkey-screen-check-{}-{nanos}", std::process::id()));
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    if b.create(&dir).is_err() {
        return false;
    }
    let out = dir.join(format!("check.{}", enc.container.ext()));
    let ok = run_quiet(&pipeline(&Source::Test { frames: Some(2) }, enc, (320, 240), &out), Duration::from_secs(10));
    let made = std::fs::metadata(&out).map(|m| m.len() > 0).unwrap_or(false);
    let _ = std::fs::remove_dir_all(&dir);
    ok && made
}

/// gst-launch with `args`, ended after `limit`; true when it ended well.
fn run_quiet(args: &[String], limit: Duration) -> bool {
    let Ok(mut child) = std::process::Command::new(GST_LAUNCH)
        .arg("-q")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn look() -> Status {
    if !SUPPORTED {
        return Status { state: "missing".into(), problem: "The screen video is on Linux only for now.".into(), ..Default::default() };
    }
    if !has("videotestsrc") {
        return Status { state: "missing".into(), problem: "GStreamer is not installed: install gstreamer and gst-plugins-good.".into(), ..Default::default() };
    }
    if !(fake() || has("pipewiresrc")) {
        return Status { state: "missing".into(), problem: "Install GStreamer's PipeWire plugin (gst-plugin-pipewire).".into(), ..Default::default() };
    }
    if !COMMON.iter().all(|e| has(e)) {
        return Status { state: "missing".into(), problem: "Install gst-plugins-base.".into(), ..Default::default() };
    }
    match choose(works) {
        Some(e) => Status { state: "ready".into(), encoder: e.label(), problem: String::new(), element: e.element.into() },
        None => Status { state: "missing".into(), problem: "No video encoder: install gst-plugins-good (and gst-plugin-va for the GPU).".into(), ..Default::default() },
    }
}

/// OPENWHISPER_FAKE_SCREEN=1: a test picture, no portal.
pub fn fake() -> bool {
    std::env::var_os("OPENWHISPER_FAKE_SCREEN").is_some_and(|v| v == "1")
}

// ------------------------------------------------------------------ recording
/// A screen video being recorded.
pub struct Recording {
    /// The video file's container and codec, and its size in pixels.
    pub container: Container,
    pub codec: &'static str,
    pub size: (u32, u32),
    /// When the video's first frame was taken (its time 0), about ±0.3 s.
    pub started: Instant,
    /// Its file's stem ("<meeting id>-<n>") and the file while it records.
    pub stem: String,
    pub part: PathBuf,
    process: Process,
    #[cfg(target_os = "linux")]
    portal: Option<portal::Cast>,
}

/// How a recording ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    /// The file is closed correctly (it ended with EOS).
    pub closed: bool,
    /// Why it ended by itself (e.g. the screen sharing was stopped); "" after `stop`.
    pub why: String,
    /// When the stream ended: the video's length is `at - started`.
    pub at: Instant,
}

/// What to record with: the portal's screen (or the test picture), and its new restore token.
pub struct Started {
    pub recording: Recording,
    /// The portal's new token for next time (None: keep the old one).
    pub token: Option<String>,
}

/// Start recording a screen video (file stem `stem`, see `stem`) into the meetings folder `dir`.
/// Opens the portal's picker unless `token` chooses the screen (blocking: as long as the picker
/// is open). `on_end`: the recording ended by itself (not by `stop`), with the reason.
pub fn start(dir: &Path, stem: &str, token: Option<&str>, on_end: Box<dyn FnOnce(String) + Send>) -> anyhow::Result<Started> {
    anyhow::ensure!(SUPPORTED, "The screen video is on Linux only for now.");
    let enc = encoder()?;
    std::fs::create_dir_all(dir)?;
    let part = part_path(dir, stem, enc.container);
    let stem = stem.to_string();
    private_file(&part)?;
    #[cfg(target_os = "linux")]
    {
        if fake() {
            let size = video_size(FAKE_SIZE.0, FAKE_SIZE.1);
            let (process, started) = Process::spawn(pipeline(&Source::Test { frames: None }, enc, size, &part), None, on_end)?;
            return Ok(Started { recording: Recording { container: enc.container, codec: enc.codec, size, started, stem, part, process, portal: None }, token: None });
        }
        let (cast, token) = match portal::Cast::open(token) {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        let size = video_size(cast.size.0, cast.size.1);
        let (process, started) = match Process::spawn(pipeline(&Source::PipeWire { node: cast.node }, enc, size, &part), Some(&cast.fd), on_end) {
            Ok(p) => p,
            Err(e) => {
                cast.close(); // the desktop's "sharing" sign goes
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        Ok(Started { recording: Recording { container: enc.container, codec: enc.codec, size, started, stem, part, process, portal: Some(cast) }, token })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (token, on_end, part, stem);
        anyhow::bail!("The screen video is on Linux only for now.")
    }
}

impl Recording {
    /// Stop: the file is closed (5 s at most) and the screen sharing ends.
    pub fn stop(self) -> Ended {
        let ended = self.process.stop();
        #[cfg(target_os = "linux")]
        if let Some(c) = self.portal {
            c.close();
        }
        ended
    }

    /// GStreamer still records (it did not end by itself).
    pub fn running(&self) -> bool {
        self.process.watcher.as_ref().is_some_and(|w| !w.is_finished())
    }

    /// Stop and delete the file (the meeting ended before the screen was chosen).
    pub fn discard(self) {
        let part = self.part.clone();
        self.stop();
        let _ = std::fs::remove_file(part);
    }
}

/// The gst-launch process.
struct Process {
    child: std::sync::Arc<Mutex<std::process::Child>>,
    /// Set by `stop`: the end is not news.
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
    watcher: Option<std::thread::JoinHandle<Ended>>,
}

/// The last lines GStreamer wrote to stderr: why it failed.
fn tail(lines: &Mutex<Vec<String>>) -> String {
    let l = lines.lock();
    // "ERROR: from element /GstPipeline:pipeline0/GstPipeWireSrc:pipewiresrc0: stream error"
    let err = l.iter().rev().find(|x| x.starts_with("ERROR")).cloned().unwrap_or_else(|| l.last().cloned().unwrap_or_default());
    let err = err.trim_start_matches("ERROR: ").to_string();
    match err.split_once(": ") {
        Some((from, what)) if from.starts_with("from element") => what.to_string(),
        _ => err,
    }
}

impl Process {
    /// Start gst-launch with `args`; `fd` becomes its fd 3. Returns when the pipeline plays (the
    /// moment is the video's time 0), or with GStreamer's error.
    fn spawn(args: Vec<String>, fd: Option<&std::os::fd::OwnedFd>, on_end: Box<dyn FnOnce(String) + Send>) -> anyhow::Result<(Self, Instant)> {
        use std::io::BufRead;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};

        let mut cmd = std::process::Command::new(GST_LAUNCH);
        cmd.arg("-e").args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::process::CommandExt;
            let raw = fd.map(|f| f.as_raw_fd());
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: only async-signal-safe calls between fork and exec.
            unsafe {
                cmd.pre_exec(move || {
                    // Its output may go nowhere after the app ends: SIGPIPE would kill it before
                    // it closes the file.
                    libc::signal(libc::SIGPIPE, libc::SIG_IGN);
                    // The app ends (also killed): GStreamer gets SIGINT and closes the file. Tied
                    // to the thread that starts it: the watcher, which lives as long as it.
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGINT) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        libc::_exit(1); // the app ended before the line above
                    }
                    if let Some(raw) = raw {
                        // dup2 clears close-on-exec on the copy.
                        if libc::dup2(raw, 3) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = fd;

        // The watcher starts the process (see PR_SET_PDEATHSIG), reads its output, and waits.
        enum Msg {
            Spawned(Arc<Mutex<std::process::Child>>),
            Playing(Instant),
            Failed(anyhow::Error),
        }
        let (tx, rx) = mpsc::channel::<Msg>();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop2 = stopping.clone();
        let watcher = std::thread::Builder::new().name("screen".into()).spawn(move || {
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Msg::Failed(anyhow::anyhow!("could not start {GST_LAUNCH}: {e}")));
                    return Ended { closed: false, why: String::new(), at: Instant::now() };
                }
            };
            let (out, err) = (child.stdout.take(), child.stderr.take());
            let child = Arc::new(Mutex::new(child));
            let _ = tx.send(Msg::Spawned(child.clone()));
            let errors = Arc::new(Mutex::new(Vec::<String>::new()));
            let e2 = errors.clone();
            let err_reader = std::thread::spawn(move || {
                for line in std::io::BufReader::new(err.unwrap()).lines().map_while(Result::ok) {
                    let mut l = e2.lock();
                    l.push(line);
                    if l.len() > 40 {
                        l.remove(0);
                    }
                }
            });
            // "Setting pipeline to PLAYING ..." comes just before it plays; "Got EOS" at the end.
            let mut playing = false;
            let mut closed = false;
            for line in std::io::BufReader::new(out.unwrap()).lines().map_while(Result::ok) {
                if line.starts_with("Setting pipeline to PLAYING") && !playing {
                    playing = true;
                    let _ = tx.send(Msg::Playing(Instant::now()));
                } else if line.starts_with("Got EOS from element") {
                    closed = true;
                }
            }
            let at = Instant::now(); // its output closed: it ends
            // Not `wait()` under the lock: `stop` must be able to signal it meanwhile.
            let status = loop {
                match child.lock().try_wait() {
                    Ok(Some(s)) => break Ok(s),
                    Ok(None) => {}
                    Err(e) => break Err(e),
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let _ = err_reader.join();
            let why = tail(&errors);
            let ok = status.as_ref().map(|s| s.success()).unwrap_or(false);
            if !playing {
                let _ = tx.send(Msg::Failed(anyhow::anyhow!("{}", if why.is_empty() { "GStreamer stopped at once" } else { why.as_str() })));
                return Ended { closed: false, why, at };
            }
            let why = if stop2.load(Ordering::SeqCst) {
                String::new()
            } else if why.is_empty() {
                if ok { "the screen sharing ended".to_string() } else { "GStreamer stopped".to_string() }
            } else {
                why
            };
            if !why.is_empty() {
                on_end(why.clone());
            }
            Ended { closed: closed && ok, why, at }
        })?;
        let until = Instant::now() + Duration::from_secs(15);
        let mut child = None;
        loop {
            match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Msg::Spawned(c)) => child = Some(c),
                Ok(Msg::Playing(at)) => {
                    let child = child.expect("spawned before it plays");
                    return Ok((Self { child, stopping, watcher: Some(watcher) }, at));
                }
                Ok(Msg::Failed(e)) => {
                    let _ = watcher.join();
                    return Err(e);
                }
                Err(_) => {
                    // No answer: it hangs (e.g. a GPU driver). Killed; the watcher ends then.
                    stopping.store(true, Ordering::SeqCst);
                    if let Some(c) = &child {
                        signal(c, Signal::Kill);
                    }
                    let _ = watcher.join();
                    anyhow::bail!("GStreamer did not start in 15 s")
                }
            }
        }
    }

    /// SIGINT (GStreamer closes the file), then a kill after 5 s.
    fn stop(mut self) -> Ended {
        use std::sync::atomic::Ordering;
        self.stopping.store(true, Ordering::SeqCst);
        let asked = Instant::now();
        signal(&self.child, Signal::Interrupt);
        let w = self.watcher.take().expect("one stop");
        let until = Instant::now() + Duration::from_secs(5);
        while !w.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        if !w.is_finished() {
            signal(&self.child, Signal::Kill);
        }
        let mut end = w.join().unwrap_or_else(|_| Ended { closed: false, why: String::new(), at: asked });
        if !end.closed {
            end.at = end.at.min(asked); // killed: the video ends about when Stop was asked
        }
        end
    }
}

impl Drop for Process {
    /// Not stopped (an error on the way): the process does not stay.
    fn drop(&mut self) {
        if self.watcher.is_some() {
            self.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
            signal(&self.child, Signal::Kill);
        }
    }
}

enum Signal {
    Interrupt,
    Kill,
}

/// To the process while it is not reaped (its pid cannot be another process's then).
fn signal(child: &Mutex<std::process::Child>, s: Signal) {
    let mut c = child.lock();
    if !matches!(c.try_wait(), Ok(None)) {
        return;
    }
    match s {
        #[cfg(unix)]
        Signal::Interrupt => unsafe {
            libc::kill(c.id() as libc::pid_t, libc::SIGINT);
        },
        _ => {
            let _ = c.kill();
        }
    }
}

// ------------------------------------------------------------------ the portal
#[cfg(target_os = "linux")]
mod portal {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::{PersistMode, Session};
    use std::os::fd::OwnedFd;

    /// An open screen cast: the PipeWire connection and the stream on it.
    pub struct Cast {
        proxy: Screencast<'static>,
        session: Session<'static, Screencast<'static>>,
        pub fd: OwnedFd,
        pub node: u32,
        pub size: (u32, u32),
    }

    fn why(e: ashpd::Error) -> anyhow::Error {
        use ashpd::desktop::ResponseError;
        match e {
            ashpd::Error::Response(ResponseError::Cancelled) => anyhow::anyhow!("the screen picker was closed"),
            ashpd::Error::Portal(ashpd::PortalError::NotAllowed(_)) => anyhow::anyhow!("the desktop does not allow it"),
            ashpd::Error::Zbus(_) | ashpd::Error::Portal(ashpd::PortalError::ZBus(_)) => {
                anyhow::anyhow!("no screen-sharing portal (install xdg-desktop-portal and the one for your desktop): {e}")
            }
            e => anyhow::anyhow!("{e}"),
        }
    }

    impl Cast {
        /// Ask the portal for the screen (its picker, unless `token` chooses it). Returns the cast
        /// and the token for next time.
        pub fn open(token: Option<&str>) -> anyhow::Result<(Self, Option<String>)> {
            tauri::async_runtime::block_on(async {
                let proxy = Screencast::new().await.map_err(why)?;
                let session = proxy.create_session().await.map_err(why)?;
                let cursors = proxy.available_cursor_modes().await.unwrap_or_default();
                let cursor = if cursors.contains(CursorMode::Embedded) { CursorMode::Embedded } else { CursorMode::Hidden };
                let token = token.filter(|t| !t.is_empty());
                proxy.select_sources(&session, cursor, SourceType::Monitor | SourceType::Window, false, token, PersistMode::ExplicitlyRevoked).await.map_err(why)?;
                let streams = proxy.start(&session, None).await.map_err(why)?.response().map_err(why)?;
                let stream = streams.streams().first().ok_or_else(|| anyhow::anyhow!("no screen was chosen"))?;
                let size = stream.size().map(|(w, h)| (w.max(2) as u32, h.max(2) as u32)).unwrap_or((1920, 1080));
                let node = stream.pipe_wire_node_id();
                let new_token = streams.restore_token().map(str::to_string);
                let fd = proxy.open_pipe_wire_remote(&session).await.map_err(why)?;
                Ok((Self { proxy, session, fd, node, size }, new_token))
            })
        }

        /// End the screen sharing (the desktop's "sharing" sign goes).
        pub fn close(self) {
            let Self { proxy, session, fd, .. } = self;
            drop(fd);
            let _ = tauri::async_runtime::block_on(session.close());
            drop(proxy);
        }
    }
}

/// For tests: a test picture recorded as meeting `id`'s video (None without GStreamer's vp8enc).
#[cfg(test)]
pub(crate) fn test_recording(dir: &Path, stem: &str) -> Option<Recording> {
    if !has("videotestsrc") || !has("webmmux") || !has("vp8enc") {
        return None;
    }
    let enc = ENCODERS.iter().find(|e| e.element == "vp8enc")?;
    let part = part_path(dir, stem, enc.container);
    std::fs::create_dir_all(dir).ok()?;
    private_file(&part).ok()?;
    let (process, started) = Process::spawn(pipeline(&Source::Test { frames: None }, enc, (320, 240), &part), None, Box::new(|_| {})).ok()?;
    Some(Recording {
        container: enc.container,
        codec: enc.codec,
        size: (320, 240),
        started,
        stem: stem.into(),
        part,
        process,
        #[cfg(target_os = "linux")]
        portal: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_encoder_that_works_is_taken() {
        assert_eq!(choose(|e| ["vp8enc", "openh264enc"].contains(&e.element)).unwrap().element, "vp8enc");
        assert_eq!(choose(|e| e.element == "openh264enc").unwrap().label(), "H.264 on the CPU");
        assert_eq!(choose(|e| e.gpu).unwrap().label(), "AV1 on the GPU");
        assert!(choose(|_| false).is_none());
        // Only containers that are readable after a kill.
        assert!(ENCODERS.iter().all(|e| matches!(e.container, Container::Webm | Container::Mp4)));
        assert!(ENCODERS.iter().filter(|e| e.container == Container::Mp4).all(|e| e.launch.ends_with("h264parse")));
    }

    #[test]
    fn the_size_is_even_and_at_most_1920_wide() {
        assert_eq!(video_size(1920, 1080), (1920, 1080));
        assert_eq!(video_size(3840, 2160), (1920, 1080));
        assert_eq!(video_size(2560, 1600), (1920, 1200));
        assert_eq!(video_size(1001, 703), (1000, 702), "a window: even numbers");
        assert_eq!(video_size(0, 0), (2, 2));
    }

    #[test]
    fn the_pipeline_reads_the_portal_on_fd_3() {
        let vp8 = ENCODERS.iter().find(|e| e.element == "vp8enc").unwrap();
        let out = Path::new("/home/a b/.m-1.webm.part");
        let text = pipeline(&Source::PipeWire { node: 57 }, vp8, (1920, 1080), out).join(" ");
        assert!(text.starts_with("pipewiresrc fd=3 path=57 do-timestamp=true keepalive-time=1000 ! videorate ! video/x-raw,framerate=10/1 ! videoconvert"), "{text}");
        assert!(text.contains("video/x-raw,width=1920,height=1080,format=I420,pixel-aspect-ratio=1/1"), "{text}");
        assert!(text.contains("! vp8enc deadline=1 "), "{text}");
        assert!(text.contains("! webmmux offset-to-zero=true ! filesink buffer-mode=unbuffered"), "{text}");
        // gst-launch joins the arguments with spaces: the path is one quoted word for it.
        assert!(text.ends_with("location=\"/home/a b/.m-1.webm.part\""), "{text}");
        let h264 = ENCODERS.iter().find(|e| e.element == "openh264enc").unwrap();
        let text = pipeline(&Source::Test { frames: Some(2) }, h264, (320, 240), Path::new("/t/x.mp4")).join(" ");
        assert!(text.starts_with("videotestsrc is-live=true pattern=ball num-buffers=2 ! video/x-raw,width=320,height=240,framerate=10/1"), "{text}");
        assert!(text.contains("! h264parse ! mp4mux fragment-duration=1000 ! filesink"), "no offset-to-zero on mp4mux: {text}");
        assert_eq!(quoted(Path::new("/a\"b\\c")), "\"/a\\\"b\\\\c\"");
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ow-screen-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn leftovers_are_found_and_deleted_with_the_meeting() {
        let d = temp("left");
        for f in [".m-12-1.webm.part", ".m-12-2.webm.part", ".m-13-1.mp4.part", ".m-x-1.webm.part", ".m-12.webm.part", ".m-14.them.pcm", "m-12.json", "m-15-1.webm", "m-15-2.mp4", "m-150-1.webm", ".m-16-1.mkv.part", ".m-17-01.webm.part"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let found: Vec<(String, String, Container)> = leftovers(&d).into_iter().map(|l| (l.id, l.stem, l.container)).collect();
        let want = [("m-12", "m-12-1", Container::Webm), ("m-12", "m-12-2", Container::Webm), ("m-13", "m-13-1", Container::Mp4)];
        assert_eq!(found, want.map(|(a, b, c)| (a.to_string(), b.to_string(), c)));
        delete_files(&d, "m-15");
        delete_files(&d, "m-13");
        assert!(!d.join("m-15-1.webm").exists() && !d.join("m-15-2.mp4").exists() && !d.join(".m-13-1.mp4.part").exists());
        assert!(d.join("m-150-1.webm").exists(), "only that meeting's");
        assert!(d.join(".m-12-1.webm.part").exists() && d.join("m-12.json").exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_video_name_is_its_meeting_and_its_number() {
        assert_eq!(stem("m-12", 3), "m-12-3");
        assert_eq!(parse_stem("m-12-3"), Some(("m-12", 3)));
        for bad in ["m-12", "m-12-0", "m-12-03", "m-12-", "m--1", "x-12-1", "m-12-1-1", "m-12-+1"] {
            assert_eq!(parse_stem(bad), None, "{bad}");
        }
        assert_eq!(video_file("m-12-3.webm"), Some(("m-12", Container::Webm)));
        assert_eq!(video_file("m-12-3.mp4"), Some(("m-12", Container::Mp4)));
        for bad in ["m-12.webm", "m-12-3.json", "../m-12-3.webm", "m-12-3"] {
            assert_eq!(video_file(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_closed_part_is_renamed() {
        let d = temp("finish");
        let part = part_path(&d, "m-7-1", Container::Webm);
        std::fs::write(&part, b"video").unwrap();
        let (out, size) = finish_part(&d, "m-7-1", &part, Container::Webm, true).unwrap();
        assert_eq!((out.file_name().unwrap().to_str().unwrap(), size), ("m-7-1.webm", 5));
        assert!(!part.exists());
        std::fs::write(&part, b"").unwrap();
        assert!(finish_part(&d, "m-7-1", &part, Container::Webm, true).is_err(), "an empty video is no video");
        let _ = std::fs::remove_dir_all(d);
    }

    /// The real portal: the desktop's picker opens (unless OW_SCREEN_TOKEN, a token printed by
    /// an earlier run, chooses the screen). Run by hand only:
    /// `cargo test --lib real_portal -- --ignored --nocapture`.
    #[test]
    #[ignore = "opens the desktop's screen picker"]
    fn the_real_portal_records_the_screen() {
        let d = temp("portal");
        let token = std::env::var("OW_SCREEN_TOKEN").ok();
        let asked = Instant::now();
        let started = start(&d, "m-1-1", token.as_deref(), Box::new(|w| eprintln!("ended by itself: {w}"))).unwrap();
        let r = &started.recording;
        eprintln!("chosen after {:?}; token {:?}; {:?} {} {:?}", asked.elapsed(), started.token, r.size, r.codec, r.container);
        std::thread::sleep(Duration::from_secs(4));
        let (part, c) = (started.recording.part.clone(), started.recording.container);
        let end = started.recording.stop();
        assert!(end.closed, "{end:?}");
        let (out, size) = finish_part(&d, "m-1-1", &part, c, end.closed).unwrap();
        eprintln!("{} bytes: {}", size, out.display());
        assert!(size > 1000);
        let keep = std::env::temp_dir().join(format!("ow-portal-video.{}", c.ext()));
        let _ = std::fs::rename(&out, &keep);
        eprintln!("kept: {}", keep.display());
    }

    /// With GStreamer installed: a test picture recorded for 2 s, stopped with SIGINT, is a whole
    /// file; one killed is made whole again.
    #[test]
    fn a_recording_is_closed_by_stop_and_repaired_after_a_kill() {
        if !has("videotestsrc") || !has("webmmux") || !has("vp8enc") {
            eprintln!("skipped: no GStreamer with vp8enc");
            return;
        }
        let d = temp("rec");
        let vp8 = ENCODERS.iter().find(|e| e.element == "vp8enc").unwrap();
        let part = part_path(&d, "m-1-1", Container::Webm);
        let ended = std::sync::Arc::new(Mutex::new(None::<String>));
        let e2 = ended.clone();
        let (p, at) = Process::spawn(pipeline(&Source::Test { frames: None }, vp8, (320, 240), &part), None, Box::new(move |w| *e2.lock() = Some(w))).unwrap();
        assert!(at.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(1500));
        let end = p.stop();
        assert!(end.closed && end.why.is_empty(), "{end:?}");
        assert!(ended.lock().is_none(), "a stop is not news");
        let (out, size) = finish_part(&d, "m-1-1", &part, Container::Webm, end.closed).unwrap();
        assert!(size > 1000, "{size}");
        let _ = std::fs::remove_file(out);

        // Killed: no index, no length; written again whole.
        let (p, _) = Process::spawn(pipeline(&Source::Test { frames: None }, vp8, (320, 240), &part), None, Box::new(|_| {})).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        signal(&p.child, Signal::Kill);
        let end = p.stop();
        assert!(!end.closed, "{end:?}");
        let (out, size) = finish_part(&d, "m-1-1", &part, Container::Webm, end.closed).unwrap();
        assert!(size > 1000 && !part.exists(), "{size}");
        let _ = std::fs::remove_file(out);

        // A pipeline that cannot start: GStreamer's reason.
        let bad = vec!["videotestsrc".to_string(), "!".into(), "nosuchelement".into()];
        let e = Process::spawn(bad, None, Box::new(|_| {})).err().unwrap();
        assert!(format!("{e:#}").contains("nosuchelement"), "{e:#}");
        let _ = std::fs::remove_dir_all(d);
    }
}
