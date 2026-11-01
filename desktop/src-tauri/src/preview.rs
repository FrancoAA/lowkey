//! Live preview while the user talks: Cactus Whistle (a 17 MB speech model that runs on the
//! CPU) streams words into the dictation overlay. The text that is typed still comes from the
//! main model; the preview is only to see that the app hears you.
//!
//! The engine is Cactus's C library ("needle", Apache-2.0), loaded at run time from the shared
//! library inside their Python wheel. The C library has no network code; nothing is sent
//! anywhere. Both files are downloaded once from pinned Hugging Face revisions and checked with
//! SHA-256. Without them (no network, an unsupported system, a load error) dictation works the
//! same, with no preview.
//!
//! Whistle knows 7 languages (`LANGUAGES`). With another language set in Options there is no
//! preview; with "auto" the preview is best effort.

use std::ffi::{c_char, c_int, CString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::audio;

pub const LANGUAGES: &[&str] = &["en", "de", "fr", "es", "it", "nl", "pl"];

/// Feed the engine about one second at a time (its streaming unit).
const PASS_SAMPLES: usize = audio::RATE as usize;
/// The engine writes up to ~4 KB of JSON per second of audio.
const OUT_BYTES: usize = 1 << 18;

struct Pinned {
    url: &'static str,
    sha256: &'static str,
}

const WEIGHTS_FILE: &str = "whistle.cact";
const WEIGHTS: Pinned = Pinned {
    url: "https://huggingface.co/Cactus-Compute/whistle/resolve/741118687e1f008f06d1351a402a731274789790/whistle.cact",
    sha256: "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb",
};

/// The engine library: a wheel to download, the member to take out of it, and its checksum.
struct EngineBuild {
    wheel: Pinned,
    member: &'static str,
    file: &'static str,
    sha256: &'static str,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
const ENGINE: Option<EngineBuild> = Some(EngineBuild {
    wheel: Pinned {
        url: "https://huggingface.co/Cactus-Compute/needle3/resolve/d6c7d6a2e9655505e0f8e23339356dd1705900eb/python/cactus_needle-3.3.3-py3-none-manylinux2014_x86_64.whl",
        sha256: "b909897e11cf4da9566df6370f8a63564693fbd6edeaac0b8678b71865d4dc24",
    },
    member: "needle/libneedle3.so",
    file: "libneedle3.so",
    sha256: "87476ff70e80334d7c766dbf236b72f42876072df22014c6ec8a9b087b82b260",
});

// macOS and Windows builds exist upstream but are not checked on a real machine yet.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
const ENGINE: Option<EngineBuild> = None;

pub fn dir() -> PathBuf {
    crate::models::models_dir().join("whistle")
}

/// Called with the preview text (committed words, then the unconfirmed tail).
pub type TextFn = Arc<dyn Fn(String) + Send + Sync>;
/// Mono samples at the given rate, from the recorder thread.
pub type AudioFn = Arc<dyn Fn(&[f32], u32) + Send + Sync>;

/// One dictation's preview. `audio` feeds it; `finish` asks for the tail; dropping it without
/// `finish` throws the stream away.
pub struct Stream {
    pub audio: AudioFn,
    end: Option<Box<dyn FnOnce(bool) + Send>>,
}

impl Stream {
    pub fn new(audio: AudioFn, end: Box<dyn FnOnce(bool) + Send>) -> Self {
        Self { audio, end: Some(end) }
    }

    pub fn finish(mut self) {
        if let Some(end) = self.end.take() {
            end(true);
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(end) = self.end.take() {
            end(false);
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    /// "off" | "preparing" | "ready" | "unavailable"
    pub state: String,
    pub error: Option<String>,
    /// The engine in use (or being prepared): "whistle" | "parakeet" ("" when off).
    pub engine: String,
}

/// The engine a choice means here: "auto" is Parakeet when its files are on disk (no 670 MB
/// download just for the preview), else Whistle. Parakeet exists only where it is built.
pub fn resolve(choice: &str) -> &'static str {
    let parakeet = crate::models::spec("parakeet-v3");
    match choice {
        "whistle" => "whistle",
        "parakeet" if parakeet.is_some() => "parakeet",
        _ if parakeet.is_some_and(crate::models::present) => "parakeet",
        _ => "whistle",
    }
}

pub const CHOICES: &[&str] = &["auto", "whistle", "parakeet"];

/// One pass of the engine: words committed by this call, and the tail not yet confirmed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Step {
    pub text: String,
    pub pending: String,
}

/// The streaming calls, so the worker can be tested without the real engine.
pub trait Asr {
    fn process(&mut self, pcm: &[f32], language: Option<&str>) -> anyhow::Result<Step>;
    fn stop(&mut self) -> anyhow::Result<Step>;
}

enum Msg {
    Begin { id: u64, language: Option<String>, on_text: TextFn },
    Audio { id: u64, pcm: Vec<f32>, rate: u32 },
    End { id: u64, flush: bool },
}

pub struct Preview {
    tx: Mutex<Option<Sender<Msg>>>,
    status: Arc<Mutex<Status>>,
    next: std::sync::atomic::AtomicU64,
    /// Counts workers: an old worker (engine switched) never writes the status again.
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for Preview {
    fn default() -> Self {
        Self::new()
    }
}

impl Preview {
    pub fn new() -> Self {
        Self {
            tx: Mutex::new(None),
            status: Arc::new(Mutex::new(Status { state: "off".into(), error: None, engine: String::new() })),
            next: std::sync::atomic::AtomicU64::new(1),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// Download (first time) and load the engine for `choice` in the background. Safe to call
    /// again: nothing happens when that engine is ready or on its way; another engine replaces
    /// the running one (a dictation in progress keeps the old one until it ends).
    pub fn prepare(&self, choice: &str, on_change: Arc<dyn Fn() + Send + Sync>) {
        use std::sync::atomic::Ordering;
        let engine = resolve(choice);
        let my_gen = {
            let mut st = self.status.lock();
            if matches!(st.state.as_str(), "preparing" | "ready") && st.engine == engine {
                return;
            }
            *st = Status { state: "preparing".into(), error: None, engine: engine.into() };
            self.generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        on_change();
        let (tx, rx) = mpsc::channel();
        *self.tx.lock() = Some(tx); // the old worker ends when its last sender is gone
        let (status, generation) = (self.status.clone(), self.generation.clone());
        std::thread::Builder::new()
            .name(format!("preview-{engine}"))
            .spawn(move || {
                let current = || generation.load(Ordering::SeqCst) == my_gen;
                // Whistle's engine is process-global and not thread-safe: this thread owns it.
                match open(engine) {
                    Ok(asr) if current() => {
                        *status.lock() = Status { state: "ready".into(), error: None, engine: engine.into() };
                        on_change();
                        run(asr, rx);
                    }
                    Ok(_) => {}
                    Err(e) if current() => {
                        eprintln!("  Live preview ({engine}) not available: {e:#}");
                        *status.lock() = Status { state: "unavailable".into(), error: Some(format!("{e:#}")), engine: engine.into() };
                        on_change();
                    }
                    Err(_) => {}
                }
            })
            .expect("spawn preview thread");
    }

    /// A preview stream for one dictation, or None (not ready, or a language Whistle lacks;
    /// Parakeet finds the language itself).
    pub fn stream(&self, language: Option<&str>, on_text: TextFn) -> Option<Stream> {
        {
            let st = self.status.lock();
            if st.state != "ready" {
                return None;
            }
            if st.engine == "whistle" && language.is_some_and(|l| !LANGUAGES.contains(&l)) {
                return None;
            }
        }
        let tx = self.tx.lock().clone()?;
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tx.send(Msg::Begin { id, language: language.map(String::from), on_text }).ok()?;
        let tx_audio = tx.clone();
        let audio: AudioFn = Arc::new(move |pcm, rate| {
            let _ = tx_audio.send(Msg::Audio { id, pcm: pcm.to_vec(), rate });
        });
        let end = Box::new(move |flush| {
            let _ = tx.send(Msg::End { id, flush });
        });
        Some(Stream::new(audio, end))
    }
}

struct Current {
    id: u64,
    language: Option<String>,
    on_text: TextFn,
    buf: Vec<f32>,
    committed: String,
    opened: bool,
}

fn join(a: &str, b: &str) -> String {
    match (a.trim(), b.trim()) {
        ("", b) => b.to_string(),
        (a, "") => a.to_string(),
        (a, b) => format!("{a} {b}"),
    }
}

/// The worker loop: one stream at a time, newest wins. Audio that queued up during a pass is
/// fed in one call, so the preview never falls far behind the speaker.
fn run(mut engine: Box<dyn Asr>, rx: Receiver<Msg>) {
    let mut cur: Option<Current> = None;
    while let Ok(first) = rx.recv() {
        let mut msgs = vec![first];
        msgs.extend(rx.try_iter());
        for msg in msgs {
            match msg {
                Msg::Begin { id, language, on_text } => {
                    if let Some(old) = cur.take() {
                        if old.opened {
                            let _ = engine.stop();
                        }
                    }
                    cur = Some(Current { id, language, on_text, buf: Vec::new(), committed: String::new(), opened: false });
                }
                Msg::Audio { id, pcm, rate } => {
                    if let Some(c) = cur.as_mut().filter(|c| c.id == id) {
                        c.buf.extend(audio::resample(&pcm, rate, audio::RATE));
                    }
                }
                Msg::End { id, flush } => {
                    let Some(mut c) = cur.take_if(|c| c.id == id) else { continue };
                    if !flush {
                        if c.opened {
                            let _ = engine.stop();
                        }
                        continue;
                    }
                    if !c.buf.is_empty() {
                        pass(engine.as_mut(), &mut c);
                    }
                    if c.opened {
                        match engine.stop() {
                            Ok(step) => {
                                c.committed = join(&c.committed, &step.text);
                                (c.on_text)(c.committed.clone());
                            }
                            Err(e) => eprintln!("  live preview: {e:#}"),
                        }
                    }
                }
            }
        }
        if let Some(c) = cur.as_mut().filter(|c| c.buf.len() >= PASS_SAMPLES) {
            pass(engine.as_mut(), c);
        }
    }
}

fn pass(engine: &mut dyn Asr, c: &mut Current) {
    let pcm = std::mem::take(&mut c.buf);
    c.opened = true;
    match engine.process(&pcm, c.language.as_deref()) {
        Ok(step) => {
            c.committed = join(&c.committed, &step.text);
            (c.on_text)(join(&c.committed, &step.pending));
        }
        Err(e) => eprintln!("  live preview: {e:#}"),
    }
}

// ------------------------------------------------------------------ the C engine
type LoadFn = unsafe extern "C" fn(*const u8, u64) -> c_int;
type ProcessFn = unsafe extern "C" fn(*const f32, c_int, *const c_char, *const c_char, *mut c_char, c_int) -> c_int;
type StopFn = unsafe extern "C" fn(*mut c_char, c_int) -> c_int;
type ErrorFn = unsafe extern "C" fn() -> *const c_char;

pub struct Needle {
    process: ProcessFn,
    stop: StopFn,
    error: ErrorFn,
    out: Vec<u8>,
    _weights: Vec<u8>,
    _lib: libloading::Library,
}

impl Needle {
    pub fn open(lib: &Path, weights: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read(weights)?;
        // SAFETY: a pinned, checksum-verified build of the Cactus engine with this C API
        // (needle.h, needle3 revision d6c7d6a2).
        unsafe {
            let library = libloading::Library::new(lib)?;
            let load: LoadFn = *library.get(b"needle_load\0")?;
            let process: ProcessFn = *library.get(b"needle_stream_transcribe_process\0")?;
            let stop: StopFn = *library.get(b"needle_stream_transcribe_stop\0")?;
            let error: ErrorFn = *library.get(b"needle_last_error\0")?;
            let me = Self { process, stop, error, out: vec![0; OUT_BYTES], _weights: data, _lib: library };
            if load(me._weights.as_ptr(), me._weights.len() as u64) < 0 {
                anyhow::bail!("could not load {}: {}", weights.display(), me.last_error());
            }
            Ok(me)
        }
    }

    fn last_error(&self) -> String {
        // SAFETY: the engine owns the string until its next call; we copy it at once.
        unsafe {
            let p = (self.error)();
            if p.is_null() {
                return "unknown error".into();
            }
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }

    fn result(&self, code: c_int) -> anyhow::Result<Step> {
        if code < 0 {
            anyhow::bail!("{}", self.last_error());
        }
        let end = self.out.iter().position(|&b| b == 0).unwrap_or(self.out.len());
        parse_step(&self.out[..end])
    }
}

pub fn parse_step(json: &[u8]) -> anyhow::Result<Step> {
    let v: serde_json::Value = serde_json::from_slice(json)?;
    Ok(Step {
        text: v["text"].as_str().unwrap_or_default().trim().to_string(),
        pending: v["pending"].as_str().unwrap_or_default().trim().to_string(),
    })
}

impl Asr for Needle {
    fn process(&mut self, pcm: &[f32], language: Option<&str>) -> anyhow::Result<Step> {
        let lang = language.map(CString::new).transpose()?;
        let need = (pcm.len() / audio::RATE as usize + 2) * 4096;
        if need > self.out.len() {
            self.out.resize(need, 0);
        }
        // SAFETY: pointers and lengths are valid for the call; the engine writes at most
        // out.len() bytes and a terminating NUL.
        let code = unsafe {
            (self.process)(
                pcm.as_ptr(),
                pcm.len() as c_int,
                lang.as_ref().map_or(std::ptr::null(), |l| l.as_ptr()),
                std::ptr::null(),
                self.out.as_mut_ptr() as *mut c_char,
                self.out.len() as c_int,
            )
        };
        self.result(code)
    }

    fn stop(&mut self) -> anyhow::Result<Step> {
        // SAFETY: as above.
        let code = unsafe { (self.stop)(self.out.as_mut_ptr() as *mut c_char, self.out.len() as c_int) };
        self.result(code)
    }
}

fn open(engine: &str) -> anyhow::Result<Box<dyn Asr>> {
    match engine {
        #[cfg(parakeet)]
        "parakeet" => Ok(Box::new(crate::preview_parakeet::ParakeetStream::new(crate::engine::parakeet_for_preview()?))),
        _ => Ok(Box::new(open_engine()?)),
    }
}

// ------------------------------------------------------------------ files
fn open_engine() -> anyhow::Result<Needle> {
    let (lib, weights) = ensure_files()?;
    Needle::open(&lib, &weights)
}

/// The engine library and the model, downloaded once and checked with SHA-256 at every start
/// (a corrupted file or a new pin downloads it again). OPENWHISPER_WHISTLE_LIB points at
/// another build of the library (for tests or an unsupported system).
pub fn ensure_files() -> anyhow::Result<(PathBuf, PathBuf)> {
    // Decide what this system can load before any download.
    let lib_override = std::env::var_os("OPENWHISPER_WHISTLE_LIB").map(PathBuf::from);
    if lib_override.is_none() && ENGINE.is_none() {
        anyhow::bail!("no Whistle engine build for this system yet");
    }
    let dir = dir();
    let weights = dir.join(WEIGHTS_FILE);
    if !verified(&weights, WEIGHTS.sha256) {
        download_checked(&WEIGHTS, &weights)?;
    }
    if let Some(p) = lib_override {
        return Ok((p, weights));
    }
    let build = ENGINE.expect("checked above");
    let lib = dir.join(build.file);
    if !verified(&lib, build.sha256) {
        let wheel = dir.join("engine.whl");
        let _ = std::fs::remove_file(&wheel); // a leftover from an interrupted start
        download_checked(&build.wheel, &wheel)?;
        let bytes = zip_member(&std::fs::read(&wheel)?, build.member);
        let _ = std::fs::remove_file(&wheel);
        let bytes = bytes?;
        if sha256_hex(&bytes) != build.sha256 {
            anyhow::bail!("{} in the engine package has an unexpected checksum", build.member);
        }
        let part = lib.with_extension("part");
        std::fs::write(&part, &bytes)?;
        std::fs::rename(&part, &lib)?;
    }
    Ok((lib, weights))
}

/// True when the file is there with the expected checksum; a wrong file is deleted.
fn verified(path: &Path, sha256: &str) -> bool {
    match std::fs::read(path) {
        Ok(bytes) if sha256_hex(&bytes) == sha256 => true,
        Ok(_) => {
            eprintln!("  {} has an unexpected checksum; downloading it again", path.display());
            let _ = std::fs::remove_file(path);
            false
        }
        Err(_) => false,
    }
}

fn download_checked(pin: &Pinned, dest: &Path) -> anyhow::Result<()> {
    crate::models::download(pin.url, dest, |_, _| {})?;
    if !verified(dest, pin.sha256) {
        anyhow::bail!("{} has an unexpected checksum after the download", dest.display());
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// One file out of a zip archive (stored or deflated), found through the central directory.
pub fn zip_member(zip: &[u8], name: &str) -> anyhow::Result<Vec<u8>> {
    let u16_at = |o: usize| -> anyhow::Result<usize> {
        Ok(u16::from_le_bytes(zip.get(o..o + 2).ok_or_else(|| anyhow::anyhow!("zip: truncated"))?.try_into()?) as usize)
    };
    let u32_at = |o: usize| -> anyhow::Result<usize> {
        Ok(u32::from_le_bytes(zip.get(o..o + 4).ok_or_else(|| anyhow::anyhow!("zip: truncated"))?.try_into()?) as usize)
    };
    let eocd = (0..zip.len().saturating_sub(21))
        .rev()
        .take(65_536 + 22)
        .find(|&o| zip[o..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or_else(|| anyhow::anyhow!("zip: no end of central directory"))?;
    let entries = u16_at(eocd + 10)?;
    let mut o = u32_at(eocd + 16)?;
    for _ in 0..entries {
        if u32_at(o)? != 0x0201_4b50 {
            anyhow::bail!("zip: bad central directory");
        }
        let (method, csize, usize_) = (u16_at(o + 10)?, u32_at(o + 20)?, u32_at(o + 24)?);
        let (nlen, elen, clen) = (u16_at(o + 28)?, u16_at(o + 30)?, u16_at(o + 32)?);
        let local = u32_at(o + 42)?;
        let entry = zip.get(o + 46..o + 46 + nlen).ok_or_else(|| anyhow::anyhow!("zip: truncated"))?;
        if entry == name.as_bytes() {
            if u32_at(local)? != 0x0403_4b50 {
                anyhow::bail!("zip: bad local header");
            }
            let start = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
            let data = zip.get(start..start + csize).ok_or_else(|| anyhow::anyhow!("zip: truncated"))?;
            return match method {
                0 => Ok(data.to_vec()),
                8 => {
                    let mut out = Vec::with_capacity(usize_);
                    flate2::read::DeflateDecoder::new(data).take(usize_ as u64 + 1).read_to_end(&mut out)?;
                    anyhow::ensure!(out.len() == usize_, "zip: {name} has the wrong size");
                    Ok(out)
                }
                m => anyhow::bail!("zip: compression method {m} is not supported"),
            };
        }
        o += 46 + nlen + elen + clen;
    }
    anyhow::bail!("zip: {name} not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A zip with one stored and one deflated file, written by hand (no zip writer crate).
    fn make_zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data, deflate) in files {
            let body = if *deflate {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]); // time, date, crc (not checked)
            out.extend((body.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(3u16.to_le_bytes());
            out.extend(name.as_bytes());
            out.extend([1, 2, 3]); // extra field
            out.extend(&body);
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend([20, 0, 20, 0, 0, 0]);
            central.extend(method.to_le_bytes());
            central.extend([0; 8]);
            central.extend((body.len() as u32).to_le_bytes());
            central.extend((data.len() as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0; 12]); // extra, comment, disk, internal and external attributes
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let cd = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd.to_le_bytes());
        out.extend([0; 2]);
        out
    }

    #[test]
    fn zip_member_reads_stored_and_deflated_files() {
        let big: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let zip = make_zip(&[("a.txt", b"hello", false), ("needle/libneedle3.so", &big, true)]);
        assert_eq!(zip_member(&zip, "a.txt").unwrap(), b"hello");
        assert_eq!(zip_member(&zip, "needle/libneedle3.so").unwrap(), big);
        assert!(zip_member(&zip, "missing").is_err());
        assert!(zip_member(&zip[..20], "a.txt").is_err());
    }

    #[test]
    fn checksum_is_lowercase_hex() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn parses_engine_json() {
        let s = parse_step(br#"{"text":" Hello ","words":[],"pending":"from open","language":"en","received":1.0,"pass_ms":20.0}"#).unwrap();
        assert_eq!(s, Step { text: "Hello".into(), pending: "from open".into() });
        assert!(parse_step(b"not json").is_err());
    }

    /// Records calls; each process() commits "w<n>" and leaves "p<n>" pending.
    struct FakeAsr {
        log: Arc<Mutex<Vec<String>>>,
        n: usize,
    }

    impl Asr for FakeAsr {
        fn process(&mut self, pcm: &[f32], language: Option<&str>) -> anyhow::Result<Step> {
            self.n += 1;
            self.log.lock().push(format!("process {} {}", pcm.len(), language.unwrap_or("-")));
            Ok(Step { text: format!("w{}", self.n), pending: format!("p{}", self.n) })
        }
        fn stop(&mut self) -> anyhow::Result<Step> {
            self.log.lock().push("stop".into());
            Ok(Step { text: "tail".into(), pending: String::new() })
        }
    }

    fn ready_preview() -> (Preview, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
        let p = Preview::new();
        let (tx, rx) = mpsc::channel();
        *p.tx.lock() = Some(tx);
        {
            let mut st = p.status.lock();
            st.state = "ready".into();
            st.engine = "whistle".into();
        }
        let log = Arc::new(Mutex::new(vec![]));
        let engine = FakeAsr { log: log.clone(), n: 0 };
        let t = std::thread::spawn(move || run(Box::new(engine), rx));
        (p, log, t)
    }

    fn texts() -> (TextFn, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(vec![]));
        let s = seen.clone();
        (Arc::new(move |t| s.lock().push(t)), seen)
    }

    fn wait_for(seen: &Arc<Mutex<Vec<String>>>, n: usize) {
        for _ in 0..200 {
            if seen.lock().len() >= n {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("got {:?}", seen.lock());
    }

    #[test]
    fn streams_one_second_passes_then_the_tail() {
        let (p, log, t) = ready_preview();
        let (on_text, seen) = texts();
        let s = p.stream(Some("es"), on_text).unwrap();
        // 48 kHz audio in half-second blocks: one pass per second of 16 kHz audio.
        for _ in 0..2 {
            (s.audio)(&vec![0.0; 24_000], 48_000);
        }
        wait_for(&seen, 1);
        (s.audio)(&vec![0.0; 24_000], 48_000);
        s.finish();
        wait_for(&seen, 3);
        assert_eq!(*seen.lock(), vec!["w1 p1", "w1 w2 p2", "w1 w2 tail"]);
        let log = log.lock().clone();
        assert_eq!(log[0], "process 16000 es");
        assert_eq!(log.last().unwrap(), "stop");
        drop(p);
        let _ = t;
    }

    #[test]
    fn a_dropped_stream_is_discarded_and_a_new_one_starts_clean() {
        let (p, log, _t) = ready_preview();
        let (on_text, seen) = texts();
        let s = p.stream(None, on_text).unwrap();
        (s.audio)(&vec![0.0; 16_000], 16_000);
        wait_for(&seen, 1);
        drop(s); // cancelled: the engine stream is stopped, no tail is reported
        let (on_text2, seen2) = texts();
        let s2 = p.stream(Some("en"), on_text2).unwrap();
        (s2.audio)(&vec![0.0; 16_000], 16_000);
        wait_for(&seen2, 1);
        assert_eq!(seen.lock().len(), 1);
        assert_eq!(*seen2.lock(), vec!["w2 p2"]);
        assert_eq!(log.lock().iter().filter(|l| *l == "stop").count(), 1);
    }

    #[test]
    fn a_new_recording_closes_the_open_stream_and_late_audio_is_ignored() {
        let (p, log, _t) = ready_preview();
        let (on_text, seen) = texts();
        let s1 = p.stream(None, on_text).unwrap();
        (s1.audio)(&vec![0.0; 16_000], 16_000);
        wait_for(&seen, 1);
        let late = s1.audio.clone();
        std::mem::forget(s1); // as if its end message were lost: the next Begin must close it
        let (on_text2, seen2) = texts();
        let s2 = p.stream(None, on_text2).unwrap();
        late(&vec![0.0; 16_000], 16_000); // old id: dropped
        (s2.audio)(&vec![0.0; 8_000], 16_000);
        s2.finish();
        wait_for(&seen2, 2);
        assert_eq!(*seen2.lock(), vec!["w2 p2", "w2 tail"]);
        assert_eq!(seen.lock().len(), 1);
        let log = log.lock().clone();
        assert_eq!(log, vec!["process 16000 -", "stop", "process 8000 -", "stop"]);
    }

    #[test]
    fn a_wrong_cached_file_is_deleted() {
        let dir = std::env::temp_dir().join(format!("ow-preview-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("whistle.cact");
        std::fs::write(&f, b"abc").unwrap();
        assert!(verified(&f, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert!(f.exists());
        assert!(!verified(&f, WEIGHTS.sha256)); // truncated or old pin: removed, downloaded again
        assert!(!f.exists());
        assert!(!verified(&f, WEIGHTS.sha256)); // missing
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_stream_when_not_ready_or_language_unsupported() {
        let p = Preview::new();
        let (on_text, _) = texts();
        assert!(p.stream(None, on_text.clone()).is_none());
        let (p, _log, _t) = ready_preview();
        assert!(p.stream(Some("ja"), on_text.clone()).is_none());
        assert!(p.stream(Some("pl"), on_text.clone()).is_some());
        p.status.lock().engine = "parakeet".into(); // finds the language itself: any language
        assert!(p.stream(Some("ja"), on_text).is_some());
    }

    #[test]
    fn engine_choices() {
        assert_eq!(resolve("whistle"), "whistle");
        let built = crate::models::spec("parakeet-v3").is_some();
        assert_eq!(resolve("parakeet"), if built { "parakeet" } else { "whistle" });
        assert!(["whistle", "parakeet"].contains(&resolve("auto")));
        assert!(CHOICES.contains(&"auto"));
    }

    /// The real engine on a real clip. Needs the downloaded files:
    /// `cargo test --lib real_whistle -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_whistle_streams_the_jfk_clip() {
        let (lib, weights) = ensure_files().unwrap();
        let mut engine = Needle::open(&lib, &weights).unwrap();
        let clip = std::env::var("OW_CLIP").unwrap_or_else(|_| "../../bench/.work/samples/en_jfk.wav".into());
        let pcm = audio::decode_file(Path::new(&clip)).unwrap();
        let mut text = String::new();
        let t0 = std::time::Instant::now();
        for chunk in pcm.chunks(PASS_SAMPLES) {
            let s = engine.process(chunk, Some("en")).unwrap();
            text = join(&text, &s.text);
            eprintln!("{:6.0} ms  {text} | {}", t0.elapsed().as_secs_f64() * 1000.0, s.pending);
        }
        text = join(&text, &engine.stop().unwrap().text);
        eprintln!("final: {text}");
        let low = text.to_lowercase();
        assert!(low.contains("ask not what your country can do for you"), "{text}");
    }
}
