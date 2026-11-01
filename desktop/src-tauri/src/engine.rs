//! Speech engines and model lifecycle (download, load, idle unload).
//!
//! whisper.cpp runs inside this process (GPU through Vulkan/Metal when present). faster-whisper
//! runs in a Python helper (`python -m openwhisper.worker`, started with uv) when such a model is
//! chosen or picked automatically on machines without a GPU.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::audio;
use crate::formats::Segment;
use crate::models::{self, EngineKind, Hardware, ModelSpec};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelState {
    /// missing | downloading | downloaded | loading | ready | error
    pub state: String,
    pub downloaded: u64,
    pub total: u64,
    pub progress: f64,
    pub error: Option<String>,
}

impl ModelState {
    fn new(state: &str) -> Self {
        Self { state: state.into(), downloaded: 0, total: 0, progress: if state == "ready" { 1.0 } else { 0.0 }, error: None }
    }
}

/// Engine → job events.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Preparing(String),
    Transcribing,
    Progress(f64),
    Language(String, Option<f64>),
    Segment(Segment),
}

pub type Sink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

pub struct Input {
    pub path: Option<PathBuf>,
    pub pcm: Option<Vec<f32>>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Output {
    pub language: Option<String>,
    pub duration: f64,
    pub segments: Vec<Segment>,
}

pub struct Engines {
    pub hw: Hardware,
    states: Mutex<HashMap<String, ModelState>>,
    loaded: Mutex<Option<(String, Arc<WhisperContext>)>>,
    sidecar: Mutex<Option<Sidecar>>,
    last_used: Mutex<Instant>,
    pub idle_minutes: AtomicU64,
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    py_root: Option<PathBuf>,
    me: std::sync::Weak<Engines>,
}

pub fn detect_hardware(py_root: Option<&Path>) -> Hardware {
    let gpu = if std::env::var_os("OPENWHISPER_NO_GPU").is_some() {
        None
    } else {
        whisper_rs::vulkan::list_devices().into_iter().map(|d| d.name).find(|n| !models::is_software_gpu(n))
    };
    Hardware { gpu, faster_whisper: py_root.is_some() && audio::which("uv") }
}

/// The Python project (pyproject.toml + openwhisper/) for the faster-whisper helper.
pub fn find_py_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OPENWHISPER_PY_ROOT") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).find(|d| d.join("pyproject.toml").is_file() && d.join("openwhisper").join("worker.py").is_file()).map(Path::to_path_buf)
}

