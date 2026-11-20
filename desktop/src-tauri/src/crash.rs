//! Crash reports. The window app writes a log for each run: `logs/lowkey.log` in the data
//! folder (Linux ~/.local/share/lowkey, Windows %APPDATA%\lowkey, macOS ~/Library/Application
//! Support/lowkey). The run before is `lowkey.previous.log`. The app's own messages (stderr) go
//! there when no terminal shows them.
//!
//! A crash leaves one line that starts with "CRASH:": a panic of the main thread, a signal
//! (Linux, macOS: illegal instruction, segmentation fault, abort…) or an exception (Windows:
//! 0xC000001D illegal instruction, 0xC0000005 access violation…), with the place in the program.
//! The next start finds the line and offers a report.
//!
//! The app sends nothing. The report opens as a new GitHub issue in the browser, where the person
//! reads it before they send it, or it goes to the clipboard, or `lowkey --crash-report` prints
//! it. It has the app's version, the system, the processor and its instruction sets, the speech
//! engine and model, the crash line and the end of the log, with the home folder written as "~".
//! The log has no audio and no text of meetings or dictations.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::Mutex;
use serde::Serialize;

pub const ISSUES_URL: &str = "https://github.com/FrancoAA/lowkey/issues/new";
const LOG: &str = "lowkey.log";
const PREVIOUS: &str = "lowkey.previous.log";
/// The start of a crash line.
pub const MARK: &str = "CRASH:";
/// Rust's own words for a stack overflow (it then aborts; Windows has no exception for it).
const OVERFLOW: &str = "has overflowed its stack";
/// The end of a log that a report reads.
const TAIL_BYTES: u64 = 64 * 1024;
const TAIL_LINES: usize = 60;
/// A longer GitHub address can fail. The report on the clipboard has the whole log.
const MAX_URL: usize = 7_000;

static LOG_FILE: OnceLock<File> = OnceLock::new();
/// The crash line of the run before, until the person reports it or closes the notice.
static PREVIOUS_CRASH: Mutex<Option<String>> = Mutex::new(None);

pub fn dir() -> PathBuf {
    crate::config::data_dir().join("logs")
}

/// Start this run's log and the crash handlers. The window app calls it once, at its start. A
/// second launch that only hands its arguments to the running app must not: it would move that
/// app's log.
pub fn install() {
    let dir = dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let (log, previous) = (dir.join(LOG), dir.join(PREVIOUS));
    let _ = std::fs::rename(&log, &previous);
    *PREVIOUS_CRASH.lock() = crash_line(&read_tail(&previous));
    let Ok(mut file) = OpenOptions::new().create(true).write(true).truncate(true).open(&log) else {
        return;
    };
    let _ = writeln!(file, "Lowkey {} ({}) started {} · {} · {}", env!("CARGO_PKG_VERSION"), build(), now(), system(), processor());
    redirect_stderr(&file);
    native::install(&file);
    let _ = LOG_FILE.set(file);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let payload = info.payload();
        let what = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default();
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        // The main thread runs the window: its panic ends the app. Another thread's panic ends
        // only that thread's work.
        let head = if name == "main" { format!("{MARK} panic") } else { "PANIC (the app went on):".to_string() };
        log_line(&format!("{head} in thread '{name}' at {at}: {what}"));
        previous_hook(info);
    }));
}

/// One line in this run's log, with the time.
pub fn log_line(text: &str) {
    if let Some(mut f) = LOG_FILE.get() {
        let _ = writeln!(f, "{} {text}", chrono::Local::now().format("%H:%M:%S"));
    }
}

/// The crash of the run before, in a few words ("illegal instruction"), until it is reported or
/// the notice is closed.
pub fn previous_crash() -> Option<String> {
    PREVIOUS_CRASH.lock().as_deref().map(reason)
}

pub fn dismiss() {
    *PREVIOUS_CRASH.lock() = None;
}

