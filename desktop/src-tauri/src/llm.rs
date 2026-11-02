//! AI notes: a language model writes the meeting notes (summary, decisions, tasks) from your
//! notes and the transcript.
//!
//! Built in: Qwen3.5 4B (Apache-2.0, 2.7 GB) run by llama.cpp's `llama-server`. Both are
//! downloaded once and checked with SHA-256 (pinned llama.cpp release builds: Vulkan on Linux and
//! Windows, Metal on macOS). The server runs in its own process, only while notes are written
//! (and 2 minutes after, for a second try), on 127.0.0.1 with a random key given through its
//! environment (not its command line, which other users can read): no web page and no other
//! user can use it. A pid file lets the next start stop a server that a crash left behind.
//!
//! Own server: any OpenAI-compatible API (Ollama, LM Studio, a cloud API). Settings shows a
//! warning when the address is not on this computer.
//!
//! Measured on the development machine: bench/summary/results.md.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};

/// A built-in model for the AI notes: a Q4_K_M GGUF at a pinned Hugging Face revision.
#[derive(Debug, Serialize)]
pub struct Model {
    pub id: &'static str,
    pub label: &'static str,
    #[serde(skip)]
    file: &'static str,
    #[serde(skip)]
    url: &'static str,
    #[serde(skip)]
    sha256: &'static str,
    #[serde(skip)]
    bytes: u64,
    /// What the test in bench/summary showed (Settings shows it).
    pub note: &'static str,
}

pub const DEFAULT_MODEL: &str = "qwen3.5-4b";

/// The three measured in bench/summary/results.md (all Apache-2.0).
pub const MODELS: &[Model] = &[
    Model {
        id: "qwen3.5-4b",
        label: "Qwen3.5 4B",
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/Qwen3.5-4B-Q4_K_M.gguf",
        sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        bytes: 2_740_937_888,
        note: "Recommended. In the test: every task with the right owner. About 25 s for 8 minutes of meeting with a GPU, a minute without.",
    },
    Model {
        id: "qwen3.5-2b",
        label: "Qwen3.5 2B",
        file: "Qwen3.5-2B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/f6d5376be1edb4d416d56da11e5397a961aca8ae/Qwen3.5-2B-Q4_K_M.gguf",
        sha256: "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223",
        bytes: 1_280_835_840,
        note: "The fastest and smallest (about 15 s with a GPU). In the test it gave tasks to the wrong people: check its notes.",
    },
    Model {
        id: "gemma-4-e4b",
        label: "Gemma 4 E4B",
        file: "gemma-4-E4B-it-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/gemma-4-E4B-it-GGUF/resolve/bfc15c382204943c3a8fff0c750b94ae2364d7a3/gemma-4-E4B-it-Q4_K_M.gguf",
        sha256: "85a896a047553e842f25297ee5b031d64ff30147d9c4af17b1e4b394cd1fab87",
        bytes: 4_977_171_584,
        note: "Google. As fast as Qwen3.5 4B and best at open questions and numbers, but it gave the recorder's own task to someone else.",
    },
];

/// The model with this id; an unknown id gives the default.
pub fn model(id: &str) -> &'static Model {
    MODELS.iter().find(|m| m.id == id).unwrap_or(&MODELS[0])
}

/// llama.cpp release b12047 (2026-11-02). SHA-256 as GitHub publishes it for each file.
struct Runtime {
    url: &'static str,
    sha256: &'static str,
}

const RELEASE: &str = "b12047";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUNTIME: Option<Runtime> = Some(Runtime {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12047/llama-b12047-bin-ubuntu-vulkan-x64.tar.gz",
    sha256: "c6816871b44d0b23ae52dd8f38c642cb5edcb3f2e762f38d50131e237dd3b9f5",
});
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const RUNTIME: Option<Runtime> = Some(Runtime {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12047/llama-b12047-bin-win-vulkan-x64.zip",
    sha256: "fc7e24f1dbf0af6050e29a3ee06a7bfcdbeb53f9232a923093d53e0b83848d74",
});
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const RUNTIME: Option<Runtime> = Some(Runtime {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12047/llama-b12047-bin-macos-arm64.tar.gz",
    sha256: "b757b8eedb6f5200036a0d862eb72c8e2e108ba13f298620f167adfb9a37fabc",
});
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const RUNTIME: Option<Runtime> = Some(Runtime {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12047/llama-b12047-bin-macos-x64.tar.gz",
    sha256: "693701c37cc112e2c8251bea7aec84cf2e8262af694d28c74149c748055ae6a0",
});
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64"))
)))]
const RUNTIME: Option<Runtime> = None;