impl Engines {
    pub fn new(idle_minutes: u64) -> Arc<Self> {
        let py_root = find_py_root();
        let hw = detect_hardware(py_root.as_deref());
        let states = models::CATALOG
            .iter()
            .map(|m| {
                let present = match m.engine {
                    EngineKind::WhisperCpp => models::local_path(m.source).exists(),
                    EngineKind::FasterWhisper => fw_cached(m.source),
                };
                (m.id.to_string(), ModelState::new(if present { "downloaded" } else { "missing" }))
            })
            .collect();
        let me = Arc::new_cyclic(|weak| Self {
            hw,
            states: Mutex::new(states),
            loaded: Mutex::new(None),
            sidecar: Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
            idle_minutes: AtomicU64::new(idle_minutes),
            on_change: Mutex::new(None),
            py_root,
            me: weak.clone(),
        });
        let weak = Arc::downgrade(&me);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(20));
            let Some(me) = weak.upgrade() else { return };
            me.unload_if_idle();
        });
        me
    }

    pub fn on_change(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.on_change.lock() = Some(f);
    }

    fn changed(&self) {
        if let Some(f) = self.on_change.lock().clone() {
            f();
        }
    }

    fn set_state(&self, id: &str, f: impl FnOnce(&mut ModelState)) {
        {
            let mut s = self.states.lock();
            let st = s.entry(id.to_string()).or_insert_with(|| ModelState::new("missing"));
            f(st);
        }
        self.changed();
    }

    pub fn state(&self, id: &str) -> ModelState {
        self.states.lock().get(id).cloned().unwrap_or_else(|| ModelState::new("missing"))
    }

    pub fn available(&self, spec: &ModelSpec) -> bool {
        spec.engine == EngineKind::WhisperCpp || self.hw.faster_whisper
    }

    fn unload_if_idle(&self) {
        let minutes = self.idle_minutes.load(Ordering::Relaxed);
        if minutes == 0 || self.last_used.lock().elapsed() < Duration::from_secs(minutes * 60) {
            return;
        }
        if let Some((id, _)) = self.loaded.lock().take() {
            eprintln!("  Unloaded {id} after {minutes} idle minutes (it loads again on the next use).");
            self.set_state(&id, |s| *s = ModelState::new("downloaded"));
        }
        if let Some(mut sc) = self.sidecar.lock().take() {
            sc.stop();
            for m in models::CATALOG.iter().filter(|m| m.engine == EngineKind::FasterWhisper) {
                if self.state(m.id).state == "ready" {
                    self.set_state(m.id, |s| *s = ModelState::new("downloaded"));
                }
            }
        }
    }

    /// Download (if needed) and load `id`. Blocking.
    pub fn prepare(&self, id: &str) -> anyhow::Result<()> {
        let spec = models::spec(id).ok_or_else(|| anyhow::anyhow!("Unknown model {id:?}"))?;
        *self.last_used.lock() = Instant::now();
        match spec.engine {
            EngineKind::WhisperCpp => self.prepare_wc(spec).map(|_| ()),
            EngineKind::FasterWhisper => self.with_sidecar(|sc| sc.prepare(spec.source)).map(|_| ()),
        }
        .inspect_err(|e| {
            let msg = friendly(e);
            self.set_state(id, |s| {
                s.state = "error".into();
                s.error = Some(msg.clone());
            });
        })
    }

    fn prepare_wc(&self, spec: &ModelSpec) -> anyhow::Result<Arc<WhisperContext>> {
        if let Some((id, ctx)) = self.loaded.lock().as_ref() {
            if id == spec.id {
                return Ok(ctx.clone());
            }
        }
        let path = models::local_path(spec.source);
        if !path.exists() {
            self.set_state(spec.id, |s| {
                *s = ModelState::new("downloading");
            });
            let id = spec.id;
            let mut last = Instant::now() - Duration::from_secs(1);
            models::download(&models::model_url(spec), &path, |done, total| {
                if last.elapsed() >= Duration::from_millis(200) || done == total {
                    last = Instant::now();
                    self.set_state(id, |s| {
                        s.downloaded = done;
                        s.total = total;
                        s.progress = if total > 0 { done as f64 / total as f64 } else { 0.0 };
                    });
                }
            })?;
        }
        let vad = models::local_path(models::VAD_FILE);
        if !vad.exists() {
            models::download(models::VAD_URL, &vad, |_, _| {})?;
        }
        self.set_state(spec.id, |s| {
            s.state = "loading".into();
            s.progress = 1.0;
        });
        // One model in memory at a time.
        if let Some((old, _)) = self.loaded.lock().take() {
            self.set_state(&old, |s| *s = ModelState::new("downloaded"));
        }
        let mut params = WhisperContextParameters::default();
        params.use_gpu(self.hw.gpu.is_some());
        params.flash_attn(self.hw.gpu.is_some());
        let ctx = Arc::new(
            WhisperContext::new_with_params(&path, params).map_err(|e| anyhow::anyhow!("Model failed to load: {e}"))?,
        );
        *self.loaded.lock() = Some((spec.id.to_string(), ctx.clone()));
        self.set_state(spec.id, |s| *s = ModelState::new("ready"));
        Ok(ctx)
    }

    fn with_sidecar<T>(&self, f: impl FnOnce(&mut Sidecar) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let mut guard = self.sidecar.lock();
        if guard.as_mut().map(|s| !s.alive()).unwrap_or(true) {
            let root = self.py_root.clone().ok_or_else(|| anyhow::anyhow!("faster-whisper needs uv and the open-whisper Python project"))?;
            let weak = self.me.clone();
            *guard = Some(Sidecar::start(&root, Arc::new(move |model: String, st: Value| {
                if let Some(me) = weak.upgrade() {
                    me.apply_fw_state(&model, &st);
                }
            }))?);
        }
        f(guard.as_mut().unwrap())
    }

    /// Model-state events from the helper (download progress, loading, ready, error).
    fn apply_fw_state(&self, model: &str, st: &Value) {
        if let Some(spec) = models::CATALOG.iter().find(|m| m.engine == EngineKind::FasterWhisper && m.source == model) {
            self.set_state(spec.id, |s| {
                s.state = st["state"].as_str().unwrap_or("loading").into();
                s.downloaded = st["downloaded"].as_u64().unwrap_or(0);
                s.total = st["total"].as_u64().unwrap_or(0);
                s.progress = st["progress"].as_f64().unwrap_or(0.0);
                s.error = st["error"].as_str().map(String::from);
            });
        }
    }

    pub fn transcribe(&self, id: &str, input: Input, sink: Sink) -> anyhow::Result<Output> {
        let spec = models::spec(id).ok_or_else(|| anyhow::anyhow!("Unknown model {id:?}"))?;
        *self.last_used.lock() = Instant::now();
        sink(EngineEvent::Preparing("Preparing model".into()));
        let out = match spec.engine {
            EngineKind::WhisperCpp => {
                let ctx = self.prepare_wc(spec).inspect_err(|e| {
                    let msg = friendly(e);
                    self.set_state(id, |s| {
                        s.state = "error".into();
                        s.error = Some(msg.clone());
                    })
                })?;
                let pcm = match input.pcm {
                    Some(p) => p,
                    None => audio::decode_file(input.path.as_deref().unwrap()).map_err(|e| anyhow::anyhow!("{e}"))?,
                };
                run_whisper_cpp(&ctx, &pcm, input.language.as_deref(), &sink)
            }
            EngineKind::FasterWhisper => {
                let (path, tmp) = match (&input.path, &input.pcm) {
                    (Some(p), _) => (p.clone(), None),
                    (None, Some(pcm)) => {
                        let tmp = std::env::temp_dir().join(format!("openwhisper-dictation-{}.wav", std::process::id()));
                        audio::write_wav(&tmp, pcm)?;
                        (tmp.clone(), Some(tmp))
                    }
                    _ => anyhow::bail!("nothing to transcribe"),
                };
                let result = self.with_sidecar(|sc| sc.transcribe(spec.source, &path, input.language.as_deref(), &sink));
                if let Some(t) = tmp {
                    let _ = std::fs::remove_file(t);
                }
                result
            }
        };
        *self.last_used.lock() = Instant::now();
        out
    }

    pub fn shutdown(&self) {
        if let Some(mut sc) = self.sidecar.lock().take() {
            sc.stop();
        }
    }
}

