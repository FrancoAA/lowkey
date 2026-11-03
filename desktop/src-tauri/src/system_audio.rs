//! The computer's sound (what the others say in a call), as 16 kHz mono.
//!
//! Linux: the monitor of the default output, through PulseAudio's "simple" API. PipeWire serves
//! the same API (pipewire-pulse). The library is loaded at run time, so the app starts and
//! builds without it; with no library, meetings record the microphone only.
//! macOS and Windows: not built yet.
//!
//! OPENWHISPER_FAKE_SYSTEM=<audio file> replaces the sound with that file played in real time,
//! looped. It exists for automated tests only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::audio;
use crate::mic::EndFn;

/// Gets 16 kHz mono blocks of about 100 ms.
pub type BlockFn = Arc<dyn Fn(&[f32]) + Send + Sync>;

pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            // A read returns within about 100 ms. Never hang the caller on a stuck server:
            // after a second, the thread ends by itself when its read returns.
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while !t.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start recording the computer's sound. Err: why it cannot (the meeting then records the mic only).
pub fn start(on_block: BlockFn, on_end: EndFn) -> anyhow::Result<Capture> {
    if let Some(path) = std::env::var_os("OPENWHISPER_FAKE_SYSTEM") {
        return fake(path.into(), on_block);
    }
    platform::record(None, "Meeting: computer sound", on_block, on_end)
}

/// Any PulseAudio/PipeWire source by name (a microphone), as 16 kHz mono blocks of 100 ms.
/// Linux only; elsewhere an error.
pub fn record_source(source: &str, stream_name: &str, on_block: BlockFn, on_end: EndFn) -> anyhow::Result<Capture> {
    platform::record(Some(source), stream_name, on_block, on_end)
}

/// The library is there (Linux with PipeWire or PulseAudio).
pub fn available() -> bool {
    platform::available()
}

