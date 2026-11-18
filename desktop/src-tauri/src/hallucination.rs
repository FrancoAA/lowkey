//! Lines that Whisper makes up when there is no speech to read. It can read its prompt back
//! (our prompt is the Vocabulary: "Glossary: Lisandro, NMR."), put the prompt's names in front
//! of real speech as a speaker label, or write a stock line from the video subtitles it learned
//! from ("Thank you.", "Thanks for watching!"). The VAD drops most silence first, but noise, a
//! breath or music can still reach the model.
//!
//! Idea and rules from Yogurt (MIT, github.com/FrigadeHQ/yogurt, app/src/main/asr/hallucination.ts).

use std::sync::Arc;

use crate::formats::Segment;

/// A run of this many prompt words, and nothing else, is the prompt read back; a single name
/// can be said for real.
const MIN_ECHO_TOKENS: usize = 2;
/// A read-back this long may carry one stray word of its own.
const LONG_ECHO_TOKENS: usize = 4;
/// Whisper cuts a read-back off mid-word: the line's last word may be the start of the prompt's.
const MIN_CUT_CHARS: usize = 3;

/// Chinese and Japanese have no spaces: each character counts as a word there.
fn no_spaces_script(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// Words, lowercased and without punctuation.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() && !no_spaces_script(c) {
            cur.push(c);
            continue;
        }
        if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if no_spaces_script(c) {
            out.push(c.to_string());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The line is the prompt, or a run of two or more of its words in order, with no words of its
/// own (one, in a long run).
pub fn is_prompt_echo(text: &str, prompt: &str) -> bool {
    let (t, p) = (tokens(text), tokens(prompt));
    if t.len() < MIN_ECHO_TOKENS || t.len() > p.len() + 1 {
        return false;
    }
    let same = |k: usize, w: &str| w == t[k] || (k == t.len() - 1 && t[k].chars().count() >= MIN_CUT_CHARS && w.starts_with(t[k].as_str()));
    // The longest run of the line that reads the prompt word for word.
    let mut run = 0;
    for a in 0..t.len() {
        for i in 0..p.len() {
            let mut n = 0;
            while a + n < t.len() && i + n < p.len() && same(a + n, &p[i + n]) {
                n += 1;
            }
            run = run.max(n);
        }
    }
    run == t.len() || (run >= LONG_ECHO_TOKENS && t.len() - run <= 1)
}

/// Whisper's stock lines on quiet audio, from video subtitles. Whole lines only.
const GUESSES: &[&str] = &[
    "you",
    "thank you",
    "thank you very much",
    "thanks",
    "thanks for watching",
    "thank you for watching",
    "thanks for watching bye",
    "thank you bye",
    "bye",
    "bye bye",
    "please subscribe",
    "like and subscribe",
    "for more information visit www fema org",
    "subtitles by the amara org community",
    "untertitel der amara org community",
    "sous titres réalisés para la communauté d amara org",
    "sous titres réalisés par la communauté d amara org",
    "ご視聴ありがとうございました",
    "продолжение следует",
    "gracias",
    "gracias por ver",
    "subtítulos realizados por la comunidad de amara org",
];

/// One of Whisper's stock lines for silence. Drop it only where the audio was not clearly
/// speech: people do say "thank you".
pub fn is_silence_guess(text: &str) -> bool {
    let line = tokens(text).join(" ");
    !line.is_empty() && GUESSES.iter().any(|g| tokens(g).join(" ") == line)
}

/// Whisper sometimes reads names from its prompt back as a speaker label before real speech
/// ("Lisandro, NMR: so let me just get"). The label goes when all of its words are the prompt's;
/// a colon after words of its own stays.
pub fn without_prompt_label(text: &str, prompt: &str) -> String {
    let Some((label, rest)) = text.split_once(':') else { return text.to_string() };
    let rest_trim = rest.trim_start();
    if label.contains('\n') || label.chars().count() > 120 || rest_trim.is_empty() || !rest.starts_with(char::is_whitespace) {
        return text.to_string();
    }
    let (l, p) = (tokens(label), tokens(prompt));
    if l.is_empty() || p.is_empty() {
        return text.to_string();
    }
    let in_order = (0..p.len()).any(|i| l.iter().enumerate().all(|(k, w)| p.get(i + k) == Some(w)));
    if in_order || l.iter().all(|w| p.contains(w)) {
        rest_trim.to_string()
    } else {
        text.to_string()
    }
}

// ------------------------------------------------------------------ loudness
/// 100 ms windows (at 16 kHz).
const WIN: usize = 1600;
/// The noise floor is the quietest tenth of the windows.
const FLOOR_SHARE: f64 = 0.1;
/// A speech level of 3 times the floor, kept between -56 dB and -34 dB; "clearly speech" is 3
/// times that. On Yogurt's tests Whisper wrote "Thank you." over faint sounds whose loudest
/// windows stayed under this, while the quietest real sentence had several windows over it.
const LEVEL_FACTOR: f32 = 3.0;
const CLEAR_FACTOR: f32 = 3.0;
const MIN_LEVEL: f32 = 0.0015;
const MAX_LEVEL: f32 = 0.02;
/// Under 300 ms of clear speech: a faint sound, not someone talking.
const CLEAR_WINDOWS: usize = 3;

/// How loud each 100 ms of the audio is, and the level that is clearly speech.
pub struct Loudness {
    windows: Vec<f32>,
    clear: f32,
    rate: f64,
}

impl Loudness {
    pub fn new(pcm: &[f32], rate: u32) -> Self {
        let windows: Vec<f32> = pcm.chunks(WIN).map(|w| (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt()).collect();
        let mut sorted = windows.clone();
        sorted.sort_unstable_by(f32::total_cmp);
        let floor = sorted.get(((sorted.len() as f64 * FLOOR_SHARE) as usize).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0);
        let level = (floor * LEVEL_FACTOR).clamp(MIN_LEVEL, MAX_LEVEL);
        Self { windows, clear: level * CLEAR_FACTOR, rate: rate as f64 / WIN as f64 }
    }

    /// At least 300 ms of the span (seconds) is clearly speech.
    pub fn clear_speech(&self, start: f64, end: f64) -> bool {
        let a = ((start.max(0.0) * self.rate).floor() as usize).min(self.windows.len());
        let b = ((end.max(start) * self.rate).ceil() as usize).clamp(a, self.windows.len());
        self.windows[a..b].iter().filter(|&&r| r >= self.clear).count() >= CLEAR_WINDOWS
    }
}

/// The checks for one transcription: the prompt it had, and the audio's loudness.
#[derive(Clone)]
pub struct Check {
    prompt: Arc<String>,
    loudness: Arc<Loudness>,
}

impl Check {
    pub fn new(prompt: Option<&str>, pcm: &[f32], rate: u32) -> Self {
        Self { prompt: Arc::new(prompt.unwrap_or_default().to_string()), loudness: Arc::new(Loudness::new(pcm, rate)) }
    }

    /// The segment as it should be kept (a prompt label taken off), or None when Whisper made it up.
    pub fn keep(&self, mut seg: Segment) -> Option<Segment> {
        if is_prompt_echo(&seg.text, &self.prompt) {
            return None;
        }
        seg.text = without_prompt_label(&seg.text, &self.prompt);
        if is_silence_guess(&seg.text) && !self.loudness.clear_speech(seg.start, seg.end) {
            return None;
        }
        Some(seg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_read_back_is_found() {
        let prompt = "Northwind sync. With Sam Lee of Northwind and Joe Park of Acme.";
        assert!(is_prompt_echo("Sam Lee of Northwind.", "With Sam Lee of Northwind."));
        assert!(is_prompt_echo("With Sam Lee of Northwind.", "With Sam Lee of Northwind."));
        assert!(is_prompt_echo("Northwind sync.", prompt));
        assert!(is_prompt_echo("joe park of acme", prompt));
        assert!(is_prompt_echo("Glossary: Lisandro, NMR.", "Glossary: Lisandro, NMR."));
        assert!(is_prompt_echo("Lisandro, NMR.", "Glossary: Lisandro, NMR."));
    }

    #[test]
    fn lines_with_words_of_their_own_are_kept() {
        let prompt = "Northwind sync. With Sam Lee of Northwind and Joe Park of Acme.";
        assert!(!is_prompt_echo("I work at Acme with Sam.", prompt));
        assert!(!is_prompt_echo("Sam Lee of Northwind, nice to meet you.", prompt));
        assert!(!is_prompt_echo("Zentrik.", prompt));
        assert!(!is_prompt_echo("Sam Lee of Northwind.", ""));
        assert!(!is_prompt_echo("Lisandro.", "Glossary: Lisandro, NMR."), "one name can be said for real");
    }

    #[test]
    fn a_read_back_cut_short_or_with_one_stray_word_is_found() {
        let standup = "Standup. With Ana, Maya Lopez, Omar and Sam Lee of Northwind.";
        assert!(is_prompt_echo("Omar and Sam Lee of Northw.", standup));
        assert!(is_prompt_echo("Ana, Maya Lopez, Omar and Sam Lee of Northw.", standup));
        assert!(is_prompt_echo("to Omar and Sam Lee of Northwind.", standup));
        assert!(!is_prompt_echo("Omar Reyes for some reason.", standup));
        assert!(!is_prompt_echo("Thanks Dana.", standup));
        assert!(!is_prompt_echo("Ask Maya Lopez about it.", standup));
        assert!(!is_prompt_echo("Dana and Kai.", standup));
    }

    #[test]
    fn stock_lines_are_silence_guesses() {
        for t in ["Thank you.", "Thanks for watching!", "you", "Bye.", "Thank you for watching.", "Thank you very much.", "Bye!"] {
            assert!(is_silence_guess(t), "{t}");
        }
        for t in ["Thank you for the update on the budget.", "You said Tuesday.", "Bye for now, see you Monday.", ""] {
            assert!(!is_silence_guess(t), "{t}");
        }
    }

    #[test]
    fn prompt_names_as_a_label_are_taken_off() {
        let prompt = "Product board. With Eric and Christian Mathiesen.";
        assert_eq!(without_prompt_label("Eric and Christian Mathiesen: So let me just get", prompt), "So let me just get");
        assert_eq!(without_prompt_label("Christian Mathiesen: okay", prompt), "okay");
        assert_eq!(without_prompt_label("Eric: yeah, let me check", prompt), "yeah, let me check");
        let p = "Weekly sync. With Corinna Weber and Jonas Braun of Acme GmbH.";
        assert_eq!(without_prompt_label("GmbH, Corinna Weber Jonas Braun: Okay.", p), "Okay.");
        assert_eq!(without_prompt_label("Corinna Weber said: sounds good", p), "Corinna Weber said: sounds good");
        assert_eq!(without_prompt_label("Here is the plan: ship Friday", prompt), "Here is the plan: ship Friday");
        assert_eq!(without_prompt_label("Eric and Christian Mathiesen joined late", prompt), "Eric and Christian Mathiesen joined late");
        assert_eq!(without_prompt_label("Eric and Christian Mathiesen:", prompt), "Eric and Christian Mathiesen:");
        assert_eq!(without_prompt_label("Eric: hi", ""), "Eric: hi");
        assert_eq!(without_prompt_label("Glossary: Lisandro, so we start", "Glossary: Lisandro, NMR."), "Lisandro, so we start");
        assert_eq!(without_prompt_label("Time 10:30 is fine", prompt), "Time 10:30 is fine");
    }

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment { start, end, text: text.into() }
    }

    #[test]
    fn a_stock_line_goes_on_faint_audio_and_stays_on_clear_speech() {
        let r = 16_000usize;
        // 0-2 s: faint noise; 2-4 s: clear speech; 4-6 s: faint noise again.
        let mut pcm = vec![0.001f32; 6 * r];
        for (i, x) in pcm[2 * r..4 * r].iter_mut().enumerate() {
            *x = 0.2 * ((i as f32) * 0.05).sin();
        }
        let check = Check::new(Some("Glossary: Lisandro, NMR."), &pcm, 16_000);
        assert!(check.keep(seg(0.0, 1.5, "Thank you.")).is_none(), "made up on noise");
        assert!(check.keep(seg(2.0, 3.5, "Thank you.")).is_some(), "said for real");
        assert!(check.keep(seg(2.0, 3.5, "Glossary: Lisandro, NMR.")).is_none(), "the prompt read back, even on speech");
        assert_eq!(check.keep(seg(2.0, 3.5, "We ship Friday.")).map(|s| s.text), Some("We ship Friday.".into()));
        // No prompt: nothing is taken for an echo.
        let plain = Check::new(None, &pcm, 16_000);
        assert!(plain.keep(seg(0.0, 1.0, "Lisandro, NMR.")).is_some());
    }

    #[test]
    fn loudness_never_reads_past_the_audio() {
        let l = Loudness::new(&[0.5; 1600 * 5], 16_000);
        assert!(l.clear_speech(0.0, 99.0));
        assert!(!l.clear_speech(50.0, 60.0));
        assert!(!l.clear_speech(0.3, 0.1));
        let empty = Loudness::new(&[], 16_000);
        assert!(!empty.clear_speech(0.0, 1.0));
    }
}
