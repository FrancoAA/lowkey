//! Speech engine and model lifecycle (download, load, idle unload).
//!
//! whisper.cpp runs inside this process: on the GPU through Vulkan/Metal when there is one,
//! else on the CPU.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::audio;
use crate::formats::Segment;
use crate::models::{self, Hardware, ModelSpec};

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
    last_used: Mutex<Instant>,
    pub idle_minutes: AtomicU64,
    /// Transcriptions running now: never unload under a running job.
    busy: AtomicU64,
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub fn detect_hardware() -> Hardware {
    let gpu = if std::env::var_os("OPENWHISPER_NO_GPU").is_some() {
        None
    } else {
        gpu_name()
    };
    Hardware { gpu }
}

#[cfg(not(target_os = "macos"))]
fn gpu_name() -> Option<String> {
    whisper_rs::vulkan::list_devices().into_iter().map(|d| d.name).find(|n| !models::is_software_gpu(n))
}

/// Apple Silicon always has a Metal GPU; Intel Macs run on the CPU.
#[cfg(target_os = "macos")]
fn gpu_name() -> Option<String> {
    cfg!(target_arch = "aarch64").then(|| "Apple GPU (Metal)".to_string())
}

impl Engines {
    pub fn new(idle_minutes: u64) -> Arc<Self> {
        let hw = detect_hardware();
        let states = models::CATALOG
            .iter()
            .map(|m| (m.id.to_string(), ModelState::new(if models::local_path(m.source).exists() { "downloaded" } else { "missing" })))
            .collect();
        let me = Arc::new(Self {
            hw,
            states: Mutex::new(states),
            loaded: Mutex::new(None),
            last_used: Mutex::new(Instant::now()),
            idle_minutes: AtomicU64::new(idle_minutes),
            busy: AtomicU64::new(0),
            on_change: Mutex::new(None),
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

    fn unload_if_idle(&self) {
        let minutes = self.idle_minutes.load(Ordering::Relaxed);
        if self.busy.load(Ordering::SeqCst) > 0 || !idle_expired(minutes, self.last_used.lock().elapsed()) {
            return;
        }
        if let Some((id, _)) = self.loaded.lock().take() {
            eprintln!("  Unloaded {id} after {minutes} idle minutes (it loads again on the next use).");
            self.set_state(&id, |s| *s = ModelState::new("downloaded"));
        }
    }

    /// Download (if needed) and load `id`. Blocking.
    pub fn prepare(&self, id: &str) -> anyhow::Result<()> {
        let spec = models::spec(id).ok_or_else(|| anyhow::anyhow!("Unknown model {id:?}"))?;
        *self.last_used.lock() = Instant::now();
        self.prepare_wc(spec).map(|_| ()).inspect_err(|e| {
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

    pub fn transcribe(&self, id: &str, input: Input, sink: Sink) -> anyhow::Result<Output> {
        let spec = models::spec(id).ok_or_else(|| anyhow::anyhow!("Unknown model {id:?}"))?;
        *self.last_used.lock() = Instant::now();
        self.busy.fetch_add(1, Ordering::SeqCst);
        let out = self.transcribe_inner(id, spec, input, sink);
        self.busy.fetch_sub(1, Ordering::SeqCst);
        *self.last_used.lock() = Instant::now();
        out
    }

    fn transcribe_inner(&self, id: &str, spec: &ModelSpec, input: Input, sink: Sink) -> anyhow::Result<Output> {
        sink(EngineEvent::Preparing("Preparing model".into()));
        let ctx = self.prepare_wc(spec).inspect_err(|e| {
            let msg = friendly(e);
            self.set_state(id, |s| {
                s.state = "error".into();
                s.error = Some(msg.clone());
            })
        })?;
        let pcm = match input.pcm {
            Some(p) => p,
            None => audio::decode_file(input.path.as_deref().ok_or_else(|| anyhow::anyhow!("nothing to transcribe"))?).map_err(|e| anyhow::anyhow!("{e}"))?,
        };
        run_whisper_cpp(&ctx, &pcm, input.language.as_deref(), &sink)
    }

}

/// 0 minutes = keep the model loaded.
pub fn idle_expired(minutes: u64, idle: Duration) -> bool {
    minutes > 0 && idle >= Duration::from_secs(minutes * 60)
}

fn friendly(e: &anyhow::Error) -> String {
    let text = format!("{e:#}").to_lowercase();
    if ["dns", "connect", "resolve", "timed out", "network", "unreachable"].iter().any(|k| text.contains(k)) {
        "No internet connection: the model must be downloaded once. Connect and try again.".into()
    } else {
        format!("{e:#}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_unload_rule() {
        assert!(!idle_expired(0, Duration::from_secs(99_999)), "0 = never unload");
        assert!(!idle_expired(15, Duration::from_secs(14 * 60)));
        assert!(idle_expired(15, Duration::from_secs(15 * 60)));
    }

    #[test]
    fn offline_errors_are_explained() {
        let e = anyhow::anyhow!("error sending request: dns error: failed to lookup address");
        assert!(friendly(&e).starts_with("No internet connection"));
        assert_eq!(friendly(&anyhow::anyhow!("Model failed to load: bad magic")), "Model failed to load: bad magic");
    }
}
