//! Meetings: your microphone ("Me") and the computer's sound ("Them", the others in a call) as
//! a live transcript, with your notes next to it. Each source is cut into pieces at pauses and
//! the pieces go to the speech model in order. Saved as JSON in <data dir>/meetings; nothing
//! leaves the computer. No audio is kept, except with Settings → Models → "Tell the other voices
//! apart": the computer's sound then stays in a temporary file until the voices are told apart
//! after Stop (src/voices.rs).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// "Them" only: which of the others (1, 2, …, see `Meeting::people`), told apart by voice
    /// after Stop; 0 = not known.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub person: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The longest name of a speaker (characters).
pub const MAX_NAME: usize = 60;

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
    /// Notes written by a language model (Markdown), a draft; "" = none yet.
    pub ai_notes: String,
    /// Which model wrote them, and when (unix seconds).
    pub ai_model: String,
    pub ai_at: u64,
    /// Questions about the meeting and the model's answers (Ask about this meeting), oldest first.
    pub chat: Vec<ChatMessage>,
    /// Settings → Note apps: the file it was saved as (in the chosen folder), and the hash of
    /// what was written (a file that differs from it was edited outside: never overwritten).
    pub export_file: String,
    pub export_hash: String,
    /// The folder it was saved in (another folder chosen later: a new note there).
    pub export_dir: String,
    /// One more for each fix of the transcript by the user; `ai_version`: the version the AI
    /// notes were written from (lower: they can be written again for the fixed text).
    pub transcript_version: u64,
    pub ai_version: u64,
    /// The names the user gave the others found by voice: `people[n - 1]` names person n ("" =
    /// "Speaker n"). As long as the highest person number in the lines.
    pub people: Vec<String>,
    /// The screen video (Settings → Meetings → Record the screen), from when it started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<Video>,
}

/// A meeting's screen video (src/screen.rs), a file next to the meeting's JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Video {
    /// Its name in the meetings folder: "m-<id>.webm" or "m-<id>.mp4".
    pub file: String,
    /// Seconds from the meeting's start (time 0 of the transcript) to the video's time 0: a
    /// moment `t` of the transcript is `t - offset` in the video. The picker can take seconds.
    pub offset: f64,
    pub width: u32,
    pub height: u32,
    /// "AV1", "VP8", "H.264"…
    pub codec: String,
    /// Bytes on disk (0 while it records).
    pub bytes: u64,
}

impl Meeting {
    /// "Me", "Them", or the person's name ("Speaker 2" before one is given).
    pub fn label(&self, l: &Line) -> String {
        match (l.speaker, l.person) {
            (Speaker::Me, _) => "Me".into(),
            (Speaker::Them, 0) => "Them".into(),
            (Speaker::Them, n) => match self.people.get(n as usize - 1).map(|s| s.trim()) {
                Some(name) if !name.is_empty() => name.to_string(),
                _ => format!("Speaker {n}"),
            },
        }
    }
}

/// One chat message: a question by "user" or an answer by "assistant".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
    /// Unix seconds.
    pub at: u64,
}

/// The longest question (characters).
pub const MAX_QUESTION: usize = 4_000;

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
    /// The first words of the notes, else of the transcript. In a search: the words around
    /// the first match.
    pub excerpt: String,
    /// In a search, where the excerpt's match is: transcript | ai_notes | notes ("" = the
    /// title only, or no search).
    pub found_in: String,
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

/// Written to a temporary file first, synced, then renamed: a crash or a power loss never
/// leaves half a file. One write at a time in this process (the notes and the transcript of
/// the same meeting are saved from two threads).
pub fn save_in(dir: &Path, m: &Meeting) -> anyhow::Result<()> {
    static ONE_WRITER: Mutex<()> = Mutex::new(());
    let _one = ONE_WRITER.lock();
    let path = file_in(dir, &m.id)?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?; // transcripts are private
    }
    let tmp = dir.join(format!(".{}.json.tmp", m.id));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(m)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all(); // the rename itself
    }
    Ok(())
}

/// Read, change and save a saved meeting, one change at a time (the page's notes and the AI
/// notes can arrive together).
/// Edits and deletes of saved meetings, one at a time (a delete never loses to an edit that
/// writes the meeting back).
static ONE_EDIT: Mutex<()> = Mutex::new(());

pub fn edit_in(dir: &Path, id: &str, change: impl FnOnce(&mut Meeting)) -> anyhow::Result<Meeting> {
    let _one = ONE_EDIT.lock();
    let mut m = load_in(dir, id)?;
    change(&mut m);
    save_in(dir, &m)?;
    Ok(m)
}