/// Room for about four hours of meeting.
const CONTEXT: u32 = 65_536;
/// Longer input does not fit in `CONTEXT` (about 3.5 characters per token, with room to write).
pub const MAX_INPUT_CHARS: usize = 200_000;
const IDLE_STOP: Duration = Duration::from_secs(120);

pub fn dir() -> PathBuf {
    crate::models::models_dir().join("llm")
}

fn model_path(m: &Model) -> PathBuf {
    dir().join(m.file)
}

fn runtime_dir() -> PathBuf {
    dir().join(format!("llama-{RELEASE}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct BuiltinStatus {
    /// A llama.cpp build exists for this system.
    pub supported: bool,
    /// The chosen model and the server are on disk.
    pub downloaded: bool,
    /// The chosen model.
    pub model: &'static str,
    pub download_mb: u64,
    /// Every built-in model, and whether it is on disk.
    pub models: Vec<ModelStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub download_mb: u64,
    pub downloaded: bool,
}

pub fn builtin_status(chosen: &str) -> BuiltinStatus {
    let runner = server_exe(&runtime_dir()).is_some();
    let m = model(chosen);
    BuiltinStatus {
        supported: RUNTIME.is_some(),
        downloaded: model_ok_quick(m) && runner,
        model: m.label,
        download_mb: (m.bytes + if runner { 0 } else { 37_000_000 }) / 1_000_000,
        models: MODELS
            .iter()
            .map(|x| ModelStatus { id: x.id, label: x.label, note: x.note, download_mb: x.bytes / 1_000_000, downloaded: model_ok_quick(x) })
            .collect(),
    }
}

/// The size is checked at every use; the SHA-256 once after the download (GBs take seconds).
fn model_ok_quick(m: &Model) -> bool {
    std::fs::metadata(model_path(m)).map(|f| f.len() == m.bytes).unwrap_or(false)
}

fn server_exe(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
    find_file(dir, name, 3)
}

fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == name) && p.is_file() {
            return Some(p);
        }
        if depth > 0 && e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            subdirs.push(p);
        }
    }
    subdirs.into_iter().find_map(|d| find_file(&d, name, depth - 1))
}

/// Progress of the built-in model's first use: (what, done bytes, total bytes).
pub type ProgressFn<'a> = &'a mut dyn FnMut(&str, u64, u64);

