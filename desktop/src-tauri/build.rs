fn main() {
    // Parakeet (sherpa-onnx + onnxruntime) is built where a checked prebuilt archive is pinned:
    // Linux x86-64 (desktop/scripts/sherpa-onnx.sh). Elsewhere the app has the Whisper models only.
    println!("cargo::rustc-check-cfg=cfg(parakeet)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    println!("cargo:rerun-if-env-changed=OPENWHISPER_NO_PARAKEET");
    // OPENWHISPER_NO_PARAKEET=1: build without it (to check the other systems' code path here).
    if os == "linux" && arch == "x86_64" && std::env::var_os("OPENWHISPER_NO_PARAKEET").is_none() {
        println!("cargo:rustc-cfg=parakeet");
    }
    tauri_build::build()
}
