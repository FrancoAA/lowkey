//! Who is speaking: the others in a call ("Them") told apart by their voices, after the meeting
//! stops. While it records, the computer's sound also goes to a temporary file next to the
//! meeting (16-bit, 16 kHz, at the clock's positions: its times are the transcript's times).
//! After Stop, sherpa-onnx finds who spoke when (pyannote segmentation 3.0 + TitaNet-S voice
//! prints, then clustering), each "Them" line gets the person whose voice covers most of it, and
//! the file is deleted. Off by default (Settings → Meetings): turning it on downloads the two
//! models (46 MB).
//!
//! Measured on two AMI meetings (four people; also three people as the call's other side, and
//! that through Opus 24 kbit/s): 89% of the speech time on the right person, against 54% for
//! one label for all; about 2 min for one hour of meeting on the test machine's CPU (8
//! threads). The threshold (1.0) and the 5% rule (`assign`) were chosen on those meetings. A
//! person found twice is merged by giving both the same name.

use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::Serialize;

use crate::meeting::{Line, Speaker};
use crate::models::{download, models_dir, sha256_file};

pub const DIR: &str = "voices";
/// (file, URL at a pinned revision, size, SHA-256).
pub const FILES: &[(&str, &str, u64, &str)] = &[
    (
        "segmentation-3.0.onnx",
        // pyannote segmentation 3.0 (MIT), sherpa-onnx export.
        "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/9403a6902bb58e3d5ae8c7e77c3422de279db2e0/model.onnx",
        5_992_913,
        "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
    ),
    (
        "titanet-small.onnx",
        // NVIDIA TitaNet-S (NeMo toolkit licence: Apache-2.0), sherpa-onnx export.
        "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/0743f301363dec56491a490f6d6cbc9d67f9a3bf/nemo_en_titanet_small.onnx",
        40_257_283,
        "ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e",
    ),
];

/// The clustering threshold (higher: fewer people). See the module comment.
const THRESHOLD: f32 = 1.0;
const RATE: usize = crate::audio::RATE as usize;

/// This build can tell voices apart (sherpa-onnx is in the Linux x86_64 build only).
pub const SUPPORTED: bool = cfg!(parakeet);

pub fn dir() -> PathBuf {
    models_dir().join(DIR)
}

/// Both models are on disk with their sizes (the checksum is checked after each download).
pub fn present() -> bool {
    FILES.iter().all(|(f, _, size, _)| std::fs::metadata(dir().join(f)).map(|m| m.len() == *size).unwrap_or(false))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Download {
    /// "" | downloading | error
    pub state: String,
    pub progress: f32,
    pub error: String,
}

static DOWNLOAD: Mutex<Option<Download>> = Mutex::new(None);

pub fn download_state() -> Download {
    DOWNLOAD.lock().clone().unwrap_or_default()
}

/// Download the missing models (one download at a time; a second call returns at once).
/// `changed` runs when the progress moves by a percent, and at the end.
pub fn download_models(changed: impl Fn()) {
    {
        let mut d = DOWNLOAD.lock();
        if d.as_ref().is_some_and(|d| d.state == "downloading") {
            return;
        }
        *d = Some(Download { state: "downloading".into(), progress: 0.0, error: String::new() });
    }
    changed();
    let r = fetch(|p| {
        let moved = match DOWNLOAD.lock().as_mut() {
            Some(d) if (p - d.progress).abs() >= 0.01 => {
                d.progress = p;
                true
            }
            _ => false,
        };
        if moved {
            changed();
        }
    });
    *DOWNLOAD.lock() = match r {
        Ok(()) => None,
        Err(e) => Some(Download { state: "error".into(), progress: 0.0, error: format!("{e:#}") }),
    };
    changed();
}

fn fetch(progress: impl Fn(f32)) -> anyhow::Result<()> {
    let total: u64 = FILES.iter().map(|f| f.2).sum();
    let mut before = 0;
    for (file, url, size, sha) in FILES {
        let dest = dir().join(file);
        if std::fs::metadata(&dest).map(|m| m.len() != *size).unwrap_or(false) {
            let _ = std::fs::remove_file(&dest);
        }
        if !dest.exists() {
            download(url, &dest, |done, _| progress((before + done) as f32 / total as f32))?;
            let got = sha256_file(&dest)?;
            if got != *sha {
                let _ = std::fs::remove_file(&dest);
                anyhow::bail!("{file} has an unexpected checksum ({got})");
            }
        }
        before += size;
    }
    Ok(())
}

// ------------------------------------------------------------------ the temporary audio file
/// The others' sound of meeting `id` while it records, and after Stop until its voices are told
/// apart (deleted then).
pub fn tape_path(meetings: &Path, id: &str) -> PathBuf {
    meetings.join(format!(".{id}.them.pcm"))
}

/// A file taken again at start (`leftovers`): its last try. Left again (the app quit, or telling
/// the voices apart crashed it), it is deleted at the next start: never a crash at every start.
pub fn retry_path(meetings: &Path, id: &str) -> PathBuf {
    meetings.join(format!(".{id}.them.retry.pcm"))
}

/// Last tries left by the previous run (see `retry_path`): deleted.
pub fn delete_retries(meetings: &Path) {
    let Ok(entries) = std::fs::read_dir(meetings) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(".m-") && name.ends_with(".them.retry.pcm") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The files of meetings whose voices were not told apart yet (the app quit, or crashed, first):
/// (meeting id, file). At start, before a meeting can record (its file is never in the list).
pub fn leftovers(meetings: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(meetings) else { return vec![] };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_prefix('.')?.strip_suffix(".them.pcm")?.to_string();
            crate::meeting::valid_id(&id).then(|| (id, e.path()))
        })
        .collect();
    out.sort();
    out
}

