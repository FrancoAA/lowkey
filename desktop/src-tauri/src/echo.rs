//! Echo: without headphones the microphone also hears the others from the speakers, so their
//! words come back as "Me" lines. The computer's own sound is clean, so the copy on the
//! microphone is the one taken out.
//!
//! The words alone cannot tell an echo from your own short reply ("I can send it" against their
//! "can you send it"), so the audio decides first. The loudness of the two sources is compared
//! over 2 s windows while both carry sound: their voice from the speakers makes the microphone
//! follow their track, a little late, at the same lag every time. Two people who talk over each
//! other move together now and then, at random lags. So echo is a rate (a good share of the
//! windows at a steady lag), never a count. Without that evidence no line is ever taken out.
//!
//! When echo is heard, each "Me" line is also judged on its own span (`Detector::over`), and its
//! words must be in a "Them" line said at the same time. A line found to be an echo is moved to
//! `Meeting::echoes`, never deleted.
//!
//! Port of Yogurt (MIT, github.com/FrigadeHQ/yogurt: app/src/main/asr/echoDetect.ts and echo.ts).

use std::collections::{HashMap, HashSet};

const RATE: usize = crate::audio::RATE as usize;
/// 10 ms blocks of loudness.
const BLOCK: usize = RATE / 100;
const WINDOW_BLOCKS: usize = 200;
const MAX_LAG_BLOCKS: usize = 30;
/// Two hours of loudness for each source (10 ms blocks, 2.9 MB each).
const RING_BLOCKS: usize = 2 * 3600 * 100;
/// They talk in a window from this mean level up; the microphone shows something from this up.
const THEM_LOUD: f32 = 0.004;
const ME_SOME: f32 = 0.0006;
const CORRELATED: f32 = 0.6;
/// Evidence: at least this many windows with both sides active, and either most of them
/// correlated (a room has more than one path from the speakers to the microphone, so the lag
/// can split in two), or a good share of them at one steady lag.
const MIN_ACTIVE: usize = 10;
const MIN_RATE: f32 = 0.4;
const MIN_STEADY: f32 = 0.7;
const STRONG_RATE: f32 = 0.55;
/// A line is judged on its own audio only with this much of their speech in it (10 ms blocks).
const LINE_MIN_SPEAKING: usize = 20;
/// A lag behind this share of the correlated windows is one the room has.
const KNOWN_LAG_SHARE: f32 = 0.2;
const LAG_SLACK: usize = 2;
/// The microphone over the whole line may be this many times the leak's usual level and still be
/// judged by the words.
const LOUD_RATIO: f32 = 2.0;
/// Nothing on the microphone but their voice: it follows their track closely, at the leak's
/// level, over a good part of the line.
const PURE_CORR: f32 = 0.8;
const PURE_RATIO: f32 = 1.5;
const PURE_COVER: f32 = 0.4;

/// What a "Me" line's own audio says: the microphone followed their track over it (correlated);
/// it was no louder than the leak usually is (loud: little on it beyond their voice); it was
/// nothing but their voice from the speakers (pure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub correlated: bool,
    pub loud: bool,
    pub pure: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Track {
    Me = 0,
    Them = 1,
}

