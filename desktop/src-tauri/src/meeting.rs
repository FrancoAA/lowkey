//! Meetings: your microphone ("Me") and the computer's sound ("Them", the others in a call) as
//! a live transcript, with your notes next to it. Each source is cut into pieces at pauses and
//! the pieces go to the speech model in order. Saved as JSON in <data dir>/meetings; nothing
//! leaves the computer, and no audio is kept.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::audio::{self, RATE};
use crate::formats::Segment;
use crate::mic::{self, EndFn, LevelFn, Recorder};
use crate::system_audio;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    Me,
    Them,
}

impl Speaker {
    fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            Speaker::Me => "Me",
            Speaker::Them => "Them",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub speaker: Speaker,
    /// Seconds from the start of the meeting.
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    /// Unix seconds.
    pub started_at: u64,
    /// Seconds recorded.
    pub duration: f64,
    /// "recording" while it runs (a file left like this was interrupted), else "done".
    pub state: String,
    pub model: String,
    /// The computer's sound was recorded too (else the microphone only).
    pub system_audio: bool,
    pub notes: String,
    pub lines: Vec<Line>,
}

pub const MAX_TITLE: usize = 200;
pub const MAX_NOTES: usize = 1_000_000;

impl Meeting {
    pub fn new(title: &str, model: &str) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let title = title.trim();
        Self {
            id: format!("m-{}", now.as_millis()),
            title: if title.is_empty() { "Meeting".into() } else { title.chars().take(MAX_TITLE).collect() },
            started_at: now.as_secs(),
            state: "recording".into(),
            model: model.into(),
            ..Default::default()
        }
    }
}

// ------------------------------------------------------------------ store
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub started_at: u64,
    pub duration: f64,
    pub state: String,
    pub lines: usize,
    /// The first words of the notes, else of the transcript.
    pub excerpt: String,
}

pub fn dir() -> PathBuf {
    crate::config::data_dir().join("meetings")
}

/// "m-" and digits only: an id never names a path outside the folder.
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("m-").is_some_and(|n| !n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn file_in(dir: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_id(id), "unknown meeting {id:?}");
    Ok(dir.join(format!("{id}.json")))
}

/// Written to a temporary file first, then renamed: a crash never leaves half a file.
pub fn save_in(dir: &Path, m: &Meeting) -> anyhow::Result<()> {
    let path = file_in(dir, &m.id)?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?; // transcripts are private
    }
    let tmp = dir.join(format!(".{}.json.tmp", m.id));
    std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load_in(dir: &Path, id: &str) -> anyhow::Result<Meeting> {
    let text = std::fs::read_to_string(file_in(dir, id)?).map_err(|e| anyhow::anyhow!("meeting {id}: {e}"))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn delete_in(dir: &Path, id: &str) -> anyhow::Result<()> {
    std::fs::remove_file(file_in(dir, id)?).map_err(|e| anyhow::anyhow!("meeting {id}: {e}"))
}

/// Newest first. Broken files are skipped.
pub fn list_in(dir: &Path) -> Vec<Summary> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<Summary> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json").filter(|id| valid_id(id))?.to_string();
            let m = load_in(dir, &id).ok()?;
            let source = if m.notes.trim().is_empty() { m.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ") } else { m.notes.clone() };
            let excerpt = excerpt(&source, 140);
            Some(Summary { id: m.id, title: m.title, started_at: m.started_at, duration: m.duration, state: m.state, lines: m.lines.len(), excerpt })
        })
        .collect();
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    out
}

fn excerpt(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a))
}

// ------------------------------------------------------------------ cutting the audio
const FRAME: usize = RATE as usize / 10; // 100 ms
/// Below this level (RMS) a frame is silence. Digital silence (nothing plays) is 0.
const SILENCE: f32 = 0.003;
/// A pause: this many quiet frames in a row (600 ms).
const PAUSE_FRAMES: usize = 6;
pub const MIN_PIECE_S: usize = 4;
pub const MAX_PIECE_S: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// Position of the first sample, in samples from the start of the meeting.
    pub start: usize,
    pub pcm: Vec<f32>,
}