/// The longest meeting whose voices are told apart: 3 hours is a 345 MB file and 690 MB of
/// samples in memory after Stop. Longer: the file is deleted, the meeting keeps "Them".
pub const MAX_TAPE_S: usize = 3 * 3600;

/// 16-bit samples at the positions the transcript uses (see `meeting.rs` Inner::feed). Silence
/// (a gap, or a block of zeros) is a hole in the file, not bytes on disk; a block that starts
/// before the end keeps only its new part. Dropped before `finish`: the file is deleted.
pub struct Tape {
    out: BufWriter<std::fs::File>,
    /// Samples up to here are written or silence; the file's cursor is at `cursor`.
    end: usize,
    cursor: usize,
    path: PathBuf,
    finished: bool,
}

impl Tape {
    pub fn create(path: &Path) -> anyhow::Result<Self> {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600); // a recording is private
        }
        Ok(Self { out: BufWriter::with_capacity(1 << 16, o.open(path)?), end: 0, cursor: 0, path: path.to_path_buf(), finished: false })
    }

    pub fn write_at(&mut self, block: &[f32], at: usize) -> std::io::Result<()> {
        if at + block.len() > MAX_TAPE_S * RATE {
            return Err(std::io::Error::other("the meeting is longer than 3 hours"));
        }
        let skip = self.end.saturating_sub(at).min(block.len());
        let (from, new) = (at + skip, &block[skip..]);
        self.end = self.end.max(from + new.len());
        if new.iter().all(|&s| s == 0.0) {
            return Ok(()); // silence: a hole
        }
        if self.cursor != from {
            self.out.seek(SeekFrom::Start(from as u64 * 2))?;
        }
        let bytes: Vec<u8> = new.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        self.out.write_all(&bytes)?;
        self.cursor = from + new.len();
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Everything on disk (silence at the end too); the file's path.
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        self.out.flush()?;
        self.out.get_ref().set_len(self.end as u64 * 2)?;
        self.finished = true;
        Ok(self.path.clone())
    }
}

impl Drop for Tape {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The samples of a tape, read in pieces (no second copy of the whole file in memory).
pub fn read_tape(path: &Path) -> anyhow::Result<Vec<f32>> {
    let f = std::fs::File::open(path)?;
    let len = f.metadata()?.len() as usize;
    anyhow::ensure!(len / 2 <= MAX_TAPE_S * RATE, "the meeting is longer than 3 hours");
    let mut out = Vec::with_capacity(len / 2);
    let mut r = std::io::BufReader::with_capacity(1 << 20, f);
    let mut buf = vec![0u8; 1 << 20];
    let mut carry: Option<u8> = None;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut b = &buf[..n];
        if let Some(lo) = carry.take() {
            out.push(i16::from_le_bytes([lo, b[0]]) as f32 / 32768.0);
            b = &b[1..];
        }
        let pairs = b.chunks_exact(2);
        carry = pairs.remainder().first().copied();
        out.extend(pairs.map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0));
    }
    Ok(out)
}