pub fn load_in(dir: &Path, id: &str) -> anyhow::Result<Meeting> {
    let text = std::fs::read_to_string(file_in(dir, id)?).map_err(|e| anyhow::anyhow!("meeting {id}: {e}"))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn delete_in(dir: &Path, id: &str) -> anyhow::Result<()> {
    let _one = ONE_EDIT.lock();
    std::fs::remove_file(file_in(dir, id)?).map_err(|e| anyhow::anyhow!("meeting {id}: {e}"))?;
    crate::screen::delete_files(dir, id); // its screen video
    Ok(())
}

/// At start: the screen videos that a crash left unfinished get their final name (written again
/// whole when they can be), and their size in the meeting. A video without its meeting is
/// deleted. `left`: `screen::leftovers`, listed before any meeting can start (a new meeting's
/// `.part` is never taken for a leftover).
pub fn recover_videos(dir: &Path, left: Vec<(String, PathBuf, crate::screen::Container)>) {
    for (id, part, c) in left {
        if load_in(dir, &id).is_err() {
            let _ = std::fs::remove_file(&part);
            continue;
        }
        let r = crate::screen::finish_part(dir, &id, &part, c, false);
        let _ = edit_in(dir, &id, |m| match &r {
            Ok((_, bytes)) => {
                let v = m.video.get_or_insert_with(|| Video { file: crate::screen::file_name(&id, c), ..Default::default() });
                v.bytes = *bytes;
            }
            Err(_) => m.video = None,
        });
        if r.is_err() {
            crate::screen::delete_files(dir, &id);
        }
    }
}

/// The screen video of meeting `id` on disk, when it has one.
pub fn video_path_in(dir: &Path, id: &str) -> Option<PathBuf> {
    let v = load_in(dir, id).ok()?.video?;
    // Only a name of this meeting: never a path elsewhere.
    let ok = [crate::screen::Container::Webm, crate::screen::Container::Mp4].iter().any(|c| v.file == crate::screen::file_name(id, *c));
    let path = dir.join(&v.file);
    (ok && path.is_file()).then_some(path)
}

/// Newest first. Broken files are skipped.
pub fn list_in(dir: &Path) -> Vec<Summary> {
    search_in(dir, "")
}

/// The meetings that have every word of `query` (any order, any case, also inside a word) in
/// their title, transcript, AI notes or notes; newest first. An empty query: all of them.
pub fn search_in(dir: &Path, query: &str) -> Vec<Summary> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<Summary> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json").filter(|id| valid_id(id))?.to_string();
            summary(load_in(dir, &id).ok()?, &words)
        })
        .collect();
    out.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    out
}

/// The list entry of `m`; None when `words` (lower case) are not all in it.
fn summary(m: Meeting, words: &[String]) -> Option<Summary> {
    let transcript = m.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
    let fields = [("transcript", transcript.as_str()), ("ai_notes", m.ai_notes.as_str()), ("notes", m.notes.as_str())];
    let mut excerpt_text = excerpt(if m.notes.trim().is_empty() { &transcript } else { &m.notes }, 140);
    let mut found_in = String::new();
    if !words.is_empty() {
        let lower: Vec<String> = std::iter::once(m.title.as_str()).chain(fields.iter().map(|f| f.1)).map(str::to_lowercase).collect();
        if !words.iter().all(|w| lower.iter().any(|f| f.contains(w.as_str()))) {
            return None;
        }
        // The excerpt: around the first word found in the transcript, else the AI notes, else
        // the notes (a match in the title only keeps the usual excerpt).
        if let Some((name, text, at)) = fields.iter().find_map(|(name, text)| words.iter().find_map(|w| find_ci(text, w)).map(|at| (*name, *text, at))) {
            excerpt_text = around(text, at, 140);
            found_in = name.into();
        }
    }
    Some(Summary { id: m.id, title: m.title, started_at: m.started_at, duration: m.duration, state: m.state, lines: m.lines.len(), excerpt: excerpt_text, found_in })
}

/// The byte index of `needle` (lower case) in `hay`, any case.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() {
        return None;
    }
    let lower: Vec<(char, usize)> = hay.char_indices().flat_map(|(i, c)| c.to_lowercase().map(move |l| (l, i))).collect();
    lower.windows(n.len()).position(|w| w.iter().map(|x| x.0).eq(n.iter().copied())).map(|p| lower[p].1)
}