/// Collects one source's audio and gives pieces of `MIN_PIECE_S`..`MAX_PIECE_S` seconds, cut at
/// a pause (or, in long speech, at the quietest moment of the last quarter). Silence is dropped.
pub struct Chunker {
    buf: Vec<f32>,
    start: usize,
    min: usize,
    max: usize,
}

enum Step {
    Wait,
    Emit(usize),
    Drop(usize),
}

fn rms(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    (block.iter().map(|x| x * x).sum::<f32>() / block.len() as f32).sqrt()
}

/// The level that 80% of the frames stay under: "how loud the speech is" in this audio.
fn loud(frames: &[f32]) -> f32 {
    if frames.is_empty() {
        return 0.0;
    }
    let mut v = frames.to_vec();
    v.sort_unstable_by(f32::total_cmp);
    v[(v.len() * 4 / 5).min(v.len() - 1)]
}

fn is_silent(pcm: &[f32]) -> bool {
    loud(&pcm.chunks(FRAME).map(rms).collect::<Vec<_>>()) < SILENCE
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new(MIN_PIECE_S * RATE as usize, MAX_PIECE_S * RATE as usize)
    }
}

impl Chunker {
    pub fn new(min: usize, max: usize) -> Self {
        Self { buf: Vec::new(), start: 0, min: min.max(FRAME * (PAUSE_FRAMES + 1)), max: max.max(min + 4 * FRAME) }
    }

    /// `at`: where the block starts by the clock (samples from the start). When a source stops
    /// sending for a while, its next audio is placed at the clock's position, not right after
    /// the old audio.
    pub fn push(&mut self, block: &[f32], at: usize) -> Vec<Chunk> {
        let mut out = Vec::new();
        let end = self.start + self.buf.len();
        if at > end + RATE as usize {
            out.extend(self.flush());
            self.start = at;
        }
        self.buf.extend_from_slice(block);
        loop {
            match self.step() {
                Step::Wait => break,
                Step::Emit(n) => out.push(self.take(n)),
                Step::Drop(n) => {
                    self.take(n);
                }
            }
        }
        out
    }

    /// What is left (at the end of the meeting), unless it is silence.
    pub fn flush(&mut self) -> Option<Chunk> {
        if self.buf.is_empty() {
            return None;
        }
        let c = self.take(self.buf.len());
        (!is_silent(&c.pcm)).then_some(c)
    }

    fn take(&mut self, n: usize) -> Chunk {
        let pcm: Vec<f32> = self.buf.drain(..n.min(self.buf.len())).collect();
        let c = Chunk { start: self.start, pcm };
        self.start += c.pcm.len();
        c
    }

    fn step(&self) -> Step {
        let len = self.buf.len();
        if len < self.min {
            return Step::Wait;
        }
        let frames: Vec<f32> = self.buf.chunks_exact(FRAME).map(rms).collect();
        let level = loud(&frames);
        if level < SILENCE {
            return Step::Drop(frames.len() * FRAME);
        }
        let quiet = (0.2 * level).max(SILENCE);
        if frames.len() >= PAUSE_FRAMES && frames[frames.len() - PAUSE_FRAMES..].iter().all(|&r| r < quiet) {
            return Step::Emit(frames.len() * FRAME);
        }
        if len >= self.max {
            let (lo, hi) = (self.max * 3 / 4 / FRAME, (self.max / FRAME).min(frames.len()));
            let i = (lo..hi).min_by(|&a, &b| frames[a].total_cmp(&frames[b])).unwrap_or(hi.saturating_sub(1));
            return Step::Emit(i * FRAME + FRAME / 2);
        }
        Step::Wait
    }
}

// ------------------------------------------------------------------ echo
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Without headphones the microphone also hears the others: their words come back as "Me".
/// A "Me" line of 3+ words is an echo when most of its words are in "Them" lines at the same time.
fn is_echo(me: &Line, them: &[&Line]) -> bool {
    let mine = words(&me.text);
    if mine.len() < 3 {
        return false;
    }
    let near: HashSet<String> =
        them.iter().filter(|t| t.start <= me.end + 3.0 && me.start <= t.end + 3.0).flat_map(|t| words(&t.text)).collect();
    let shared = mine.iter().filter(|w| near.contains(*w)).count();
    shared as f64 >= 0.6 * mine.len() as f64
}

