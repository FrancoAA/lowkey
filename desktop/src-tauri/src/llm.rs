//! AI notes: a language model writes the meeting notes (summary, decisions, tasks) from your
//! notes and the transcript.
//!
//! Built in: Qwen3.5 4B (Apache-2.0, 2.7 GB) run by llama.cpp's `llama-server`. Both are
//! downloaded once and checked with SHA-256 (pinned llama.cpp release builds: Vulkan on Linux and
//! Windows, Metal on macOS). The server runs in its own process, only while notes are written
//! (and 2 minutes after, for a second try), on 127.0.0.1 with a random key: no other program
//! and no web page can use it.
//!
//! Own server: any OpenAI-compatible API (Ollama, LM Studio, a cloud API). Settings shows a
//! warning when the address is not on this computer.
//!
//! Measured on the development machine: bench/summary/results.md.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};

pub const MODEL_LABEL: &str = "Qwen3.5 4B";
const MODEL_FILE: &str = "Qwen3.5-4B-Q4_K_M.gguf";
const MODEL_URL: &str = "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/Qwen3.5-4B-Q4_K_M.gguf";
const MODEL_SHA256: &str = "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4";
const MODEL_BYTES: u64 = 2_740_937_888;

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

fn model_path() -> PathBuf {
    dir().join(MODEL_FILE)
}

