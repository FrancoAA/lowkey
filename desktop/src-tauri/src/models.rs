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
    /// whisper.cpp inside this app (GPU through Vulkan/Metal, else CPU).
    WhisperCpp,
    /// faster-whisper (CTranslate2) in a Python helper process run with uv.
    FasterWhisper,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelSpec {
    pub id: &'static str,
    pub engine: EngineKind,
    pub label: &'static str,
    /// whisper.cpp: file in ggerganov/whisper.cpp. faster-whisper: model name.
    pub source: &'static str,
    pub download_mb: u32,
    pub ram_mb: u32,
    /// × real time on the test machine's GPU (None: no GPU path).
    pub speed_gpu: Option<f32>,
    /// × real time on the test machine's CPU.
    pub speed_cpu: f32,
    /// WER on the benchmark clips (lower is better).
    pub wer: f32,
}

pub const CATALOG: &[ModelSpec] = &[
    ModelSpec { id: "large-v3-turbo-q5_0", engine: EngineKind::WhisperCpp, label: "Large v3 Turbo (q5)", source: "ggml-large-v3-turbo-q5_0.bin", download_mb: 547, ram_mb: 900, speed_gpu: Some(10.9), speed_cpu: 1.4, wer: 3.2 },
    ModelSpec { id: "large-v3-turbo", engine: EngineKind::WhisperCpp, label: "Large v3 Turbo", source: "ggml-large-v3-turbo.bin", download_mb: 1549, ram_mb: 1800, speed_gpu: Some(8.9), speed_cpu: 1.3, wer: 3.9 },
    ModelSpec { id: "medium", engine: EngineKind::WhisperCpp, label: "Medium", source: "ggml-medium.bin", download_mb: 1463, ram_mb: 2100, speed_gpu: Some(6.6), speed_cpu: 1.7, wer: 3.2 },
    ModelSpec { id: "small", engine: EngineKind::WhisperCpp, label: "Small · fastest", source: "ggml-small.bin", download_mb: 466, ram_mb: 760, speed_gpu: Some(17.6), speed_cpu: 5.6, wer: 12.7 },
    ModelSpec { id: "fw:large-v3-turbo", engine: EngineKind::FasterWhisper, label: "Large v3 Turbo · faster-whisper", source: "large-v3-turbo", download_mb: 1546, ram_mb: 1700, speed_gpu: None, speed_cpu: 4.0, wer: 3.9 },
    ModelSpec { id: "fw:large-v3", engine: EngineKind::FasterWhisper, label: "Large v3 · faster-whisper (most accurate)", source: "large-v3", download_mb: 2947, ram_mb: 3100, speed_gpu: None, speed_cpu: 2.2, wer: 2.3 },
    ModelSpec { id: "fw:small", engine: EngineKind::FasterWhisper, label: "Small · faster-whisper", source: "small", download_mb: 464, ram_mb: 670, speed_gpu: None, speed_cpu: 8.9, wer: 13.6 },
];

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
    /// faster-whisper helper can run (uv + the Python project are present).
    pub faster_whisper: bool,
}

/// Benchmark-picked default: whisper.cpp large-v3-turbo-q5_0 on a GPU; without a GPU the
/// faster-whisper turbo model (4.0× vs 1.4× on CPU) when the helper can run.
pub fn auto_model(hw: &Hardware) -> &'static str {
    if hw.gpu.is_none() && hw.faster_whisper {
        "fw:large-v3-turbo"
    } else {
        "large-v3-turbo-q5_0"
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
    if dest.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let part = dest.with_extension("part");
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(None)
        .user_agent(concat!("open-whisper/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut last_err = None;
    for attempt in 1..=4u32 {
        match download_once(&client, url, &part, &mut progress) {
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
        let gpu = Hardware { gpu: Some("AMD Radeon 780M".into()), faster_whisper: true };
        assert_eq!(auto_model(&gpu), "large-v3-turbo-q5_0");
        let cpu_fw = Hardware { gpu: None, faster_whisper: true };
        assert_eq!(auto_model(&cpu_fw), "fw:large-v3-turbo");
        let cpu_only = Hardware { gpu: None, faster_whisper: false };
        assert_eq!(auto_model(&cpu_only), "large-v3-turbo-q5_0");
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
