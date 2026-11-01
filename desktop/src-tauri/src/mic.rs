//! Microphone capture with cpal (ALSA/PipeWire, CoreAudio, WASAPI). The app records the mic
//! itself; nothing goes through the webview.
//!
//! OPENWHISPER_FAKE_MIC=<audio file> replaces the mic with that file played in real time,
//! looped. It exists for automated tests only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::audio;

pub const MAX_RECORDING_S: usize = 10 * 60;

pub type LevelFn = Arc<dyn Fn(f32) + Send + Sync>;
/// Called once if capture ends on its own: "limit" or "failed: <why>".
pub type EndFn = Arc<dyn Fn(String) + Send + Sync>;

pub trait Recorder: Send {
    fn start(&mut self) -> anyhow::Result<()>;
    /// Stop and return 16 kHz mono samples.
    fn stop(&mut self) -> Vec<f32>;
}

pub fn new_recorder(on_level: LevelFn, on_end: EndFn) -> Box<dyn Recorder> {
    match std::env::var_os("OPENWHISPER_FAKE_MIC") {
        Some(path) => Box::new(FakeMic::new(path.into(), on_level, on_end)),
        None => Box::new(CpalMic::new(on_level, on_end)),
    }
}

struct Shared {
    samples: Mutex<Vec<f32>>,
    rate: Mutex<u32>,
    stopping: AtomicBool,
    ended: AtomicBool,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Self { samples: Mutex::default(), rate: Mutex::new(audio::RATE), stopping: AtomicBool::new(false), ended: AtomicBool::new(false) })
    }

    /// Append mono samples; report the level every ~50 ms; stop at the length limit.
    fn push(&self, mono: &[f32], pending: &mut Vec<f32>, on_level: &LevelFn, on_end: &EndFn) {
        let rate = *self.rate.lock() as usize;
        let mut s = self.samples.lock();
        s.extend_from_slice(mono);
        pending.extend_from_slice(mono);
        let block = rate / 20;
        while pending.len() >= block {
            on_level(audio::level(&pending[..block]));
            pending.drain(..block);
        }
        if s.len() >= MAX_RECORDING_S * rate && !self.stopping.load(Ordering::Relaxed) && !self.ended.swap(true, Ordering::Relaxed) {
            on_end("limit".into());
        }
    }

    fn take_16k(&self) -> Vec<f32> {
        let samples = std::mem::take(&mut *self.samples.lock());
        audio::resample(&samples, *self.rate.lock(), audio::RATE)
    }
}

pub struct CpalMic {
    on_level: LevelFn,
    on_end: EndFn,
    shared: Arc<Shared>,
    stop_tx: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CpalMic {
    fn new(on_level: LevelFn, on_end: EndFn) -> Self {
        Self { on_level, on_end, shared: Shared::new(), stop_tx: None, thread: None }
    }
}

impl Recorder for CpalMic {
    fn start(&mut self) -> anyhow::Result<()> {
        // cpal streams are not Send on every platform: one thread owns the stream.
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (shared, on_level, on_end) = (self.shared.clone(), self.on_level.clone(), self.on_end.clone());
        let thread = std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<cpal::Stream> {
                let host = cpal::default_host();
                let device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("no microphone found"))?;
                let config = device.default_input_config()?;
                let channels = config.channels() as usize;
                *shared.rate.lock() = config.sample_rate().0;
                let err_end = on_end.clone();
                let err_shared = shared.clone();
                let on_err = move |e: cpal::StreamError| {
                    if !err_shared.stopping.load(Ordering::Relaxed) && !err_shared.ended.swap(true, Ordering::Relaxed) {
                        err_end(format!("failed: {e}"));
                    }
                };
                macro_rules! build {
                    ($t:ty, $conv:expr) => {{
                        let (shared, on_level, on_end) = (shared.clone(), on_level.clone(), on_end.clone());
                        let mut pending = Vec::new();
                        device.build_input_stream(
                            &config.clone().into(),
                            move |data: &[$t], _: &cpal::InputCallbackInfo| {
                                let mono: Vec<f32> = data.chunks(channels).map(|f| f.iter().map(|&x| $conv(x)).sum::<f32>() / channels as f32).collect();
                                shared.push(&mono, &mut pending, &on_level, &on_end);
                            },
                            on_err,
                            None,
                        )?
                    }};
                }
                let stream = match config.sample_format() {
                    cpal::SampleFormat::F32 => build!(f32, |x: f32| x),
                    cpal::SampleFormat::I16 => build!(i16, |x: i16| x as f32 / 32768.0),
                    cpal::SampleFormat::U16 => build!(u16, |x: u16| (x as f32 - 32768.0) / 32768.0),
                    cpal::SampleFormat::I32 => build!(i32, |x: i32| x as f32 / 2147483648.0),
                    other => anyhow::bail!("unsupported microphone sample format {other:?}"),
                };
                stream.play()?;
                Ok(stream)
            })();
            match result {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                self.stop_tx = Some(stop_tx);
                self.thread = Some(thread);
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Too slow: tell the thread to drop the stream if it opens later.
                self.shared.stopping.store(true, Ordering::Relaxed);
                let _ = stop_tx.send(());
                Err(anyhow::anyhow!("the microphone did not start"))
            }
        }
    }

    fn stop(&mut self) -> Vec<f32> {
        self.shared.stopping.store(true, Ordering::Relaxed);
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.shared.take_16k()
    }
}

pub struct FakeMic {
    path: std::path::PathBuf,
    on_level: LevelFn,
    on_end: EndFn,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeMic {
    fn new(path: std::path::PathBuf, on_level: LevelFn, on_end: EndFn) -> Self {
        Self { path, on_level, on_end, shared: Shared::new(), thread: None }
    }
}

impl Recorder for FakeMic {
    fn start(&mut self) -> anyhow::Result<()> {
        let clip = audio::decode_file(&self.path).map_err(|e| anyhow::anyhow!("{e}"))?;
        let (shared, on_level, on_end) = (self.shared.clone(), self.on_level.clone(), self.on_end.clone());
        self.thread = Some(std::thread::spawn(move || {
            let chunk = audio::RATE as usize / 20;
            let mut pending = Vec::new();
            let mut pos = 0usize;
            while !shared.stopping.load(Ordering::Relaxed) {
                let end = (pos + chunk).min(clip.len());
                shared.push(&clip[pos..end], &mut pending, &on_level, &on_end);
                pos = if end >= clip.len() { 0 } else { end };
                std::thread::sleep(Duration::from_millis(50));
            }
        }));
        Ok(())
    }

    fn stop(&mut self) -> Vec<f32> {
        self.shared.stopping.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.shared.take_16k()
    }
}
