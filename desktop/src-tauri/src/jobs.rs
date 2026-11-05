//! Job queue: one worker thread, so jobs do not fight for the CPU/GPU. Dictation goes ahead
//! of file transcriptions (priority 0 before 1); FIFO within a priority.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use serde::Serialize;

use crate::engine::{EngineEvent, Input, Output, Sink};
use crate::formats::{self, Segment};

pub trait Transcriber: Send + Sync {
    fn transcribe(&self, model: &str, input: Input, sink: Sink) -> anyhow::Result<Output>;
}

impl Transcriber for crate::engine::Engines {
    fn transcribe(&self, model: &str, input: Input, sink: Sink) -> anyhow::Result<Output> {
        crate::engine::Engines::transcribe(self, model, input, sink)
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct JobResult {
    pub language: Option<String>,
    pub duration: f64,
    pub segments: Vec<Segment>,
    pub text: String,
    pub srt: String,
    pub vtt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSnapshot {
    pub id: String,
    pub filename: String,
    /// queued | preparing | transcribing | done | error
    pub state: String,
    pub progress: f64,
    pub message: String,
    pub language: Option<String>,
    pub language_probability: Option<f64>,
    pub model: String,
    /// All segments so far (the UI merges idempotently on segment_count).
    pub new_segments: Vec<Segment>,
    pub segment_count: usize,
    pub result: Option<JobResult>,
    pub error: Option<String>,
}

struct Job {
    snap: JobSnapshot,
    input: Option<Input>,
    priority: u8,
    finished: Option<std::time::Instant>,
}

/// Finished jobs stay readable this long (the page may re-read them), then they are dropped.
pub const KEEP_FINISHED: std::time::Duration = std::time::Duration::from_secs(3600);

pub type EmitFn = Arc<dyn Fn(&JobSnapshot) + Send + Sync>;

pub struct Jobs {
    jobs: Mutex<HashMap<String, Job>>,
    queue: Mutex<BinaryHeap<Reverse<(u8, u64, String)>>>,
    wake: Condvar,
    done: Condvar,
    seq: Mutex<u64>,
    emit: EmitFn,
}

pub const MAX_PENDING: usize = 8;

impl Jobs {
    pub fn new(engine: Arc<dyn Transcriber>, emit: EmitFn) -> Arc<Self> {
        let me = Arc::new(Self {
            jobs: Mutex::default(),
            queue: Mutex::default(),
            wake: Condvar::new(),
            done: Condvar::new(),
            seq: Mutex::new(0),
            emit,
        });
        let worker = me.clone();
        std::thread::Builder::new().name("transcriber".into()).spawn(move || worker.run(engine)).unwrap();
        me
    }

    pub fn pending(&self) -> usize {
        self.jobs.lock().values().filter(|j| matches!(j.snap.state.as_str(), "queued" | "preparing" | "transcribing")).count()
    }

    pub fn submit(&self, filename: &str, input: Input, model: &str, priority: u8) -> anyhow::Result<String> {
        if self.pending() >= MAX_PENDING {
            anyhow::bail!("{MAX_PENDING} transcriptions are already waiting. Try again when they finish.");
        }
        let mut seq = self.seq.lock();
        *seq += 1;
        let id = format!("job-{}-{}", std::process::id(), *seq);
        let snap = JobSnapshot {
            id: id.clone(),
            filename: filename.into(),
            state: "queued".into(),
            progress: 0.0,
            message: "Waiting in queue".into(),
            language: input.language.clone(),
            language_probability: None,
            model: model.into(),
            new_segments: vec![],
            segment_count: 0,
            result: None,
            error: None,
        };
        (self.emit)(&snap);
        {
            let mut jobs = self.jobs.lock();
            jobs.retain(|_, j| j.finished.map(|t| t.elapsed() < KEEP_FINISHED).unwrap_or(true));
            jobs.insert(id.clone(), Job { snap, input: Some(input), priority, finished: None });
        }
        self.queue.lock().push(Reverse((priority, *seq, id.clone())));
        self.wake.notify_one();
        Ok(id)
    }

    pub fn snapshot(&self, id: &str) -> Option<JobSnapshot> {
        self.jobs.lock().get(id).map(|j| j.snap.clone())
    }

    /// Block until the job is done or failed; returns its final snapshot.
    pub fn wait(&self, id: &str) -> Option<JobSnapshot> {
        let mut jobs = self.jobs.lock();
        loop {
            let snap = jobs.get(id)?.snap.clone();
            if matches!(snap.state.as_str(), "done" | "error") {
                return Some(snap);
            }
            self.done.wait(&mut jobs);
        }
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut JobSnapshot)) {
        let snap = {
            let mut jobs = self.jobs.lock();
            let Some(job) = jobs.get_mut(id) else { return };
            f(&mut job.snap);
            job.snap.clone()
        };
        (self.emit)(&snap);
    }

    fn run(self: Arc<Self>, engine: Arc<dyn Transcriber>) {
        loop {
            let id = {
                let mut q = self.queue.lock();
                loop {
                    if let Some(Reverse((_, _, id))) = q.pop() {
                        break id;
                    }
                    self.wake.wait(&mut q);
                }
            };
            let (input, model) = {
                let mut jobs = self.jobs.lock();
                let Some(job) = jobs.get_mut(&id) else { continue };
                let _ = job.priority;
                (job.input.take(), job.snap.model.clone())
            };
            let Some(input) = input else { continue };
            let me = self.clone();
            let jid = id.clone();
            let sink: Sink = Arc::new(move |ev| {
                me.update(&jid, |s| match ev {
                    EngineEvent::Preparing(m) => {
                        s.state = "preparing".into();
                        s.message = m;
                    }
                    EngineEvent::Transcribing => {
                        s.state = "transcribing".into();
                        s.message = "Transcribing".into();
                    }
                    EngineEvent::Progress(p) => s.progress = p,
                    EngineEvent::Language(l, p) => {
                        s.language = Some(l);
                        s.language_probability = p;
                    }
                    EngineEvent::Segment(seg) => {
                        s.new_segments.push(seg);
                        s.segment_count = s.new_segments.len();
                    }
                })
            });
            let out = engine.transcribe(&model, input, sink);
            if let Some(job) = self.jobs.lock().get_mut(&id) {
                job.finished = Some(std::time::Instant::now());
            }
            self.update(&id, |s| match out {
                Ok(out) => {
                    s.state = "done".into();
                    s.progress = 1.0;
                    s.message = "Done".into();
                    s.language = out.language.clone().or(s.language.take());
                    s.new_segments = out.segments.clone();
                    s.segment_count = out.segments.len();
                    s.result = Some(JobResult {
                        language: out.language,
                        duration: out.duration,
                        text: formats::to_txt(&out.segments).trim().to_string(),
                        srt: formats::to_srt(&out.segments),
                        vtt: formats::to_vtt(&out.segments),
                        segments: out.segments,
                    });
                }
                Err(e) => {
                    s.state = "error".into();
                    s.message = "Failed".into();
                    s.error = Some(format!("{e:#}"));
                }
            });
            self.done.notify_all();
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub struct FakeEngine {
        pub calls: Mutex<Vec<String>>,
        pub languages: Mutex<Vec<Option<String>>>,
        pub gate: Mutex<bool>,
        pub cv: Condvar,
    }

    impl FakeEngine {
        pub fn new(open: bool) -> Arc<Self> {
            Arc::new(Self { calls: Mutex::default(), languages: Mutex::default(), gate: Mutex::new(open), cv: Condvar::new() })
        }
        pub fn open(&self) {
            *self.gate.lock() = true;
            self.cv.notify_all();
        }
    }

    impl Transcriber for FakeEngine {
        fn transcribe(&self, model: &str, input: Input, sink: Sink) -> anyhow::Result<Output> {
            {
                let mut g = self.gate.lock();
                while !*g {
                    self.cv.wait(&mut g);
                }
            }
            self.calls.lock().push(model.to_string());
            self.languages.lock().push(input.language.clone());
            sink(EngineEvent::Transcribing);
            let segs = vec![
                Segment { start: 0.0, end: 0.5, text: "Hello there.".into() },
                Segment { start: 0.5, end: 1.0, text: "General test.".into() },
            ];
            for s in &segs {
                sink(EngineEvent::Segment(s.clone()));
            }
            Ok(Output { language: Some(input.language.unwrap_or("en".into())), duration: 1.0, segments: segs })
        }
    }

    pub fn input() -> Input {
        Input { path: None, pcm: Some(vec![0.0; 16_000]), language: None }
    }

    #[test]
    fn job_runs_and_produces_formats() {
        let events = Arc::new(Mutex::new(Vec::<JobSnapshot>::new()));
        let ev = events.clone();
        let jobs = Jobs::new(FakeEngine::new(true), Arc::new(move |s| ev.lock().push(s.clone())));
        let id = jobs.submit("a.wav", input(), "small", 1).unwrap();
        let done = jobs.wait(&id).unwrap();
        assert_eq!(done.state, "done");
        let r = done.result.unwrap();
        assert_eq!(r.text, "Hello there. General test.");
        assert!(r.srt.starts_with("1\n00:00:00,000 --> 00:00:00,500\nHello there.\n"));
        assert!(r.vtt.starts_with("WEBVTT\n\n00:00:00.000 --> 00:00:00.500\nHello there."));
        let states: Vec<_> = events.lock().iter().map(|s| s.state.clone()).collect();
        assert_eq!(states.first().map(String::as_str), Some("queued"));
        assert!(states.iter().any(|s| s == "transcribing"));
    }

    #[test]
    fn dictation_goes_ahead_of_uploads() {
        let engine = FakeEngine::new(false);
        let jobs = Jobs::new(engine.clone(), Arc::new(|_| {}));
        let busy = jobs.submit("busy.wav", input(), "busy", 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let up = jobs.submit("upload.wav", input(), "upload", 1).unwrap();
        let dict = jobs.submit("dictation.wav", input(), "dictation", 0).unwrap();
        engine.open();
        for id in [&busy, &up, &dict] {
            jobs.wait(id);
        }
        assert_eq!(*engine.calls.lock(), vec!["busy", "dictation", "upload"]);
    }

    #[test]
    fn queue_is_capped() {
        let engine = FakeEngine::new(false);
        let jobs = Jobs::new(engine.clone(), Arc::new(|_| {}));
        for _ in 0..MAX_PENDING {
            jobs.submit("x.wav", input(), "m", 1).unwrap();
        }
        assert!(jobs.submit("x.wav", input(), "m", 1).unwrap_err().to_string().contains("already waiting"));
        engine.open();
    }
}
