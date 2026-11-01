//! Tauri app: windows, tray, single instance, commands and the global key.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::config::{Cli, Config, HELP};
use crate::dictation::{self, Dictation, Io, Snapshot};
use crate::engine::{Engines, Input};
use crate::jobs::{JobSnapshot, Jobs};
use crate::mic::{self, EndFn, LevelFn, Recorder};
use crate::models;
use crate::preview::{AudioFn, Preview, Stream, TextFn};
use crate::{hyprland, output};

pub struct App {
    handle: AppHandle,
    config: Mutex<Config>,
    cli: Cli,
    pub engines: Arc<Engines>,
    jobs: Arc<Jobs>,
    pub dictation: Mutex<Option<Arc<Dictation>>>,
    hotkey: Mutex<Option<Arc<hyprland::Hotkey>>>,
    hotkey_text: Mutex<String>,
    window_rec: Mutex<Option<Box<dyn Recorder>>>,
    open_files: Mutex<Vec<PathBuf>>,
    started: Instant,
    /// The overlay page has loaded and rendered (shown only then: no blank first frame).
    overlay_ready: AtomicBool,
    overlay_pending: AtomicBool,
    preview: Preview,
}

type AppRef = Arc<App>;

/// Hand-offs from other launches that arrive before setup has finished (e.g. a key release
/// right after the key started the app). Replayed once the app is ready.
static EARLY_ARGS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

/// Test seam: OPENWHISPER_TEST_LOG=<file> appends every UI event as a JSON line, so automated
/// tests can follow the app without a WebDriver. No effect when unset.
fn emit_logged<S: serde::Serialize + Clone>(handle: &AppHandle, event: &str, payload: S) {
    if let Some(path) = std::env::var_os("OPENWHISPER_TEST_LOG") {
        use std::io::Write;
        static LOG: Mutex<()> = Mutex::new(()); // one line at a time from all threads
        let _guard = LOG.lock();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(format!("{}\n", json!({"event": event, "payload": payload.clone()})).as_bytes());
        }
    }
    let _ = handle.emit(event, payload);
}

impl App {
    /// CLI --model > config.toml > automatic choice for this machine.
    pub fn current_model(&self) -> String {
        let cfg = self.config.lock().model.clone();
        for id in [self.cli.model.clone().unwrap_or_default(), cfg] {
            if let Some(spec) = models::spec(&id) {
                if self.engines.available(spec) {
                    return id;
                }
            }
        }
        models::auto_model(&self.engines.hw).to_string()
    }

    /// CLI --language > config.toml; None = detect.
    pub fn current_language(&self) -> Option<String> {
        let lang = self.cli.language.clone().unwrap_or_else(|| self.config.lock().language.clone());
        (!lang.is_empty() && lang != "auto").then_some(lang)
    }

    fn prepare_preview(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.preview.prepare(Arc::new(move || {
            if let Some(a) = weak.upgrade() {
                a.emit_status();
            }
        }));
    }

    fn status(&self) -> Value {
        let current = self.current_model();
        let cfg = self.config.lock().clone();
        let list: Vec<Value> = models::CATALOG
            .iter()
            .map(|m| {
                let st = self.engines.state(m.id);
                json!({
                    "name": m.id, "label": m.label, "engine": m.engine, "approx_mb": m.download_mb,
                    "ram_mb": m.ram_mb, "speed_gpu": m.speed_gpu, "speed_cpu": m.speed_cpu, "wer": m.wer,
                    "available": self.engines.available(m),
                    "state": st.state, "downloaded": st.downloaded, "total": st.total, "progress": st.progress, "error": st.error,
                })
            })
            .collect();
        json!({
            "desktop": true,
            "device": self.engines.hw.gpu.clone().map(|g| format!("GPU ({g})")).unwrap_or_else(|| "CPU".into()),
            "gpu": self.engines.hw.gpu.is_some(),
            "default_model": current,
            "auto_model": models::auto_model(&self.engines.hw),
            "models": list,
            "max_upload_bytes": crate::audio::MAX_FILE_BYTES,
            "options": { "model": cfg.model, "language": cfg.language, "idle_unload_minutes": cfg.idle_unload_minutes, "live_preview": cfg.live_preview },
            "preview": self.preview.status(),
            "hotkey": *self.hotkey_text.lock(),
        })
    }

