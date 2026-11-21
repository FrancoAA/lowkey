//! The GPU that whisper.cpp uses: Vulkan on Linux and Windows, Metal on Apple Silicon.
//!
//! The GPU is found through ggml's device registry. The registry starts Vulkan inside a
//! try/catch: with no Vulkan driver, no GPU or a Vulkan older than 1.2 it has the CPU only, and
//! the app runs on the CPU. (`whisper_rs::vulkan::list_devices` starts Vulkan with no catch: a
//! computer with the Vulkan loader but no driver aborted there.)
//!
//! Windows (the `windows-gpu` feature, on in the releases): `vulkan-1.dll`, the Vulkan loader,
//! comes with the GPU drivers, not with Windows. A PC without it (a virtual machine, the basic
//! display driver) must still start the app. So the program loads `vulkan-1.dll` only when ggml
//! first asks for it (/DELAYLOAD in build.rs), and when it is missing, `delay_load` gives ggml a
//! Vulkan that says "not available" (VK_ERROR_INITIALIZATION_FAILED). ggml catches that, as on a
//! computer without a GPU, and the app runs on the CPU.
//!
//! Safe mode (`safe`): a GPU driver can still stop the app. So before the app first uses a GPU
//! (each app version), a second run of the app (`lowkey --gpu-check`) loads the speech model
//! on it and transcribes two seconds; when that run stops or hangs, only it is lost, and the
//! app uses the CPU. And when the app stops while the speech model runs on the GPU, the next
//! start uses the CPU. The page shows why, with "Try the GPU again".

use crate::models;

/// The first GPU in ggml's registry that is not a software one (llvmpipe): its name.
#[cfg(any(target_os = "linux", all(windows, feature = "windows-gpu")))]
pub fn name() -> Option<String> {
    registry_gpus().into_iter().find(|n| !models::is_software_gpu(n))
}

/// Windows without the `windows-gpu` feature (a cross build here): whisper.cpp is built for the CPU.
#[cfg(all(windows, not(feature = "windows-gpu")))]
pub fn name() -> Option<String> {
    None
}

/// Apple Silicon always has a Metal GPU; Intel Macs run on the CPU.
#[cfg(target_os = "macos")]
pub fn name() -> Option<String> {
    cfg!(target_arch = "aarch64").then(|| "Apple GPU (Metal)".to_string())
}

/// The names of the GPUs in ggml's registry (its CPU is not one).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn registry_gpus() -> Vec<String> {
    use whisper_rs::whisper_rs_sys as sys;
    let mut out = Vec::new();
    unsafe {
        for i in 0..sys::ggml_backend_dev_count() {
            let dev = sys::ggml_backend_dev_get(i);
            if dev.is_null() {
                continue;
            }
            let kind = sys::ggml_backend_dev_type(dev);
            if kind != sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU && kind != sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU {
                continue;
            }
            let desc = sys::ggml_backend_dev_description(dev);
            if !desc.is_null() {
                out.push(std::ffi::CStr::from_ptr(desc).to_string_lossy().into_owned());
            }
        }
    }
    out
}

