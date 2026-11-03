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
    // macOS: whisper.cpp's Metal code uses @available(), which needs __isPlatformVersionAtLeast
    // from clang's runtime (libclang_rt.osx.a); rustc does not link it by itself.
    if os == "macos" {
        if let Ok(out) = std::process::Command::new("clang").arg("--print-resource-dir").output() {
            let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !dir.is_empty() {
                println!("cargo:rustc-link-search=native={dir}/lib/darwin");
                println!("cargo:rustc-link-lib=static=clang_rt.osx");
            }
        }
    }
    // Windows (MSVC): Microsoft's static C runtime libraries point to .pdb debug files that are
    // not shipped; the linker warns once per object (LNK4099). Nothing is missing at run time.
    if env == "msvc" {
        println!("cargo:rustc-link-arg=/ignore:4099");
    }
    tauri_build::build()
}