/// After a failure: check both models with SHA-256 and delete a wrong one, so that turning the
/// setting on again downloads it.
pub fn verify() {
    for (file, _, _, sha) in FILES {
        let path = dir().join(file);
        if path.exists() && sha256_file(&path).ok().as_deref() != Some(*sha) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ------------------------------------------------------------------ who spoke when
/// One person's speech: seconds from the start of the meeting; `who` from 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    pub who: i32,
}

#[cfg(parakeet)]
pub fn turns(samples: &[f32]) -> anyhow::Result<Vec<Turn>> {
    use sherpa_onnx::{FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig, OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig, SpeakerEmbeddingExtractorConfig};
    anyhow::ensure!(present(), "the voice models are not downloaded");
    let threads = crate::engine::physical_threads();
    let path = |f: &str| Some(dir().join(f).to_string_lossy().into_owned());
    let cfg = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig { model: path(FILES[0].0), ..Default::default() },
            num_threads: threads,
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig { model: path(FILES[1].0), num_threads: threads, ..Default::default() },
        clustering: FastClusteringConfig { num_clusters: -1, threshold: THRESHOLD, ..Default::default() },
        ..Default::default()
    };
    let sd = OfflineSpeakerDiarization::create(&cfg).ok_or_else(|| anyhow::anyhow!("could not load the voice models"))?;
    anyhow::ensure!(sd.sample_rate() as usize == RATE, "unexpected sample rate {}", sd.sample_rate());
    let r = sd.process(samples).ok_or_else(|| anyhow::anyhow!("telling the voices apart failed"))?;
    Ok(r.sort_by_start_time().into_iter().map(|s| Turn { start: s.start as f64, end: s.end as f64, who: s.speaker }).collect())
}

#[cfg(not(parakeet))]
pub fn turns(_samples: &[f32]) -> anyhow::Result<Vec<Turn>> {
    anyhow::bail!("this build cannot tell voices apart")
}

