//! Global dictation state machine (the desktop key and its overlay).
//!
//! (starting →) idle → listening → transcribing → done/error → (overlay hides) …
//! Hold the key = push-to-talk; tap = start, tap again = stop. "starting" is used when the key
//! started the app: the overlay shows start-up/download progress, listening begins when the
//! model is ready. `epoch` counts recordings, so late events from an old one cannot touch a newer one.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

use crate::engine::{Input, ModelState};
use crate::jobs::Jobs;
use crate::mic::{EndFn, LevelFn, Recorder};
use crate::output::joining_space;
use crate::preview::{AudioFn, Stream, TextFn};

pub const HOLD_S: f64 = 0.35;
pub const MIN_RECORDING_S: f64 = 0.4;
pub const RESULT_SHOWN_S: f64 = 2.0;
pub const ERROR_SHOWN_S: f64 = 5.0;

/// Side effects, so tests can run without a mic, a compositor or a clipboard.
pub trait Io: Send + Sync {
    fn recorder(&self, on_level: LevelFn, on_end: EndFn, on_audio: Option<AudioFn>) -> Box<dyn Recorder>;
    /// Live preview for one recording (None: off, not ready, or the language is not covered).
    fn preview(&self, on_text: TextFn) -> Option<Stream>;
    /// The language from Options (None = detect).
    fn language(&self) -> Option<String>;
    fn deliver(&self, text: &str, prefix: &str) -> Vec<&'static str>;
    fn focused_window(&self) -> Option<String>;
    fn notify(&self, message: &str);
    fn overlay(&self, visible: bool);
    fn emit(&self, snap: &Snapshot);
    fn clock(&self) -> f64;
    fn model(&self) -> String;
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snapshot {
    pub state: String,
    pub level: f32,
    pub text: String,
    pub error: Option<String>,
    pub detail: String,
    pub delivered: Vec<String>,
    pub elapsed: f64,
    /// Live preview text while listening and transcribing (not what gets typed).
    pub preview: String,
}

struct Inner {
    state: String,
    text: String,
    error: Option<String>,
    detail: String,
    delivered: Vec<String>,
    epoch: u64,
    pressed_at: f64,
    started_at: f64,
    recorder: Option<Box<dyn Recorder>>,
    preview: String,
    /// Dropping it discards the preview stream; `finish` asks for its tail.
    stream: Option<Stream>,
    last_typed_at: Option<f64>,
    last_typed_window: Option<String>,
}

pub struct Dictation {
    io: Arc<dyn Io>,
    jobs: Arc<Jobs>,
    inner: Mutex<Inner>,
    /// Mic level (f32 bits). Outside the lock: the recorder reports it from any thread.
    level: AtomicU32,
    me: std::sync::Weak<Dictation>,
}

impl Dictation {
    pub fn new(io: Arc<dyn Io>, jobs: Arc<Jobs>) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            io,
            jobs,
            me: me.clone(),
            level: AtomicU32::new(0),
            inner: Mutex::new(Inner {
                state: "idle".into(),
                text: String::new(),
                error: None,
                detail: String::new(),
                delivered: vec![],
                epoch: 0,
                pressed_at: 0.0,
                started_at: 0.0,
                recorder: None,
                preview: String::new(),
                stream: None,
                last_typed_at: None,
                last_typed_window: None,
            }),
        })
    }

    fn arc(&self) -> Arc<Self> {
        self.me.upgrade().expect("dictation alive")
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snap(&self.inner.lock())
    }

    fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn set_level(&self, v: f32) {
        self.level.store(v.to_bits(), Ordering::Relaxed);
    }

    fn snap(&self, i: &Inner) -> Snapshot {
        let now = self.io.clock();
        Snapshot {
            state: i.state.clone(),
            level: if i.state == "listening" { self.level() } else { 0.0 },
            text: i.text.clone(),
            error: i.error.clone(),
            detail: i.detail.clone(),
            delivered: i.delivered.clone(),
            elapsed: if i.state == "listening" { ((now - i.started_at) * 10.0).round() / 10.0 } else { 0.0 },
            preview: if matches!(i.state.as_str(), "listening" | "transcribing") { i.preview.clone() } else { String::new() },
        }
    }

    fn publish(&self, i: &Inner) {
        self.io.emit(&self.snap(i));
    }

    fn set(&self, i: &mut Inner, state: &str) {
        i.state = state.into();
        self.publish(i);
    }

    // ------------------------------------------------------------ events from the key
    pub fn press(&self) -> Snapshot {
        let mut i = self.inner.lock();
        match i.state.as_str() {
            "starting" => {
                i.epoch += 1; // tapped again before it was ready: never mind
                i.detail.clear();
                self.set(&mut i, "idle");
                self.schedule_hide(&i, 0.0);
            }
            "listening" => {
                i.pressed_at = self.io.clock();
                self.stop(&mut i); // second tap in toggle mode
            }
            "transcribing" => {
                i.detail = "Still transcribing the last one. Try again in a moment.".into();
                self.publish(&i);
            }
            _ => {
                i.pressed_at = self.io.clock();
                self.start(&mut i);
            }
        }
        self.snap(&i)
    }

    pub fn release(&self) -> Snapshot {
        let mut i = self.inner.lock();
        // A long press is push-to-talk; a short one (tap) keeps listening until the next press.
        if i.state == "listening" && self.io.clock() - i.pressed_at >= HOLD_S {
            self.stop(&mut i);
        }
        self.snap(&i)
    }

    pub fn toggle(&self) -> Snapshot {
        let mut i = self.inner.lock();
        match i.state.as_str() {
            "listening" => self.stop(&mut i),
            "transcribing" | "starting" => {}
            _ => {
                i.pressed_at = self.io.clock();
                self.start(&mut i)
            }
        }
        self.snap(&i)
    }

    pub fn cancel(&self) -> Snapshot {
        let mut i = self.inner.lock();
        if let Some(mut r) = i.recorder.take() {
            r.stop();
        }
        i.stream = None;
        if matches!(i.state.as_str(), "starting" | "listening" | "transcribing") {
            i.epoch += 1; // a running transcript is dropped, not typed
            i.detail.clear();
            self.set(&mut i, "idle");
            self.schedule_hide(&i, 0.0);
        }
        self.snap(&i)
    }

    /// The key started the app: show the overlay now; listen when the model is ready. The key
    /// press happened about now, so a later release still counts as a hold.
    pub fn start_when_ready(&self, model_state: Arc<dyn Fn() -> ModelState + Send + Sync>, poll: Duration) {
        let epoch = {
            let mut i = self.inner.lock();
            if i.state != "idle" {
                return; // a press reached us first
            }
            i.epoch += 1;
            i.pressed_at = self.io.clock();
            i.detail = "Starting Lowkey…".into();
            i.text.clear();
            i.error = None;
            i.delivered.clear();
            self.set(&mut i, "starting");
            i.epoch
        };
        self.io.overlay(true);
        let me = self.arc();
        std::thread::spawn(move || {
            let mut last = String::new();
            loop {
                let ms = model_state();
                let mut i = me.inner.lock();
                if i.epoch != epoch || i.state != "starting" {
                    return;
                }
                match ms.state.as_str() {
                    "ready" => {
                        me.start_keep_press(&mut i);
                        return;
                    }
                    "error" => {
                        me.fail(&mut i, ms.error.as_deref().unwrap_or("The model could not be loaded."), "");
                        return;
                    }
                    "downloading" => i.detail = format!("Downloading the model (first run)… {}%", (ms.progress * 100.0) as u32),
                    _ => i.detail = "Loading the model…".into(),
                }
                if i.detail != last {
                    last = i.detail.clone();
                    me.publish(&i);
                }
                drop(i);
                std::thread::sleep(poll);
            }
        });
    }

    // ------------------------------------------------------------ internals (lock held)
    fn start(&self, i: &mut Inner) {
        self.start_inner(i, false);
    }

    fn start_keep_press(&self, i: &mut Inner) {
        self.start_inner(i, true);
    }

    fn start_inner(&self, i: &mut Inner, keep_press: bool) {
        if !keep_press {
            i.pressed_at = self.io.clock();
        }
        i.epoch += 1;
        let epoch = i.epoch;
        i.started_at = self.io.clock();
        i.text.clear();
        i.error = None;
        i.detail.clear();
        i.delivered.clear();
        i.preview.clear();
        i.stream = None;
        self.set_level(0.0);
        self.set(i, "listening");
        let me_t = self.me.clone();
        let on_text: TextFn = Arc::new(move |text| {
            if let Some(me) = me_t.upgrade() {
                let mut i = me.inner.lock();
                if i.epoch == epoch && matches!(i.state.as_str(), "listening" | "transcribing") && i.preview != text {
                    i.preview = text;
                    me.publish(&i);
                }
            }
        });
        let stream = self.io.preview(on_text);
        let me_l = self.me.clone();
        let on_level: LevelFn = Arc::new(move |lv| {
            if let Some(me) = me_l.upgrade() {
                me.set_level(lv);
                // Publish only if nobody holds the lock (the next level will publish otherwise).
                if let Some(i) = me.inner.try_lock() {
                    if i.epoch == epoch && i.state == "listening" {
                        me.publish(&i);
                    }
                }
            }
        });
        let me_e = self.me.clone();
        let on_end: EndFn = Arc::new(move |reason| {
            if let Some(me) = me_e.upgrade() {
                // From the recorder's own thread: never block it on the lock owner.
                std::thread::spawn(move || {
                    let mut i = me.inner.lock();
                    if i.epoch != epoch || i.state != "listening" {
                        return;
                    }
                    if reason == "limit" {
                        me.stop(&mut i);
                    } else {
                        i.recorder = None;
                        let why = reason.trim_start_matches("failed: ").to_string();
                        me.fail(&mut i, &format!("The microphone stopped: {why}"), "");
                    }
                });
            }
        });
        let mut rec = self.io.recorder(on_level, on_end, stream.as_ref().map(|s| s.audio.clone()));
        if let Err(e) = rec.start() {
            self.fail(i, &format!("Could not open the microphone: {e}"), "");
            return;
        }
        i.recorder = Some(rec);
        i.stream = stream;
        self.io.overlay(true);
    }

    fn stop(&self, i: &mut Inner) {
        let pcm = i.recorder.take().map(|mut r| r.stop()).unwrap_or_default();
        let recorded_at = self.io.clock();
        let stream = i.stream.take();
        if (pcm.len() as f64) < MIN_RECORDING_S * crate::audio::RATE as f64 {
            drop(stream);
            self.fail(i, "Too short. Hold the key while you talk, or tap it once to start and again to stop.", "");
            return;
        }
        self.set_level(0.0);
        i.detail.clear();
        self.set(i, "transcribing");
        if let Some(s) = stream {
            s.finish(); // the preview's last words arrive while the main model works
        }
        let epoch = i.epoch;
        let model = self.io.model();
        let language = self.io.language();
        let job = self.jobs.submit("dictation", Input { path: None, pcm: Some(pcm), language }, &model, 0);
        let me = self.arc();
        std::thread::spawn(move || me.finish(epoch, job, recorded_at));
    }

    fn finish(&self, epoch: u64, job: anyhow::Result<String>, recorded_at: f64) {
        let id = match job {
            Ok(id) => id,
            Err(e) => {
                let mut i = self.inner.lock();
                if i.epoch == epoch {
                    self.fail(&mut i, &e.to_string(), "");
                }
                return;
            }
        };
        // Show why it waits: another job ahead, or the first-run model download.
        let me = self.arc();
        let id2 = id.clone();
        let watcher = std::thread::spawn(move || loop {
            let Some(s) = me.jobs.snapshot(&id2) else { return };
            if matches!(s.state.as_str(), "done" | "error") {
                return;
            }
            let msg = match s.state.as_str() {
                "queued" => "Waiting for another transcription…".to_string(),
                "preparing" => s.message.clone(),
                _ => String::new(),
            };
            {
                let mut i = me.inner.lock();
                if i.epoch != epoch || i.state != "transcribing" {
                    return;
                }
                if i.detail != msg && !i.detail.starts_with("Still transcribing") {
                    i.detail = msg;
                    me.publish(&i);
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        });
        let snap = self.jobs.wait(&id);
        let _ = watcher.join();
        let text = {
            let mut i = self.inner.lock();
            if i.epoch != epoch {
                return; // cancelled or superseded
            }
            match snap {
                Some(s) if s.state == "done" => {
                    let text = s.result.map(|r| r.text).unwrap_or_default();
                    if text.trim().is_empty() {
                        self.fail(&mut i, "No speech was heard.", "");
                        return;
                    }
                    text
                }
                Some(s) => {
                    self.fail(&mut i, s.error.as_deref().unwrap_or("Transcription failed."), "");
                    return;
                }
                None => return,
            }
        };
        let window = self.io.focused_window();
        let prefix = {
            let i = self.inner.lock();
            // Unknown window (not Hyprland): time alone decides.
            let same = window.is_none() || window == i.last_typed_window;
            joining_space(&text, recorded_at, i.last_typed_at, same)
        };
        let delivered = self.io.deliver(&text, prefix);
        let mut i = self.inner.lock();
        if delivered.contains(&"typed") {
            i.last_typed_at = Some(self.io.clock());
            i.last_typed_window = window;
        }
        if i.epoch != epoch {
            return;
        }
        if delivered.is_empty() {
            self.fail(&mut i, &format!("Could not type or copy the text (install wtype and wl-clipboard). Text: {text}"), &text);
            return;
        }
        i.text = text;
        i.delivered = delivered.into_iter().map(String::from).collect();
        i.detail.clear();
        self.set(&mut i, "done");
        self.schedule_hide(&i, RESULT_SHOWN_S);
    }

    fn fail(&self, i: &mut Inner, message: &str, text: &str) {
        i.stream = None;
        i.error = Some(message.to_string());
        self.set_level(0.0);
        i.text = text.to_string();
        i.detail.clear();
        self.set(i, "error");
        self.io.notify(message);
        self.schedule_hide(i, ERROR_SHOWN_S);
    }

    fn schedule_hide(&self, i: &Inner, delay: f64) {
        let epoch = i.epoch;
        let me = self.arc();
        std::thread::spawn(move || {
            if delay > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(delay));
            }
            let i = me.inner.lock();
            if i.epoch != epoch || matches!(i.state.as_str(), "starting" | "listening" | "transcribing") {
                return; // a newer dictation owns the overlay now
            }
            me.io.overlay(false);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::tests::FakeEngine;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct FakeRec {
        secs: f64,
        on_end: EndFn,
        on_level: LevelFn,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Recorder for FakeRec {
        fn start(&mut self) -> anyhow::Result<()> {
            (self.on_level)(0.7);
            self.log.lock().push("start".into());
            Ok(())
        }
        fn stop(&mut self) -> Vec<f32> {
            self.log.lock().push("stop".into());
            vec![0.0; (self.secs * 16_000.0) as usize]
        }
    }

    struct FakeIo {
        t: Mutex<f64>,
        secs: Mutex<f64>,
        delivered: Mutex<Vec<String>>,
        result: Mutex<Vec<&'static str>>,
        window: Mutex<Option<String>>,
        notes: Mutex<Vec<String>>,
        overlay: Mutex<Vec<bool>>,
        rec_log: Arc<Mutex<Vec<String>>>,
        ends: Mutex<Vec<EndFn>>,
        emits: AtomicU64,
        language: Mutex<Option<String>>,
        /// Live preview: on, the text callback of the last stream, and what the stream got.
        preview_on: Mutex<bool>,
        preview_text: Mutex<Option<TextFn>>,
        preview_log: Arc<Mutex<Vec<String>>>,
    }

    impl FakeIo {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                t: Mutex::new(100.0),
                secs: Mutex::new(1.0),
                delivered: Mutex::default(),
                result: Mutex::new(vec!["copied", "typed"]),
                window: Mutex::new(Some("0xa".into())),
                notes: Mutex::default(),
                overlay: Mutex::default(),
                rec_log: Arc::default(),
                ends: Mutex::default(),
                emits: AtomicU64::new(0),
                language: Mutex::new(None),
                preview_on: Mutex::new(false),
                preview_text: Mutex::new(None),
                preview_log: Arc::default(),
            })
        }
        fn advance(&self, s: f64) {
            *self.t.lock() += s;
        }
    }

    impl Io for FakeIo {
        fn recorder(&self, on_level: LevelFn, on_end: EndFn, on_audio: Option<AudioFn>) -> Box<dyn Recorder> {
            self.ends.lock().push(on_end.clone());
            if let Some(a) = on_audio {
                a(&[0.0; 8_000], 16_000);
            }
            Box::new(FakeRec { secs: *self.secs.lock(), on_end, on_level, log: self.rec_log.clone() })
        }
        fn preview(&self, on_text: TextFn) -> Option<Stream> {
            if !*self.preview_on.lock() {
                return None;
            }
            *self.preview_text.lock() = Some(on_text);
            let (l1, l2) = (self.preview_log.clone(), self.preview_log.clone());
            Some(Stream::new(
                Arc::new(move |pcm, _| l1.lock().push(format!("audio {}", pcm.len()))),
                Box::new(move |flush| l2.lock().push(if flush { "finish".into() } else { "discard".into() })),
            ))
        }
        fn language(&self) -> Option<String> {
            self.language.lock().clone()
        }
        fn deliver(&self, text: &str, prefix: &str) -> Vec<&'static str> {
            self.delivered.lock().push(format!("{prefix}{text}"));
            self.result.lock().clone()
        }
        fn focused_window(&self) -> Option<String> {
            self.window.lock().clone()
        }
        fn notify(&self, m: &str) {
            self.notes.lock().push(m.into());
        }
        fn overlay(&self, v: bool) {
            self.overlay.lock().push(v);
        }
        fn emit(&self, _: &Snapshot) {
            self.emits.fetch_add(1, Ordering::Relaxed);
        }
        fn clock(&self) -> f64 {
            *self.t.lock()
        }
        fn model(&self) -> String {
            "large-v3-turbo-q5_0".into()
        }
    }

    fn setup(open: bool) -> (Arc<Dictation>, Arc<FakeIo>, Arc<FakeEngine>) {
        let io = FakeIo::new();
        let engine = FakeEngine::new(open);
        let jobs = Jobs::new(engine.clone(), Arc::new(|_| {}));
        (Dictation::new(io.clone(), jobs), io, engine)
    }

    fn wait_state(d: &Dictation, states: &[&str]) -> String {
        for _ in 0..500 {
            let s = d.snapshot().state;
            if states.contains(&s.as_str()) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("state stayed {}", d.snapshot().state);
    }

    fn say(io: &FakeIo, text: &str) {
        let f = io.preview_text.lock().clone().expect("a preview stream");
        f(text.to_string());
    }

    #[test]
    fn live_preview_shows_while_listening_and_transcribing_but_never_is_typed() {
        let (d, io, engine) = setup(false);
        *io.preview_on.lock() = true;
        d.toggle();
        say(&io, "hello from");
        assert_eq!(d.snapshot().preview, "hello from");
        d.toggle();
        assert_eq!(d.snapshot().state, "transcribing");
        say(&io, "hello from open whisper");
        assert_eq!(d.snapshot().preview, "hello from open whisper");
        assert_eq!(*io.preview_log.lock(), vec!["audio 8000", "finish"]);
        engine.open();
        wait_state(&d, &["done"]);
        assert_eq!(d.snapshot().preview, "");
        assert_eq!(*io.delivered.lock(), vec!["Hello there. General test."]);
        say(&io, "late words"); // after done: ignored
        assert_eq!(d.snapshot().preview, "");
    }

    #[test]
    fn cancel_discards_the_preview_and_a_new_recording_starts_empty() {
        let (d, io, _) = setup(true);
        *io.preview_on.lock() = true;
        d.toggle();
        let old = io.preview_text.lock().clone().unwrap();
        old("first try".into());
        d.cancel();
        assert_eq!(*io.preview_log.lock(), vec!["audio 8000", "discard"]);
        d.toggle();
        assert_eq!(d.snapshot().preview, "");
        old("stale".into()); // the old stream's late text does not reach the new recording
        assert_eq!(d.snapshot().preview, "");
        d.cancel();
    }

    #[test]
    fn too_short_discards_the_preview() {
        let (d, io, _) = setup(true);
        *io.preview_on.lock() = true;
        *io.secs.lock() = 0.1;
        d.toggle();
        d.toggle();
        assert_eq!(d.snapshot().state, "error");
        assert_eq!(io.preview_log.lock().last().unwrap(), "discard");
    }

    #[test]
    fn dictation_without_preview_works_as_before() {
        let (d, io, _) = setup(true);
        d.toggle();
        d.toggle();
        wait_state(&d, &["done"]);
        assert!(io.preview_log.lock().is_empty());
        assert_eq!(io.delivered.lock().len(), 1);
    }

    #[test]
    fn the_key_uses_the_language_from_options() {
        let (d, io, engine) = setup(true);
        *io.language.lock() = Some("es".into());
        d.toggle();
        d.toggle();
        wait_state(&d, &["done"]);
        assert_eq!(*engine.languages.lock(), vec![Some("es".to_string())]);
    }

    #[test]
    fn hold_is_push_to_talk() {
        let (d, io, _) = setup(true);
        assert_eq!(d.press().state, "listening");
        assert!((d.snapshot().level - 0.7).abs() < 1e-6);
        io.advance(2.0);
        assert_eq!(d.release().state, "transcribing");
        wait_state(&d, &["done"]);
        assert_eq!(*io.delivered.lock(), vec!["Hello there. General test."]);
        assert_eq!(d.snapshot().delivered, vec!["copied", "typed"]);
    }

    #[test]
    fn tap_toggles() {
        let (d, io, _) = setup(true);
        d.press();
        io.advance(0.1);
        assert_eq!(d.release().state, "listening");
        io.advance(3.0);
        assert_eq!(d.press().state, "transcribing");
        io.advance(0.1);
        d.release();
        wait_state(&d, &["done"]);
        assert_eq!(io.delivered.lock().len(), 1);
    }

    #[test]
    fn too_short_is_an_error() {
        let (d, io, _) = setup(true);
        *io.secs.lock() = 0.1;
        d.press();
        io.advance(1.0);
        assert_eq!(d.release().state, "error");
        assert!(d.snapshot().error.unwrap().contains("Too short"));
        assert!(io.delivered.lock().is_empty());
    }

    #[test]
    fn cancel_while_transcribing_drops_the_text() {
        let (d, io, engine) = setup(false);
        d.toggle();
        d.toggle();
        assert_eq!(d.cancel().state, "idle");
        engine.open();
        std::thread::sleep(Duration::from_millis(150));
        assert!(io.delivered.lock().is_empty());
        assert_eq!(d.snapshot().state, "idle");
    }

    #[test]
    fn press_while_transcribing_says_busy() {
        let (d, _, engine) = setup(false);
        d.toggle();
        d.toggle();
        let s = d.press();
        assert_eq!(s.state, "transcribing");
        assert!(s.detail.contains("Still transcribing"));
        engine.open();
        wait_state(&d, &["done"]);
    }

    #[test]
    fn nothing_delivered_is_an_error_that_keeps_the_text() {
        let (d, io, _) = setup(true);
        *io.result.lock() = vec![];
        d.toggle();
        d.toggle();
        wait_state(&d, &["error"]);
        let s = d.snapshot();
        assert!(s.error.unwrap().contains("wtype") && s.text == "Hello there. General test.");
        assert!(!io.notes.lock().is_empty());
    }

    #[test]
    fn join_space_within_15s_same_window() {
        let (d, io, _) = setup(true);
        for _ in 0..2 {
            d.toggle();
            io.advance(2.0);
            d.toggle();
            wait_state(&d, &["done"]);
            io.advance(3.0);
        }
        io.advance(30.0);
        d.toggle();
        io.advance(2.0);
        d.toggle();
        for _ in 0..200 {
            if io.delivered.lock().len() == 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let got = io.delivered.lock().clone();
        assert_eq!(got, vec!["Hello there. General test.", " Hello there. General test.", "Hello there. General test."]);
    }

    #[test]
    fn no_space_into_another_window() {
        let (d, io, _) = setup(true);
        d.toggle();
        io.advance(2.0);
        d.toggle();
        wait_state(&d, &["done"]);
        *io.window.lock() = Some("0xb".into());
        io.advance(3.0);
        d.toggle();
        io.advance(2.0);
        d.toggle();
        for _ in 0..200 {
            if io.delivered.lock().len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(io.delivered.lock()[1], "Hello there. General test.");
    }

    fn states(seq: Vec<&'static str>) -> Arc<dyn Fn() -> ModelState + Send + Sync> {
        let q = Mutex::new(seq);
        Arc::new(move || {
            let mut q = q.lock();
            let s = if q.len() > 1 { q.remove(0) } else { q[0] };
            ModelState { state: s.into(), downloaded: 0, total: 0, progress: if s == "downloading" { 0.42 } else { 1.0 }, error: (s == "error").then(|| "No internet connection".into()) }
        })
    }

    #[test]
    fn key_started_the_app_listening_begins_when_ready() {
        let (d, io, _) = setup(true);
        d.start_when_ready(states(vec!["downloading", "downloading", "loading", "ready"]), Duration::from_millis(5));
        let mut details = vec![];
        for _ in 0..400 {
            let s = d.snapshot();
            details.push(s.detail.clone());
            if s.state == "listening" {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(details.iter().any(|x| x.contains("42%")) && details.iter().any(|x| x.contains("Loading")));
        assert_eq!(d.snapshot().state, "listening");
        io.advance(3.0); // held through start-up: the release is push-to-talk
        assert_eq!(d.release().state, "transcribing");
        wait_state(&d, &["done"]);
    }

    #[test]
    fn second_tap_during_start_up_cancels_and_start_does_not_override() {
        let (d, io, _) = setup(true);
        d.start_when_ready(states(vec!["loading"]), Duration::from_millis(5));
        assert_eq!(d.press().state, "idle");
        std::thread::sleep(Duration::from_millis(50));
        assert!(io.rec_log.lock().is_empty());
        d.toggle(); // a press reached us first
        d.start_when_ready(states(vec!["ready"]), Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(io.rec_log.lock().iter().filter(|x| *x == "start").count(), 1);
    }

    #[test]
    fn model_error_during_start_up_is_reported() {
        let (d, io, _) = setup(true);
        d.start_when_ready(states(vec!["error"]), Duration::from_millis(5));
        wait_state(&d, &["error"]);
        assert!(d.snapshot().error.unwrap().contains("No internet") && !io.notes.lock().is_empty());
    }

    #[test]
    fn mic_failure_and_length_limit() {
        let (d, io, _) = setup(true);
        d.toggle();
        let end = io.ends.lock()[0].clone();
        end("failed: no source".into());
        wait_state(&d, &["error"]);
        assert!(d.snapshot().error.unwrap().contains("no source"));
        d.toggle();
        let end2 = io.ends.lock()[1].clone();
        end2("limit".into()); // the cap stops and transcribes
        wait_state(&d, &["transcribing", "done"]);
        wait_state(&d, &["done"]);
        // a late event from the first recorder is ignored
        end("failed: old".into());
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(d.snapshot().state, "done");
    }
}