    fn emit_status(&self) {
        emit_logged(&self.handle, "status", self.status());
    }

    fn overlay(&self, visible: bool) {
        let usable = self.hotkey.lock().as_ref().map(|h| h.rule_installed()).unwrap_or(false) || !hyprland::running();
        self.overlay_pending.store(visible && usable, Ordering::SeqCst);
        if visible && !self.overlay_ready.load(Ordering::SeqCst) {
            return; // shown by overlay_ready() once the page has drawn
        }
        if let Some(w) = self.handle.get_webview_window("overlay") {
            if visible && usable {
                let _ = w.show();
                emit_logged(&self.handle, "overlay", true);
            } else if !visible {
                let _ = w.hide();
                emit_logged(&self.handle, "overlay", false);
            }
        }
    }

    fn handle_cli(self: &Arc<Self>, cli: &Cli, first: bool) {
        if let Some(action) = cli.dictate.as_deref() {
            let d = self.dictation.lock().clone();
            let Some(d) = d else { return };
            match action {
                "press" | "toggle" if first => {
                    let engines = self.engines.clone();
                    let me = self.clone();
                    let model = me.current_model();
                    d.start_when_ready(Arc::new(move || engines.state(&model)), Duration::from_millis(200));
                }
                "press" => {
                    d.press();
                }
                "release" => {
                    d.release();
                }
                "toggle" => {
                    d.toggle();
                }
                "cancel" => {
                    d.cancel();
                }
                "quit" => self.handle.exit(0),
                other => eprintln!("  unknown --dictate action {other:?}"),
            }
            return;
        }
        let files: Vec<PathBuf> = cli.files.iter().filter(|p| p.is_file()).cloned().collect();
        if !files.is_empty() && !first {
            let _ = self.handle.emit("open-files", files);
        }
        if !first || !cli.background {
            self.show_main();
        }
    }

    fn show_main(&self) {
        if let Some(w) = self.handle.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    fn shutdown(&self) {
        if let Some(h) = self.hotkey.lock().take() {
            h.uninstall();
        }
        self.engines.shutdown();
        let f = runtime_dir().join("desktop.json");
        if std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["pid"].as_u64())
            == Some(std::process::id() as u64)
        {
            let _ = std::fs::remove_file(f);
        }
    }
}

struct TauriIo {
    app: std::sync::Weak<App>,
    handle: AppHandle,
}

impl Io for TauriIo {
    fn recorder(&self, on_level: LevelFn, on_end: EndFn, on_audio: Option<AudioFn>) -> Box<dyn Recorder> {
        mic::new_recorder(on_level, on_end, on_audio)
    }
    fn preview(&self, on_text: TextFn) -> Option<Stream> {
        let app = self.app.upgrade()?;
        if !app.config.lock().live_preview {
            return None;
        }
        app.preview.stream(app.current_language().as_deref(), on_text)
    }
    fn language(&self) -> Option<String> {
        self.app.upgrade().and_then(|a| a.current_language())
    }
    fn deliver(&self, text: &str, prefix: &str) -> Vec<&'static str> {
        output::deliver(text, prefix)
    }
    fn focused_window(&self) -> Option<String> {
        hyprland::focused_window()
    }
    fn notify(&self, message: &str) {
        output::notify(message);
    }
    fn overlay(&self, visible: bool) {
        if let Some(app) = self.app.upgrade() {
            app.overlay(visible);
        }
    }
    fn emit(&self, snap: &Snapshot) {
        emit_logged(&self.handle, "dictation", snap.clone());
    }
    fn clock(&self) -> f64 {
        self.app.upgrade().map(|a| a.started.elapsed().as_secs_f64()).unwrap_or(0.0)
    }
    fn model(&self) -> String {
        self.app.upgrade().map(|a| a.current_model()).unwrap_or_default()
    }
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("open-whisper")
}