fn fake(path: std::path::PathBuf, on_block: BlockFn) -> anyhow::Result<Capture> {
    let clip = audio::decode_file(&path).map_err(|e| anyhow::anyhow!("{e}"))?;
    anyhow::ensure!(!clip.is_empty(), "empty test sound");
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::spawn(move || {
        let block = audio::RATE as usize / 10;
        let mut pos = 0usize;
        while !s.load(Ordering::SeqCst) {
            let end = (pos + block).min(clip.len());
            on_block(&clip[pos..end]);
            pos = if end >= clip.len() { 0 } else { end };
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    Ok(Capture { stop, thread: Some(thread) })
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::ffi::{c_char, c_int, c_void, CStr};

    #[repr(C)]
    struct SampleSpec {
        format: c_int,
        rate: u32,
        channels: u8,
    }

    #[repr(C)]
    struct BufferAttr {
        maxlength: u32,
        tlength: u32,
        prebuf: u32,
        minreq: u32,
        fragsize: u32,
    }

    const PA_STREAM_RECORD: c_int = 2;
    const PA_SAMPLE_FLOAT32LE: c_int = 5;
    /// The monitor of the default output: what the speakers or headphones play.
    const DEVICE: &CStr = c"@DEFAULT_MONITOR@";

    type NewFn = unsafe extern "C" fn(
        server: *const c_char,
        name: *const c_char,
        dir: c_int,
        dev: *const c_char,
        stream_name: *const c_char,
        ss: *const SampleSpec,
        map: *const c_void,
        attr: *const BufferAttr,
        error: *mut c_int,
    ) -> *mut c_void;
    type ReadFn = unsafe extern "C" fn(s: *mut c_void, data: *mut c_void, bytes: usize, error: *mut c_int) -> c_int;
    type FreeFn = unsafe extern "C" fn(s: *mut c_void);
    type StrErrorFn = unsafe extern "C" fn(error: c_int) -> *const c_char;

    struct Api {
        _lib: libloading::Library,
        new: NewFn,
        read: ReadFn,
        free: FreeFn,
        strerror: Option<StrErrorFn>,
    }

    impl Api {
        fn load() -> anyhow::Result<Self> {
            // SAFETY: a system library with a stable C API; the symbols get their documented types.
            unsafe {
                let lib = libloading::Library::new("libpulse-simple.so.0")
                    .map_err(|_| anyhow::anyhow!("recording the computer's sound needs PipeWire or PulseAudio (libpulse-simple.so.0 not found)"))?;
                let new = *lib.get::<NewFn>(b"pa_simple_new\0")?;
                let read = *lib.get::<ReadFn>(b"pa_simple_read\0")?;
                let free = *lib.get::<FreeFn>(b"pa_simple_free\0")?;
                // From libpulse, which libpulse-simple loads.
                let strerror = lib.get::<StrErrorFn>(b"pa_strerror\0").ok().map(|f| *f);
                Ok(Self { _lib: lib, new, read, free, strerror })
            }
        }

        fn error(&self, code: c_int) -> String {
            match self.strerror {
                // SAFETY: pa_strerror returns a static string (or NULL).
                Some(f) => unsafe {
                    let p = f(code);
                    if p.is_null() { format!("error {code}") } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
                },
                None => format!("error {code}"),
            }
        }
    }

    pub fn available() -> bool {
        static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *OK.get_or_init(|| Api::load().is_ok())
    }

    /// `source`: a source name; None = the monitor of the default output.
    pub fn record(source: Option<&str>, stream_name: &str, on_block: BlockFn, on_end: EndFn) -> anyhow::Result<Capture> {
        let device = match source {
            Some(s) => std::ffi::CString::new(s).map_err(|_| anyhow::anyhow!("bad source name"))?,
            None => DEVICE.to_owned(),
        };
        let stream = std::ffi::CString::new(stream_name).unwrap_or_else(|_| c"open-whisper".to_owned());
        let is_mic = source.is_some();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
        let s = stop.clone();
        // The stream is opened, read and freed on this one thread.
        let thread = std::thread::Builder::new().name("system-audio".into()).spawn(move || {
            let api = match Api::load() {
                Ok(a) => a,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let spec = SampleSpec { format: PA_SAMPLE_FLOAT32LE, rate: audio::RATE, channels: 1 };
            let block = audio::RATE as usize / 10; // 100 ms
            let attr = BufferAttr { maxlength: u32::MAX, tlength: u32::MAX, prebuf: u32::MAX, minreq: u32::MAX, fragsize: (block * 4) as u32 };
            let mut err: c_int = 0;
            // SAFETY: valid C strings and structs that live through the call.
            let pa = unsafe {
                (api.new)(
                    std::ptr::null(),
                    c"open-whisper".as_ptr(),
                    PA_STREAM_RECORD,
                    device.as_ptr(),
                    stream.as_ptr(),
                    &spec,
                    std::ptr::null(),
                    &attr,
                    &mut err,
                )
            };
            if pa.is_null() {
                let what = if is_mic { "the microphone" } else { "the computer's sound" };
                let _ = ready_tx.send(Err(anyhow::anyhow!("could not record {what}: {}", api.error(err))));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            let mut buf = vec![0f32; block];
            while !s.load(Ordering::SeqCst) {
                // SAFETY: buf has room for `block` f32 samples, the size we ask for.
                let r = unsafe { (api.read)(pa, buf.as_mut_ptr().cast(), block * 4, &mut err) };
                if r < 0 {
                    if !s.load(Ordering::SeqCst) {
                        on_end(format!("failed: {}", api.error(err)));
                    }
                    break;
                }
                on_block(&buf);
            }
            // SAFETY: the stream from pa_simple_new, freed once.
            unsafe { (api.free)(pa) };
        })?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Capture { stop, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err(anyhow::anyhow!("the recording did not start"))
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub fn available() -> bool {
        false
    }

    pub fn record(_source: Option<&str>, _stream_name: &str, _on_block: BlockFn, _on_end: EndFn) -> anyhow::Result<Capture> {
        anyhow::bail!("recording the computer's sound is not built for this system yet")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    /// Opens the real monitor for one second and reads it: checks the binding against the real
    /// library. Nothing is played and nothing is kept. Run with --ignored on a desktop.
    #[test]
    #[ignore]
    fn the_real_monitor_gives_16_khz_blocks() {
        let got: Arc<Mutex<usize>> = Arc::default();
        let g = got.clone();
        let mut cap = start(Arc::new(move |b: &[f32]| *g.lock() += b.len()), Arc::new(|r| panic!("{r}"))).expect("monitor");
        std::thread::sleep(Duration::from_millis(1000));
        cap.stop();
        let n = *got.lock();
        assert!((12_000..=20_000).contains(&n), "{n} samples in one second");
    }
}
