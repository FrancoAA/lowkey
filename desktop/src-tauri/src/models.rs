//! Model catalog, automatic default and downloads.
//!
//! Speed and WER figures come from the benchmark on the development machine (Ryzen 7 H 255,
//! Radeon 780M; docs/benchmark.md). They are shown in Options to help choose, not promised.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::config::data_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    /// whisper.cpp (GPU through Vulkan/Metal, else CPU).
    WhisperCpp,
    /// NVIDIA Parakeet TDT through sherpa-onnx (onnxruntime, CPU).
    Parakeet,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSpec {
    pub id: &'static str,
    pub engine: EngineKind,
    pub label: &'static str,
    /// whisper.cpp: the file in ggerganov/whisper.cpp. Parakeet: the folder in the models dir.
    pub source: &'static str,
    pub download_mb: u32,
    pub ram_mb: u32,
    /// × real time on the test machine's GPU (None: CPU only).
    pub speed_gpu: Option<f32>,
    /// × real time on the test machine's CPU.
    pub speed_cpu: f32,
    /// WER on the benchmark clips (lower is better).
    pub wer: f32,
}

pub const CATALOG: &[ModelSpec] = &[
    ModelSpec { engine: EngineKind::WhisperCpp, id: "large-v3-turbo-q5_0", label: "Large v3 Turbo (q5)", source: "ggml-large-v3-turbo-q5_0.bin", download_mb: 547, ram_mb: 900, speed_gpu: Some(10.9), speed_cpu: 1.4, wer: 3.2 },
    ModelSpec { engine: EngineKind::WhisperCpp, id: "large-v3-turbo", label: "Large v3 Turbo", source: "ggml-large-v3-turbo.bin", download_mb: 1549, ram_mb: 1800, speed_gpu: Some(8.9), speed_cpu: 1.3, wer: 3.9 },
    ModelSpec { engine: EngineKind::WhisperCpp, id: "medium", label: "Medium", source: "ggml-medium.bin", download_mb: 1463, ram_mb: 2100, speed_gpu: Some(6.6), speed_cpu: 1.7, wer: 3.2 },
    ModelSpec { engine: EngineKind::WhisperCpp, id: "small", label: "Small", source: "ggml-small.bin", download_mb: 466, ram_mb: 760, speed_gpu: Some(17.6), speed_cpu: 5.6, wer: 12.7 },
    // CPU only. WER on the same 5 clips; one is extreme noise (0 dB + telephone band), where it
    // gets 49%: on the other four it is 0-5.7% (docs/parakeet.md).
    ModelSpec { engine: EngineKind::Parakeet, id: "parakeet-v3", label: "Parakeet v3 · fastest (25 languages)", source: PARAKEET_DIR, download_mb: 640, ram_mb: 1150, speed_gpu: None, speed_cpu: 35.9, wer: 14.3 },
];