fn drop_echoes(lines: &mut Vec<Line>, since: f64) {
    let them: Vec<Line> = lines.iter().filter(|l| l.speaker == Speaker::Them && l.end >= since - 30.0).cloned().collect();
    if them.is_empty() {
        return;
    }
    let refs: Vec<&Line> = them.iter().collect();
    lines.retain(|l| l.speaker != Speaker::Me || l.end < since - 30.0 || !is_echo(l, &refs));
}

/// "[Music]", "(applause)": sound labels, not speech.
fn is_label(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')'))
}

// ------------------------------------------------------------------ export
/// Lines of one speaker with short gaps, joined into one paragraph.
pub fn paragraphs(lines: &[Line]) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for l in lines {
        match out.last_mut() {
            Some(p) if p.speaker == l.speaker && l.start - p.end < 2.0 && p.text.len() < 600 => {
                p.text.push(' ');
                p.text.push_str(&l.text);
                p.end = p.end.max(l.end);
            }
            _ => out.push(l.clone()),
        }
    }
    out
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Days since 1970-01-01 → (year, month, day). (Howard Hinnant's civil_from_days.)
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// "2026-11-02 14:05" in the page's time zone (`offset_min` east of UTC).
pub fn local_time(unix: u64, offset_min: i32) -> String {
    let t = unix as i64 + offset_min as i64 * 60;
    let (y, mo, d) = civil(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!("{y:04}-{mo:02}-{d:02} {:02}:{:02}", s / 3600, s / 60 % 60)
}

fn duration_text(seconds: f64) -> String {
    let min = (seconds / 60.0).round() as u64;
    match min {
        0 => "under 1 min".into(),
        m if m < 60 => format!("{m} min"),
        m => format!("{} h {:02} min", m / 60, m % 60),
    }
}

pub fn to_markdown(m: &Meeting, offset_min: i32) -> String {
    let mut s = format!("# {}\n\n{} · {}\n\n", m.title, local_time(m.started_at, offset_min), duration_text(m.duration));
    if !m.notes.trim().is_empty() {
        s += &format!("## Notes\n\n{}\n\n", m.notes.trim());
    }
    s += "## Transcript\n\n";
    if m.lines.is_empty() {
        s += "No speech was found.\n";
    }
    for p in paragraphs(&m.lines) {
        s += &format!("**{}** ({}): {}\n\n", p.speaker.label(), clock(p.start), p.text);
    }
    s.trim_end().to_string() + "\n"
}

// ------------------------------------------------------------------ the recording
pub type TranscribeFn = Arc<dyn Fn(Vec<f32>) -> anyhow::Result<Vec<Segment>> + Send + Sync>;

#[derive(Debug, Clone, Serialize)]
pub struct Live {
    pub meeting: Meeting,
    /// recording | finishing | done
    pub state: String,
    /// Seconds since the start.
    pub elapsed: f64,
    /// Pieces of audio not transcribed yet.
    pub pending: usize,
    /// Problems to show: no computer sound, the mic stopped, a piece failed.
    pub notices: Vec<String>,
}

pub enum Event {
    Live(Live),
    Levels { me: f32, them: f32 },
}

pub type EmitFn = Arc<dyn Fn(Event) + Send + Sync>;

struct Inner {
    dir: PathBuf,
    doc: Mutex<Meeting>,
    clock: Instant,
    /// Set when the meeting stops: the length of the recording.
    ended: Mutex<Option<f64>>,
    chunkers: [Mutex<Chunker>; 2],
    tx: Mutex<Option<mpsc::Sender<(Speaker, Chunk)>>>,
    pending: AtomicUsize,
    state: Mutex<&'static str>,
    notices: Mutex<Vec<String>>,
    levels: Mutex<([f32; 2], Instant)>,
    emit: EmitFn,
}

impl Inner {
    fn elapsed(&self) -> f64 {
        self.ended.lock().unwrap_or_else(|| self.clock.elapsed().as_secs_f64())
    }

    /// Audio of one source; `at` = its position by the clock, in samples.
    fn feed(&self, who: Speaker, block: &[f32], at: usize) {
        let chunks = self.chunkers[who.index()].lock().push(block, at);
        for c in chunks {
            self.send(who, c);
        }
    }

    fn feed_now(&self, who: Speaker, block: &[f32]) {
        if self.ended.lock().is_some() && *self.state.lock() != "finishing" {
            return;
        }
        let now = (self.clock.elapsed().as_secs_f64() * RATE as f64) as usize;
        self.feed(who, block, now.saturating_sub(block.len()));
    }

    fn send(&self, who: Speaker, c: Chunk) {
        if let Some(tx) = self.tx.lock().as_ref() {
            self.pending.fetch_add(1, Ordering::SeqCst);
            if tx.send((who, c)).is_err() {
                self.pending.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    fn level(&self, who: Speaker, value: f32) {
        let mut lv = self.levels.lock();
        lv.0[who.index()] = value;
        if lv.1.elapsed().as_millis() >= 80 {
            lv.1 = Instant::now();
            let [me, them] = lv.0;
            drop(lv);
            (self.emit)(Event::Levels { me, them });
        }
    }

    fn notice(&self, text: String) {
        let mut n = self.notices.lock();
        if !n.contains(&text) {
            n.push(text);
        }
    }

    fn add(&self, who: Speaker, chunk_start: usize, chunk_len: usize, segments: Vec<Segment>) {
        let (offset, len) = (chunk_start as f64 / RATE as f64, chunk_len as f64 / RATE as f64);
        let mut doc = self.doc.lock();
        let mut first = f64::MAX;
        for s in segments {
            let text = s.text.trim();
            if text.is_empty() || is_label(text) {
                continue;
            }
            let start = offset + s.start.clamp(0.0, len);
            let line = Line { speaker: who, start, end: (offset + s.end.clamp(0.0, len)).max(start), text: text.to_string() };
            first = first.min(line.start);
            let at = doc.lines.partition_point(|l| l.start <= line.start);
            doc.lines.insert(at, line);
        }
        if first < f64::MAX {
            drop_echoes(&mut doc.lines, first);
        }
    }

    fn save(&self) {
        let doc = self.doc.lock().clone();
        if let Err(e) = save_in(&self.dir, &doc) {
            self.notice(format!("Could not save the meeting: {e:#}"));
        }
    }

    fn live(&self) -> Live {
        Live {
            meeting: self.doc.lock().clone(),
            state: self.state.lock().to_string(),
            elapsed: self.elapsed(),
            pending: self.pending.load(Ordering::SeqCst),
            notices: self.notices.lock().clone(),
        }
    }

    fn changed(&self) {
        (self.emit)(Event::Live(self.live()));
    }
}

pub struct Session {
    inner: Arc<Inner>,
    mic: Mutex<Option<Box<dyn Recorder>>>,
    system: Mutex<Option<system_audio::Capture>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Session {
    /// A new meeting, saved at once; the speech model gets its pieces on its own thread.
    /// No audio yet: see `open_sources`.
    pub fn new(dir: PathBuf, doc: Meeting, transcribe: TranscribeFn, emit: EmitFn) -> anyhow::Result<Arc<Self>> {
        save_in(&dir, &doc)?;
        let (tx, rx) = mpsc::channel::<(Speaker, Chunk)>();
        let inner = Arc::new(Inner {
            dir,
            doc: Mutex::new(doc),
            clock: Instant::now(),
            ended: Mutex::new(None),
            chunkers: [Mutex::default(), Mutex::default()],
            tx: Mutex::new(Some(tx)),
            pending: AtomicUsize::new(0),
            state: Mutex::new("recording"),
            notices: Mutex::default(),
            levels: Mutex::new(([0.0; 2], Instant::now())),
            emit,
        });
        let w = inner.clone();
        let worker = std::thread::Builder::new().name("meeting".into()).spawn(move || {
            // Ends when the meeting stops and every piece is done.
            for (who, chunk) in rx {
                let (start, len) = (chunk.start, chunk.pcm.len());
                match transcribe(chunk.pcm) {
                    Ok(segments) => w.add(who, start, len, segments),
                    Err(e) => w.notice(format!("A part of the audio could not be transcribed: {e:#}")),
                }
                w.pending.fetch_sub(1, Ordering::SeqCst);
                w.save();
                w.changed();
            }
        })?;
        Ok(Arc::new(Self { inner, mic: Mutex::new(None), system: Mutex::new(None), worker: Mutex::new(Some(worker)) }))
    }

    pub fn id(&self) -> String {
        self.inner.doc.lock().id.clone()
    }

    /// The microphone and the computer's sound. Fails only when neither one opens.
    pub fn open_sources(self: &Arc<Self>) -> anyhow::Result<()> {
        let inner = self.inner.clone();
        let on_audio: crate::preview::AudioFn = {
            let inner = inner.clone();
            Arc::new(move |pcm: &[f32], rate: u32| inner.feed_now(Speaker::Me, &audio::resample(pcm, rate, RATE)))
        };
        let on_level: LevelFn = {
            let inner = inner.clone();
            Arc::new(move |lv| inner.level(Speaker::Me, lv))
        };
        let on_mic_end: EndFn = {
            let inner = inner.clone();
            Arc::new(move |why: String| {
                inner.notice(format!("The microphone stopped ({why}).").replace("(failed: ", "("));
                inner.changed();
            })
        };
        let mut rec = mic::new_tap(on_level, on_mic_end, on_audio);
        let mic_err = rec.start().err();
        if mic_err.is_none() {
            *self.mic.lock() = Some(rec);
        }
        let on_block: system_audio::BlockFn = {
            let inner = inner.clone();
            Arc::new(move |b: &[f32]| {
                inner.level(Speaker::Them, audio::level(b));
                inner.feed_now(Speaker::Them, b);
            })
        };
        let on_sys_end: EndFn = {
            let inner = inner.clone();
            Arc::new(move |why: String| {
                inner.notice(format!("Recording the computer's sound stopped ({why}).").replace("(failed: ", "("));
                inner.changed();
            })
        };
        let sys_err = match system_audio::start(on_block, on_sys_end) {
            Ok(c) => {
                *self.system.lock() = Some(c);
                inner.doc.lock().system_audio = true;
                None
            }
            Err(e) => Some(e),
        };
        match (mic_err, sys_err) {
            (Some(m), Some(s)) => anyhow::bail!("Could not open the microphone ({m:#}) nor the computer's sound ({s:#})."),
            (Some(m), None) => inner.notice(format!("No microphone ({m:#}): only the computer's sound is recorded.")),
            (None, Some(s)) => inner.notice(format!("Only your microphone is recorded: {s:#}.")),
            (None, None) => {}
        }
        inner.save();
        inner.changed();
        Ok(())
    }

    pub fn live(&self) -> Live {
        self.inner.live()
    }

    pub fn meeting(&self) -> Meeting {
        self.inner.doc.lock().clone()
    }

    /// The page saves the title and the notes while you type.
    pub fn update(&self, title: Option<String>, notes: Option<String>) {
        {
            let mut doc = self.inner.doc.lock();
            if let Some(t) = title {
                doc.title = t;
            }
            if let Some(n) = notes {
                doc.notes = n;
            }
        }
        self.inner.save();
    }

    fn stop_sources(&self) {
        *self.inner.ended.lock() = Some(self.inner.clock.elapsed().as_secs_f64());
        // The mic's stop sends its last block to the tap; then the rest of each source goes out.
        if let Some(mut m) = self.mic.lock().take() {
            m.stop();
        }
        if let Some(mut s) = self.system.lock().take() {
            s.stop();
        }
        for who in [Speaker::Me, Speaker::Them] {
            let rest = self.inner.chunkers[who.index()].lock().flush();
            if let Some(c) = rest {
                self.inner.send(who, c);
            }
        }
    }

    /// Stop recording, wait for the last pieces, save. Blocking (seconds at most).
    pub fn finish(&self) -> Meeting {
        *self.inner.state.lock() = "finishing";
        self.stop_sources();
        self.inner.changed();
        drop(self.inner.tx.lock().take()); // the worker ends after the last piece
        if let Some(w) = self.worker.lock().take() {
            let _ = w.join();
        }
        {
            let mut doc = self.inner.doc.lock();
            doc.state = "done".into();
            doc.duration = self.inner.elapsed();
        }
        *self.inner.state.lock() = "done";
        self.inner.save();
        self.inner.changed();
        self.meeting()
    }

    /// The app quits: keep what is transcribed, do not wait for the rest.
    pub fn abandon(&self) {
        self.stop_sources();
        drop(self.inner.tx.lock().take());
        {
            let mut doc = self.inner.doc.lock();
            doc.state = "done".into();
            doc.duration = self.inner.elapsed();
        }
        self.inner.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: usize = RATE as usize;

    fn tone(secs: f64) -> Vec<f32> {
        (0..(secs * R as f64) as usize).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect()
    }

    fn quiet(secs: f64) -> Vec<f32> {
        vec![0.0; (secs * R as f64) as usize]
    }

    fn line(who: Speaker, start: f64, end: f64, text: &str) -> Line {
        Line { speaker: who, start, end, text: text.into() }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ow-meet-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn pieces_are_cut_at_pauses() {
        let mut c = Chunker::default();
        let mut audio = tone(5.0);
        audio.extend(quiet(1.0));
        let out = c.push(&audio, 0);
        assert_eq!(out.len(), 1, "speech then a pause: one piece");
        assert_eq!(out[0].start, 0);
        assert!(out[0].pcm.len() >= 5 * R && out[0].pcm.len() <= 6 * R);
        // Under the minimum length: a pause does not cut yet.
        let mut c = Chunker::default();
        let mut short = tone(2.0);
        short.extend(quiet(1.0));
        assert!(c.push(&short, 0).is_empty());
        assert_eq!(c.flush().map(|p| p.pcm.len()), Some(3 * R), "the end of the meeting gives the rest");
    }

    #[test]
    fn long_speech_is_cut_at_the_quietest_moment_and_silence_is_dropped() {
        let mut c = Chunker::default();
        let mut audio = tone(16.0);
        audio.extend(quiet(0.2)); // a short breath at 16 s: not a pause, but the quietest moment
        audio.extend(tone(10.0));
        let out = c.push(&audio, 0);
        assert_eq!(out.len(), 1);
        let cut = out[0].pcm.len();
        assert!(cut >= 16 * R && cut <= 16 * R + R / 5, "cut at the breath: {cut}");
        assert!(cut <= MAX_PIECE_S * R);
        // Nothing plays (zeros): no pieces at all, and the position moves on.
        let mut c = Chunker::default();
        assert!(c.push(&quiet(30.0), 0).is_empty());
        assert!(c.flush().is_none());
        let out = c.push(&[tone(5.0), quiet(1.0)].concat(), 30 * R);
        assert_eq!(out[0].start, 30 * R, "times stay right after silence");
    }

    #[test]
    fn a_source_that_stalls_keeps_its_times() {
        let mut c = Chunker::default();
        assert!(c.push(&tone(2.0), 0).is_empty());
        // The next audio comes 10 s later by the clock: the old part goes out, the new starts at 10 s.
        let out = c.push(&tone(1.0), 10 * R);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].start, out[0].pcm.len()), (0, 2 * R));
        assert_eq!(c.flush().map(|p| p.start), Some(10 * R));
    }

    #[test]
    fn echoes_of_the_others_are_dropped_from_me() {
        let them = line(Speaker::Them, 10.0, 14.0, "We should ship the new version on Friday.");
        let echo = line(Speaker::Me, 10.5, 14.2, "we should ship the new version friday");
        let mine = line(Speaker::Me, 15.0, 17.0, "I think Monday is better for us.");
        let later = line(Speaker::Me, 40.0, 44.0, "we should ship the new version on Friday");
        let short = line(Speaker::Me, 11.0, 11.5, "the version");
        let mut lines = vec![them.clone(), echo, mine.clone(), short.clone(), later.clone()];
        drop_echoes(&mut lines, 0.0);
        assert_eq!(lines, vec![them, mine, short, later], "the echo goes; own words, short words and later repeats stay");
    }

    #[test]
    fn the_session_transcribes_both_sources_in_time_order_and_saves() {
        let dir = temp("session");
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let ev = events.clone();
        let emit: EmitFn = Arc::new(move |e| {
            if let Event::Live(l) = e {
                ev.lock().push(l.state);
            }
        });
        // The "model" knows each piece by its length (other words, so no echo rule applies).
        let transcribe: TranscribeFn = Arc::new(|pcm: Vec<f32>| {
            let secs = pcm.len() / R;
            let text = if secs == 3 { "good morning to all of you" } else { "the numbers look fine this week" };
            Ok(vec![Segment { start: 0.5, end: secs as f64, text: text.into() }, Segment { start: 1.0, end: 1.0, text: "[Music]".into() }])
        });
        let doc = Meeting::new("  Weekly sync  ", "parakeet-v3");
        let id = doc.id.clone();
        let s = Session::new(dir.clone(), doc, transcribe, emit).unwrap();
        assert_eq!(load_in(&dir, &id).unwrap().state, "recording", "saved at once (a crash keeps it)");
        let them = [tone(5.0), quiet(1.0)].concat();
        s.inner.feed(Speaker::Them, &them, 2 * R); // the others from 2 s
        s.inner.feed(Speaker::Me, &tone(3.0), 0); // me from 0 s: under the minimum, sent at the end
        s.update(None, Some("- ask about Friday".into()));
        let m = s.finish();
        assert_eq!(m.title, "Weekly sync");
        assert_eq!(m.state, "done");
        let got: Vec<(Speaker, f64, &str)> = m.lines.iter().map(|l| (l.speaker, l.start, l.text.as_str())).collect();
        assert_eq!(got, vec![(Speaker::Me, 0.5, "good morning to all of you"), (Speaker::Them, 2.5, "the numbers look fine this week")], "in time order, no [Music]");
        let saved = load_in(&dir, &id).unwrap();
        assert_eq!(saved, m);
        assert_eq!(saved.notes, "- ask about Friday");
        assert_eq!(events.lock().last().map(String::as_str), Some("done"));
        let list = list_in(&dir);
        assert_eq!((list.len(), list[0].lines, list[0].excerpt.as_str()), (1, 2, "- ask about Friday"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_store_lists_newest_first_and_refuses_odd_ids() {
        let dir = temp("store");
        assert!(list_in(&dir).is_empty(), "no folder yet: no meetings");
        let mut a = Meeting::new("", "small");
        a.id = "m-100".into();
        a.started_at = 100;
        a.lines = vec![line(Speaker::Them, 0.0, 1.0, "hello there")];
        let mut b = a.clone();
        b.id = "m-200".into();
        b.started_at = 200;
        b.title = "Later".into();
        save_in(&dir, &a).unwrap();
        save_in(&dir, &b).unwrap();
        std::fs::write(dir.join("m-300.json"), "not json").unwrap();
        let list = list_in(&dir);
        assert_eq!(list.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["m-200", "m-100"]);
        assert_eq!((list[1].title.as_str(), list[1].excerpt.as_str()), ("Meeting", "hello there"));
        for bad in ["../x", "m-", "m-1/2", "x-1", "m-12a", ""] {
            assert!(!valid_id(bad), "{bad}");
            assert!(load_in(&dir, bad).is_err());
            assert!(delete_in(&dir, bad).is_err());
        }
        delete_in(&dir, "m-100").unwrap();
        assert_eq!(list_in(&dir).len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "private folder");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn markdown_has_the_notes_and_joined_paragraphs() {
        let mut m = Meeting::new("Planning", "small");
        m.started_at = 1_793_624_700; // 2026-11-02 13:05 UTC
        m.duration = 1_830.0;
        m.notes = "- ship Friday\n".into();
        m.lines = vec![
            line(Speaker::Them, 1.0, 3.0, "Hello."),
            line(Speaker::Them, 3.5, 5.0, "Can you hear me?"),
            line(Speaker::Me, 6.0, 7.0, "Yes."),
            line(Speaker::Them, 3700.0, 3701.0, "Bye."),
        ];
        let md = to_markdown(&m, 60);
        assert_eq!(
            md,
            "# Planning\n\n2026-11-02 14:05 · 31 min\n\n## Notes\n\n- ship Friday\n\n## Transcript\n\n**Them** (00:01): Hello. Can you hear me?\n\n**Me** (00:06): Yes.\n\n**Them** (1:01:40): Bye.\n"
        );
        assert_eq!(local_time(0, 0), "1970-01-01 00:00");
        assert_eq!(local_time(951_782_400, 0), "2000-02-29 00:00", "leap day");
        assert_eq!(local_time(1_793_624_700, -300), "2026-11-02 08:05");
        assert_eq!(duration_text(20.0), "under 1 min");
        assert_eq!(duration_text(4_000.0), "1 h 07 min");
        assert_eq!(excerpt("one two three four", 9), "one two…");
    }
}