/// Private (0700) runtime dir; refuses one that another user owns or a symlink.
#[cfg(unix)]
fn private_runtime_dir() -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let meta = std::fs::symlink_metadata(&dir)?;
    if meta.file_type().is_symlink() || meta.uid() != unsafe { libc_getuid() } {
        anyhow::bail!("{} belongs to another user", dir.display());
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

#[cfg(unix)]
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

#[cfg(not(unix))]
fn private_runtime_dir() -> anyhow::Result<PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ------------------------------------------------------------------ commands
#[tauri::command]
fn get_status(app: State<'_, AppRef>) -> Value {
    app.status()
}

#[tauri::command]
fn get_languages() -> Value {
    let mut map = serde_json::Map::new();
    for id in 0..=whisper_rs::get_lang_max_id() {
        if let (Some(code), Some(name)) = (whisper_rs::get_lang_str(id), whisper_rs::get_lang_str_full(id)) {
            map.insert(code.into(), json!(name));
        }
    }
    Value::Object(map)
}

fn valid_language(code: &str) -> bool {
    code == "auto" || (0..=whisper_rs::get_lang_max_id()).any(|i| whisper_rs::get_lang_str(i) == Some(code))
}

#[tauri::command]
fn transcribe_path(app: State<'_, AppRef>, path: String, language: String, model: String) -> Result<String, String> {
    if !language.is_empty() && !valid_language(&language) {
        return Err(format!("Unknown language code {language:?}."));
    }
    let p = PathBuf::from(&path);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
    if !p.is_file() {
        return Err(format!("{name}: file not found."));
    }
    let model = if models::spec(&model).is_some() { model } else { app.current_model() };
    let language = (language != "auto" && !language.is_empty()).then_some(language);
    app.jobs.submit(&name, Input { path: Some(p), pcm: None, language }, &model, 1).map_err(|e| e.to_string())
}

#[tauri::command]
fn job_snapshot(app: State<'_, AppRef>, id: String) -> Option<JobSnapshot> {
    app.jobs.snapshot(&id)
}

#[tauri::command]
fn take_open_files(app: State<'_, AppRef>) -> Vec<PathBuf> {
    std::mem::take(&mut *app.open_files.lock())
}

#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    if output::copy(&text) {
        Ok(())
    } else {
        Err("Could not copy to the clipboard.".into())
    }
}

/// Save an export where the user chooses in a native dialog opened here, so the page never
/// names a path itself. Returns false when the user cancels.
#[tauri::command]
async fn save_export(app: tauri::AppHandle, file_name: String, content: String) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let name = std::path::Path::new(&file_name).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "transcript.txt".into());
    let ext = name.rsplit('.').next().unwrap_or("txt").to_string();
    let Some(target) = app.dialog().file().set_file_name(&name).add_filter(ext.to_uppercase(), &[ext.as_str()]).blocking_save_file() else {
        return Ok(false);
    };
    let path = target.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| format!("Could not save {}: {e}", path.display()))?;
    Ok(true)
}

#[derive(Deserialize)]
struct OptionsIn {
    model: Option<String>,
    language: Option<String>,
    idle_unload_minutes: Option<u64>,
    live_preview: Option<bool>,
}