fn friendly(e: &anyhow::Error) -> String {
    let text = format!("{e:#}").to_lowercase();
    if ["dns", "connect", "resolve", "timed out", "network", "unreachable"].iter().any(|k| text.contains(k)) {
        "No internet connection: the model must be downloaded once. Connect and try again.".into()
    } else {
        format!("{e:#}")
    }
}

fn fw_cached(name: &str) -> bool {
    let repo = match name {
        "large-v3-turbo" => "mobiuslabsgmbh--faster-whisper-large-v3-turbo".to_string(),
        other => format!("Systran--faster-whisper-{other}"),
    };
    let hub = std::env::var_os("HF_HOME").map(PathBuf::from).unwrap_or_else(|| dirs::cache_dir().unwrap_or_default().join("huggingface")).join("hub");
    let snaps = hub.join(format!("models--{repo}")).join("snapshots");
    std::fs::read_dir(snaps)
        .map(|it| it.flatten().any(|d| d.path().join("model.bin").exists()))
        .unwrap_or(false)
}

fn physical_threads() -> i32 {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    (if n >= 4 { n / 2 } else { n }) as i32
}

fn vad_params() -> WhisperVadParams {
    let mut vp = WhisperVadParams::default();
    vp.set_threshold(0.3);
    vp
}

/// Does the audio contain speech at all? whisper.cpp's built-in VAD falls back to the whole
/// audio when it finds none, and Whisper then invents text ("Thank you.") on silence.
fn has_speech(vad_model: &Path, pcm: &[f32]) -> bool {
    let mut cp = WhisperVadContextParams::default();
    cp.set_n_threads(2);
    cp.set_use_gpu(false);
    let Ok(mut vad) = WhisperVadContext::new(&vad_model.to_string_lossy(), cp) else { return true };
    match vad.segments_from_samples(vad_params(), pcm) {
        Ok(segs) => segs.num_segments() > 0,
        Err(_) => true, // when unsure, transcribe
    }
}

