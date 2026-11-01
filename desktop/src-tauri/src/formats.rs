//! Transcript export formats: plain text, SubRip (.srt) and WebVTT (.vtt).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

fn stamp(seconds: f64, sep: char) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let (h, rest) = (ms / 3_600_000, ms % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let (s, ms) = (rest / 1000, rest % 1000);
    format!("{h:02}:{m:02}:{s:02}{sep}{ms:03}")
}

fn spoken(segments: &[Segment]) -> impl Iterator<Item = &Segment> {
    segments.iter().filter(|s| !s.text.trim().is_empty())
}

pub fn to_txt(segments: &[Segment]) -> String {
    let joined = spoken(segments).map(|s| s.text.trim()).collect::<Vec<_>>().join(" ");
    format!("{}\n", joined.trim())
}

pub fn to_srt(segments: &[Segment]) -> String {
    spoken(segments)
        .enumerate()
        .map(|(i, s)| format!("{}\n{} --> {}\n{}\n", i + 1, stamp(s.start, ','), stamp(s.end, ','), s.text.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn to_vtt(segments: &[Segment]) -> String {
    let cues = spoken(segments)
        .map(|s| format!("{} --> {}\n{}\n", stamp(s.start, '.'), stamp(s.end, '.'), s.text.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    format!("WEBVTT\n\n{cues}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs() -> Vec<Segment> {
        vec![
            Segment { start: 0.0, end: 1.234, text: " Hi. ".into() },
            Segment { start: 3661.5, end: 3662.0, text: "Later".into() },
            Segment { start: 4000.0, end: 4001.0, text: "   ".into() },
        ]
    }

    #[test]
    fn txt_joins_and_skips_blank() {
        assert_eq!(to_txt(&segs()), "Hi. Later\n");
    }

    #[test]
    fn srt_numbering_and_hours() {
        assert_eq!(to_srt(&segs()), "1\n00:00:00,000 --> 00:00:01,234\nHi.\n\n2\n01:01:01,500 --> 01:01:02,000\nLater\n");
    }

    #[test]
    fn vtt_header_and_dot_millis() {
        assert_eq!(to_vtt(&segs()), "WEBVTT\n\n00:00:00.000 --> 00:00:01.234\nHi.\n\n01:01:01.500 --> 01:01:02.000\nLater\n");
    }
}
