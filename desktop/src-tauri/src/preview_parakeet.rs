//! Live preview with Parakeet: every pass transcribes the audio not yet confirmed (at most
//! `MAX_TAIL_S`); when that tail grows past it, the older part is confirmed, cut at its
//! quietest moment. So a pass never handles more than ~12 s, however long the dictation.

use std::sync::Arc;

use sherpa_onnx::OfflineRecognizer;

use crate::audio;
use crate::preview::{Asr, Step};

const MAX_TAIL_S: f64 = 12.0;
/// Keep at least this much audio unconfirmed after a cut (the newest words change most).
const KEEP_S: f64 = 2.0;

pub struct ParakeetStream {
    rec: Arc<OfflineRecognizer>,
    tail: Vec<f32>,
}

impl ParakeetStream {
    pub fn new(rec: Arc<OfflineRecognizer>) -> Self {
        Self { rec, tail: Vec::new() }
    }

    fn decode(&self, pcm: &[f32]) -> anyhow::Result<String> {
        if pcm.len() < audio::RATE as usize / 5 {
            return Ok(String::new());
        }
        let stream = self.rec.create_stream();
        stream.accept_waveform(audio::RATE as i32, pcm);
        self.rec.decode(&stream);
        Ok(stream.get_result().map(|r| r.text.trim().to_string()).unwrap_or_default())
    }
}

/// The quietest 100 ms between `from` and `to` (sample indexes), as a cut point.
fn quietest(pcm: &[f32], from: usize, to: usize) -> usize {
    let frame = audio::RATE as usize / 10;
    (from..to.saturating_sub(frame).max(from + 1))
        .step_by(frame)
        .min_by(|&a, &b| {
            let e = |x: usize| pcm[x..(x + frame).min(pcm.len())].iter().map(|v| v * v).sum::<f32>();
            e(a).total_cmp(&e(b))
        })
        .map(|x| x + frame / 2)
        .unwrap_or(to)
}

impl Asr for ParakeetStream {
    fn process(&mut self, pcm: &[f32], _language: Option<&str>) -> anyhow::Result<Step> {
        self.tail.extend_from_slice(pcm);
        let rate = audio::RATE as f64;
        let mut text = String::new();
        if self.tail.len() as f64 > MAX_TAIL_S * rate {
            let from = (MAX_TAIL_S / 2.0 * rate) as usize;
            let to = self.tail.len() - (KEEP_S * rate) as usize;
            let cut = quietest(&self.tail, from, to);
            text = self.decode(&self.tail[..cut])?;
            self.tail.drain(..cut);
        }
        let pending = self.decode(&self.tail)?;
        Ok(Step { text, pending })
    }

    fn stop(&mut self) -> anyhow::Result<Step> {
        let text = self.decode(&self.tail)?;
        self.tail.clear();
        Ok(Step { text, pending: String::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Instant;

    fn norm(t: &str) -> Vec<String> {
        t.to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
            .collect::<String>()
            .replace("openwhisper", "open whisper")
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    fn wer(r: &str, h: &str) -> f64 {
        let (r, h) = (norm(r), norm(h));
        let mut d: Vec<usize> = (0..=h.len()).collect();
        for i in 1..=r.len() {
            let mut p = d[0];
            d[0] = i;
            for j in 1..=h.len() {
                let t = d[j];
                d[j] = (d[j] + 1).min(d[j - 1] + 1).min(p + (r[i - 1] != h[j - 1]) as usize);
                p = t;
            }
        }
        100.0 * d[h.len()] as f64 / r.len().max(1) as f64
    }

    /// One engine over one clip, fed in 1 s chunks as the mic does: the text shown just before
    /// the key is released, the text after stop, and pass times.
    fn stream_clip(engine: &mut dyn Asr, pcm: &[f32], lang: Option<&str>) -> (String, String, f64, f64, f64) {
        let (mut committed, mut shown) = (String::new(), String::new());
        let (mut max_ms, mut sum_ms, mut n, mut first_ms) = (0f64, 0f64, 0, -1f64);
        let join = |a: &str, b: &str| format!("{a} {b}").trim().to_string();
        for (i, chunk) in pcm.chunks(audio::RATE as usize).enumerate() {
            let t = Instant::now();
            let s = engine.process(chunk, lang).unwrap();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            (max_ms, sum_ms, n) = (max_ms.max(ms), sum_ms + ms, n + 1);
            committed = join(&committed, &s.text);
            shown = join(&committed, &s.pending);
            if first_ms < 0.0 && !shown.is_empty() {
                first_ms = (i + 1) as f64 * 1000.0 + ms; // audio received + this pass
            }
        }
        let fin = join(&committed, &engine.stop().unwrap().text);
        (shown, fin, sum_ms / n as f64, max_ms, first_ms)
    }

    /// `cargo test --release --lib preview_engines -- --ignored --nocapture`
    /// Needs the Parakeet and Whistle files in the models dir, and bench/.work/samples.
    #[test]
    #[ignore]
    fn preview_engines_compared() {
        let samples = Path::new("../../bench/.work/samples");
        let refs: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(samples.join("references.json")).unwrap()).unwrap();
        let mut clips: Vec<(String, std::path::PathBuf, String)> = refs
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), samples.join(format!("{k}.wav")), v["text"].as_str().unwrap().to_string()))
            .collect();
        if let Ok(extra) = std::env::var("OW_LONG_CLIP") {
            // A long dictation: the English clip three times (about 70 s).
            let t = refs["en_tts"]["text"].as_str().unwrap();
            clips.push(("en_long70".into(), extra.into(), format!("{t} {t} {t}")));
        }
        let pk = Arc::new(crate::engine::load_parakeet().unwrap());
        let (lib, weights) = crate::preview::ensure_files().unwrap();
        let mut whistle = crate::preview::Needle::open(&lib, &weights).unwrap();
        println!("{:12} | {:>28} | {:>28}", "clip", "Parakeet: shown/final WER, avg/max ms, first", "Whistle: same");
        for (name, path, reference) in clips {
            let pcm = audio::decode_file(&path).unwrap();
            let lang = name.split('_').next().filter(|l| *l == "en" || *l == "es");
            let mut p = ParakeetStream::new(pk.clone());
            let (ps, pf, pa, pm, p1) = stream_clip(&mut p, &pcm, None);
            let (ws, wf, wa, wm, w1) = stream_clip(&mut whistle, &pcm, lang);
            println!(
                "{name:12} | P {:5.1}%/{:5.1}%  {:4.0}/{:4.0} ms  first {:4.0} ms | W {:5.1}%/{:5.1}%  {:4.0}/{:4.0} ms  first {:4.0} ms",
                wer(&reference, &ps), wer(&reference, &pf), pa, pm, p1, wer(&reference, &ws), wer(&reference, &wf), wa, wm, w1
            );
        }
    }
}