/// "CRASH: exception 0xC000001D (illegal instruction) at …" → "illegal instruction".
fn reason(line: &str) -> String {
    if line.contains(OVERFLOW) {
        return "stack overflow".into();
    }
    match (line.find('('), line.find(')')) {
        (Some(a), Some(b)) if a < b => line[a + 1..b].to_string(),
        _ if line.contains(" panic ") => "panic".into(),
        _ => "crash".into(),
    }
}

/// The first crash line of a log: the first is the cause (a stack overflow also aborts).
fn crash_line(log: &str) -> Option<String> {
    log.lines().find_map(|l| match l.find(MARK) {
        Some(i) => Some(l[i..].trim().to_string()), // without the time before it
        None if l.contains(OVERFLOW) => Some(l.trim().to_string()),
        None => None,
    })
}

/// The end of a file (a log can be long), as text.
fn read_tail(path: &Path) -> String {
    let Ok(mut f) = File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > TAIL_BYTES {
        let _ = f.seek(SeekFrom::Start(len - TAIL_BYTES));
    }
    let mut bytes = Vec::new();
    let _ = f.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string()
}

/// How whisper.cpp was built: for AVX2 (the releases, GGML_NATIVE=OFF in
/// .github/workflows/desktop.yml), or for the computer that built it.
fn build() -> &'static str {
    match option_env!("GGML_NATIVE") {
        Some("OFF") => "release build",
        _ => "built for its own computer",
    }
}

/// The built-in speech engine of a release needs AVX2, FMA and F16C. Without them whisper.cpp
/// stops the app with an illegal instruction the first time it runs: say so instead.
pub fn cpu_problem() -> Option<String> {
    #[cfg(target_arch = "x86_64")]
    if option_env!("GGML_NATIVE") == Some("OFF") {
        let missing: Vec<&str> = [
            ("AVX2", std::arch::is_x86_feature_detected!("avx2")),
            ("FMA", std::arch::is_x86_feature_detected!("fma")),
            ("F16C", std::arch::is_x86_feature_detected!("f16c")),
        ]
        .into_iter()
        .filter(|(_, has)| !has)
        .map(|(name, _)| name)
        .collect();
        if !missing.is_empty() {
            return Some(format!(
                "This computer's processor does not have {}, which the built-in speech models need. Use a speech provider in the cloud (Settings → Speech), or build Lowkey from source on this computer.",
                missing.join(", ")
            ));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The system and the processor

pub fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    }
}

/// "Arch Linux, kernel 6.17.2, Hyprland, x86_64" · "Windows 11 24H2 (build 26100), x86_64" ·
/// "macOS 15.1, aarch64".
pub fn system() -> String {
    format!("{}, {}", system_name(), std::env::consts::ARCH)
}

#[cfg(target_os = "linux")]
fn system_name() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let distro = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| "Linux".into());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|k| k.trim().to_string()).unwrap_or_default();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let mut out = distro;
    if !kernel.is_empty() {
        out += &format!(", kernel {kernel}");
    }
    if !desktop.is_empty() {
        out += &format!(", {desktop}");
    }
    if !session.is_empty() {
        out += &format!(" ({session})");
    }
    out
}

#[cfg(target_os = "macos")]
fn system_name() -> String {
    let v = std::process::Command::new("sw_vers").arg("-productVersion").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    format!("macOS {v}")
}

#[cfg(windows)]
fn system_name() -> String {
    let product = win::registry("ProductName").unwrap_or_else(|| "Windows".into());
    let build = win::registry("CurrentBuildNumber").unwrap_or_default();
    let display = win::registry("DisplayVersion").unwrap_or_default();
    // Windows 11 still says "Windows 10" in ProductName: its builds start at 22000.
    let product = if build.parse::<u32>().is_ok_and(|b| b >= 22000) { product.replace("Windows 10", "Windows 11") } else { product };
    let mut out = product;
    if !display.is_empty() {
        out += &format!(" {display}");
    }
    if !build.is_empty() {
        out += &format!(" (build {build})");
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn system_name() -> String {
    std::env::consts::OS.to_string()
}

/// The processor's name, and the instruction sets that a speech engine build can need.
pub fn processor() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        let yes = |b: bool| if b { "yes" } else { "no" };
        format!(
            "{} · AVX2 {} · FMA {} · F16C {} · AVX-512 {}",
            cpu_brand(),
            yes(std::arch::is_x86_feature_detected!("avx2")),
            yes(std::arch::is_x86_feature_detected!("fma")),
            yes(std::arch::is_x86_feature_detected!("f16c")),
            yes(std::arch::is_x86_feature_detected!("avx512f")),
        )
    }
    #[cfg(all(target_os = "macos", not(target_arch = "x86_64")))]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| std::env::consts::ARCH.into())
    }
    #[cfg(not(any(target_arch = "x86_64", target_os = "macos")))]
    {
        std::env::consts::ARCH.to_string()
    }
}