/// The server and the model, downloaded and checked once. Returns (server, model).
pub fn ensure_builtin(m: &Model, progress: ProgressFn, cancel: &AtomicBool) -> anyhow::Result<(PathBuf, PathBuf)> {
    let rt = RUNTIME.as_ref().ok_or_else(|| anyhow::anyhow!("the built-in model is not available for this system; use your own server in Settings → Models → AI notes"))?;
    let rdir = runtime_dir();
    if server_exe(&rdir).is_none() {
        let archive = dir().join(rt.url.rsplit('/').next().unwrap_or("llama.archive"));
        let _ = std::fs::remove_file(&archive);
        crate::models::download_cancellable(rt.url, &archive, |d, t| progress("the model runner", d, t), cancel)?;
        let sum = crate::models::sha256_file(&archive)?;
        if sum != rt.sha256 {
            let _ = std::fs::remove_file(&archive);
            anyhow::bail!("the model runner download has an unexpected checksum");
        }
        let tmp = dir().join(format!(".llama-{RELEASE}.part"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let r = extract(&archive, &tmp);
        let _ = std::fs::remove_file(&archive);
        r?;
        let _ = std::fs::remove_dir_all(&rdir);
        std::fs::rename(&tmp, &rdir)?;
        server_exe(&rdir).ok_or_else(|| anyhow::anyhow!("llama-server is missing from the model runner download"))?;
    }
    let model = model_path(m);
    if !model_ok_quick(m) {
        let _ = std::fs::remove_file(&model);
        let part = dir().join(format!("{}.download", m.file));
        crate::models::download_cancellable(m.url, &part, |d, t| progress(m.label, d, t), cancel)?;
        progress("checking the download", 0, 0);
        if crate::models::sha256_file(&part)? != m.sha256 {
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("the {} download has an unexpected checksum; try again", m.label);
        }
        std::fs::rename(&part, &model)?;
    }
    let exe = server_exe(&rdir).ok_or_else(|| anyhow::anyhow!("llama-server is missing"))?;
    Ok((exe, model))
}

fn extract(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    let name = archive.to_string_lossy();
    if name.ends_with(".zip") {
        extract_zip(&std::fs::read(archive)?, dest)
    } else {
        let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
        let mut tar = tar::Archive::new(gz);
        tar.set_preserve_permissions(true);
        for entry in tar.entries()? {
            let mut e = entry?;
            // unpack_in refuses paths that leave `dest` (.., absolute paths).
            e.unpack_in(dest)?;
        }
        Ok(())
    }
}

/// Every file of a zip archive (stored or deflated) into `dest`. Paths that would leave `dest`
/// are refused.
pub fn extract_zip(zip: &[u8], dest: &Path) -> anyhow::Result<()> {
    for name in zip_names(zip)? {
        if name.ends_with('/') {
            continue;
        }
        let rel = safe_relative(&name).ok_or_else(|| anyhow::anyhow!("zip: unsafe path {name:?}"))?;
        let out = dest.join(rel);
        std::fs::create_dir_all(out.parent().unwrap_or(dest))?;
        std::fs::write(&out, crate::preview::zip_member(zip, &name)?)?;
    }
    Ok(())
}

fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut p = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            s if s.contains(':') => return None, // C:, alternate streams
            s => p.push(s),
        }
    }
    (!p.as_os_str().is_empty() && !name.starts_with(['/', '\\'])).then_some(p)
}

fn zip_names(zip: &[u8]) -> anyhow::Result<Vec<String>> {
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
    let (entries, mut o) = (u16_at(eocd + 10)?, u32_at(eocd + 16)?);
    let mut names = Vec::with_capacity(entries);
    for _ in 0..entries {
        anyhow::ensure!(u32_at(o)? == 0x0201_4b50, "zip: bad central directory");
        let (nlen, elen, clen) = (u16_at(o + 28)?, u16_at(o + 30)?, u16_at(o + 32)?);
        let n = zip.get(o + 46..o + 46 + nlen).ok_or_else(|| anyhow::anyhow!("zip: truncated"))?;
        names.push(String::from_utf8_lossy(n).into_owned());
        o += 46 + nlen + elen + clen;
    }
    Ok(names)
}

// ------------------------------------------------------------------ the built-in server
struct Running {
    child: Child,
    base: String,
    key: String,
    model: PathBuf,
}

struct Runner {
    running: Mutex<Option<Running>>,
    last_used: Mutex<Instant>,
    /// A job is starting the server or writing: never stopped for being idle then (an hour of
    /// meeting on a CPU takes longer than the idle time).
    in_use: AtomicBool,
}

/// Marks the built-in server as in use until dropped; the idle time starts then.
struct InUse;

impl InUse {
    fn new() -> Self {
        runner().in_use.store(true, Ordering::SeqCst);
        InUse
    }
}

impl Drop for InUse {
    fn drop(&mut self) {
        let r = runner();
        *r.last_used.lock() = Instant::now();
        r.in_use.store(false, Ordering::SeqCst);
    }
}

fn runner() -> &'static Runner {
    static R: OnceLock<Runner> = OnceLock::new();
    R.get_or_init(|| {
        // Stop the server after a while without use: it holds about 3 GB.
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(15));
            stop_if_idle();
        });
        Runner { running: Mutex::new(None), last_used: Mutex::new(Instant::now()), in_use: AtomicBool::new(false) }
    })
}