/// Safe mode for this run: the GPU is not used. On Windows vulkan-1.dll is not even loaded (a
/// broken driver can stop the app when Vulkan starts). Before the first call into ggml.
pub fn disable() {
    #[cfg(all(windows, feature = "windows-gpu"))]
    delay_load::DISABLED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// `lowkey --devices`: what the speech engine can use here.
pub fn describe() -> String {
    let all = registry_gpus();
    let mut s = match name() {
        Some(g) => format!("Speech engine: GPU ({g})"),
        None => "Speech engine: CPU (no GPU that whisper.cpp can use)".to_string(),
    };
    for g in all.iter().filter(|g| models::is_software_gpu(g)) {
        s += &format!("\nNot used (software): {g}");
    }
    #[cfg(all(windows, feature = "windows-gpu"))]
    if delay_load::skipped() {
        s += if std::env::var_os("OPENWHISPER_NO_GPU").is_some() {
            "\nvulkan-1.dll (the Vulkan loader) not loaded: OPENWHISPER_NO_GPU is set."
        } else {
            "\nvulkan-1.dll (the Vulkan loader) not loaded: safe mode (the GPU failed before)."
        };
    } else if delay_load::loader_missing() {
        s += "\nvulkan-1.dll (the Vulkan loader) is not on this PC: a GPU driver installs it.";
    }
    s
}

#[cfg(all(windows, feature = "windows-gpu"))]
pub mod delay_load {
    //! The hooks of Microsoft's delay-load helper (delayimp.lib, delayimp.h): for a DLL that
    //! cannot be loaded or a function that is not in it (`__pfnDliFailureHook2`), and before a
    //! DLL loads (`__pfnDliNotifyHook2`: with OPENWHISPER_NO_GPU set, vulkan-1.dll is not loaded
    //! at all; a way around a broken GPU driver, and the CI's test of this path). Only
    //! vulkan-1.dll is handled; for any other DLL the hooks return null, and nothing changes.
    //!
    //! With vulkan-1.dll there, ggml gets `vkGetInstanceProcAddr` through
    //! `checked_get_instance_proc_addr`: ggml calls the matrix-core queries that it asks for
    //! there without a check for null, and a driver or a Vulkan layer (an overlay) that does not
    //! have one stopped the app with a call to address 0.

    use std::ffi::{c_char, c_void, CStr};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static MISSING: AtomicBool = AtomicBool::new(false);
    static SKIPPED: AtomicBool = AtomicBool::new(false);
    /// Safe mode (`super::disable`).
    pub(super) static DISABLED: AtomicBool = AtomicBool::new(false);

    /// vulkan-1.dll could not be loaded in this run.
    pub fn loader_missing() -> bool {
        MISSING.load(Ordering::Relaxed)
    }

    /// vulkan-1.dll was not loaded on purpose (OPENWHISPER_NO_GPU).
    pub fn skipped() -> bool {
        SKIPPED.load(Ordering::Relaxed)
    }

    const DLI_NOTE_PRE_LOAD_LIBRARY: u32 = 1;
    const DLI_NOTE_PRE_GET_PROC_ADDRESS: u32 = 2;

    const DLI_FAIL_LOAD_LIB: u32 = 3;
    const DLI_FAIL_GET_PROC: u32 = 4;
    /// VK_ERROR_INITIALIZATION_FAILED: vulkan.hpp throws vk::InitializationFailedError, which
    /// ggml catches.
    const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;

    /// delayimp.h `DelayLoadProc`: a name, or an ordinal (in the same place).
    #[repr(C)]
    struct DelayLoadProc {
        import_by_name: i32,
        name: *const c_char,
    }

    /// delayimp.h `DelayLoadInfo`.
    #[repr(C)]
    pub struct DelayLoadInfo {
        cb: u32,
        pidd: *const c_void,
        ppfn: *mut *const c_void,
        dll: *const c_char,
        dlp: DelayLoadProc,
        hmod: *mut c_void,
        pfn: *const c_void,
        last_error: u32,
    }

    type Hook = unsafe extern "system" fn(u32, *const DelayLoadInfo) -> *const c_void;

    /// The helper reads this when a delay-loaded DLL or function fails. (delayimp.lib's own one
    /// is only a default: the linker takes this one.)
    #[allow(non_upper_case_globals)]
    #[unsafe(no_mangle)]
    #[used]
    pub static __pfnDliFailureHook2: Option<Hook> = Some(on_failure);

    #[allow(non_upper_case_globals)]
    #[unsafe(no_mangle)]
    #[used]
    pub static __pfnDliNotifyHook2: Option<Hook> = Some(on_notify);

    fn is_vulkan(info: *const DelayLoadInfo) -> bool {
        !info.is_null() && !unsafe { (*info).dll }.is_null() && unsafe { CStr::from_ptr((*info).dll) }.to_bytes().eq_ignore_ascii_case(b"vulkan-1.dll")
    }

    /// This program: a module without Vulkan functions. Given in place of vulkan-1.dll, the
    /// helper then asks `on_failure` for each function.
    fn this_program() -> *const c_void {
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) as *const c_void }
    }

    /// The function that the helper asks for, when it is asked for by name.
    fn proc_name<'a>(info: *const DelayLoadInfo) -> &'a [u8] {
        let by_name = unsafe { (*info).dlp.import_by_name } != 0;
        if by_name && !unsafe { (*info).dlp.name }.is_null() { unsafe { CStr::from_ptr((*info).dlp.name) }.to_bytes() } else { b"" }
    }

    unsafe extern "system" fn on_notify(notify: u32, info: *const DelayLoadInfo) -> *const c_void {
        if !is_vulkan(info) {
            return std::ptr::null();
        }
        if notify == DLI_NOTE_PRE_LOAD_LIBRARY && (DISABLED.load(Ordering::SeqCst) || std::env::var_os("OPENWHISPER_NO_GPU").is_some()) {
            SKIPPED.store(true, Ordering::Relaxed);
            return this_program();
        }
        if notify == DLI_NOTE_PRE_GET_PROC_ADDRESS && proc_name(info) == b"vkGetInstanceProcAddr" {
            let hmod = unsafe { (*info).hmod };
            if let Some(real) = unsafe { windows_sys::Win32::System::LibraryLoader::GetProcAddress(hmod, c"vkGetInstanceProcAddr".as_ptr().cast()) } {
                REAL_GET_INSTANCE_PROC_ADDR.store(real as usize, Ordering::SeqCst);
                return checked_get_instance_proc_addr as *const c_void;
            }
        }
        std::ptr::null()
    }

    type GetProcAddr = unsafe extern "system" fn(*mut c_void, *const c_char) -> *const c_void;

    /// vulkan-1.dll's own vkGetInstanceProcAddr.
    static REAL_GET_INSTANCE_PROC_ADDR: AtomicUsize = AtomicUsize::new(0);

    /// The queries that ggml calls without a check: each one gives the number of matrix shapes
    /// first, and ggml uses that number.
    const MATRIX_QUERIES: [&[u8]; 2] = [b"vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR", b"vkGetPhysicalDeviceCooperativeMatrixFlexibleDimensionsPropertiesNV"];

    /// vulkan-1.dll's vkGetInstanceProcAddr, but a matrix-core query that the driver does not have
    /// gives `no_matrix_shapes` (ggml then runs without matrix cores), not null. It gives itself
    /// for "vkGetInstanceProcAddr", so vulkan.hpp's dispatcher keeps it after its start.
    unsafe extern "system" fn checked_get_instance_proc_addr(instance: *mut c_void, name: *const c_char) -> *const c_void {
        let real = REAL_GET_INSTANCE_PROC_ADDR.load(Ordering::SeqCst);
        if real == 0 {
            return std::ptr::null();
        }
        let real: GetProcAddr = unsafe { std::mem::transmute(real) };
        if name.is_null() {
            return unsafe { real(instance, name) };
        }
        let wanted = unsafe { CStr::from_ptr(name) }.to_bytes();
        if wanted == b"vkGetInstanceProcAddr" {
            return checked_get_instance_proc_addr as *const c_void;
        }
        let f = if hidden(wanted) { std::ptr::null() } else { unsafe { real(instance, name) } };
        if f.is_null() && !instance.is_null() && MATRIX_QUERIES.contains(&wanted) {
            // stderr: the app's log (src/crash.rs), or the terminal of a command-line run.
            eprintln!("Vulkan: the driver gives no {}; the speech engine runs without these matrix cores", String::from_utf8_lossy(wanted));
            return no_matrix_shapes as *const c_void;
        }
        f
    }

    /// A test switch: OPENWHISPER_VK_HIDE=vkName,vkOther acts as a driver without those
    /// functions (vkGetInstanceProcAddr gives null). With Wine on Linux, this tests the path of
    /// a PC whose driver or layer does not give one.
    fn hidden(name: &[u8]) -> bool {
        static HIDE: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
        HIDE.get_or_init(|| std::env::var("OPENWHISPER_VK_HIDE").unwrap_or_default().split(',').map(|s| s.trim().as_bytes().to_vec()).filter(|s| !s.is_empty()).collect()).iter().any(|h| h == name)
    }

    /// A matrix-core query without the function: no shapes.
    unsafe extern "system" fn no_matrix_shapes(_: *mut c_void, count: *mut u32, _: *mut c_void) -> i32 {
        if !count.is_null() {
            unsafe { *count = 0 };
        }
        0 // VK_SUCCESS
    }

    unsafe extern "system" fn on_failure(notify: u32, info: *const DelayLoadInfo) -> *const c_void {
        if !is_vulkan(info) {
            return std::ptr::null();
        }
        match notify {
            // No vulkan-1.dll: this program in its place.
            DLI_FAIL_LOAD_LIB => {
                MISSING.store(true, Ordering::Relaxed);
                this_program()
            }
            DLI_FAIL_GET_PROC => {
                if proc_name(info).ends_with(b"ProcAddr") {
                    vk_get_proc_addr as *const c_void
                } else {
                    vk_unavailable as *const c_void
                }
            }
            _ => std::ptr::null(),
        }
    }

    /// vkGetInstanceProcAddr / vkGetDeviceProcAddr without Vulkan: every function says "not
    /// available". ggml gets its other functions this way (vulkan.hpp's dynamic dispatcher).
    unsafe extern "system" fn vk_get_proc_addr(_: *mut c_void, _: *const c_char) -> *const c_void {
        vk_unavailable as *const c_void
    }

    /// Any Vulkan function without Vulkan. The first one ggml calls is
    /// vkEnumerateInstanceVersion, and this result ends its Vulkan start there.
    unsafe extern "system" fn vk_unavailable() -> i32 {
        VK_ERROR_INITIALIZATION_FAILED
    }
}