/// Parakeet TDT 0.6B v3 (int8, sherpa-onnx export), pinned revision; sizes and SHA-256 per file.
pub const PARAKEET_DIR: &str = "parakeet-tdt-0.6b-v3-int8";
const PARAKEET_URL: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78/";
pub const PARAKEET_FILES: &[(&str, u64, &str)] = &[
    ("encoder.int8.onnx", 652_184_281, "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247"),
    ("decoder.int8.onnx", 11_845_275, "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e"),
    ("joiner.int8.onnx", 6_355_277, "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3"),
    ("tokens.txt", 93_939, "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
];

/// Is the model on disk?
pub fn present(spec: &ModelSpec) -> bool {
    match spec.engine {
        EngineKind::WhisperCpp => local_path(spec.source).exists(),
        EngineKind::Parakeet => PARAKEET_FILES.iter().all(|(f, _, _)| models_dir().join(PARAKEET_DIR).join(f).exists()),
    }
}

/// Download the Parakeet files that are missing, each checked with SHA-256 (a wrong file is
/// deleted). `progress(done, total)` covers all files.
pub fn download_parakeet(mut progress: impl FnMut(u64, u64)) -> anyhow::Result<()> {
    let dir = models_dir().join(PARAKEET_DIR);
    let total: u64 = PARAKEET_FILES.iter().map(|f| f.1).sum();
    let mut before = 0;
    for (file, size, sha) in PARAKEET_FILES {
        let dest = dir.join(file);
        if !dest.exists() {
            download(&format!("{PARAKEET_URL}{file}"), &dest, |done, _| progress(before + done, total))?;
            let got = sha256_file(&dest)?;
            if got != *sha {
                let _ = std::fs::remove_file(&dest);
                anyhow::bail!("{file} has an unexpected checksum ({got})");
            }
        }
        before += size;
        progress(before, total);
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Silero VAD for whisper.cpp (drops silence, avoids "Thank you." on silent audio).
pub const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";
pub const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().find(|m| m.id == id)
}

/// What this machine can run well, decided once at start.
#[derive(Debug, Clone, Serialize)]
pub struct Hardware {
    /// Usable GPU for whisper.cpp (name), if any. Software renderers do not count.
    pub gpu: Option<String>,
}

/// Benchmark-picked default: large-v3-turbo-q5_0 on a GPU (best WER on the test clips, 10.9×
/// real time on the test machine). Without a GPU it runs at only 1.4×, so "small" (5.6× on the
/// CPU, WER 12.7%) is the default there; Options can still pick the accurate one.
pub fn auto_model(hw: &Hardware) -> &'static str {
    if hw.gpu.is_some() {
        "large-v3-turbo-q5_0"
    } else {
        "small"
    }
}

pub fn is_software_gpu(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("llvmpipe") || n.contains("swiftshader") || n.contains("lavapipe") || n.contains("software")
}

pub fn models_dir() -> PathBuf {
    std::env::var_os("OPENWHISPER_MODELS_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir().join("models"))
}

pub fn local_path(file: &str) -> PathBuf {
    models_dir().join(file)
}

pub fn model_url(spec: &ModelSpec) -> String {
    format!("{MODEL_URL}{}", spec.source)
}

/// Download `url` to `dest` with resume, retries for network errors, and byte progress.
/// `progress(done, total)`. The file appears under its final name only when complete.
pub fn download(url: &str, dest: &Path, mut progress: impl FnMut(u64, u64)) -> anyhow::Result<()> {
    // One download per file: the start-up preload and a first job can ask for the same model.
    static ACTIVE: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    static DONE: std::sync::Condvar = std::sync::Condvar::new();
    {
        let mut active = ACTIVE.lock().unwrap();
        while active.contains(&dest.to_path_buf()) {
            active = DONE.wait(active).unwrap();
        }
        if dest.exists() {
            return Ok(());
        }
        active.push(dest.to_path_buf());
    }
    let result = download_locked(url, dest, &mut progress);
    ACTIVE.lock().unwrap().retain(|p| p != dest);
    DONE.notify_all();
    result
}

fn download_locked(url: &str, dest: &Path, progress: &mut impl FnMut(u64, u64)) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let part = dest.with_extension("part");
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(None)
        .user_agent(concat!("open-whisper/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut last_err = None;
    for attempt in 1..=4u32 {
        match download_once(&client, url, &part, progress) {
            Ok(()) => {
                std::fs::rename(&part, dest)?;
                return Ok(());
            }
            Err(DlError::Fatal(e)) => return Err(e),
            Err(DlError::Retry(e)) => {
                eprintln!("  download failed ({e}); retry {attempt}/4");
                last_err = Some(e);
                std::thread::sleep(Duration::from_secs(2 * attempt as u64));
            }
        }
    }
    Err(last_err.unwrap())
}

enum DlError {
    Retry(anyhow::Error),
    Fatal(anyhow::Error),
}

fn download_once(
    client: &reqwest::blocking::Client,
    url: &str,
    part: &Path,
    progress: &mut impl FnMut(u64, u64),
) -> Result<(), DlError> {
    let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut resp = req.send().map_err(|e| DlError::Retry(e.into()))?;
    let status = resp.status();
    if status.is_client_error() && status.as_u16() != 429 && status.as_u16() != 416 {
        return Err(DlError::Fatal(anyhow::anyhow!("{url}: HTTP {status}")));
    }
    if !status.is_success() {
        return Err(DlError::Retry(anyhow::anyhow!("{url}: HTTP {status}")));
    }
    let resumed = status.as_u16() == 206;
    let start = if resumed { have } else { 0 };
    let total = start + resp.content_length().unwrap_or(0);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(part)
        .map_err(|e| DlError::Fatal(e.into()))?;
    let mut done = start;
    progress(done, total);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = resp.read(&mut buf).map_err(|e| DlError::Retry(e.into()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| DlError::Fatal(e.into()))?;
        done += n as u64;
        progress(done, total);
    }
    if total > 0 && done < total {
        return Err(DlError::Retry(anyhow::anyhow!("connection closed at {done} of {total} bytes")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_choice_follows_the_benchmark() {
        assert_eq!(auto_model(&Hardware { gpu: Some("AMD Radeon 780M".into()) }), "large-v3-turbo-q5_0");
        assert_eq!(auto_model(&Hardware { gpu: None }), "small");
        assert!(CATALOG.iter().all(|m| !m.id.starts_with("fw:")), "no Python engine any more");
        assert_eq!(spec("parakeet-v3").unwrap().engine, EngineKind::Parakeet);
        assert_eq!(PARAKEET_FILES.iter().map(|f| f.1).sum::<u64>() / 1_000_000, 670);
    }

    #[test]
    fn catalog_ids_are_unique_and_known() {
        let mut ids: Vec<_> = CATALOG.iter().map(|m| m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        assert!(spec("large-v3-turbo-q5_0").is_some() && spec("nope").is_none());
    }

    #[test]
    fn software_renderers_are_not_gpus() {
        assert!(is_software_gpu("llvmpipe (LLVM 19.1.7, 256 bits)"));
        assert!(!is_software_gpu("AMD Radeon 780M Graphics (RADV PHOENIX)"));
    }
}