/// Stop the built-in server (idle, cancel, or the app quits).
pub fn stop_builtin() {
    if let Some(mut r) = runner().running.lock().take() {
        let _ = r.child.kill();
        let _ = r.child.wait();
        let _ = std::fs::remove_file(pid_file());
    }
}

/// The check and the stop under the server's lock: a job that takes the server meanwhile keeps it.
fn stop_if_idle() {
    let r = runner();
    let mut slot = r.running.lock();
    if slot.is_some() && !r.in_use.load(Ordering::SeqCst) && r.last_used.lock().elapsed() >= IDLE_STOP {
        if let Some(mut run) = slot.take() {
            let _ = run.child.kill();
            let _ = run.child.wait();
            let _ = std::fs::remove_file(pid_file());
        }
    }
}

fn pid_file() -> PathBuf {
    dir().join("llama-server.pid")
}

/// A server that an earlier run left behind (a crash, a kill, a logout): stop it. Only a
/// process that is our llama-server is stopped, never another program that got the same pid.
pub fn stop_stale() {
    let Ok(text) = std::fs::read_to_string(pid_file()) else { return };
    let _ = std::fs::remove_file(pid_file());
    let Ok(pid) = text.trim().parse::<u32>() else { return };
    if pid == std::process::id() {
        return;
    }
    #[cfg(target_os = "linux")]
    let ours = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .zip(std::fs::canonicalize(runtime_dir()).ok())
        .is_some_and(|(exe, rt)| exe.starts_with(rt));
    #[cfg(target_os = "macos")]
    let ours = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output().is_ok_and(|o| {
        let comm = String::from_utf8_lossy(&o.stdout).to_string();
        comm.trim_end().ends_with("llama-server") && comm.contains(&*dir().to_string_lossy())
    });
    #[cfg(windows)]
    let ours = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("llama-server.exe"));
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let ours = false;
    if !ours {
        return;
    }
    eprintln!("  Stopping a model runner left by an earlier run (pid {pid})");
    #[cfg(unix)]
    let _ = Command::new("kill").arg(pid.to_string()).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).status();
}

/// 192 bits from the system's random generator (getrandom / BCryptGenRandom).
fn random_key() -> anyhow::Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no random numbers from the system: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// The built-in server's address and key, started if needed (waits until it answers).
fn builtin_endpoint(exe: &Path, model: &Path, gpu: bool, cancel: &AtomicBool) -> anyhow::Result<(String, String)> {
    let r = runner();
    *r.last_used.lock() = Instant::now();
    let mut slot = r.running.lock();
    if let Some(run) = slot.as_mut() {
        if run.model == model && run.child.try_wait().ok().flatten().is_none() {
            return Ok((run.base.clone(), run.key.clone()));
        }
        let _ = run.child.kill();
        let _ = run.child.wait();
        *slot = None;
    }
    stop_stale();
    let (port, key) = (free_port()?, random_key()?);
    let log = dir().join("llama-server.log");
    let mut cmd = Command::new(exe);
    // The key through the environment: /proc/<pid>/environ is private, the command line is not.
    cmd.env("LLAMA_API_KEY", &key);
    cmd.args(["-m", &model.to_string_lossy(), "--host", "127.0.0.1", "--port", &port.to_string()]);
    cmd.args(["-c", &CONTEXT.to_string(), "-np", "1", "-fa", "on", "--jinja", "--no-webui"]);
    cmd.args(["-ngl", if gpu { "99" } else { "0" }]);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(std::fs::File::create(&log).map(Stdio::from).unwrap_or_else(|_| Stdio::null()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("could not start the model runner: {e}"))?;
    let _ = std::fs::write(pid_file(), child.id().to_string());
    let fail = |child: &mut Child| {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(pid_file());
    };
    let base = format!("http://127.0.0.1:{port}/v1");
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).build()?;
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            fail(&mut child);
            anyhow::bail!("cancelled");
        }
        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_file(pid_file());
            let tail = std::fs::read_to_string(&log).unwrap_or_default();
            let last: Vec<&str> = tail.lines().rev().take(3).collect();
            anyhow::bail!("the model runner stopped ({status}): {}", last.into_iter().rev().collect::<Vec<_>>().join(" | "));
        }
        let ok = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .ok()
            .and_then(|r| r.json::<Value>().ok())
            .is_some_and(|v| v["status"] == "ok");
        if ok {
            break;
        }
        if started.elapsed() > Duration::from_secs(180) {
            fail(&mut child);
            anyhow::bail!("the model runner did not start in 3 minutes (log: {})", log.display());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    *slot = Some(Running { child, base: base.clone(), key: key.clone(), model: model.to_path_buf() });
    Ok((base, key))
}