pub mod safe {
    //! The GPU's record (gpu-check.json in the data folder): the check passed for this GPU in
    //! this version, or the GPU failed ("ok": false, and why). A failed GPU stays off in this
    //! version until "Try the GPU again"; a new version checks again (it can have the fix).

    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use serde::{Deserialize, Serialize};

    const VERSION: &str = env!("CARGO_PKG_VERSION");
    /// The check's run prints this when the GPU worked.
    pub const PASSED: &str = "GPU check: ok";
    /// The first run on a GPU compiles its shaders: seconds, a minute on a slow driver.
    const TIMEOUT: Duration = Duration::from_secs(180);

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct Record {
        pub app: String,
        pub gpu: String,
        pub ok: bool,
        #[serde(default)]
        pub reason: String,
    }

    fn path() -> PathBuf {
        crate::config::data_dir().join("gpu-check.json")
    }

    /// There while the speech model loads or runs on the GPU; it has the GPU's name.
    fn busy_path() -> PathBuf {
        crate::crash::dir().join("gpu-busy")
    }

    fn read() -> Option<Record> {
        serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()
    }

    fn save(r: &Record) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, serde_json::to_string_pretty(r).unwrap_or_default());
    }

    /// Why the GPU is off in this version; None: it can be used.
    pub fn off_reason() -> Option<String> {
        off_reason_in(read(), VERSION)
    }

    fn off_reason_in(r: Option<Record>, version: &str) -> Option<String> {
        r.filter(|r| !r.ok && r.app == version).map(|r| r.reason)
    }

    fn passed_in(r: Option<Record>, version: &str, gpu: &str) -> bool {
        r.is_some_and(|r| r.ok && r.app == version && r.gpu == gpu)
    }

    /// "Try the GPU again": the record goes; the next start checks again.
    pub fn retry() {
        let _ = std::fs::remove_file(path());
    }

    fn failed(gpu: &str, reason: &str) {
        crate::crash::log_line(&format!("Safe mode: the speech engine uses the CPU from now on. {reason}"));
        save(&Record { app: VERSION.into(), gpu: gpu.into(), ok: false, reason: reason.into() });
    }

    /// At the start, after `crash::install`: the last run stopped while the speech model was on
    /// the GPU, so the GPU is off now.
    pub fn startup() {
        if let Some((gpu, why)) = crashed_on_gpu(&busy_path(), crate::crash::previous_crash()) {
            failed(&gpu, &why);
        }
    }

    /// The mark of the run before (it goes), and that run's crash: the GPU and why it is off.
    fn crashed_on_gpu(busy: &Path, crash: Option<String>) -> Option<(String, String)> {
        let gpu = std::fs::read_to_string(busy).ok()?;
        let _ = std::fs::remove_file(busy);
        let gpu = gpu.trim().to_string();
        let crash = crash?;
        let why = format!("Lowkey stopped ({crash}) while the speech model ran on the GPU ({gpu}).");
        Some((gpu, why))
    }

    static BUSY: AtomicUsize = AtomicUsize::new(0);

    /// While this lives, the speech model is in use on the GPU (`startup` reads the mark).
    pub struct Busy;

    pub fn busy(gpu: &str) -> Busy {
        if BUSY.fetch_add(1, Ordering::SeqCst) == 0 {
            let _ = std::fs::write(busy_path(), gpu);
        }
        Busy
    }

    impl Drop for Busy {
        fn drop(&mut self) {
            if BUSY.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _ = std::fs::remove_file(busy_path());
            }
        }
    }

    /// Before the first use of this GPU in this version: `lowkey --gpu-check <model>`, a second
    /// run of the app, loads the model on the GPU and transcribes two seconds. Its stop or hang
    /// does not stop the app. Err: why the GPU is off now (also in the record and the log).
    pub fn check(gpu: &str, model: &Path) -> Result<(), String> {
        static ONE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let _one = ONE.lock();
        if passed_in(read(), VERSION, gpu) {
            return Ok(());
        }
        if let Some(why) = off_reason() {
            return Err(why);
        }
        crate::crash::log_line(&format!("GPU check: {gpu}, with {}", model.file_name().unwrap_or_default().to_string_lossy()));
        let started = Instant::now();
        let result = run_check(model);
        match &result {
            Ok(()) => {
                crate::crash::log_line(&format!("GPU check: passed in {:.1} s", started.elapsed().as_secs_f64()));
                save(&Record { app: VERSION.into(), gpu: gpu.into(), ok: true, reason: String::new() });
            }
            Err(why) => failed(gpu, why),
        }
        result
    }

    fn run_check(model: &Path) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("The GPU check did not start: {e}"))?;
        let mut cmd = Command::new(exe);
        cmd.arg("--gpu-check").arg(model).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| format!("The GPU check did not start: {e}"))?;
        let read_all = |r: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut r) = r {
                    let mut bytes = Vec::new();
                    let _ = r.read_to_end(&mut bytes);
                    text = String::from_utf8_lossy(&bytes).into_owned();
                }
                text
            })
        };
        let out = read_all(child.stdout.take().map(|r| Box::new(r) as Box<dyn std::io::Read + Send>));
        let err = read_all(child.stderr.take().map(|r| Box::new(r) as Box<dyn std::io::Read + Send>));
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if started.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(100)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
        // Its messages go into this run's log: whisper.cpp's, and a crash line with its stack.
        let lines: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        for line in &lines[lines.len().saturating_sub(40)..] {
            crate::crash::log_line(&format!("  gpu-check: {line}"));
        }
        verdict(status.map(|s| s.code()), &out, &err)
    }

    /// The check's result from its exit (None: it hung; Some(None): a signal ended it), stdout
    /// and stderr.
    fn verdict(exit: Option<Option<i32>>, out: &str, err: &str) -> Result<(), String> {
        match exit {
            Some(Some(0)) if out.contains(PASSED) => Ok(()),
            None => Err(format!("The GPU check did not finish in {} minutes.", TIMEOUT.as_secs() / 60)),
            Some(code) => {
                if let Some(line) = err.lines().find(|l| l.contains("CRASH:")) {
                    return Err(format!("The GPU check stopped ({}).", crate::crash::reason(line)));
                }
                if let Some(line) = err.lines().rev().find(|l| l.starts_with("GPU check: ")) {
                    return Err(format!("The GPU check failed: {}", line.trim_start_matches("GPU check: ")));
                }
                Err(match code {
                    Some(c) => format!("The GPU check stopped (exit code 0x{:08X}).", c as u32),
                    None => "The GPU check stopped (a signal).".into(),
                })
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn rec(app: &str, gpu: &str, ok: bool) -> Option<Record> {
            Some(Record { app: app.into(), gpu: gpu.into(), ok, reason: "why".into() })
        }

        #[test]
        fn a_failed_gpu_is_off_until_a_new_version() {
            assert_eq!(off_reason_in(rec("1.0", "G", false), "1.0").as_deref(), Some("why"));
            assert_eq!(off_reason_in(rec("1.0", "G", false), "1.1"), None);
            assert_eq!(off_reason_in(rec("1.0", "G", true), "1.0"), None);
            assert_eq!(off_reason_in(None, "1.0"), None);
        }

        #[test]
        fn a_pass_counts_for_that_gpu_and_version_only() {
            assert!(passed_in(rec("1.0", "G", true), "1.0", "G"));
            assert!(!passed_in(rec("1.0", "G", true), "1.0", "H"));
            assert!(!passed_in(rec("1.0", "G", true), "1.1", "G"));
            assert!(!passed_in(rec("1.0", "G", false), "1.0", "G"));
        }

        #[test]
        fn a_crash_while_the_gpu_was_busy_turns_it_off() {
            let dir = std::env::temp_dir().join(format!("lowkey-gpu-busy-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let busy = dir.join("gpu-busy");
            assert_eq!(crashed_on_gpu(&busy, Some("access violation".into())), None, "no mark: not the GPU");
            std::fs::write(&busy, "RTX\n").unwrap();
            assert_eq!(crashed_on_gpu(&busy, None), None, "the mark, but no crash (the app was ended)");
            assert!(!busy.exists(), "the mark goes");
            std::fs::write(&busy, "RTX\n").unwrap();
            let (gpu, why) = crashed_on_gpu(&busy, Some("access violation".into())).unwrap();
            assert_eq!(gpu, "RTX");
            assert_eq!(why, "Lowkey stopped (access violation) while the speech model ran on the GPU (RTX).");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn the_verdict_names_the_crash() {
            assert_eq!(verdict(Some(Some(0)), "GPU check: ok (X)\n", ""), Ok(()));
            assert!(verdict(Some(Some(0)), "", "").is_err(), "no ok line: not passed");
            let crash = "CRASH: exception 0xC0000005 (access violation) at 0x0\n  called from 0x1 = lowkey.exe+0x1\n";
            assert_eq!(verdict(Some(Some(-1073741819)), "", crash), Err("The GPU check stopped (access violation).".into()));
            assert_eq!(verdict(Some(Some(-1073741819)), "", ""), Err("The GPU check stopped (exit code 0xC0000005).".into()));
            assert_eq!(verdict(Some(Some(2)), "", "GPU check: the model did not load\n"), Err("The GPU check failed: the model did not load".into()));
            assert!(verdict(None, "", "").unwrap_err().contains("did not finish"));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_registry_answers_without_a_crash() {
        // A computer without a GPU (CI) gives an empty list, never an abort.
        let gpus = super::registry_gpus();
        let text = super::describe();
        assert!(text.starts_with("Speech engine: "), "{text}");
        assert!(gpus.len() < 64);
    }
}
