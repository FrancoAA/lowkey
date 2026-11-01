//! Decode audio/video files to 16 kHz mono f32 (what Whisper takes).
//!
//! First symphonia (pure Rust: WAV, MP3, FLAC, OGG/Vorbis, AAC/M4A/MP4, …). Formats it cannot
//! read (Opus, WebM, many video files) go through the `ffmpeg` command when it is installed.

use std::path::Path;
use std::process::{Command, Stdio};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const RATE: u32 = 16_000;

/// Same limit as the web app's uploads: a bad drop cannot fill the memory.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// At most 4 hours of audio (~920 MB of samples in memory).
pub const MAX_SECONDS: usize = 4 * 3600;

pub const SUPPORTED_HINT: &str = "Try MP3, WAV, M4A, OGG, FLAC, WEBM or a video file with sound.";

#[derive(Debug)]
pub enum DecodeError {
    /// Not audio/video we can read: a user-facing message.
    Unsupported(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unsupported(m) => write!(f, "{m}"),
            DecodeError::Other(e) => write!(f, "{e}"),
        }
    }
}

pub fn decode_file(path: &Path) -> Result<Vec<f32>, DecodeError> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err(DecodeError::Unsupported(format!("{name}: the file is empty.")));
    }
    if size > MAX_FILE_BYTES {
        return Err(DecodeError::Unsupported(format!("{name} is larger than 2 GB.")));
    }
    match decode_symphonia(path) {
        Ok(Some(samples)) => Ok(samples),
        Ok(None) | Err(_) => match decode_ffmpeg(path) {
            Some(Ok(samples)) => Ok(samples),
            Some(Err(no_audio)) if no_audio => {
                Err(DecodeError::Unsupported(format!("{name}: this file has no audio track. {SUPPORTED_HINT}")))
            }
            _ => Err(DecodeError::Unsupported(format!(
                "{name}: this file is not an audio or video format that can be decoded. {SUPPORTED_HINT}{}",
                if which("ffmpeg") { "" } else { " (Installing ffmpeg adds Opus, WebM and most video formats.)" }
            ))),
        },
    }
}

/// Ok(None): symphonia cannot read this format (try ffmpeg).
fn decode_symphonia(path: &Path) -> anyhow::Result<Option<Vec<f32>>> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(probed) = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
    else {
        return Ok(None);
    };
    let mut format = probed.format;
    let Some(track) = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())
    else {
        return Ok(None);
    };
    let track_id = track.id;
    let rate = track.codec_params.sample_rate.unwrap_or(RATE);
    let Ok(mut decoder) = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()) else {
        return Ok(None); // e.g. Opus: no symphonia decoder
    };
    let mut mono = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue, // skip a corrupt frame
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let b = buf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        if b.capacity() < decoded.capacity() {
            *b = SampleBuffer::new(decoded.capacity() as u64, spec);
        }
        b.copy_interleaved_ref(decoded);
        let ch = spec.channels.count().max(1);
        mono.extend(b.samples().chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32));
        if mono.len() > MAX_SECONDS * rate as usize {
            anyhow::bail!("longer than 4 hours");
        }
    }
    if mono.is_empty() {
        return Ok(None);
    }
    Ok(Some(resample(&mono, rate, RATE)))
}

/// Some(Ok): samples. Some(Err(true)): no audio stream. Some(Err(false)) / None: failed / no ffmpeg.
fn decode_ffmpeg(path: &Path) -> Option<Result<Vec<f32>, bool>> {
    if !which("ffmpeg") {
        return None;
    }
    // Read the samples as they come (no second full copy of the audio in memory).
    use std::io::Read;
    let mut child = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-f", "f32le", "-ac", "1", "-ar", "16000", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let err_thread = std::thread::spawn(move || {
        let mut e = String::new();
        let _ = stderr.read_to_string(&mut e);
        e
    });
    let mut samples = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = stdout.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        let whole = carry.len() / 4 * 4;
        samples.extend(carry[..whole].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        carry.drain(..whole);
        if samples.len() > MAX_SECONDS * RATE as usize {
            let _ = child.kill();
            return Some(Err(false));
        }
    }
    let status = child.wait().ok()?;
    let err = err_thread.join().unwrap_or_default().to_lowercase();
    if !status.success() || samples.is_empty() {
        return Some(Err(err.contains("does not contain any stream") || err.contains("output file #0 does not contain")));
    }
    Some(Ok(samples))
}

pub fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

/// Resample mono audio. Integer down-factors (48k→16k) average each group (a simple
/// low-pass, enough for speech); other ratios use linear interpolation.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    if from > to && from % to == 0 {
        let k = (from / to) as usize;
        return input.chunks(k).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
    }
    let ratio = from as f64 / to as f64;
    let n = ((input.len() as f64) / ratio).floor() as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos.floor() as usize;
            let frac = (pos - j as f64) as f32;
            let a = input[j];
            let b = *input.get(j + 1).unwrap_or(&a);
            a + (b - a) * frac
        })
        .collect()
}

/// Write 16 kHz mono f32 samples as 16-bit WAV.
pub fn write_wav(path: &Path, samples: &[f32]) -> anyhow::Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

/// 0..1 level of a block of samples (the waveform in the window and the overlay).
pub fn level(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let rms = (block.iter().map(|x| x * x).sum::<f32>() / block.len() as f32).sqrt();
    ((20.0 * (rms + 1e-6).log10() + 55.0) / 45.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize).map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / rate as f32).sin() * 0.25).collect()
    }

    #[test]
    fn resample_lengths() {
        assert_eq!(resample(&tone(48_000, 1.0), 48_000, 16_000).len(), 16_000);
        let r = resample(&tone(44_100, 1.0), 44_100, 16_000);
        assert!((r.len() as i64 - 16_000).abs() <= 1);
    }

    #[test]
    fn wav_roundtrip_and_errors() {
        let dir = std::env::temp_dir().join(format!("ow-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        write_wav(&wav, &tone(16_000, 1.0)).unwrap();
        let back = decode_file(&wav).unwrap();
        assert!((back.len() as i64 - 16_000).abs() < 50);
        assert!(level(&back) > 0.5);

        let pdf = dir.join("notes.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 not audio at all\n".repeat(50)).unwrap();
        let msg = decode_file(&pdf).unwrap_err().to_string();
        assert!(msg.starts_with("notes.pdf:") && msg.contains("MP3"), "{msg}");

        let empty = dir.join("empty.wav");
        std::fs::write(&empty, b"").unwrap();
        assert!(decode_file(&empty).unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn level_curve() {
        assert_eq!(level(&[0.0; 800]), 0.0);
        assert!(level(&[0.25; 800]) > 0.5);
    }
}