// ------------------------------------------------------------------ writing the notes
/// Where the notes are written.
#[derive(Debug, Clone, PartialEq)]
pub enum Engine {
    /// `model`: an id from MODELS.
    Builtin { gpu: bool, model: String },
    /// An OpenAI-compatible API: base URL (".../v1"), model name, key ("" = none).
    Server { url: String, model: String, key: String },
}

/// The instructions. Measured in bench/summary (the language must be named: with "the
/// language of the meeting", two of three models wrote English notes for a Spanish meeting).
pub fn system_prompt(lang: &str) -> String {
    format!(
        "You write meeting notes from a transcript. \"Me\" is the person who recorded the meeting; \
\"Them\" is everyone else (several people: tell them apart by the names they use). The notes \
written by \"Me\" during the meeting show what mattered to them.

Write in {lang}. Use only facts from the transcript; never invent names, dates or numbers. \
Markdown, with these sections (headings in {lang}):

## Summary
Three to five sentences.

## Decisions
One line per decision.

## Action items
One line per task: **who** — what — when (if said). A task that \"Me\" takes is for **Me**.

## Open questions
Things left without a decision, with the next date if one was set.

## Numbers
Figures that were said (money, dates, metrics), one per line, each with what it is."
    )
}

/// The language's English name ("Spanish") from the text itself, for the instructions.
pub fn language_of(text: &str) -> &'static str {
    whatlang::detect(text).filter(|i| i.confidence() > 0.3).map(|i| i.lang().eng_name()).unwrap_or("the language of the transcript")
}