/// "12th Gen Intel(R) Core(TM) i5-1235U", from the processor itself.
#[cfg(target_arch = "x86_64")]
#[allow(unused_unsafe)]
fn cpu_brand() -> String {
    use std::arch::x86_64::__cpuid;
    let max = unsafe { __cpuid(0x8000_0000) }.eax;
    if max < 0x8000_0004 {
        return "x86_64 processor".into();
    }
    let mut bytes = Vec::with_capacity(48);
    for leaf in 0x8000_0002..=0x8000_0004u32 {
        let r = unsafe { __cpuid(leaf) };
        for v in [r.eax, r.ebx, r.ecx, r.edx] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    String::from_utf8_lossy(&bytes).trim_matches(char::from(0)).split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------------------------
// The report

/// What the app knows about its speech engine, for the report.
pub struct Facts {
    /// "CPU" or "GPU (…)".
    pub device: String,
    /// The speech model or provider.
    pub speech: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub title: String,
    /// The whole report (Markdown), for the clipboard.
    pub text: String,
    /// A new GitHub issue with the report (its log shortened when it is too long for an address).
    pub url: String,
}

/// The report in the window app: the crash of the run before and that run's log, else this
/// run's log.
pub fn report(facts: &Facts) -> Report {
    let crash = PREVIOUS_CRASH.lock().clone();
    let log = if crash.is_some() { dir().join(PREVIOUS) } else { dir().join(LOG) };
    make_report(facts, crash.as_deref(), &read_tail(&log))
}

/// `lowkey --crash-report`, with or without the window app running: the last crash in the two
/// logs (the newest first), with its log; else the newest log.
pub fn report_from_files(facts: &Facts) -> Report {
    let (log, previous) = (read_tail(&dir().join(LOG)), read_tail(&dir().join(PREVIOUS)));
    let (crash, text) = match (crash_line(&log), crash_line(&previous)) {
        (Some(c), _) => (Some(c), log),
        (None, Some(c)) => (Some(c), previous),
        (None, None) => (None, log),
    };
    make_report(facts, crash.as_deref(), &text)
}

fn make_report(facts: &Facts, crash: Option<&str>, log: &str) -> Report {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).filter(|h| h.len() > 1);
    let private = |s: &str| match &home {
        Some(h) => s.replace(h.as_str(), "~"),
        None => s.to_string(),
    };
    let lines: Vec<String> = log.lines().map(&private).collect();
    let lines = &lines[lines.len().saturating_sub(TAIL_LINES)..];
    let title = match crash {
        Some(c) => format!("Crash: {} ({}, {})", reason(c), os_name(), env!("CARGO_PKG_VERSION")),
        None => format!("Problem report ({}, {})", os_name(), env!("CARGO_PKG_VERSION")),
    };
    let head = {
        let mut h = String::new();
        h += "**What happened**\n\n<!-- What did you do just before? For example: \"I started a meeting; after 3 s the app closed.\" -->\n\n";
        h += &format!("**Lowkey** {} ({})  \n", env!("CARGO_PKG_VERSION"), build());
        h += &format!("**System** {}  \n", private(&system()));
        h += &format!("**Processor** {}  \n", processor());
        h += &format!("**Speech** {} · {}  \n", facts.device, facts.speech);
        h += &match crash {
            Some(c) => format!("**Crash** `{}`\n", private(c)),
            None => "**Crash** none found in the log\n".into(),
        };
        h
    };
    let body = |lines: &[String], note: &str| {
        format!("{head}\n<details><summary>The end of the app's log{note}</summary>\n\n```\n{}\n```\n</details>\n", lines.join("\n"))
    };
    let text = body(lines, "");
    let address = |body: &str| format!("{ISSUES_URL}?title={}&body={}", encode(&title), encode(body));
    // Too long for an address: fewer log lines (the newest stay). The clipboard has them all.
    let mut skip = 0;
    let mut url = address(&text);
    while url.len() > MAX_URL && skip < lines.len() {
        skip = (skip + 5).min(lines.len());
        url = address(&body(&lines[skip..], " (shortened: the whole report is on the clipboard)"));
    }
    Report { title, text, url }
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

/// Open a web address or a folder with the system.
pub fn open(target: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        win::open(target)
    }
    #[cfg(not(windows))]
    {
        crate::export::open_link(target)
    }
}

// ---------------------------------------------------------------------------------------------
// Tests of the handlers: `lowkey --crash-test <kind>`

pub const TEST_KINDS: &str = "panic | segfault | illegal-instruction";

/// Crash on purpose, as a real crash would (the CI checks that the log gets its line).
pub fn test(kind: &str) {
    match kind {
        "panic" => panic!("crash test (--crash-test panic)"),
        // An address that no program has: a segmentation fault / an access violation.
        "segfault" => unsafe { std::ptr::write_volatile(8 as *mut u8, 1) },
        // The instruction that is never valid: what a CPU without AVX-512 does with one.
        "illegal-instruction" => unsafe {
            #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
            std::arch::asm!("ud2");
            #[cfg(target_arch = "aarch64")]
            std::arch::asm!("udf #0");
        },
        other => {
            eprintln!("unknown --crash-test {other:?}: {TEST_KINDS}");
            return;
        }
    }
    eprintln!("--crash-test {kind}: the app did not stop");
}

// ---------------------------------------------------------------------------------------------
// Native crashes. A handler may only use what is safe in a signal handler: no memory is taken,
// no lock; it writes one line into the log's file descriptor, and the handler that was there
// before (Rust's, or the system's: a core dump, Windows Error Reporting) takes over.

/// A line made without taking memory.
struct Buf {
    buf: [u8; 512],
    len: usize,
}

impl Buf {
    const fn new() -> Self {
        Self { buf: [0; 512], len: 0 }
    }

    fn push(&mut self, b: &[u8]) {
        let n = b.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn dec(&mut self, mut v: usize) {
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.push(&tmp[i..]);
    }

    fn hex(&mut self, v: usize) {
        self.hex_width(v, 1);
    }

    fn hex_width(&mut self, mut v: usize, min: usize) {
        let mut tmp = [0u8; 16];
        let mut i = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b"0123456789ABCDEF"[v & 15];
            v >>= 4;
            if v == 0 && tmp.len() - i >= min {
                break;
            }
        }
        self.push(&tmp[i..]);
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[cfg(unix)]
fn redirect_stderr(file: &File) {
    use std::io::IsTerminal;
    use std::os::fd::AsRawFd;
    if !std::io::stderr().is_terminal() {
        unsafe { libc::dup2(file.as_raw_fd(), 2) };
    }
}

#[cfg(windows)]
fn redirect_stderr(file: &File) {
    use std::io::IsTerminal;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    // Rust asks Windows for the handle at each write; a GUI program has none.
    if !std::io::stderr().is_terminal() {
        unsafe { SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle() as _) };
    }
}

#[cfg(unix)]
mod native {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::OnceLock;

    use super::Buf;

    static FD: AtomicI32 = AtomicI32::new(-1);
    /// The program in memory (Linux): where it starts and ends, for "lowkey+0x…" (the address
    /// in the file that `objdump -d` shows).
    static EXE: OnceLock<(usize, usize)> = OnceLock::new();
    /// The actions that were there before, set before the handlers are.
    static OLD: OnceLock<Vec<(libc::c_int, libc::sigaction)>> = OnceLock::new();
    const SIGNALS: [libc::c_int; 5] = [libc::SIGILL, libc::SIGSEGV, libc::SIGBUS, libc::SIGFPE, libc::SIGABRT];

    pub fn install(file: &std::fs::File) {
        FD.store(file.as_raw_fd(), Ordering::SeqCst);
        #[cfg(target_os = "linux")]
        if let Some(range) = exe_range() {
            let _ = EXE.set(range);
        }
        let mut old = Vec::new();
        for sig in SIGNALS {
            let mut prev: libc::sigaction = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(sig, std::ptr::null(), &mut prev) } == 0 {
                old.push((sig, prev));
            }
        }
        if OLD.set(old).is_err() {
            return; // already installed
        }
        for sig in SIGNALS {
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) as usize;
                // ONSTACK: Rust gives its threads a second stack, where a stack overflow can
                // still be handled.
                sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigemptyset(&mut sa.sa_mask);
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }
    }

    extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
        let mut w = Buf::new();
        w.push(b"CRASH: signal ");
        w.dec(sig as usize);
        w.push(b" (");
        w.push(match sig {
            libc::SIGILL => b"illegal instruction",
            libc::SIGSEGV => b"segmentation fault",
            libc::SIGBUS => b"bus error",
            libc::SIGFPE => b"arithmetic error",
            libc::SIGABRT => b"abort",
            _ => b"signal",
        });
        w.push(b")");
        if sig != libc::SIGABRT && !info.is_null() {
            let addr = unsafe { (*info).si_addr() } as usize;
            w.push(b" at 0x");
            w.hex(addr);
            if let Some(&(start, end)) = EXE.get() {
                if (start..end).contains(&addr) {
                    w.push(b" = lowkey+0x");
                    w.hex(addr - start);
                }
            }
        }
        w.push(b"\n");
        let fd = FD.load(Ordering::Relaxed);
        if fd >= 0 {
            let b = w.as_bytes();
            unsafe { libc::write(fd, b.as_ptr().cast(), b.len()) };
        }
        // The action from before takes over: the fault happens again when this returns (Rust's
        // handler names a stack overflow; the default dumps core). An abort is sent again.
        let old = OLD.get().and_then(|o| o.iter().find(|(s, _)| *s == sig));
        unsafe {
            match old {
                Some((_, a)) => libc::sigaction(sig, a, std::ptr::null_mut()),
                None => libc::sigaction(sig, &default_action(), std::ptr::null_mut()),
            };
            if sig == libc::SIGABRT {
                libc::raise(sig);
            }
        }
    }

    fn default_action() -> libc::sigaction {
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = libc::SIG_DFL;
        sa
    }

    /// The program's mappings in /proc/self/maps: from the one at file offset 0 to the last.
    #[cfg(target_os = "linux")]
    fn exe_range() -> Option<(usize, usize)> {
        let exe = std::fs::read_link("/proc/self/exe").ok()?;
        let exe = exe.to_str()?;
        let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
        let (mut start, mut end) = (None, 0usize);
        for line in maps.lines() {
            let mut parts = line.split_whitespace();
            let (Some(range), _, Some(offset), _, _, Some(path)) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            if path != exe {
                continue;
            }
            let (a, b) = range.split_once('-')?;
            let (a, b) = (usize::from_str_radix(a, 16).ok()?, usize::from_str_radix(b, 16).ok()?);
            if start.is_none() && usize::from_str_radix(offset, 16).ok()? == 0 {
                start = Some(a);
            }
            end = end.max(b);
        }
        Some((start?, end))
    }
}

