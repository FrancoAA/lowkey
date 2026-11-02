fn main() {
    // Parakeet (sherpa-onnx + onnxruntime) is built where a checked prebuilt archive is pinned:
    // Linux x86-64 with glibc (desktop/scripts/sherpa-onnx.sh); not musl. Elsewhere the app has the Whisper models only.
    println!("cargo::rustc-check-cfg=cfg(parakeet)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default(); // glibc archive: not musl
    println!("cargo:rerun-if-env-changed=OPENWHISPER_NO_PARAKEET");
    // OPENWHISPER_NO_PARAKEET=1: build without it (to check the other systems' code path here).
    if os == "linux" && arch == "x86_64" && env == "gnu" && std::env::var_os("OPENWHISPER_NO_PARAKEET").is_none() {
        println!("cargo:rustc-cfg=parakeet");
    }
    tauri_build::build()
}