/// True when the address is on this computer (the transcript does not leave it).
/// Parsed as the HTTP client parses it (WHATWG URL), so the host checked is the host reached.
pub fn is_local(url: &str) -> bool {
    use reqwest::Url;
    match Url::parse(url.trim()).ok().and_then(|u| u.host().map(|h| h.to_owned())) {
        Some(url::Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Write the notes for `input` (the meeting as Markdown). `on_text` gets the text so far, as it
/// grows. `progress` reports the built-in model's first download.
pub fn write_notes(
    engine: &Engine,
    input: &str,
    lang: &str,
    progress: ProgressFn,
    on_text: &mut dyn FnMut(&str),
    cancel: &AtomicBool,
) -> anyhow::Result<String> {
    anyhow::ensure!(input.chars().count() <= MAX_INPUT_CHARS, "this meeting is too long for AI notes for now (over about four hours)");
    let mut _in_use = None;
    let (base, key, model, builtin) = match engine {
        Engine::Builtin { gpu, model: id } => {
            _in_use = Some(InUse::new()); // from the start of the server to the last word
            let spec = model(id);
            let (exe, model) = ensure_builtin(spec, &mut *progress, cancel)?;
            progress("starting", 0, 0);
            let (base, key) = builtin_endpoint(&exe, &model, *gpu, cancel)?;
            (base, key, spec.label.to_string(), true)
        }
        Engine::Server { url, model, key } => (url.trim_end_matches('/').to_string(), key.clone(), model.clone(), false),
    };
    progress("writing", 0, 0);
    let mut body = json!({
        "model": model,
        "messages": [{"role": "system", "content": system_prompt(lang)}, {"role": "user", "content": input}],
        "temperature": 0.2,
        "max_tokens": 2000,
        "stream": true,
    });
    if builtin {
        // Qwen3.5 thinks first by default: not needed for notes, and much slower.
        body["chat_template_kwargs"] = json!({"enable_thinking": false});
    }
    // The request runs on its own thread, so Cancel works at any moment: also while the model
    // reads a long meeting before its first word, or while a server never answers.
    enum Msg {
        Text(String),
        Done(anyhow::Result<String>),
    }
    let (tx, rx) = mpsc::channel::<Msg>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new().name("ai-notes-request".into()).spawn(move || {
        let r = (|| -> anyhow::Result<String> {
            let client = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(10)).timeout(None).build()?;
            let mut req = client.post(format!("{base}/chat/completions")).json(&body);
            if !key.is_empty() {
                req = req.bearer_auth(&key);
            }
            let resp = req.send().map_err(|e| anyhow::anyhow!("could not reach {base}: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text: String = resp.text().unwrap_or_default().chars().take(300).collect();
                anyhow::bail!("{base} answered {status}: {text}");
            }
            let tx2 = tx.clone();
            read_stream(BufReader::new(resp), &mut |t| drop(tx2.send(Msg::Text(t.to_string()))), &stop2)
        })();
        let _ = tx.send(Msg::Done(r));
    })?;
    loop {
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(Msg::Text(t)) => on_text(&t),
            Ok(Msg::Done(r)) => {
                let text = r?;
                anyhow::ensure!(!text.trim().is_empty(), "the model wrote nothing");
                return Ok(text.trim().to_string());
            }
            Err(mpsc::RecvTimeoutError::Timeout) if cancel.load(Ordering::SeqCst) => {
                stop.store(true, Ordering::SeqCst);
                if builtin {
                    stop_builtin(); // ends its stream at once and frees the memory
                }
                anyhow::bail!("cancelled");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("the request ended without an answer"),
        }
    }
}

/// Server-sent events: `data: {json}` lines with `choices[0].delta.content`, then `data: [DONE]`.
/// A server that does not stream answers with one JSON body: that works too.
pub fn read_stream(reader: impl BufRead, on_text: &mut dyn FnMut(&str), cancel: &AtomicBool) -> anyhow::Result<String> {
    let mut text = String::new();
    let mut plain = String::new();
    let mut event: Vec<String> = Vec::new(); // the data lines of one event
    let mut cut = false;
    let mut done = false;
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    // One event: its data lines joined with "\n" (SSE allows a value over several lines).
    let mut dispatch = |data: &str, text: &mut String, cut: &mut bool| -> anyhow::Result<bool> {
        if data == "[DONE]" {
            return Ok(true);
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(false) };
        if let Some(err) = v.get("error") {
            anyhow::bail!("the model failed: {}", err["message"].as_str().unwrap_or(&err.to_string()));
        }
        if let Some(piece) = v["choices"][0]["delta"]["content"].as_str() {
            text.push_str(piece);
            if last_emit.elapsed() >= Duration::from_millis(120) {
                last_emit = Instant::now();
                on_text(text);
            }
        }
        if v["choices"][0]["finish_reason"] == "length" {
            *cut = true;
        }
        Ok(false)
    };
    for line in reader.lines() {
        if cancel.load(Ordering::SeqCst) {
            anyhow::bail!("cancelled");
        }
        let line = line?;
        if line.is_empty() {
            if !event.is_empty() && dispatch(&event.join("\n"), &mut text, &mut cut)? {
                done = true;
                break;
            }
            event.clear();
        } else if let Some(data) = line.strip_prefix("data:") {
            event.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        } else if line.starts_with(':') || line.starts_with("event:") || line.starts_with("id:") || line.starts_with("retry:") {
            // a keep-alive comment or another SSE field
        } else {
            plain.push_str(&line);
            plain.push('\n');
        }
    }
    if !done && !event.is_empty() {
        dispatch(&event.join("\n"), &mut text, &mut cut)?;
    }
    if text.is_empty() && !plain.trim().is_empty() {
        let v: Value = serde_json::from_str(&plain).map_err(|_| anyhow::anyhow!("the server's answer is not an OpenAI-compatible one"))?;
        text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string();
        cut = v["choices"][0]["finish_reason"] == "length";
    }
    let mut text = strip_thinking(&text);
    if cut {
        text.push_str("\n\n_(The model stopped at its length limit: the end may be missing.)_");
    }
    on_text(&text);
    Ok(text)
}

/// Some models (and servers) put their reasoning in <think>…</think> before the answer.
fn strip_thinking(text: &str) -> String {
    match (text.find("<think>"), text.find("</think>")) {
        (Some(a), Some(b)) if a < b => format!("{}{}", &text[..a], &text[b + "</think>".len()..]).trim().to_string(),
        _ => text.to_string(),
    }
}

/// One AI-notes job at a time in the app.
pub struct Busy(Arc<AtomicBool>);

impl Busy {
    pub fn take(flag: &Arc<AtomicBool>) -> Option<Self> {
        (!flag.swap(true, Ordering::SeqCst)).then(|| Self(flag.clone()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn local_addresses() {
        for u in ["http://localhost:11434/v1", "http://127.0.0.1:1234/v1", "http://[::1]:8080/v1", "http://LOCALHOST/v1", "http://user@localhost:1/v1"] {
            assert!(is_local(u), "{u}");
        }
        for u in ["https://api.openai.com/v1", "http://192.168.1.5:11434/v1", "http://localhost.evil.com/v1", "http://127.0.0.1.evil.com/v1x", "https://evil.com/?localhost", "http://evil.com\\@localhost/v1", "not a url"] {
            assert!(!is_local(u), "{u}");
        }
    }

    #[test]
    fn the_stream_is_read_as_it_comes() {
        let sse = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"## Resumen\\n\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"Todo bien.\"}}]}\n\n\
                   data: [DONE]\n\n";
        let mut seen = Vec::new();
        let text = read_stream(sse.as_bytes(), &mut |t| seen.push(t.to_string()), &AtomicBool::new(false)).unwrap();
        assert_eq!(text, "## Resumen\nTodo bien.");
        assert_eq!(seen.last().map(String::as_str), Some("## Resumen\nTodo bien."));
        // A server that does not stream; reasoning before the answer is dropped.
        let plain = "{\"choices\":[{\"message\":{\"content\":\"<think>hmm</think>\\n## Summary\\nOk.\"}}]}";
        assert_eq!(read_stream(plain.as_bytes(), &mut |_| {}, &AtomicBool::new(false)).unwrap(), "## Summary\nOk.");
        // A value over two data lines, a keep-alive comment, CRLF, a cut answer.
        let multi = ": ping\r\ndata: {\"choices\":[{\"delta\":\r\ndata: {\"content\":\"Hola\"}}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\r\n\r\n";
        let t = read_stream(multi.as_bytes(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert!(t.starts_with("Hola") && t.contains("length limit"), "{t}");
        let err = "data: {\"error\":{\"message\":\"context too long\"}}\n";
        assert!(read_stream(err.as_bytes(), &mut |_| {}, &AtomicBool::new(false)).unwrap_err().to_string().contains("context too long"));
        assert!(read_stream(sse.as_bytes(), &mut |_| {}, &AtomicBool::new(true)).is_err(), "cancel stops it");
    }

    /// A tiny OpenAI-compatible server on a local port: checks the request, streams an answer.
    #[test]
    fn own_server_gets_the_instructions_the_key_and_the_model() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/v1/", listener.local_addr().unwrap().port());
        let got = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = s.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len: usize = text.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if buf.len() >= i + 4 + len {
                        break;
                    }
                }
            }
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"## Summary\\nDone.\"}}]}\n\ndata: [DONE]\n\n";
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8_lossy(&buf).to_string()
        });
        let engine = Engine::Server { url, model: "qwen3.5:4b".into(), key: "sk-test".into() };
        let text = write_notes(&engine, "**Them** (00:01): Hola.", "Spanish", &mut |_, _, _| {}, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(text, "## Summary\nDone.");
        let req = got.join().unwrap();
        assert!(req.starts_with("POST /v1/chat/completions "), "{req}");
        assert!(req.to_ascii_lowercase().contains("authorization: bearer sk-test"));
        assert!(req.contains("\"model\":\"qwen3.5:4b\"") && req.contains("Write in Spanish") && req.contains("Hola."));
        assert!(!req.contains("chat_template_kwargs"), "only the built-in server gets llama.cpp options");
    }

    /// The first use for real: the runner is downloaded from GitHub (36 MB), checked, unpacked,
    /// started with its key, and writes Spanish notes. The model comes from the bench cache
    /// (bench/.work/llm), linked into a temporary models folder. Run with --ignored.
    #[test]
    fn a_zip_is_unpacked_inside_its_folder_and_a_bad_path_is_refused() {
        // A stored zip, written by hand: one file in a folder.
        fn zip(name: &str, data: &[u8]) -> Vec<u8> {
            let (mut out, mut central) = (Vec::new(), Vec::new());
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(data);
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend((data.len() as u32).to_le_bytes());
            central.extend((data.len() as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0; 12]);
            central.extend(0u32.to_le_bytes());
            central.extend(name.as_bytes());
            let cd_at = out.len() as u32;
            out.extend(&central);
            out.extend(0x0605_4b50u32.to_le_bytes());
            out.extend([0, 0, 0, 0, 1, 0, 1, 0]);
            out.extend((central.len() as u32).to_le_bytes());
            out.extend(cd_at.to_le_bytes());
            out.extend([0, 0]);
            out
        }
        let dir = std::env::temp_dir().join(format!("ow-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        extract_zip(&zip("llama/llama-server.exe", b"MZ"), &dir).unwrap();
        assert_eq!(std::fs::read(dir.join("llama").join("llama-server.exe")).unwrap(), b"MZ");
        assert_eq!(server_exe(&dir).is_some(), cfg!(windows));
        assert!(extract_zip(&zip("../evil.dll", b"x"), &dir).is_err());
        assert!(!dir.parent().unwrap().join("evil.dll").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore]
    #[cfg(unix)]
    fn real_builtin_first_use_downloads_the_runner_and_writes_notes() {
        let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/.work/llm").canonicalize().expect("cached model");
        let tmp = std::env::temp_dir().join(format!("ow-llm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("llm")).unwrap();
        let m = model(DEFAULT_MODEL);
        std::os::unix::fs::symlink(bench.join(m.file), tmp.join("llm").join(m.file)).unwrap();
        std::env::set_var("OPENWHISPER_MODELS_DIR", &tmp);
        assert!(!builtin_status(DEFAULT_MODEL).downloaded);
        let mut steps = Vec::new();
        let input = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/summary/meeting-es.md")).unwrap();
        let text = write_notes(&Engine::Builtin { gpu: true, model: DEFAULT_MODEL.into() }, &input, "Spanish", &mut |w, _, _| steps.push(w.to_string()), &mut |_| {}, &AtomicBool::new(false));
        stop_builtin();
        let text = text.unwrap();
        assert!(steps.iter().any(|s| s == "the model runner"), "{steps:?}");
        assert!(builtin_status(DEFAULT_MODEL).downloaded);
        assert!(text.contains("Martín") && text.contains("14"), "{text}");
        eprintln!("{text}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_model_catalog_is_pinned_and_the_default_comes_first() {
        assert_eq!(MODELS[0].id, DEFAULT_MODEL);
        assert_eq!(model("nope").id, DEFAULT_MODEL, "an unknown id gives the default");
        for m in MODELS {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(m.url.contains("/resolve/") && m.url.ends_with(m.file), "{}: a pinned revision, the file", m.id);
            let rev = m.url.split("/resolve/").nth(1).unwrap().split('/').next().unwrap();
            assert_eq!(rev.len(), 40, "{}: a full commit, not a branch", m.id);
        }
        let st = builtin_status("qwen3.5-2b");
        assert_eq!((st.model, st.models.len()), ("Qwen3.5 2B", 3));
    }

    #[test]
    fn the_language_is_named_and_zip_paths_stay_inside() {
        assert_eq!(language_of("Hola a todos, hoy vamos a ver la fecha de la versión y lo que falta para el lanzamiento."), "Spanish");
        assert_eq!(language_of("Hello everyone, today we look at the release date and what is left before the launch."), "English");
        assert_eq!(safe_relative("llama/bin/llama-server.exe"), Some(PathBuf::from("llama/bin/llama-server.exe")));
        for bad in ["../x", "/etc/passwd", "a/../../b", "C:\\x", "\\\\server\\x", ""] {
            assert_eq!(safe_relative(bad), None, "{bad}");
        }
        assert!(system_prompt("Spanish").contains("Write in Spanish"));
    }
}