pub struct Detector {
    env: [Vec<f32>; 2],
    latest: [i64; 2],
    next_check: usize,
    /// Windows where both sides carried sound.
    pub active: usize,
    /// Of those, the windows where the microphone followed their track: at what lag (blocks),
    /// and how loud the microphone was next to their track.
    lags: Vec<usize>,
    ratios: Vec<f32>,
    correlated: HashSet<usize>,
    /// Every span judged, by its start and end in blocks: the sweep after the meeting gets the
    /// same answers for lines judged live.
    verdicts: HashMap<(usize, usize), Moment>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        Self {
            env: [vec![0.0; RING_BLOCKS], vec![0.0; RING_BLOCKS]],
            latest: [-1, -1],
            next_check: WINDOW_BLOCKS,
            active: 0,
            lags: Vec::new(),
            ratios: Vec::new(),
            correlated: HashSet::new(),
            verdicts: HashMap::new(),
        }
    }

    /// Windows where the microphone followed their track.
    pub fn windows(&self) -> usize {
        self.lags.len()
    }

    /// Audio of one source; `at`: its first sample's position on the meeting clock.
    pub fn push(&mut self, track: Track, at: usize, samples: &[f32]) {
        let t = track as usize;
        let first = at / BLOCK;
        // A jump forward (a pause, a restart) leaves old blocks in the ring: they are silence now.
        if first as i64 > self.latest[t] + 1 {
            let from = ((self.latest[t] + 1).max(first as i64 - RING_BLOCKS as i64)).max(0) as usize;
            for i in from..first {
                self.env[t][i % RING_BLOCKS] = 0.0;
            }
        }
        for (b, block) in samples.chunks(BLOCK).enumerate() {
            let idx = first + b;
            self.env[t][idx % RING_BLOCKS] = (block.iter().map(|x| x * x).sum::<f32>() / block.len() as f32).sqrt();
            if idx as i64 > self.latest[t] {
                self.latest[t] = idx as i64;
            }
        }
        // A window is looked at once both tracks have reached past it (the microphone's lagged
        // blocks too); a gap in either track lets the window go unchecked rather than hold every
        // later one.
        loop {
            let nc = self.next_check as i64;
            let ready = self.latest[1] >= nc - 1 && self.latest[0] >= nc + MAX_LAG_BLOCKS as i64 - 1;
            if ready {
                self.check(self.next_check - WINDOW_BLOCKS, self.next_check);
                self.next_check += WINDOW_BLOCKS;
                continue;
            }
            if self.latest[1] > nc + 2 * WINDOW_BLOCKS as i64 {
                self.next_check += WINDOW_BLOCKS;
                continue;
            }
            break;
        }
    }

    /// Still in the ring: recorded, and within the last two hours.
    fn in_ring(&self, t: Track, idx: usize) -> bool {
        let l = self.latest[t as usize];
        (idx as i64) <= l && (idx as i64) > l - RING_BLOCKS as i64
    }

    /// A block nobody recorded (before the first sound, a pause, after the latest) is silence.
    fn at(&self, t: Track, idx: usize) -> f32 {
        if self.in_ring(t, idx) {
            self.env[t as usize][idx % RING_BLOCKS]
        } else {
            0.0
        }
    }

    /// One 2 s window of their track against the microphone's, at lags of 0 to 300 ms.
    fn check(&mut self, from: usize, to: usize) {
        let them: Vec<f32> = (from..to).map(|i| self.at(Track::Them, i)).collect();
        let me: Vec<Vec<f32>> = (0..=MAX_LAG_BLOCKS).map(|lag| (from..to).map(|i| self.at(Track::Me, i + lag)).collect()).collect();
        if mean(&them) < THEM_LOUD || mean(&me[0]) < ME_SOME {
            return;
        }
        self.active += 1;
        // Only the moments they speak count: your own words in the same window would drown a faint leak.
        let speaking: Vec<bool> = them.iter().map(|&v| v >= THEM_LOUD).collect();
        let pick = |xs: &[f32]| -> Vec<f32> { xs.iter().zip(&speaking).filter(|(_, &s)| s).map(|(&x, _)| x).collect() };
        let t = pick(&them);
        if t.len() < 20 {
            return;
        }
        let (mut best, mut best_lag) = (0.0f32, 0usize);
        for (lag, m) in me.iter().enumerate() {
            let c = pearson(&t, &pick(m));
            if c > best {
                best = c;
                best_lag = lag;
            }
        }
        if best >= CORRELATED {
            self.lags.push(best_lag);
            self.ratios.push(mean(&pick(&me[best_lag])) / mean(&t));
            self.correlated.insert(from / WINDOW_BLOCKS);
        }
    }

    /// Echo has been heard in this meeting: a steady share of the active windows, at one lag.
    pub fn present(&self) -> bool {
        if self.active < MIN_ACTIVE {
            return false;
        }
        let rate = self.lags.len() as f32 / self.active as f32;
        if rate < MIN_RATE {
            return false;
        }
        if rate >= STRONG_RATE {
            return true;
        }
        let mut count: HashMap<usize, usize> = HashMap::new();
        for &l in &self.lags {
            *count.entry(l).or_default() += 1;
        }
        let Some(mode) = count.iter().max_by_key(|(l, n)| (**n, std::cmp::Reverse(**l))).map(|(l, _)| *l) else { return false };
        let steady = self.lags.iter().filter(|&&l| l.abs_diff(mode) <= 1).count();
        steady as f32 / self.lags.len() as f32 >= MIN_STEADY
    }

    /// The microphone followed their track in the window that holds this moment (seconds).
    pub fn correlated_at(&self, s: f64) -> bool {
        self.correlated.contains(&(blocks(s) / WINDOW_BLOCKS))
    }

    /// The lags the room has shown (blocks).
    fn known_lags(&self) -> Vec<usize> {
        let mut count: HashMap<usize, usize> = HashMap::new();
        for &l in &self.lags {
            *count.entry(l).or_default() += 1;
        }
        let mut out: Vec<usize> = count.into_iter().filter(|(_, n)| *n as f32 / self.lags.len() as f32 >= KNOWN_LAG_SHARE).map(|(l, _)| l).collect();
        out.sort_unstable();
        out
    }

    /// The leak's usual level on the microphone, as a share of their track's.
    fn gain(&self) -> f32 {
        let mut r = self.ratios.clone();
        r.sort_unstable_by(f32::total_cmp);
        r.get(r.len() / 2).copied().unwrap_or(0.0)
    }

    /// A "Me" line (seconds) judged by its own audio. None before the room's lag is known, and
    /// for audio older than two hours that was never asked about.
    pub fn over(&mut self, start: f64, end: f64) -> Option<Moment> {
        let (a, b) = (blocks(start), (end.max(start) * 100.0).ceil() as usize);
        if let Some(m) = self.verdicts.get(&(a, b)) {
            return Some(*m);
        }
        let known = self.known_lags();
        if known.is_empty() || !self.in_ring(Track::Them, a) || !self.in_ring(Track::Me, a) {
            return None;
        }
        let speaking = (a..b).filter(|&i| self.at(Track::Them, i) >= THEM_LOUD).count();
        let m = if speaking < LINE_MIN_SPEAKING {
            Moment { correlated: false, loud: false, pure: false }
        } else {
            // The whole span, pauses too: your own words in the pauses pull the correlation down
            // and the level up, as they should.
            let them: Vec<f32> = (a..b).map(|i| self.at(Track::Them, i)).collect();
            let (mut best, mut best_lag) = (0.0f32, known[0]);
            for &k in &known {
                for lag in k.saturating_sub(LAG_SLACK)..=(k + LAG_SLACK).min(MAX_LAG_BLOCKS) {
                    let me: Vec<f32> = (a..b).map(|i| self.at(Track::Me, i + lag)).collect();
                    let c = pearson(&them, &me);
                    if c > best {
                        best = c;
                        best_lag = lag;
                    }
                }
            }
            let me: Vec<f32> = (a..b).map(|i| self.at(Track::Me, i + best_lag)).collect();
            let ratio = mean(&me) / mean(&them);
            let g = self.gain();
            let cover = speaking as f32 / (b - a).max(1) as f32;
            Moment { correlated: best >= CORRELATED, loud: ratio <= LOUD_RATIO * g, pure: best >= PURE_CORR && ratio <= PURE_RATIO * g && cover >= PURE_COVER }
        };
        self.verdicts.insert((a, b), m);
        Some(m)
    }
}

