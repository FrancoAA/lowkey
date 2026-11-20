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
        s += "\nvulkan-1.dll (the Vulkan loader) not loaded: OPENWHISPER_NO_GPU is set.";
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

    use std::ffi::{c_char, c_void, CStr};
    use std::sync::atomic::{AtomicBool, Ordering};

    static MISSING: AtomicBool = AtomicBool::new(false);
    static SKIPPED: AtomicBool = AtomicBool::new(false);

    /// vulkan-1.dll could not be loaded in this run.
    pub fn loader_missing() -> bool {
        MISSING.load(Ordering::Relaxed)
    }

    /// vulkan-1.dll was not loaded on purpose (OPENWHISPER_NO_GPU).
    pub fn skipped() -> bool {
        SKIPPED.load(Ordering::Relaxed)
    }

    const DLI_NOTE_PRE_LOAD_LIBRARY: u32 = 1;

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

    unsafe extern "system" fn on_notify(notify: u32, info: *const DelayLoadInfo) -> *const c_void {
        if notify == DLI_NOTE_PRE_LOAD_LIBRARY && is_vulkan(info) && std::env::var_os("OPENWHISPER_NO_GPU").is_some() {
            SKIPPED.store(true, Ordering::Relaxed);
            return this_program();
        }
        std::ptr::null()
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
                let by_name = unsafe { (*info).dlp.import_by_name } != 0;
                let name = if by_name && !unsafe { (*info).dlp.name }.is_null() { unsafe { CStr::from_ptr((*info).dlp.name) }.to_bytes() } else { b"" };
                if name.ends_with(b"ProcAddr") {
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