#[cfg(windows)]
mod native {
    use std::os::windows::io::AsRawHandle;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Diagnostics::Debug::{SetUnhandledExceptionFilter, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};

    use super::Buf;

    static HANDLE: AtomicUsize = AtomicUsize::new(0);

    pub fn install(file: &std::fs::File) {
        HANDLE.store(file.as_raw_handle() as usize, Ordering::SeqCst);
        unsafe { SetUnhandledExceptionFilter(Some(on_exception)) };
    }

    /// An exception that nothing handled: one line, then Windows ends the program as before.
    unsafe extern "system" fn on_exception(info: *const EXCEPTION_POINTERS) -> i32 {
        let mut w = Buf::new();
        let rec = if info.is_null() { std::ptr::null_mut() } else { unsafe { (*info).ExceptionRecord } };
        if rec.is_null() {
            w.push(b"CRASH: exception (no record)");
        } else {
            let (code, addr) = unsafe { ((*rec).ExceptionCode as u32, (*rec).ExceptionAddress as usize) };
            w.push(b"CRASH: exception 0x");
            w.hex_width(code as usize, 8);
            w.push(b" (");
            w.push(name(code));
            w.push(b") at 0x");
            w.hex(addr);
            // The module and the address in it: "lowkey.exe+0x2F1A3B" (`objdump -d` of the
            // release's lowkey.exe, at its image base + that offset).
            let mut module = std::ptr::null_mut();
            let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
            if unsafe { GetModuleHandleExW(flags, addr as *const u16, &mut module) } != 0 && !module.is_null() {
                let mut path = [0u16; 260];
                let n = (unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) } as usize).min(path.len());
                let start = path[..n].iter().rposition(|&c| c == b'\\' as u16).map_or(0, |i| i + 1);
                w.push(b" = ");
                for &c in &path[start..n] {
                    w.push(&[if c < 128 { c as u8 } else { b'?' }]);
                }
                w.push(b"+0x");
                w.hex(addr.wrapping_sub(module as usize));
            }
        }
        w.push(b"\r\n");
        let h = HANDLE.load(Ordering::SeqCst);
        if h != 0 {
            let b = w.as_bytes();
            let mut written = 0u32;
            unsafe { WriteFile(h as _, b.as_ptr(), b.len() as u32, &mut written, std::ptr::null_mut()) };
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    fn name(code: u32) -> &'static [u8] {
        match code {
            0xC000_001D => b"illegal instruction",
            0xC000_0005 => b"access violation",
            0xC000_00FD => b"stack overflow",
            0xC000_0094 => b"integer division by zero",
            0xC000_0096 => b"privileged instruction",
            0xC000_0409 => b"stack buffer overrun",
            0xC000_0006 => b"in-page error",
            0xE06D_7363 => b"C++ exception",
            0x8000_0003 => b"breakpoint",
            _ => b"exception",
        }
    }
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A text value of HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion.
    pub fn registry(name: &str) -> Option<String> {
        let (key, value) = (wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"), wide(name));
        let mut buf = [0u16; 256];
        let mut size = std::mem::size_of_val(&buf) as u32;
        let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
        if r != 0 {
            return None;
        }
        let n = (size as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]);
        Some(s.trim_end_matches('\0').trim().to_string()).filter(|s| !s.is_empty())
    }

    /// The system's handler: the browser for a web address, Explorer for a folder. (Not
    /// `cmd /c start`: cmd reads "%…%" in an address as a variable.)
    pub fn open(target: &str) -> anyhow::Result<()> {
        let (verb, file) = (wide("open"), wide(target));
        let r = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
        anyhow::ensure!(r as isize > 32, "Windows could not open it (error {})", r as isize);
        Ok(())
    }

    /// `lowkey --version` and the other commands from a terminal: a GUI program has no console,
    /// so its text goes to the console of the terminal that started it. Output to a pipe or a
    /// file already works and stays.
    pub fn attach_console() {
        use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
        unsafe {
            if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
                AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

/// See `win::attach_console`. Nothing to do on other systems.
pub fn attach_console() {
    #[cfg(windows)]
    win::attach_console();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts { device: "CPU".into(), speech: "small".into() }
    }

    #[test]
    fn the_first_crash_line_is_the_cause() {
        let log = "Lowkey 0.11.1 started\n10:00:01 PANIC (the app went on): in thread 'x'\nCRASH: signal 11 (segmentation fault) at 0x8\nCRASH: signal 6 (abort)\n";
        assert_eq!(crash_line(log).as_deref(), Some("CRASH: signal 11 (segmentation fault) at 0x8"));
        assert_eq!(reason(&crash_line(log).unwrap()), "segmentation fault");
        assert_eq!(crash_line("Lowkey started\nall good\n"), None, "a panic of a worker thread is no crash");
        let overflow = "\nthread 'meeting' has overflowed its stack\nfatal runtime error: stack overflow\n";
        assert_eq!(reason(&crash_line(overflow).unwrap()), "stack overflow");
        assert_eq!(reason("CRASH: exception 0xC000001D (illegal instruction) at 0x7FF6 = lowkey.exe+0x2F1A3B"), "illegal instruction");
        let panic = crash_line("10:00:00 CRASH: panic in thread 'main' at src/app.rs:1: boom\n").unwrap();
        assert_eq!(panic, "CRASH: panic in thread 'main' at src/app.rs:1: boom", "without the time");
        assert_eq!(reason(&panic), "panic");
    }

    #[test]
    fn a_report_names_the_crash_and_hides_the_home_folder() {
        let home = dirs::home_dir().unwrap().to_string_lossy().into_owned();
        let log = format!("Lowkey started\n  Ignoring {home}/.config/lowkey/config.toml: bad\nCRASH: signal 4 (illegal instruction) at 0x5F00 = lowkey+0x1F00\n");
        let r = make_report(&facts(), crash_line(&log).as_deref(), &log);
        assert!(r.title.starts_with("Crash: illegal instruction ("), "{}", r.title);
        assert!(r.text.contains("**Crash** `CRASH: signal 4 (illegal instruction) at 0x5F00 = lowkey+0x1F00`"), "{}", r.text);
        assert!(r.text.contains("Ignoring ~/.config/lowkey/config.toml"), "{}", r.text);
        assert!(!r.text.contains(&home), "the home folder is not in the report");
        assert!(r.text.contains("**Speech** CPU · small"));
        assert!(r.url.starts_with(&format!("{ISSUES_URL}?title=Crash%3A%20illegal%20instruction")), "{}", r.url);
        assert!(!r.url.contains('+') && !r.url.contains(' '), "spaces are %20: {}", r.url);
    }

    #[test]
    fn a_long_log_is_shortened_in_the_address_only() {
        let log: String = (0..TAIL_LINES * 3).map(|i| format!("line {i} {}\n", "x".repeat(150))).collect();
        let r = make_report(&facts(), None, &log);
        assert!(r.url.len() <= MAX_URL, "{} characters", r.url.len());
        assert!(r.url.contains("shortened"), "the address says so");
        let last = format!("line {} ", TAIL_LINES * 3 - 1);
        assert!(r.url.contains(&encode(&last)), "the newest lines stay");
        assert_eq!(r.text.lines().filter(|l| l.starts_with("line ")).count(), TAIL_LINES, "the clipboard has the whole end of the log");
        assert!(r.title.starts_with("Problem report ("));
    }

    #[test]
    fn numbers_are_written_without_memory() {
        let mut b = Buf::new();
        b.dec(0);
        b.push(b" ");
        b.dec(11);
        b.push(b" ");
        b.hex(0x2F1A3B);
        b.push(b" ");
        b.hex_width(0xC000001D, 8);
        b.push(b" ");
        b.hex_width(0x1D, 8);
        assert_eq!(std::str::from_utf8(b.as_bytes()).unwrap(), "0 11 2F1A3B C000001D 0000001D");
    }

    #[test]
    fn this_processor_and_system_are_named() {
        let p = processor();
        assert!(!p.is_empty());
        #[cfg(target_arch = "x86_64")]
        assert!(p.contains("AVX2 ") && p.contains("AVX-512 "), "{p}");
        assert!(system().ends_with(std::env::consts::ARCH));
    }
}