fn run_whisper_cpp(ctx: &WhisperContext, pcm: &[f32], language: Option<&str>, sink: &Sink) -> anyhow::Result<Output> {
    let vad = models::local_path(models::VAD_FILE);
    let duration = pcm.len() as f64 / audio::RATE as f64;
    if vad.exists() && !has_speech(&vad, pcm) {
        sink(EngineEvent::Transcribing);
        return Ok(Output { language: None, duration, segments: vec![] });
    }
    let mut state = ctx.create_state().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut params = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
    params.set_n_threads(physical_threads());
    params.set_language(Some(language.unwrap_or("auto")));
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_print_special(false);
    // Silero VAD: drops silence (Whisper invents "Thank you." on silent audio). Threshold 0.3:
    // the 0.5 default cut speech in noisy audio (docs/benchmark.md).
    if vad.exists() {
        params.set_vad_model_path(Some(&vad.to_string_lossy()));
        params.set_vad_params(vad_params());
        params.enable_vad(true);
    }
    let s1 = sink.clone();
    params.set_progress_callback_safe(move |p: i32| s1(EngineEvent::Progress((p as f64 / 100.0).min(0.99))));
    let s2 = sink.clone();
    params.set_segment_callback_safe_lossy(move |seg: whisper_rs::SegmentCallbackData| {
        s2(EngineEvent::Segment(Segment { start: seg.start_timestamp as f64 / 100.0, end: seg.end_timestamp as f64 / 100.0, text: seg.text.trim().to_string() }))
    });
    sink(EngineEvent::Transcribing);
    state.full(params, pcm).map_err(|e| anyhow::anyhow!("Transcription failed: {e}"))?;
    let lang = whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(String::from);
    if let Some(l) = &lang {
        sink(EngineEvent::Language(l.clone(), None));
    }
    let segments = state
        .as_iter()
        .map(|s| Segment {
            start: s.start_timestamp() as f64 / 100.0,
            end: s.end_timestamp() as f64 / 100.0,
            text: s.to_str_lossy().map(|t| t.trim().to_string()).unwrap_or_default(),
        })
        .filter(|s| !s.text.is_empty())
        .collect();
    Ok(Output { language: lang, duration, segments })
}

// ------------------------------------------------------------------ faster-whisper helper
type StateFn = Arc<dyn Fn(String, Value) + Send + Sync>;

struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>,
    next: AtomicU64,
}

impl Sidecar {
    fn start(root: &Path, on_state: StateFn) -> anyhow::Result<Self> {
        let mut child = Command::new("uv")
            .args(["run", "--frozen", "--no-dev", "--project"])
            .arg(root)
            .args(["python", "-m", "openwhisper.worker"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| anyhow::anyhow!("could not start the faster-whisper helper (uv): {e}"))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let pending: Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>> = Arc::default();
        let p2 = pending.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if v["event"] == "model_state" {
                    on_state(v["model"].as_str().unwrap_or("").into(), v["state"].clone());
                    continue;
                }
                if let Some(id) = v["id"].as_u64() {
                    if let Some(tx) = p2.lock().get(&id) {
                        let _ = tx.send(v);
                    }
                }
            }
            // Helper exited: wake every waiter.
            for (_, tx) in p2.lock().drain() {
                let _ = tx.send(json!({"event": "error", "error": "the faster-whisper helper stopped"}));
            }
        });
        Ok(Self { child, stdin, pending, next: AtomicU64::new(1) })
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn request(&mut self, mut msg: Value) -> anyhow::Result<(u64, mpsc::Receiver<Value>)> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        msg["id"] = json!(id);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().insert(id, tx);
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok((id, rx))
    }

    fn prepare(&mut self, model: &str) -> anyhow::Result<()> {
        let (id, rx) = self.request(json!({"cmd": "prepare", "model": model}))?;
        let res = wait_done(&rx, |_| {});
        self.pending.lock().remove(&id);
        res.map(|_| ())
    }

    fn transcribe(&mut self, model: &str, path: &Path, language: Option<&str>, sink: &Sink) -> anyhow::Result<Output> {
        let (id, rx) = self.request(json!({"cmd": "transcribe", "model": model, "path": path, "language": language}))?;
        let res = wait_done(&rx, |v| match v["event"].as_str() {
            Some("state") if v["state"] == "transcribing" => sink(EngineEvent::Transcribing),
            Some("state") => sink(EngineEvent::Preparing(v["message"].as_str().unwrap_or("Preparing").into())),
            Some("progress") => sink(EngineEvent::Progress(v["progress"].as_f64().unwrap_or(0.0))),
            Some("language") => sink(EngineEvent::Language(v["language"].as_str().unwrap_or("").into(), v["probability"].as_f64())),
            Some("segment") => {
                if let Ok(seg) = serde_json::from_value::<Segment>(v["segment"].clone()) {
                    sink(EngineEvent::Segment(seg))
                }
            }
            _ => {}
        });
        self.pending.lock().remove(&id);
        let done = res?;
        Ok(Output {
            language: done["language"].as_str().map(String::from),
            duration: done["duration"].as_f64().unwrap_or(0.0),
            segments: serde_json::from_value(done["segments"].clone()).unwrap_or_default(),
        })
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_done(rx: &mpsc::Receiver<Value>, mut each: impl FnMut(&Value)) -> anyhow::Result<Value> {
    loop {
        let v = rx.recv().map_err(|_| anyhow::anyhow!("the faster-whisper helper stopped"))?;
        match v["event"].as_str() {
            Some("done") => return Ok(v),
            Some("error") => anyhow::bail!("{}", v["error"].as_str().unwrap_or("faster-whisper failed")),
            _ => each(&v),
        }
    }
}