/// Each "Them" line gets the person whose speech covers most of it (no cover: the nearest
/// turn). People are numbered 1, 2, … in the order they first speak. A "person" with under 5%
/// of the speech time and under 60 s is dropped: mostly one voice found twice (measured: the
/// right number of people, or one more, and 89% of the time right, against 86% with them).
/// Returns how many people the lines have.
pub fn assign(lines: &mut [Line], turns: &[Turn]) -> usize {
    let mut talk: Vec<(i32, f64)> = Vec::new();
    for t in turns {
        match talk.iter_mut().find(|x| x.0 == t.who) {
            Some(x) => x.1 += t.end - t.start,
            None => talk.push((t.who, t.end - t.start)),
        }
    }
    let total: f64 = talk.iter().map(|x| x.1).sum();
    let kept = |w: i32| talk.iter().any(|x| x.0 == w && (x.1 >= 0.05 * total || x.1 >= 60.0));
    let turns: Vec<Turn> = turns.iter().copied().filter(|t| kept(t.who)).collect();
    let mut order: Vec<i32> = Vec::new();
    for l in lines.iter_mut().filter(|l| l.speaker == Speaker::Them) {
        let mut cover: Vec<(i32, f64)> = Vec::new();
        for t in turns.iter().filter(|t| t.start < l.end && l.start < t.end) {
            let o = t.end.min(l.end) - t.start.max(l.start);
            match cover.iter_mut().find(|c| c.0 == t.who) {
                Some(c) => c.1 += o,
                None => cover.push((t.who, o)),
            }
        }
        let best = cover.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|c| c.0).or_else(|| {
            let gap = |t: &Turn| (t.start - l.end).max(l.start - t.end).max(0.0);
            turns.iter().min_by(|a, b| gap(a).total_cmp(&gap(b))).map(|t| t.who)
        });
        l.person = match best {
            Some(w) => {
                let i = order.iter().position(|&x| x == w).unwrap_or_else(|| {
                    order.push(w);
                    order.len() - 1
                });
                i as u32 + 1
            }
            None => 0,
        };
    }
    order.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn them(start: f64, end: f64) -> Line {
        Line { speaker: Speaker::Them, start, end, text: "x".into(), person: 0 }
    }

    #[test]
    fn the_tape_keeps_the_clock_positions() {
        let p = std::env::temp_dir().join(format!("lowkey-tape-{}.pcm", std::process::id()));
        let mut t = Tape::create(&p).unwrap();
        t.write_at(&[0.5; 4], 2).unwrap(); // starts after 2 samples of silence
        t.write_at(&[0.25; 4], 4).unwrap(); // its first 2 samples are already there
        t.write_at(&[0.0; 3], 8).unwrap(); // silence: a hole
        t.write_at(&[1.5; 1], 11).unwrap(); // clamped
        t.write_at(&[0.0; 2], 12).unwrap(); // silence at the end
        let path = t.finish().unwrap();
        let s = read_tape(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let r = |x: f32| (x * 100.0).round() / 100.0;
        assert_eq!(s.iter().map(|&x| r(x)).collect::<Vec<_>>(), vec![0.0, 0.0, 0.5, 0.5, 0.5, 0.5, 0.25, 0.25, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        // Not finished (the app stopped it, or a write failed): the file goes.
        let mut t = Tape::create(&p).unwrap();
        t.write_at(&[0.5; 4], 0).unwrap();
        drop(t);
        assert!(!p.exists());
        let mut t = Tape::create(&p).unwrap();
        assert!(t.write_at(&[0.5; 4], MAX_TAPE_S * RATE).is_err(), "over 3 hours");
    }

    #[test]
    fn lines_get_the_voice_that_covers_most_of_them() {
        let mut lines = vec![
            Line { speaker: Speaker::Me, start: 0.0, end: 2.0, text: "me".into(), person: 0 },
            them(2.0, 6.0),   // B 2–3, A 3–6: A
            them(7.0, 9.0),   // B
            them(9.5, 10.0),  // no cover, B ends 0.3 s before: B
            them(40.0, 41.0), // only C's 0.4 s (dropped): the nearest kept turn, A
        ];
        let turns = [
            Turn { start: 2.0, end: 3.0, who: 5 },
            Turn { start: 3.0, end: 6.0, who: 2 },
            Turn { start: 6.5, end: 9.2, who: 5 },
            Turn { start: 30.0, end: 32.0, who: 2 },
            Turn { start: 40.2, end: 40.6, who: 7 }, // 4% of the time
        ];
        assert_eq!(assign(&mut lines, &turns), 2);
        assert_eq!(lines.iter().map(|l| l.person).collect::<Vec<_>>(), vec![0, 1, 2, 2, 1], "numbered in the order they first speak");
        let mut none = vec![them(1.0, 2.0)];
        assert_eq!(assign(&mut none, &[]), 0);
        assert_eq!(none[0].person, 0, "no voices found: still Them");
    }

    #[test]
    fn leftovers_are_found() {
        let d = std::env::temp_dir().join(format!("lowkey-voices-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for f in [".m-2.them.pcm", ".m-1.them.pcm", ".x-3.them.pcm", ".m-4.them.wav", ".m-5.them.retry.pcm", "m-1.json"] {
            std::fs::write(d.join(f), b"xx").unwrap();
        }
        assert_eq!(leftovers(&d), vec![("m-1".to_string(), tape_path(&d, "m-1")), ("m-2".to_string(), tape_path(&d, "m-2"))]);
        // A last try left again: deleted (telling those voices apart may be what crashed).
        delete_retries(&d);
        assert!(!retry_path(&d, "m-5").exists() && tape_path(&d, "m-1").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// On a real recording: LOWKEY_VOICES_WAV=<16 kHz mono 16-bit wav> [LOWKEY_VOICES_OUT=<turns.txt>]
    /// OPENWHISPER_MODELS_DIR=<a folder with voices/> cargo test -- --ignored
    #[test]
    #[ignore]
    fn a_real_recording() {
        let Ok(wav) = std::env::var("LOWKEY_VOICES_WAV") else { return };
        let bytes = std::fs::read(wav).unwrap();
        let wav: Vec<f32> = bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        // Through the tape, as a meeting writes it (100 ms blocks) and reads it after Stop.
        let p = std::env::temp_dir().join(format!("lowkey-real-{}.pcm", std::process::id()));
        let mut tape = Tape::create(&p).unwrap();
        for (i, b) in wav.chunks(RATE / 10).enumerate() {
            tape.write_at(b, i * RATE / 10).unwrap();
        }
        let path = tape.finish().unwrap();
        let samples = read_tape(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(samples.len(), wav.len());
        assert!(samples.iter().zip(&wav).all(|(a, b)| (a - b).abs() < 1e-4), "the tape gives the same samples");
        let t0 = std::time::Instant::now();
        let t = turns(&samples).unwrap();
        let people: std::collections::BTreeSet<i32> = t.iter().map(|t| t.who).collect();
        eprintln!("{} turns, {} people, {:.1} s for {:.0} s of audio", t.len(), people.len(), t0.elapsed().as_secs_f64(), samples.len() as f64 / RATE as f64);
        if let Ok(out) = std::env::var("LOWKEY_VOICES_OUT") {
            std::fs::write(out, t.iter().map(|x| format!("{} {} {}\n", x.start, x.end, x.who)).collect::<String>()).unwrap();
        }
        assert!(!t.is_empty());
    }
}