/// About `max` characters of `text` around byte `at`, cut at spaces, with "…" where cut.
fn around(text: &str, at: usize, max: usize) -> String {
    let before: String = {
        let head: Vec<char> = text[..at].chars().collect();
        let start = head.len().saturating_sub(max / 3);
        let mut s: String = head[start..].iter().collect();
        if start > 0 {
            s = format!("…{}", s.split_once(char::is_whitespace).map_or("", |(_, rest)| rest));
        }
        s
    };
    let rest = excerpt(&text[at..], max - before.chars().count().min(max / 3));
    excerpt(&format!("{before}{rest}"), max + 2)
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

/// No frame above the silence level. (A short word in seconds of silence is not silence.)
fn is_silent(pcm: &[f32]) -> bool {
    pcm.chunks(FRAME).map(rms).all(|r| r < SILENCE)
}

/// Silence kept before speech when the silence in front of it is dropped.
const LEAD_FRAMES: usize = 3;

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
    /// Where `push(block, at)` puts the block: right after the audio so far, or at `at` after a
    /// gap of over 1 s.
    pub fn position_for(&self, at: usize) -> usize {
        let end = self.start + self.buf.len();
        if at > end + RATE as usize {
            at
        } else {
            end
        }
    }

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
        // Silence in front of the speech (or only silence): drop it, so a piece starts near the
        // speech and a short word after a long quiet time is not lost with that quiet time.
        match frames.iter().position(|&r| r >= SILENCE) {
            None => return Step::Drop(frames.len() * FRAME),
            Some(i) if i > LEAD_FRAMES => return Step::Drop((i - LEAD_FRAMES) * FRAME),
            _ => {}
        }
        // How loud the speech is: from the frames that are not silence.
        let level = loud(&frames.iter().copied().filter(|&r| r >= SILENCE).collect::<Vec<_>>());
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
/// Chinese and Japanese have no spaces: each character counts as a word there.
fn no_spaces_script(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in text.split_whitespace() {
        let mut cur = String::new();
        for c in w.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase) {
            if no_spaces_script(c) {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// Longest common subsequence of two word lists (words in the same order).
fn common_in_order(a: &[String], b: &[String]) -> usize {
    let mut row = vec![0usize; b.len() + 1];
    for x in a {
        let mut diag = 0;
        for (j, y) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if x == y { diag + 1 } else { up.max(row[j]) };
            diag = up;
        }
    }
    row[b.len()]
}

/// Without headphones the microphone also hears the others: their words come back as "Me", at
/// the same time. A "Me" line of 3+ words is an echo when it lies inside the time of "Them"
/// lines (1.5 s of slack for the different cuts) and 80% of its words are in them, in order.
/// So "Me" repeating the others' words after they spoke is kept.
fn is_echo(me: &Line, them: &[&Line]) -> bool {
    const SLACK: f64 = 1.5;
    let mine = words(&me.text);
    if mine.len() < 3 {
        return false;
    }
    let near: Vec<&&Line> = them.iter().filter(|t| t.start < me.end && me.start < t.end).collect();
    let (Some(from), Some(to)) = (near.iter().map(|t| t.start).reduce(f64::min), near.iter().map(|t| t.end).reduce(f64::max)) else {
        return false;
    };
    if me.start < from - SLACK || me.end > to + SLACK {
        return false;
    }
    let theirs: Vec<String> = near.iter().flat_map(|t| words(&t.text)).collect();
    common_in_order(&mine, &theirs) as f64 >= 0.8 * mine.len() as f64
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
            Some(p) if p.speaker == l.speaker && p.person == l.person && l.start - p.end < 2.0 && p.text.len() < 600 => {
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

pub fn duration_text(seconds: f64) -> String {
    let min = (seconds / 60.0).round() as u64;
    match min {
        0 => "under 1 min".into(),
        m if m < 60 => format!("{m} min"),
        m => format!("{} h {:02} min", m / 60, m % 60),
    }
}

/// A paragraph fixed by the user: lines `from..=to` (one paragraph as the page shows it, see
/// `paragraphs`) become one line with `text`, said by `speaker` (and `person` of the others; 0
/// for Me or not known); its start and end stay. Empty
/// text removes the paragraph. `before`: its text as the page showed it (the meeting changed
/// meanwhile: nothing is changed).
pub fn edit_paragraph(m: &mut Meeting, from: usize, to: usize, before: &str, text: &str, speaker: Speaker, person: u32) -> anyhow::Result<()> {
    let stale = "The transcript changed meanwhile: open the meeting again.";
    let person = if speaker == Speaker::Them { person } else { 0 };
    anyhow::ensure!(person as usize <= m.people.len(), "There is no Speaker {person} in this meeting.");
    anyhow::ensure!(from <= to && to < m.lines.len(), stale);
    let shown = m.lines[from..=to].iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
    anyhow::ensure!(shown == before, stale);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = m.lines[from..=to].iter().map(|l| l.end).fold(m.lines[from].end, f64::max);
    if text.is_empty() {
        m.lines.drain(from..=to);
    } else {
        m.lines[from] = Line { speaker, start: m.lines[from].start, end, text, person };
        m.lines.drain(from + 1..=to);
    }
    m.transcript_version += 1;
    Ok(())
}

/// Name person `person` (1, 2, …) of the others. The name the user gave another person merges
/// the two (the voices of one person were found as two): its lines go to the other one ("Speaker
/// 3" is not a name: it never merges). "" gives back "Speaker n".
pub fn name_speaker(m: &mut Meeting, person: u32, name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(person >= 1 && person as usize <= m.people.len(), "There is no Speaker {person} in this meeting.");
    let name: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
    anyhow::ensure!(name.chars().count() <= MAX_NAME, "A name has at most {MAX_NAME} characters.");
    let other = (1..=m.people.len() as u32).find(|&n| {
        n != person && !name.is_empty() && m.people[n as usize - 1].to_lowercase() == name.to_lowercase() && m.lines.iter().any(|l| l.person == n)
    });
    match other {
        Some(into) => {
            for l in m.lines.iter_mut().filter(|l| l.speaker == Speaker::Them && l.person == person) {
                l.person = into;
            }
            m.people[person as usize - 1].clear();
        }
        None => m.people[person as usize - 1] = name,
    }
    m.transcript_version += 1;
    Ok(())
}

/// After Stop: who of the others said each line (`voices::assign`), and room for their names.
pub fn set_people(m: &mut Meeting, turns: &[crate::voices::Turn]) {
    let n = crate::voices::assign(&mut m.lines, turns);
    m.people = vec![String::new(); n];
    m.transcript_version += 1;
}

/// What the language model reads: the title, your notes and the transcript (no AI notes).
/// No date: the app may not know the time zone here, and a wrong date would end up in the notes.
pub fn model_input(m: &Meeting) -> String {
    let mut plain = m.clone();
    plain.ai_notes.clear();
    markdown(&plain, &duration_text(m.duration), Parts::ALL)
}

/// What the chat model reads about the meeting: the title, your notes, the AI notes and the
/// transcript (no date, as in `model_input`).
pub fn chat_input(m: &Meeting) -> String {
    markdown(m, &duration_text(m.duration), Parts::ALL)
}

pub fn to_markdown(m: &Meeting, offset_min: i32) -> String {
    markdown(m, &format!("{} · {}", local_time(m.started_at, offset_min), duration_text(m.duration)), Parts::ALL)
}

/// The chosen parts, with the start in local time (`offset_min` east of UTC): a note app's file.
pub fn to_markdown_parts(m: &Meeting, offset_min: i32, parts: Parts) -> String {
    markdown(m, &format!("{} · {}", local_time(m.started_at, offset_min), duration_text(m.duration)), parts)
}

/// The parts of a meeting in a Markdown copy (the title and the time are always there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    pub ai_notes: bool,
    pub notes: bool,
    pub transcript: bool,
}

impl Parts {
    pub const ALL: Parts = Parts { ai_notes: true, notes: true, transcript: true };
}

/// For AI agents (MCP): the start in UTC (the server does not know the user's time zone), the id,
/// and only `parts`.
pub fn to_markdown_utc(m: &Meeting, parts: Parts) -> String {
    let when = format!("{} UTC · {} · id {}", local_time(m.started_at, 0), duration_text(m.duration), m.id);
    markdown(m, &when, parts)
}

fn markdown(m: &Meeting, when: &str, parts: Parts) -> String {
    let mut s = format!("# {}\n\n{when}\n\n", m.title);
    if parts.ai_notes && !m.ai_notes.trim().is_empty() {
        // The model's own "## …" headings go one level down, under this one.
        let body: String = m.ai_notes.trim().lines().map(|l| if l.starts_with('#') { format!("#{l}\n") } else { format!("{l}\n") }).collect();
        s += &format!("## AI notes ({}, a draft)\n\n{}\n\n", m.ai_model, body.trim_end());
    }
    if parts.notes && !m.notes.trim().is_empty() {
        s += &format!("## Notes\n\n{}\n\n", m.notes.trim());
    }
    if parts.transcript {
        s += "## Transcript\n\n";
        if m.lines.is_empty() {
            s += "No speech was found.\n";
        }
        for p in paragraphs(&m.lines) {
            s += &format!("**{}** ({}): {}\n\n", m.label(&p), clock(p.start), p.text);
        }
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
    /// Your microphone is muted (the strip's button): silence goes in its place.
    pub muted: bool,
    /// The screen video: "" (not recorded) | "waiting" (the screen picker is open) | "on" |
    /// "stopped" (it ended before the meeting; see the notices).
    pub screen: String,
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
    /// The microphone is muted: its audio is replaced by silence (the clock goes on).
    muted: AtomicBool,
    emit: EmitFn,
    saving: Mutex<()>,
    /// Settings → Models → Tell the other voices apart: the computer's sound, kept until Stop.
    tape: Mutex<Option<crate::voices::Tape>>,
    /// See `Live::screen`.
    screen: Mutex<&'static str>,
}

/// More pieces than this waiting: the model is slower than the speech.
const BEHIND: usize = 6;

impl Inner {
    fn elapsed(&self) -> f64 {
        self.ended.lock().unwrap_or_else(|| self.clock.elapsed().as_secs_f64())
    }

    /// Audio of one source; `at` = its position by the clock, in samples.
    fn feed(&self, who: Speaker, block: &[f32], at: usize) {
        let (pos, chunks) = {
            let mut c = self.chunkers[who.index()].lock();
            (c.position_for(at), c.push(block, at))
        };
        if who == Speaker::Them {
            // At the transcript's own position: the voices' times are the lines' times.
            let mut tape = self.tape.lock();
            if let Some(t) = tape.as_mut() {
                if let Err(e) = t.write_at(block, pos) {
                    *tape = None; // the file is deleted; the voices are not told apart this time
                    drop(tape);
                    self.notice(format!("The others' voices will not be told apart in this meeting: {e}."));
                }
            }
        }
        for c in chunks {
            self.send(who, c);
        }
    }

    fn feed_now(&self, who: Speaker, block: &[f32]) {
        if self.ended.lock().is_some() && *self.state.lock() != "finishing" {
            return;
        }
        let now = (self.clock.elapsed().as_secs_f64() * RATE as f64) as usize;
        if who == Speaker::Me && self.muted.load(Ordering::SeqCst) {
            // Muted: silence of the same length, so the times of later lines stay right.
            self.feed(who, &vec![0.0; block.len()], now.saturating_sub(block.len()));
            return;
        }
        self.feed(who, block, now.saturating_sub(block.len()));
    }

    fn send(&self, who: Speaker, c: Chunk) {
        if let Some(tx) = self.tx.lock().as_ref() {
            if self.pending.fetch_add(1, Ordering::SeqCst) + 1 > BEHIND && *self.state.lock() == "recording" {
                self.notice("The speech model is slower than the speech: the text comes late, and Stop waits for it. Parakeet or Small keep up.".into());
            }
            if tx.send((who, c)).is_err() {
                self.pending.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    fn level(&self, who: Speaker, value: f32) {
        let value = if who == Speaker::Me && self.muted.load(Ordering::SeqCst) { 0.0 } else { value };
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
            let line = Line { speaker: who, start, end: (offset + s.end.clamp(0.0, len)).max(start), text: text.to_string(), person: 0 };
            first = first.min(line.start);
            let at = doc.lines.partition_point(|l| l.start <= line.start);
            doc.lines.insert(at, line);
        }
        if first < f64::MAX {
            drop_echoes(&mut doc.lines, first);
        }
    }

    /// The snapshot is taken under the save lock: a later state never loses to an older one.
    fn save(&self) {
        let _one = self.saving.lock();
        let elapsed = self.elapsed();
        let doc = {
            let mut d = self.doc.lock();
            if d.state == "recording" {
                d.duration = elapsed; // right after a crash too
            }
            d.clone()
        };
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
            muted: self.muted.load(Ordering::SeqCst),
            screen: self.screen.lock().to_string(),
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
    tape_done: Mutex<Option<PathBuf>>,
    /// The screen video being recorded.
    video: Mutex<Option<crate::screen::Recording>>,
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
            muted: AtomicBool::new(false),
            emit,
            saving: Mutex::new(()),
            tape: Mutex::new(None),
            screen: Mutex::new(""),
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
        Ok(Arc::new(Self {
            inner,
            mic: Mutex::new(None),
            system: Mutex::new(None),
            worker: Mutex::new(Some(worker)),
            tape_done: Mutex::new(None),
            video: Mutex::new(None),
        }))
    }

    pub fn id(&self) -> String {
        self.inner.doc.lock().id.clone()
    }

    /// Keep the computer's sound in `path` until Stop, to tell the voices apart (call before
    /// `open_sources`).
    pub fn keep_their_sound(&self, path: &Path) -> anyhow::Result<()> {
        *self.inner.tape.lock() = Some(crate::voices::Tape::create(path)?);
        Ok(())
    }

    /// After `finish`: the file with the computer's sound (None: it was not kept, or it failed).
    pub fn their_sound(&self) -> Option<PathBuf> {
        self.tape_done.lock().take()
    }

    /// A problem to show in the meeting.
    pub fn notice(&self, text: String) {
        self.inner.notice(text);
        self.inner.changed();
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

    // -------------------------------------------------------------- the screen video
    /// The screen picker is open (the audio records already).
    pub fn screen_waiting(&self) {
        *self.inner.screen.lock() = "waiting";
        self.inner.changed();
    }

    /// The screen is not recorded in this meeting: why.
    pub fn screen_failed(&self, why: &str) {
        *self.inner.screen.lock() = "";
        self.notice(format!("The screen is not recorded: {why}."));
    }

    /// For `screen::start`: the video ended by itself (the sharing was stopped…). The file so
    /// far is kept.
    pub fn screen_end_fn(self: &Arc<Self>) -> Box<dyn FnOnce(String) + Send> {
        let inner = self.inner.clone();
        Box::new(move |why: String| {
            if inner.ended.lock().is_some() {
                return; // the meeting stops: that ended it
            }
            *inner.screen.lock() = "stopped";
            inner.notice(format!("The screen recording stopped ({why}); the video so far is kept."));
            inner.changed();
        })
    }

    /// The screen video joins the meeting: its time 0 is `rec.started` on the meeting's clock.
    /// The meeting stopped meanwhile: `rec` comes back (to be discarded).
    pub fn attach_video(&self, rec: crate::screen::Recording) -> Result<(), crate::screen::Recording> {
        let ended = self.inner.ended.lock(); // held: `stop_sources` takes the video after this
        if ended.is_some() {
            return Err(rec);
        }
        let offset = rec.started.saturating_duration_since(self.inner.clock).as_secs_f64();
        {
            let mut doc = self.inner.doc.lock();
            let file = crate::screen::file_name(&doc.id, rec.container);
            doc.video = Some(Video { file, offset, width: rec.size.0, height: rec.size.1, codec: rec.codec.into(), bytes: 0 });
        }
        *self.video.lock() = Some(rec);
        *self.inner.screen.lock() = "on";
        drop(ended);
        self.inner.save();
        self.inner.changed();
        Ok(())
    }

    /// Stop the screen video and give it its final name (its size in the meeting).
    fn stop_video(&self) {
        let Some(rec) = self.video.lock().take() else { return };
        let (part, container) = (rec.part.clone(), rec.container);
        let end = rec.stop();
        let id = self.id();
        let r = crate::screen::finish_part(&self.inner.dir, &id, &part, container, end.closed);
        let mut doc = self.inner.doc.lock();
        match r {
            Ok((_, bytes)) => {
                if let Some(v) = doc.video.as_mut() {
                    v.bytes = bytes;
                }
            }
            Err(e) => {
                doc.video = None;
                drop(doc);
                crate::screen::delete_files(&self.inner.dir, &id);
                self.inner.notice(format!("The screen video could not be saved: {e:#}."));
            }
        }
    }

    /// Mute or unmute your microphone (the computer's sound goes on).
    pub fn set_muted(&self, on: bool) {
        if self.inner.muted.swap(on, Ordering::SeqCst) != on {
            self.inner.changed();
        }
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
        self.stop_video();
        if *self.inner.screen.lock() == "waiting" {
            *self.inner.screen.lock() = ""; // the picker's answer is discarded (`attach_video`)
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
        if let Some(t) = self.inner.tape.lock().take() {
            let path = t.path().to_path_buf();
            match t.finish() {
                Ok(p) => *self.tape_done.lock() = Some(p),
                Err(_) => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        self.inner.save();
        self.inner.changed();
        self.meeting()
    }

    /// The app quits: keep what is transcribed, do not wait for the rest.
    pub fn abandon(&self) {
        *self.inner.state.lock() = "finishing"; // the mic's last block is still taken
        self.stop_sources();
        drop(self.inner.tx.lock().take());
        {
            let mut doc = self.inner.doc.lock();
            doc.state = "done".into();
            doc.duration = self.inner.elapsed();
        }
        drop(self.inner.tape.lock().take()); // an unfinished tape deletes its file
        if let Some(p) = self.tape_done.lock().take() {
            let _ = std::fs::remove_file(p); // `finish` got that far, then failed
        }
        self.inner.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_is_fixed_in_place() {
        let l = |speaker, start: f64, text: &str| Line { speaker, start, end: start + 1.5, text: text.into(), person: 0 };
        let mut m = Meeting {
            lines: vec![l(Speaker::Me, 0.0, "Hello"), l(Speaker::Me, 1.6, "from Lisando."), l(Speaker::Them, 5.0, "Hi.")],
            ..Default::default()
        };
        assert!(edit_paragraph(&mut m, 0, 1, "Hello from Lisando!", "x", Speaker::Me, 0).is_err(), "changed meanwhile");
        assert!(edit_paragraph(&mut m, 0, 5, "", "x", Speaker::Me, 0).is_err());
        edit_paragraph(&mut m, 0, 1, "Hello from Lisando.", "Hello  from\nLisandro.", Speaker::Me, 0).unwrap();
        assert_eq!(m.lines[0], Line { speaker: Speaker::Me, start: 0.0, end: 3.1, text: "Hello from Lisandro.".into(), person: 0 });
        assert_eq!(m.lines.len(), 2);
        edit_paragraph(&mut m, 1, 1, "Hi.", "Hi.", Speaker::Me, 0).unwrap();
        assert_eq!(m.lines[1].speaker, Speaker::Me, "an echo moved to the right side");
        edit_paragraph(&mut m, 1, 1, "Hi.", "  ", Speaker::Me, 0).unwrap();
        assert_eq!(m.lines.len(), 1, "an empty paragraph is removed");
        assert_eq!(m.transcript_version, 3, "one more for each fix");
    }

    #[test]
    fn the_others_get_names_and_two_of_one_person_merge() {
        let l = |speaker, start: f64, person, text: &str| Line { speaker, start, end: start + 1.0, text: text.into(), person };
        let mut m = Meeting {
            title: "Sync".into(),
            lines: vec![l(Speaker::Me, 0.0, 0, "Hi all."), l(Speaker::Them, 2.0, 1, "Hello."), l(Speaker::Them, 3.5, 2, "Good morning."), l(Speaker::Them, 9.0, 3, "Me too.")],
            people: vec![String::new(); 3],
            ..Default::default()
        };
        let md = |m: &Meeting| markdown(m, "", Parts::ALL);
        assert!(md(&m).contains("**Speaker 1** (00:02): Hello.\n\n**Speaker 2** (00:03): Good morning."), "two people: two paragraphs\n{}", md(&m));
        name_speaker(&mut m, 1, "  Ana  María ").unwrap();
        assert_eq!(m.people[0], "Ana María");
        assert!(md(&m).contains("**Ana María** (00:02): Hello."));
        name_speaker(&mut m, 3, "ana maría").unwrap(); // Speaker 3 was Ana too
        assert_eq!(m.lines.iter().map(|l| l.person).collect::<Vec<_>>(), vec![0, 1, 2, 1]);
        assert!(m.people[2].is_empty());
        assert!(name_speaker(&mut m, 4, "Bo").is_err() && name_speaker(&mut m, 0, "Bo").is_err());
        assert!(name_speaker(&mut m, 2, &"x".repeat(MAX_NAME + 1)).is_err());
        name_speaker(&mut m, 2, "Speaker 1").unwrap(); // not a merge: only names the user gave merge
        assert_eq!((m.label(&m.lines[2]), m.lines[2].person), ("Speaker 1".to_string(), 2));
        name_speaker(&mut m, 2, "").unwrap();
        assert_eq!(m.label(&m.lines[2]), "Speaker 2", "an empty name gives the number back");
        // The paragraph fix moves a line to another person; a number that is not there is refused.
        edit_paragraph(&mut m, 2, 2, "Good morning.", "Good morning.", Speaker::Them, 1).unwrap();
        assert_eq!(m.lines[2].person, 1);
        assert!(edit_paragraph(&mut m, 2, 2, "Good morning.", "x", Speaker::Them, 9).is_err());
        edit_paragraph(&mut m, 0, 0, "Hi all.", "Hi all.", Speaker::Me, 2).unwrap();
        assert_eq!(m.lines[0].person, 0, "Me has no person number");
        let json = serde_json::to_string(&m.lines[0]).unwrap();
        assert!(!json.contains("person"), "not written when 0: {json}");
    }

    const R: usize = RATE as usize;

    fn tone(secs: f64) -> Vec<f32> {
        (0..(secs * R as f64) as usize).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect()
    }

    fn quiet(secs: f64) -> Vec<f32> {
        vec![0.0; (secs * R as f64) as usize]
    }

    fn line(who: Speaker, start: f64, end: f64, text: &str) -> Line {
        Line { speaker: who, start, end, text: text.into(), person: 0 }
    }

    #[test]
    fn the_screen_video_is_saved_with_the_meeting() {
        let dir = temp("video");
        let doc = Meeting::new("Demo", "test");
        let id = doc.id.clone();
        let s = Session::new(dir.clone(), doc, Arc::new(|_| Ok(vec![])), Arc::new(|_| {})).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300)); // the picker took a moment
        let Some(rec) = crate::screen::test_recording(&dir, &id) else {
            eprintln!("skipped: no GStreamer with vp8enc");
            return;
        };
        s.attach_video(rec).map_err(|_| ()).unwrap();
        assert_eq!(s.live().screen, "on");
        let v = load_in(&dir, &id).unwrap().video.unwrap();
        assert!(v.offset >= 0.3 && v.offset < 2.0, "the video starts later than the meeting: {}", v.offset);
        assert_eq!((v.file.as_str(), v.width, v.codec.as_str(), v.bytes), (format!("{id}.webm").as_str(), 320, "VP8", 0));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let m = s.finish();
        let v = m.video.unwrap();
        assert!(v.bytes > 1000, "{}", v.bytes);
        assert_eq!(video_path_in(&dir, &id), Some(dir.join(&v.file)));
        assert!(!crate::screen::part_path(&dir, &id, crate::screen::Container::Webm).exists());
        // A screen chosen after Stop does not join.
        let late = crate::screen::test_recording(&dir, "m-9").unwrap();
        let late = s.attach_video(late).err().expect("the meeting has ended");
        late.discard();
        assert!(!crate::screen::part_path(&dir, "m-9", crate::screen::Container::Webm).exists());
        // Only a file of this meeting is played.
        edit_in(&dir, &id, |m| m.video.as_mut().unwrap().file = "../config.toml".into()).unwrap();
        assert_eq!(video_path_in(&dir, &id), None);
        delete_in(&dir, &id).unwrap();
        assert!(!dir.join(&v.file).exists(), "deleted with its meeting");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_video_left_by_a_crash_is_made_whole() {
        let dir = temp("recover");
        let doc = Meeting::new("Crashed", "test");
        let id = doc.id.clone();
        save_in(&dir, &doc).unwrap(); // the crash came before the video joined
        let Some(rec) = crate::screen::test_recording(&dir, &id) else {
            eprintln!("skipped: no GStreamer with vp8enc");
            return;
        };
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let part = rec.part.clone();
        let kept = dir.join("kept.part");
        std::fs::copy(&part, &kept).unwrap(); // as the kill left it
        rec.discard();
        std::fs::rename(&kept, &part).unwrap();
        std::fs::write(crate::screen::part_path(&dir, "m-404", crate::screen::Container::Webm), b"x").unwrap();
        recover_videos(&dir, crate::screen::leftovers(&dir));
        let v = load_in(&dir, &id).unwrap().video.unwrap();
        assert!(v.bytes > 0 && dir.join(&v.file).is_file(), "{v:?}");
        assert!(crate::screen::leftovers(&dir).is_empty(), "a video without its meeting is deleted");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_search_excerpt_shows_the_words_around_the_match() {
        assert_eq!(find_ci("Hello ÑANDÚ world", "ñandú"), Some(6));
        assert_eq!(find_ci("abc", "x"), None);
        let long = format!("{} the budget for next year {}", "word ".repeat(60), "more ".repeat(60));
        let ex = around(&long, long.find("budget").unwrap(), 140);
        assert!(ex.starts_with('…') && ex.ends_with('…') && ex.contains("the budget for next year"), "{ex}");
        assert!(ex.chars().count() <= 145, "{}", ex.chars().count());
        assert_eq!(around("budget at the start", 0, 140), "budget at the start");
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
    fn a_short_word_in_a_long_quiet_time_is_kept() {
        // Quiet, then "bye" (0.5 s), then Stop: the word reaches the model.
        let mut c = Chunker::default();
        assert!(c.push(&[quiet(3.5), tone(0.5)].concat(), 0).is_empty());
        let last = c.flush().expect("the word");
        assert!(last.pcm.len() >= R / 2 && last.start <= 3 * R + R / 2 && last.start + last.pcm.len() == 4 * R, "{} at {}", last.pcm.len(), last.start);
        // A short reply, then a long quiet time: one piece with the word, not dropped as silence.
        let mut c = Chunker::default();
        let out = c.push(&[tone(0.5), quiet(3.6)].concat(), 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start, 0);
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
        // Repeating a part of what they said, after they spoke (with headphones): kept.
        let them = line(Speaker::Them, 10.0, 14.0, "we should do that tomorrow at noon and then review");
        let repeat = line(Speaker::Me, 16.0, 18.0, "we should do that at noon");
        let mut lines = vec![them.clone(), repeat.clone()];
        drop_echoes(&mut lines, 0.0);
        assert_eq!(lines.len(), 2, "said after them: not an echo");
        // The same words in another order are not an echo either.
        let mixed = line(Speaker::Me, 10.5, 13.5, "review then noon at tomorrow that do should we");
        assert!(!is_echo(&mixed, &[&them]));
        // Chinese: each character is a word, so the rule works without spaces.
        let zh_them = line(Speaker::Them, 0.0, 3.0, "我们明天中午发布新版本");
        let zh_echo = line(Speaker::Me, 0.2, 3.1, "我们明天中午发布新版");
        assert!(is_echo(&zh_echo, &[&zh_them]));
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
        // Search: every word, any case, in the title, transcript, AI notes or notes.
        b.lines = vec![line(Speaker::Me, 0.0, 1.0, "we ship the new version on Friday after the review")];
        b.ai_notes = "## Decisions\n- Ship on Friday".into();
        save_in(&dir, &b).unwrap();
        let ids = |q: &str| search_in(&dir, q).into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids("FRIDAY ship"), vec!["m-200"]);
        assert_eq!(ids("hello"), vec!["m-100"]);
        assert_eq!(ids("later"), vec!["m-200"], "the title");
        assert_eq!(ids("decisions"), vec!["m-200"], "the AI notes");
        assert_eq!(ids("frid"), vec!["m-200"], "inside a word");
        assert!(ids("friday nothing").is_empty(), "every word must be there");
        assert_eq!(ids("  ").len(), 2, "no words: all of them");
        let hit = &search_in(&dir, "review")[0];
        assert_eq!((hit.found_in.as_str(), hit.excerpt.as_str()), ("transcript", "we ship the new version on Friday after the review"));
        assert_eq!(search_in(&dir, "later")[0].found_in, "", "a title match keeps the usual excerpt");
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
        // AI notes go first, their headings one level down; the model never reads them back.
        m.ai_notes = "## Resumen\nTodo bien.\n\n## Decisiones\n- Jueves 14".into();
        m.ai_model = "Qwen3.5 4B".into();
        let md = to_markdown(&m, 60);
        assert!(md.contains("## AI notes (Qwen3.5 4B, a draft)\n\n### Resumen\nTodo bien.\n\n### Decisiones\n- Jueves 14\n\n## Notes"), "{md}");
        assert!(!model_input(&m).contains("Todo bien"));
        assert!(model_input(&m).starts_with("# Planning\n\n31 min\n\n"), "no date for the model: {}", model_input(&m));
        assert!(model_input(&m).contains("- ship Friday"));
        // The chat reads the AI notes too; the questions and answers stay out of the export.
        m.chat = vec![ChatMessage { role: "user".into(), text: "Who calls?".into(), at: 1 }];
        assert!(chat_input(&m).contains("Todo bien") && chat_input(&m).contains("**Me** (00:06): Yes."));
        assert!(!to_markdown(&m, 0).contains("Who calls?") && !chat_input(&m).contains("Who calls?"));
    }

    #[test]
    fn the_chat_is_saved_with_the_meeting_and_old_files_have_none() {
        let dir = temp("chat");
        let mut m = Meeting::new("Chat", "small");
        m.id = "m-500".into();
        m.chat = vec![
            ChatMessage { role: "user".into(), text: "¿Quién llama a Acme?".into(), at: 10 },
            ChatMessage { role: "assistant".into(), text: "**Me**, el viernes.".into(), at: 11 },
        ];
        save_in(&dir, &m).unwrap();
        assert_eq!(load_in(&dir, "m-500").unwrap().chat, m.chat);
        assert!(search_in(&dir, "Acme").is_empty(), "search does not look in the chat");
        // A meeting saved before the chat existed.
        let mut old: serde_json::Value = serde_json::to_value(&m).unwrap();
        old.as_object_mut().unwrap().remove("chat");
        std::fs::write(dir.join("m-500.json"), old.to_string()).unwrap();
        assert!(load_in(&dir, "m-500").unwrap().chat.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