fn runtime_dir() -> PathBuf {
    dir().join(format!("llama-{RELEASE}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct BuiltinStatus {
    /// A llama.cpp build exists for this system.
    pub supported: bool,
    /// The model and the server are on disk.
    pub downloaded: bool,
    pub model: &'static str,
    pub download_mb: u64,
}

pub fn builtin_status() -> BuiltinStatus {
    BuiltinStatus {
        supported: RUNTIME.is_some(),
        downloaded: model_ok_quick(&model_path()) && server_exe(&runtime_dir()).is_some(),
        model: MODEL_LABEL,
        download_mb: (MODEL_BYTES + 37_000_000) / 1_000_000,
    }
}

/// The size is checked at every use; the SHA-256 once after the download (2.7 GB take seconds).
fn model_ok_quick(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() == MODEL_BYTES).unwrap_or(false)
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
pub fn ensure_builtin(progress: ProgressFn) -> anyhow::Result<(PathBuf, PathBuf)> {
    let rt = RUNTIME.as_ref().ok_or_else(|| anyhow::anyhow!("the built-in model is not available for this system; use your own server in Settings → AI notes"))?;
    let rdir = runtime_dir();
    if server_exe(&rdir).is_none() {
        let archive = dir().join(rt.url.rsplit('/').next().unwrap_or("llama.archive"));
        let _ = std::fs::remove_file(&archive);
        crate::models::download(rt.url, &archive, |d, t| progress("the model runner", d, t))?;
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
    let model = model_path();
    if !model_ok_quick(&model) {
        let _ = std::fs::remove_file(&model);
        let part = dir().join(format!("{MODEL_FILE}.download"));
        crate::models::download(MODEL_URL, &part, |d, t| progress(MODEL_LABEL, d, t))?;
        progress("checking the download", 0, 0);
        if crate::models::sha256_file(&part)? != MODEL_SHA256 {
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("the {MODEL_LABEL} download has an unexpected checksum; try again");
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
}

fn runner() -> &'static Runner {
    static R: OnceLock<Runner> = OnceLock::new();
    R.get_or_init(|| {
        // Stop the server after a while without use: it holds about 3 GB.
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(15));
            let r = runner();
            if r.last_used.lock().elapsed() >= IDLE_STOP {
                stop_builtin();
            }
        });
        Runner { running: Mutex::new(None), last_used: Mutex::new(Instant::now()) }
    })
}

/// Stop the built-in server (idle, or the app quits).
pub fn stop_builtin() {
    if let Some(mut r) = runner().running.lock().take() {
        let _ = r.child.kill();
        let _ = r.child.wait();
    }
}

fn random_key() -> String {
    let mut bytes = [0u8; 24];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_err() {
        // Windows: no /dev/urandom. The hasher's per-process random seed, mixed with the time.
        use std::hash::{BuildHasher, Hasher};
        for chunk in bytes.chunks_mut(8) {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
            chunk.copy_from_slice(&h.finish().to_le_bytes()[..chunk.len()]);
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    let (port, key) = (free_port()?, random_key());
    let log = dir().join("llama-server.log");
    let mut cmd = Command::new(exe);
    cmd.args(["-m", &model.to_string_lossy(), "--host", "127.0.0.1", "--port", &port.to_string(), "--api-key", &key]);
    cmd.args(["-c", &CONTEXT.to_string(), "-np", "1", "-fa", "on", "--jinja", "--no-webui"]);
    cmd.args(["-ngl", if gpu { "99" } else { "0" }]);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(std::fs::File::create(&log).map(Stdio::from).unwrap_or_else(|_| Stdio::null()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("could not start the model runner: {e}"))?;
    let base = format!("http://127.0.0.1:{port}/v1");
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).build()?;
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            anyhow::bail!("cancelled");
        }
        if let Ok(Some(status)) = child.try_wait() {
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
            let _ = child.kill();
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
    Builtin { gpu: bool },
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
pub fn is_local(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(v6) = host_port.strip_prefix('[') { v6.split(']').next().unwrap_or("") } else { host_port.split(':').next().unwrap_or("") };
    let host = host.to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
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
    anyhow::ensure!(input.len() <= MAX_INPUT_CHARS, "this meeting is too long for AI notes for now (over about four hours)");
    let (base, key, model, builtin) = match engine {
        Engine::Builtin { gpu } => {
            let (exe, model) = ensure_builtin(&mut *progress)?;
            progress("starting", 0, 0);
            let (base, key) = builtin_endpoint(&exe, &model, *gpu, cancel)?;
            (base, key, MODEL_LABEL.to_string(), true)
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
    let text = read_stream(BufReader::new(resp), on_text, cancel)?;
    if builtin {
        *runner().last_used.lock() = Instant::now();
    }
    anyhow::ensure!(!text.trim().is_empty(), "the model wrote nothing");
    Ok(text.trim().to_string())
}

/// Server-sent events: `data: {json}` lines with `choices[0].delta.content`, then `data: [DONE]`.
/// A server that does not stream answers with one JSON body: that works too.
pub fn read_stream(reader: impl BufRead, on_text: &mut dyn FnMut(&str), cancel: &AtomicBool) -> anyhow::Result<String> {
    let mut text = String::new();
    let mut plain = String::new();
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    for line in reader.lines() {
        if cancel.load(Ordering::SeqCst) {
            anyhow::bail!("cancelled");
        }
        let line = line?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            plain.push_str(&line);
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(err) = v.get("error") {
            anyhow::bail!("the model failed: {}", err["message"].as_str().unwrap_or(&err.to_string()));
        }
        if let Some(piece) = v["choices"][0]["delta"]["content"].as_str() {
            text.push_str(piece);
            if last_emit.elapsed() >= Duration::from_millis(120) {
                last_emit = Instant::now();
                on_text(&text);
            }
        }
    }
    if text.is_empty() && !plain.trim().is_empty() {
        let v: Value = serde_json::from_str(&plain).map_err(|_| anyhow::anyhow!("the server's answer is not an OpenAI-compatible one"))?;
        text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string();
    }
    let text = strip_thinking(&text);
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
    use std::io::Write;

    #[test]
    fn local_addresses() {
        for u in ["http://localhost:11434/v1", "http://127.0.0.1:1234/v1", "http://[::1]:8080/v1", "http://LOCALHOST/v1", "http://user@localhost:1/v1"] {
            assert!(is_local(u), "{u}");
        }
        for u in ["https://api.openai.com/v1", "http://192.168.1.5:11434/v1", "http://localhost.evil.com/v1", "http://127.0.0.1.evil.com/v1x", "https://evil.com/?localhost"] {
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