fn blocks(s: f64) -> usize {
    (s.max(0.0) * 100.0).floor() as usize
}

fn mean(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f32>() / xs.len() as f32
    }
}

fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (ma, mb) = (mean(&a[..n]), mean(&b[..n]));
    let (mut cov, mut va, mut vb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..n {
        let (x, y) = (a[i] - ma, b[i] - mb);
        cov += x * y;
        va += x * x;
        vb += y * y;
    }
    if va > 0.0 && vb > 0.0 {
        cov / (va * vb).sqrt()
    } else {
        0.0
    }
}

// ------------------------------------------------------------------ the words
/// A line: seconds from the start, and its text.
#[derive(Debug, Clone, Copy)]
pub struct Seg<'a> {
    pub start: f64,
    pub end: f64,
    pub text: &'a str,
}

/// Their lines that a "Me" line could be a copy of are within this of it.
const WINDOW_S: f64 = 1.5;
/// A long line (8+ words) is an echo at this share of its words found in theirs; shorter lines
/// need more: a few shared words come easily ("Sounds good, I can send it" against "yes I can
/// send it so you have it").
const THRESHOLD: f64 = 0.6;
const LONG_TOKENS: usize = 8;
const MID_THRESHOLD: f64 = 0.8;
const MID_TOKENS: usize = 6;
const SHORT_THRESHOLD: f64 = 0.95;
/// Against several of their lines at once, more of the words must be there.
const JOINED_THRESHOLD: f64 = 0.75;
/// A one or two word reply ("okay", "yes") is too short to compare by words.
const MIN_TOKENS: usize = 3;
/// The speakers' tail: a "Me" line that starts later than this after their line ended is your
/// own (a read-back).
const TAIL_S: f64 = 0.5;
/// A line the audio calls pure still needs this many of its words in their line.
const PURE_SHARED: usize = 3;