#[tauri::command]
fn set_options(app: State<'_, AppRef>, options: OptionsIn) -> Result<Value, String> {
    let model_changed;
    {
        let mut cfg = app.config.lock();
        let before = cfg.model.clone();
        if let Some(m) = options.model {
            if !m.is_empty() && models::spec(&m).is_none() {
                return Err(format!("Unknown model {m:?}"));
            }
            cfg.model = m;
        }
        if let Some(l) = options.language {
            if !valid_language(&l) {
                return Err(format!("Unknown language code {l:?}."));
            }
            cfg.language = l;
        }
        if let Some(n) = options.idle_unload_minutes {
            cfg.idle_unload_minutes = n;
            app.engines.idle_minutes.store(n, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(on) = options.live_preview {
            cfg.live_preview = on;
        }
        model_changed = cfg.model != before;
        cfg.save().map_err(|e| e.to_string())?;
    }
    if app.config.lock().live_preview {
        app.prepare_preview(); // no-op when it is ready or on its way
    }
    if model_changed {
        let engines = app.engines.clone();
        let model = app.current_model();
        std::thread::spawn(move || {
            let _ = engines.prepare(&model);
        });
    }
    app.emit_status();
    Ok(app.status())
}

#[tauri::command]
fn overlay_ready(app: State<'_, AppRef>) {
    app.overlay_ready.store(true, Ordering::SeqCst);
    if app.overlay_pending.load(Ordering::SeqCst) {
        app.overlay(true);
    }
}

#[tauri::command]
fn dictation_state(app: State<'_, AppRef>) -> Option<Snapshot> {
    app.dictation.lock().as_ref().map(|d| d.snapshot())
}

/// In-window dictation (mic button / Space in the main window): the app records the mic.
#[tauri::command]
fn window_dictation(app: State<'_, AppRef>, action: String) -> Result<Option<String>, String> {
    let app = app.inner().clone();
    let result = window_dictation_inner(&app, &action);
    emit_logged(&app.handle, "window-dictation", json!({"action": action, "result": format!("{result:?}")}));
    result
}

fn window_dictation_inner(app: &AppRef, action: &str) -> Result<Option<String>, String> {
    match action {
        "start" => {
            let mut slot = app.window_rec.lock();
            if slot.is_some() {
                return Err("Already recording.".into());
            }
            let h = app.handle.clone();
            let on_level: LevelFn = Arc::new(move |lv| {
                let _ = h.emit_to("main", "window-level", lv);
            });
            let h2 = app.handle.clone();
            let on_end: EndFn = Arc::new(move |reason| {
                let _ = h2.emit_to("main", "window-dictation-end", reason);
            });
            let mut rec = mic::new_recorder(on_level, on_end, None);
            rec.start().map_err(|e| format!("Could not open the microphone: {e}"))?;
            *slot = Some(rec);
            Ok(None)
        }
        "stop" => {
            let Some(mut rec) = app.window_rec.lock().take() else { return Ok(None) };
            let pcm = rec.stop();
            if (pcm.len() as f64) < dictation::MIN_RECORDING_S * crate::audio::RATE as f64 {
                return Err("That was too short. Hold Space (or click the mic) while you talk.".into());
            }
            let language = app.current_language();
            let model = app.current_model();
            app.jobs.submit("Dictation", Input { path: None, pcm: Some(pcm), language }, &model, 0).map(Some).map_err(|e| e.to_string())
        }
        "cancel" => {
            if let Some(mut rec) = app.window_rec.lock().take() {
                rec.stop();
            }
            Ok(None)
        }
        other => Err(format!("unknown action {other}")),
    }
}

/// X11, macOS, Windows: the OS lets a running app own a key (press and release).
fn register_global_shortcut(app: &AppRef, key: &str) -> String {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
    let Some(sc) = crate::config::to_plugin_shortcut(key) else {
        return format!("not added: cannot read the key {key:?}");
    };
    let weak = Arc::downgrade(app);
    let result = app.handle.global_shortcut().on_shortcut(sc.as_str(), move |_h, _s, ev| {
        let Some(app) = weak.upgrade() else { return };
        let Some(d) = app.dictation.lock().clone() else { return };
        match ev.state {
            ShortcutState::Pressed => {
                d.press();
            }
            ShortcutState::Released => {
                d.release();
            }
        }
    });
    match result {
        Ok(()) => format!("{key} — hold to talk, tap to start/stop (while open-whisper runs)"),
        Err(e) => format!("not added: {key} ({e}); it may be taken by another app. Use --hotkey."),
    }
}

// ------------------------------------------------------------------ entry
/// Headless commands that need no window. Returns true when handled.
pub fn headless(cli: &Cli) -> bool {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("open-whisper"));
    if cli.help {
        print!("{HELP}");
        return true;
    }
    if cli.version {
        println!("open-whisper {}", env!("CARGO_PKG_VERSION"));
        return true;
    }
    if let Some(kind) = &cli.print_snippet {
        let key = cli.hotkey.clone().unwrap_or_else(|| crate::config::DEFAULT_HOTKEY.into());
        println!("{}", if kind == "conf" { hyprland::conf_snippet(&key, &exe) } else { hyprland::permanent_snippet(&key, &exe) });
        return true;
    }
    if cli.remove_hotkey {
        let removed = if hyprland::running() { hyprland::remove_app_binds() } else { vec![] };
        println!("removed: {}", if removed.is_empty() { "nothing".into() } else { removed.join(", ") });
        return true;
    }
    if let Some(path) = &cli.transcribe {
        let cfg = Config::load();
        let engines = Engines::new(cfg.idle_unload_minutes);
        let model = cli
            .model
            .clone()
            .filter(|m| models::spec(m).is_some())
            .or_else(|| models::spec(&cfg.model).map(|m| m.id.to_string()))
            .unwrap_or_else(|| models::auto_model(&engines.hw).to_string());
        let t0 = Instant::now();
        let input = Input { path: Some(path.clone()), pcm: None, language: cli.language.clone() };
        let out = engines.transcribe(&model, input, Arc::new(|_| {}));
        match out {
            Ok(o) => println!(
                "{}",
                json!({"model": model, "device": engines.hw.gpu, "seconds": t0.elapsed().as_secs_f64(), "language": o.language,
                       "duration": o.duration, "text": crate::formats::to_txt(&o.segments).trim(), "segments": o.segments})
            ),
            Err(e) => {
                println!("{}", json!({"model": model, "error": format!("{e:#}")}));
                std::process::exit(1);
            }
        }
        engines.shutdown();
        return true;
    }
    false
}

pub fn run() {
    let cli = Cli::parse(std::env::args());
    if headless(&cli) {
        return;
    }
    let first_cli = cli.clone();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|handle, argv, _cwd| {
            // Another launch (the key bind, a file opened with the app, the menu entry): hand it over.
            match handle.try_state::<AppRef>() {
                Some(app) => app.handle_cli(&Cli::parse(argv), false),
                None => EARLY_ARGS.lock().push(argv),
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_languages,
            transcribe_path,
            job_snapshot,
            take_open_files,
            copy_text,
            save_export,
            set_options,
            dictation_state,
            overlay_ready,
            window_dictation
        ])
        .setup(move |tauri_app| {
            let cli = first_cli.clone();
            // A key release / cancel / quit with no app running: nothing to do.
            if matches!(cli.dictate.as_deref(), Some("release" | "cancel" | "quit")) {
                std::process::exit(0);
            }
            let handle = tauri_app.handle().clone();
            let config = Config::load();
            let engines = Engines::new(config.idle_unload_minutes);
            let h_jobs = handle.clone();
            let jobs = Jobs::new(engines.clone(), Arc::new(move |s: &JobSnapshot| {
                emit_logged(&h_jobs, "job", s.clone());
            }));
            let app = Arc::new(App {
                handle: handle.clone(),
                config: Mutex::new(config.clone()),
                cli: cli.clone(),
                engines: engines.clone(),
                jobs: jobs.clone(),
                dictation: Mutex::new(None),
                hotkey: Mutex::new(None),
                hotkey_text: Mutex::new(String::new()),
                window_rec: Mutex::new(None),
                open_files: Mutex::new(vec![]),
                started: Instant::now(),
                overlay_ready: AtomicBool::new(false),
                overlay_pending: AtomicBool::new(false),
                preview: Preview::new(),
            });
            let io = Arc::new(TauriIo { app: Arc::downgrade(&app), handle: handle.clone() });
            *app.dictation.lock() = Some(Dictation::new(io, jobs));
            let weak = Arc::downgrade(&app);
            engines.on_change(Arc::new(move || {
                if let Some(a) = weak.upgrade() {
                    a.emit_status();
                }
            }));
            // Files from the command line: queued before the page exists, so it always finds them.
            app.open_files.lock().extend(cli.files.iter().filter(|p| p.is_file()).cloned());
            tauri_app.manage(app.clone());

            // Windows: the main window, and the dictation overlay (hidden; never focused).
            let background = cli.background || cli.dictate.is_some();
            let main = WebviewWindowBuilder::new(tauri_app, "main", WebviewUrl::App("index.html".into()))
                .title("open-whisper")
                .inner_size(860.0, 820.0)
                .min_inner_size(420.0, 520.0)
                .visible(!background)
                .build()?;
            let main_for_close = main.clone();
            main.on_window_event(move |ev| {
                if let WindowEvent::CloseRequested { api, .. } = ev {
                    // Keep running for the dictation key; the tray menu quits.
                    api.prevent_close();
                    let _ = main_for_close.hide();
                }
            });
            WebviewWindowBuilder::new(tauri_app, "overlay", WebviewUrl::App("overlay.html".into()))
                .title(hyprland::OVERLAY_TITLE)
                .inner_size(380.0, 76.0)
                .min_inner_size(380.0, 76.0)
                .max_inner_size(380.0, 76.0)
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .focused(false)
                .visible(false)
                .build()?;
            // WebKitGTK asks for at least 200 px of height; the overlay is a 76 px strip.
            #[cfg(target_os = "linux")]
            {
                let overlay = tauri_app.get_webview_window("overlay").expect("overlay");
                let _ = overlay.with_webview(|wv| {
                    use gtk::prelude::WidgetExt;
                    wv.inner().set_size_request(380, 76);
                });
            }

            let open = MenuItem::with_id(tauri_app, "open", "Open open-whisper", true, None::<&str>)?;
            let quit = MenuItem::with_id(tauri_app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(tauri_app, &[&open, &quit])?;
            let tray_app = app.clone();
            TrayIconBuilder::with_id("main")
                .icon(tauri_app.default_window_icon().cloned().expect("app icon"))
                .tooltip("open-whisper")
                .menu(&menu)
                .on_menu_event(move |h, ev| match ev.id().as_ref() {
                    "open" => tray_app.show_main(),
                    "quit" => h.exit(0),
                    _ => {}
                })
                .build(tauri_app)?;

            // The ow-dictate shim (used by the bindings.lua block) finds us here.
            match private_runtime_dir() {
                Ok(dir) => {
                    let exe = std::env::current_exe().unwrap_or_default();
                    let _ = std::fs::write(dir.join("desktop.json"), json!({"exe": exe, "pid": std::process::id()}).to_string());
                }
                Err(e) => eprintln!("  Global dictation: cannot use {} ({e})", runtime_dir().display()),
            }

            // Global key (Hyprland).
            let key = cli.hotkey.clone().unwrap_or_else(|| config.hotkey.clone());
            let wayland_other = cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() && !hyprland::running();
            let text = if !hyprland::running() && (cli.no_hotkey || key.trim().is_empty()) {
                "not added (--no-hotkey)".to_string()
            } else if wayland_other {
                // GNOME/KDE/sway: apps cannot grab keys on Wayland; the desktop binds them.
                "this Wayland desktop does not let apps add keys: bind `open-whisper --dictate press` (and `--dictate release` on key release, or `--dictate toggle`) in your desktop's keyboard settings".to_string()
            } else if !hyprland::running() {
                register_global_shortcut(&app, &key)
            } else if cli.no_hotkey || key.trim().is_empty() {
                let removed = hyprland::remove_app_binds();
                let mut t = "not added (--no-hotkey)".to_string();
                if !removed.is_empty() {
                    t += &format!("; removed {}", removed.join(", "));
                }
                if hyprland::key_owners(crate::config::DEFAULT_HOTKEY).iter().any(|o| o == hyprland::CONFIG_DESC) {
                    t += ". The key in your Hyprland config (open-whisper.dictation block) still works; delete that block to turn it off";
                }
                t
            } else {
                let exe = std::env::current_exe().unwrap_or_default();
                let hk = hyprland::Hotkey::new(&key, &exe);
                let t = match hk.install(cli.dictate.is_some()) {
                    Ok(t) => t,
                    Err(e) => format!("could not add it ({e}); see `open-whisper --print-snippet`"),
                };
                hk.watch_reloads();
                *app.hotkey.lock() = Some(hk);
                t
            };
            println!("  open-whisper {}: {}, model {}", env!("CARGO_PKG_VERSION"), app.engines.hw.gpu.clone().map(|g| format!("GPU {g}")).unwrap_or("CPU".into()), app.current_model());
            println!("  Global dictation key: {text}");
            *app.hotkey_text.lock() = text;

            // Load the model now, so the first transcription is quick.
            let (e2, model) = (engines.clone(), app.current_model());
            std::thread::spawn(move || {
                if let Err(e) = e2.prepare(&model) {
                    eprintln!("  model {model}: {e:#}");
                }
            });
            // Live preview: 17 MB, downloaded once; dictation works without it meanwhile.
            if config.live_preview {
                app.prepare_preview();
            }
            app.handle_cli(&cli, true);
            for argv in std::mem::take(&mut *EARLY_ARGS.lock()) {
                app.handle_cli(&Cli::parse(argv), false);
            }
            Ok(())
        });

    let context = tauri::generate_context!();
    let app = builder.build(context).expect("open-whisper failed to start");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(a) = handle.try_state::<AppRef>() {
                a.shutdown();
            }
        }
    });
}