/// Sounds that are not words.
fn is_filler(w: &str) -> bool {
    let w = w.as_bytes();
    let all = |c: u8, from: usize| w.len() > from && w[from..].iter().all(|&x| x == c);
    (w.first() == Some(&b'u') && all(b'm', 1))
        || (w.first() == Some(&b'u') && all(b'h', 1))
        || (w.first() == Some(&b'h') && all(b'm', 1))
        || all(b'm', 0)
        || matches!(w, b"mhm" | b"erm" | b"ah" | b"oh" | b"eh" | b"huh")
}

/// Your own short replies: never taken out, however their words fit in theirs.
const ACK: &[&str] = &[
    "yeah", "yes", "yep", "yup", "no", "nope", "right", "okay", "ok", "sure", "exactly", "true", "agreed", "fine", "great", "good", "thanks", "thank", "you", "please", "sorry", "absolutely", "definitely", "indeed", "correct", "cool", "perfect", "hi", "hello", "bye", "alright", "well", "so", "and", "but", "i", "me", "too", "also", "si", "sí", "claro", "vale", "gracias", "bueno",
];

use crate::hallucination::tokens;

/// Share of the shorter text's words that the other has too (each word counted as often as it
/// is in both).
pub fn token_overlap(a: &str, b: &str) -> f64 {
    let (ta, tb) = (tokens(a), tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    shared(&ta, &tb) as f64 / ta.len().min(tb.len()) as f64
}

fn shared(a: &[String], b: &[String]) -> usize {
    let mut left: HashMap<&str, usize> = HashMap::new();
    for w in b {
        *left.entry(w.as_str()).or_default() += 1;
    }
    let mut n = 0;
    for w in a {
        if let Some(c) = left.get_mut(w.as_str()) {
            if *c > 0 {
                n += 1;
                *c -= 1;
            }
        }
    }
    n
}

/// Is this "Me" line a copy of what they said around the same time? Only asked when echo is
/// heard in the meeting. `correlated_at` and `over`: the audio's answers (see `Detector`).
pub fn is_echo(m: Seg, them: &[Seg], correlated_at: &dyn Fn(f64) -> bool, over: Option<Moment>) -> bool {
    // An echo is their voice on the microphone while the line was said: a copy said a second
    // later (a read-back) is your own. The line's own audio decides where it can, and never
    // when the microphone was far louder than the leak (your own words).
    let in_window = correlated_at(m.start) || correlated_at((m.start + m.end) / 2.0) || correlated_at(m.end);
    let audio_ok = match over {
        Some(span) => (span.correlated || in_window) && span.loud,
        None => in_window,
    };
    if !audio_ok {
        return false;
    }
    let mine = tokens(m.text);
    let all_ack = !mine.is_empty() && mine.iter().all(|w| ACK.contains(&w.as_str()));
    // Nothing on the microphone but their voice, and their line at that moment has most of its
    // words: an echo at a lower word bar. Never on the audio alone: a person who speaks as
    // quietly as the leak sounds much the same.
    if over.is_some_and(|s| s.pure)
        && mine.len() >= MIN_TOKENS
        && !all_ack
        && them.iter().any(|o| o.start < m.end && o.end > m.start && shared(&mine, &tokens(o.text)) >= PURE_SHARED && token_overlap(m.text, o.text) >= THRESHOLD)
    {
        return true;
    }
    let near: Vec<&Seg> = them.iter().filter(|o| m.start <= o.end + WINDOW_S && m.end >= o.start - WINDOW_S && m.start < o.end + TAIL_S).collect();
    if mine.len() < MIN_TOKENS {
        // Too short to compare by words: only filler, or a scrap of a line of theirs said at the
        // same moment. Your own short reply ("yeah", "right") stays.
        if !mine.is_empty() && mine.iter().all(|w| is_filler(w)) {
            return !near.is_empty();
        }
        if mine.is_empty() || all_ack {
            return false;
        }
        return them.iter().filter(|o| m.start < o.end + 0.3 && m.end > o.start - 0.3).any(|o| {
            let t = tokens(o.text);
            mine.iter().all(|w| t.contains(w))
        });
    }
    let long_enough = |o: &&&Seg| tokens(o.text).len() >= MIN_TOKENS;
    if mine.len() < MID_TOKENS {
        return near.iter().filter(long_enough).any(|o| token_overlap(m.text, o.text) >= SHORT_THRESHOLD);
    }
    let needed = if mine.len() < LONG_TOKENS { MID_THRESHOLD } else { THRESHOLD };
    let overlap = near.iter().filter(long_enough).map(|o| token_overlap(m.text, o.text)).fold(0.0, f64::max);
    if overlap >= needed {
        return true;
    }
    let joined: Vec<&str> = near.iter().map(|o| o.text).collect();
    mine.len() >= LONG_TOKENS && near.len() > 1 && token_overlap(m.text, &joined.join(" ")) >= JOINED_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech-like bursts: 300 ms on, 200 ms off, `sec` long.
    fn bursts(sec: f64, amp: f32, seed: u64) -> Vec<f32> {
        let n = (sec * RATE as f64) as usize;
        let mut x = seed;
        let mut rnd = move || {
            x = (x.wrapping_mul(1103515245).wrapping_add(12345)) & 0x7fffffff;
            x as f32 / 0x7fffffff as f32
        };
        (0..n)
            .map(|i| {
                let t = i as f64 / RATE as f64;
                let on = if t % 0.5 < 0.3 { 1.0 } else { 0.02 };
                amp * on * ((i as f32 / 7.0).sin() * 0.6 + (rnd() - 0.5) * 0.8)
            })
            .collect()
    }

    /// The other side leaking into the microphone: quieter, `ms` late.
    fn leak(them: &[f32], gain: f32, ms: usize) -> Vec<f32> {
        let d = ms * 16;
        (0..them.len()).map(|i| if i >= d { gain * them[i - d] } else { 0.0 }).collect()
    }

    fn feed(det: &mut Detector, me: &[f32], them: &[f32]) {
        let mut i = 0;
        while i < me.len().max(them.len()) {
            if i < them.len() {
                det.push(Track::Them, i, &them[i..(i + 1600).min(them.len())]);
            }
            if i < me.len() {
                det.push(Track::Me, i, &me[i..(i + 1600).min(me.len())]);
            }
            i += 1600;
        }
    }

    #[test]
    fn headphones_are_never_echo() {
        let them = bursts(12.0, 0.3, 1);
        let mut det = Detector::new();
        feed(&mut det, &vec![0.0; them.len()], &them);
        assert!(!det.present());
        // You talk at the same time, in your own rhythm.
        let own: Vec<f32> = bursts(12.0, 0.3, 7).iter().enumerate().map(|(i, v)| if (i as f64 / RATE as f64) % 0.7 < 0.35 { *v } else { 0.01 * v }).collect();
        let mut det2 = Detector::new();
        feed(&mut det2, &own, &them);
        assert!(!det2.present());
    }

    #[test]
    fn a_leak_from_the_speakers_is_heard() {
        let them = bursts(24.0, 0.3, 1);
        let mut det = Detector::new();
        feed(&mut det, &leak(&them, 0.3, 60), &them);
        assert!(det.active >= MIN_ACTIVE);
        assert!(det.present());
        assert!(det.windows() >= 3);
        assert!(det.correlated_at(5.0));
        assert!(!det.correlated_at(60.0));
        assert_eq!(det.over(10.0, 12.0), Some(Moment { correlated: true, loud: true, pure: true }));
    }

    #[test]
    fn a_short_stretch_is_not_yet_evidence() {
        let them = bursts(16.0, 0.3, 1);
        let mut det = Detector::new();
        feed(&mut det, &leak(&them, 0.3, 60), &them);
        assert!(det.active < MIN_ACTIVE);
        assert!(!det.present());
    }

    #[test]
    fn two_paths_from_the_speakers_still_count() {
        let them = bursts(30.0, 0.3, 1);
        let half = 15 * RATE;
        let mut me = leak(&them[..half], 0.3, 60);
        me.extend(leak(&them[half..], 0.3, 140));
        let mut det = Detector::new();
        feed(&mut det, &me, &them);
        assert!(det.present());
    }

    #[test]
    fn nothing_is_known_before_the_lag_is() {
        let mut det = Detector::new();
        feed(&mut det, &vec![0.0; 10 * RATE], &bursts(10.0, 0.3, 1));
        assert_eq!(det.over(2.0, 4.0), None);
    }

    #[test]
    fn your_own_loud_line_is_not_loud_like_the_leak() {
        let them = bursts(24.0, 0.3, 1);
        let own = bursts(6.0, 0.3, 7);
        let mut me = leak(&them, 0.3, 60);
        me.extend(&own);
        let mut theirs = them.clone();
        theirs.extend(own.iter().map(|v| 0.2 * v));
        let mut det = Detector::new();
        feed(&mut det, &me, &theirs);
        let m = det.over(26.0, 29.0).unwrap();
        assert!(!m.loud && !m.pure);
    }

    #[test]
    fn a_span_where_they_hardly_speak_is_no_echo() {
        let them = bursts(24.0, 0.3, 1);
        let mut me = leak(&them, 0.3, 60);
        me.extend(bursts(4.0, 0.3, 7));
        let mut theirs = them.clone();
        theirs.extend(vec![0.0; 4 * RATE]);
        let mut det = Detector::new();
        feed(&mut det, &me, &theirs);
        assert_eq!(det.over(25.0, 27.0), Some(Moment { correlated: false, loud: false, pure: false }));
    }

    #[test]
    fn verdicts_are_kept_past_two_hours() {
        let them = bursts(24.0, 0.3, 1);
        let mut det = Detector::new();
        feed(&mut det, &leak(&them, 0.3, 60), &them);
        let first = det.over(10.0, 12.0);
        assert!(first.is_some());
        let late = (2 * 3600 + 30) * RATE;
        det.push(Track::Them, late, &vec![0.0; RATE]);
        det.push(Track::Me, late, &vec![0.0; RATE]);
        assert_eq!(det.over(10.0, 12.0), first);
        assert_eq!(det.over(12.0, 14.0), None);
    }

    fn seg(start: f64, end: f64, text: &str) -> Seg<'_> {
        Seg { start, end, text }
    }

    #[test]
    fn the_words_decide_only_with_the_audio() {
        let them = [seg(10.0, 14.0, "We should ship the new version on Friday after the review")];
        let echo = seg(10.5, 14.2, "we should ship the new version Friday after review");
        let yes = |_: f64| true;
        let no = |_: f64| false;
        assert!(is_echo(echo, &them, &yes, None));
        assert!(!is_echo(echo, &them, &no, None), "no echo in the audio at that moment: kept");
        // Your own words, said loudly over a leak: kept.
        assert!(!is_echo(echo, &them, &yes, Some(Moment { correlated: true, loud: false, pure: false })));
        // A read-back long after their line ended: kept.
        assert!(!is_echo(seg(16.0, 19.0, "we should ship the new version Friday after review"), &them, &yes, None));
    }

    #[test]
    fn short_replies_stay_and_scraps_go() {
        let them = [seg(0.0, 4.0, "my role is to design the onboarding flow")];
        let yes = |_: f64| true;
        assert!(!is_echo(seg(1.0, 1.5, "yeah"), &them, &yes, None));
        assert!(!is_echo(seg(1.0, 1.5, "okay right"), &them, &yes, None));
        assert!(is_echo(seg(0.5, 1.2, "role is"), &them, &yes, None));
        assert!(is_echo(seg(0.5, 1.2, "um"), &them, &yes, None));
        // 4 words: only nearly whole.
        assert!(!is_echo(seg(1.0, 2.0, "I can design that"), &them, &yes, None));
        assert!(is_echo(seg(1.0, 2.0, "design the onboarding flow"), &them, &yes, None));
    }

    #[test]
    fn an_echo_split_over_two_of_their_lines_is_found() {
        let them = [seg(0.0, 3.0, "we need the budget numbers"), seg(3.1, 6.0, "before the board meeting next Tuesday")];
        let yes = |_: f64| true;
        assert!(is_echo(seg(0.4, 6.2, "we need the budget numbers before the board meeting next Tuesday"), &them, &yes, None));
    }

    #[test]
    fn chinese_is_compared_by_character() {
        let them = [seg(0.0, 3.0, "我们明天中午发布新版")];
        let yes = |_: f64| true;
        assert!(is_echo(seg(0.2, 3.1, "我们明天中午发布新版"), &them, &yes, None));
    }
}
