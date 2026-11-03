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
use crate::{llm, meeting};
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
    /// X11, macOS, Windows: the key registered through the global-shortcut plugin.
    os_key: Mutex<Option<String>>,
    window_rec: Mutex<Option<Box<dyn Recorder>>>,
    open_files: Mutex<Vec<PathBuf>>,
    started: Instant,
    /// The overlay page has loaded and rendered (shown only then: no blank first frame).
    overlay_ready: AtomicBool,
    overlay_pending: AtomicBool,
    preview: Preview,
    /// The meeting being recorded (Meetings section), until it is saved.
    meeting: Mutex<Option<Arc<meeting::Session>>>,
    /// A meeting's devices are opening / it is being stopped.
    meeting_starting: AtomicBool,
    meeting_stopping: AtomicBool,
    /// The meeting key was tapped to stop while the meeting was starting.
    meeting_stop_pending: AtomicBool,
    /// Hyprland: the overlay's window rule, added when no key added it (no keys at all).
    overlay_rule: AtomicBool,
    /// The meeting key (same three parts as the dictation key's).
    meeting_hotkey: Mutex<Option<Arc<hyprland::Hotkey>>>,
    meeting_hotkey_text: Mutex<String>,
    meeting_os_key: Mutex<Option<String>>,
    /// The paste-last key (types the last dictation again).
    paste_hotkey: Mutex<Option<Arc<hyprland::Hotkey>>>,
    paste_hotkey_text: Mutex<String>,
    paste_os_key: Mutex<Option<String>>,
    /// Finished dictations (Settings → History), and the last one for the paste-last key.
    history: Arc<crate::history::History>,
    /// Settings' microphone test is running.
    mic_testing: AtomicBool,
    /// Who wants the overlay: [dictation, meeting]. It shows while either does.
    overlay_wants: [AtomicBool; 2],
    /// AI notes: one job at a time, and its cancel flag.
    ai_busy: Arc<AtomicBool>,
    ai_cancel: Arc<AtomicBool>,
    /// The meeting whose AI notes are being written.
    ai_job: Mutex<Option<String>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyKind {
    Dictation,
    Meeting,
    PasteLast,
}

impl KeyKind {
    const ALL: [KeyKind; 3] = [KeyKind::Dictation, KeyKind::Meeting, KeyKind::PasteLast];

    fn name(self) -> &'static str {
        match self {
            KeyKind::Dictation => "dictation",
            KeyKind::Meeting => "meeting",
            KeyKind::PasteLast => "paste-last",
        }
    }

    /// What a desktop bind runs for this key.
    fn flag(self) -> &'static str {
        match self {
            KeyKind::Dictation => "--dictate toggle",
            KeyKind::Meeting => "--meeting toggle",
            KeyKind::PasteLast => "--paste-last",
        }
    }
}

/// One global key's parts: the Hyprland key, the text shown in Options, the OS shortcut.
struct KeySlot<'a> {
    hotkey: &'a Mutex<Option<Arc<hyprland::Hotkey>>>,
    text: &'a Mutex<String>,
    os_key: &'a Mutex<Option<String>>,
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
            if models::spec(&id).is_some() {
                return id;
            }
        }
        models::auto_model(&self.engines.hw).to_string()
    }

    /// CLI --language > config.toml; None = detect.
    pub fn current_language(&self) -> Option<String> {
        let lang = self.cli.language.clone().unwrap_or_else(|| self.config.lock().language.clone());
        (!lang.is_empty() && lang != "auto").then_some(lang)
    }

    fn slot(&self, kind: KeyKind) -> KeySlot<'_> {
        match kind {
            KeyKind::Dictation => KeySlot { hotkey: &self.hotkey, text: &self.hotkey_text, os_key: &self.os_key },
            KeyKind::Meeting => KeySlot { hotkey: &self.meeting_hotkey, text: &self.meeting_hotkey_text, os_key: &self.meeting_os_key },
            KeyKind::PasteLast => KeySlot { hotkey: &self.paste_hotkey, text: &self.paste_hotkey_text, os_key: &self.paste_os_key },
        }
    }

    /// A key as it is now ("" = none). The meeting key: the chosen one also when it could not
    /// be added (Options then shows it "not active", with the reason).
    fn key_of(&self, kind: KeyKind) -> String {
        let slot = self.slot(kind);
        if let Some(h) = slot.hotkey.lock().as_ref() {
            return h.key();
        }
        if let Some(k) = slot.os_key.lock().clone() {
            return k;
        }
        match kind {
            KeyKind::Dictation => String::new(),
            KeyKind::Meeting => self.config.lock().meeting_hotkey.clone(),
            KeyKind::PasteLast => self.config.lock().paste_hotkey.clone(),
        }
    }

    /// The dictation key as it is now ("" = none).
    fn hotkey_key(&self) -> String {
        self.key_of(KeyKind::Dictation)
    }

    /// "app", "manual" or "none" (the meeting key never comes from the user's config).
    fn source_of(&self, kind: KeyKind) -> &'static str {
        if kind == KeyKind::Dictation {
            return self.hotkey_source();
        }
        if keys_are_manual() {
            return "manual";
        }
        let slot = self.slot(kind);
        if let Some(h) = slot.hotkey.lock().as_ref() {
            return h.source();
        }
        if slot.os_key.lock().is_some() {
            return "app";
        }
        "none"
    }

    /// Where the key lives: "app" (the app adds it and can change it), "config" (the user's
    /// Hyprland config block: the app can take it over), "manual" (the desktop's keyboard
    /// settings or a classic hyprland.conf), "none".
    fn hotkey_source(&self) -> &'static str {
        if keys_are_manual() {
            return "manual";
        }
        if let Some(h) = self.hotkey.lock().as_ref() {
            return h.source();
        }
        if self.os_key.lock().is_some() {
            return "app";
        }
        "none"
    }

    fn start_at_login(&self) -> bool {
        #[cfg(target_os = "linux")]
        return crate::autostart::enabled_in(&crate::autostart::dir()); // and its program exists
        #[cfg(not(target_os = "linux"))]
        {
            use tauri_plugin_autostart::ManagerExt;
            self.handle.autolaunch().is_enabled().unwrap_or(false)
        }
    }

    fn status_emitter(self: &Arc<Self>) -> Arc<dyn Fn() + Send + Sync> {
        let weak = Arc::downgrade(self);
        Arc::new(move || {
            if let Some(a) = weak.upgrade() {
                a.emit_status();
            }
        })
    }

    fn prepare_preview(self: &Arc<Self>) {
        let choice = self.config.lock().preview_engine.clone();
        self.preview.prepare(&choice, self.status_emitter());
    }

    fn status(&self) -> Value {
        let current = self.current_model();
        let cfg = self.config.lock().clone();
        let list: Vec<Value> = models::catalog()
            .map(|m| {
                let st = self.engines.state(m.id);
                json!({
                    "name": m.id, "label": m.label, "engine": m.engine, "approx_mb": m.download_mb,
                    "ram_mb": m.ram_mb, "speed_gpu": m.speed_gpu, "speed_cpu": m.speed_cpu, "wer": m.wer,
                                        "state": st.state, "downloaded": st.downloaded, "total": st.total, "progress": st.progress, "error": st.error,
                })
            })
            .collect();
        json!({
            "device": self.engines.hw.gpu.clone().map(|g| format!("GPU ({g})")).unwrap_or_else(|| "CPU".into()),
            "gpu": self.engines.hw.gpu.is_some(),
            "default_model": current,
            "auto_model": models::auto_model(&self.engines.hw),
            "models": list,
            "options": { "model": cfg.model, "language": cfg.language, "idle_unload_minutes": cfg.idle_unload_minutes, "live_preview": cfg.live_preview, "preview_engine": cfg.preview_engine, "vocabulary": cfg.vocabulary },
            "preview": self.preview.status(),
            "version": env!("CARGO_PKG_VERSION"),
            "hotkey": *self.hotkey_text.lock(),
            "hotkey_key": self.hotkey_key(),
            "hotkey_source": self.hotkey_source(),
            // Why the key is not active (source "none"): shown in Options.
            "hotkey_problem": if self.hotkey_source() == "none" && !self.hotkey_key().is_empty() { self.hotkey_text.lock().clone() } else { String::new() },
            "start_at_login": self.start_at_login(),
            "meeting_key": self.key_of(KeyKind::Meeting),
            "meeting_key_source": self.source_of(KeyKind::Meeting),
            "meeting_key_problem": if self.source_of(KeyKind::Meeting) == "none" && !self.key_of(KeyKind::Meeting).is_empty() { self.meeting_hotkey_text.lock().clone() } else { String::new() },
            "paste_key": self.key_of(KeyKind::PasteLast),
            "paste_key_source": self.source_of(KeyKind::PasteLast),
            "paste_key_problem": if self.source_of(KeyKind::PasteLast) == "none" && !self.key_of(KeyKind::PasteLast).is_empty() { self.paste_hotkey_text.lock().clone() } else { String::new() },
            "history_keep": cfg.history_keep,
            "recording": self.meeting.lock().is_some(),
            "ai": {
                "engine": cfg.ai_engine, "url": cfg.ai_url, "model": cfg.ai_model, "has_key": !cfg.ai_key.is_empty(),
                "auto": cfg.ai_auto,
                "builtin_model": llm::model(&cfg.ai_builtin_model).id,
                "builtin": llm::builtin_status(&cfg.ai_builtin_model),
            },
        })
    }

    fn emit_status(&self) {
        emit_logged(&self.handle, "status", self.status());
    }

    /// The dictation's wish (see `want_overlay`).
    fn overlay(&self, visible: bool) {
        self.want_overlay(0, visible);
    }

    /// 0 = dictation, 1 = meeting: the strip shows while either wants it.
    fn want_overlay(&self, who: usize, on: bool) {
        self.overlay_wants[who].store(on, Ordering::SeqCst);
        let visible = self.overlay_wants.iter().any(|w| w.load(Ordering::SeqCst));
        let rule = |h: &Mutex<Option<Arc<hyprland::Hotkey>>>| h.lock().as_ref().map(|h| h.rule_installed()).unwrap_or(false);
        let mut usable = rule(&self.hotkey) || rule(&self.meeting_hotkey) || rule(&self.paste_hotkey) || !hyprland::running();
        if visible && !usable {
            // No key added the window rule (no keys at all): add it now, once.
            if !self.overlay_rule.load(Ordering::SeqCst) && hyprland::install_overlay_rule() {
                self.overlay_rule.store(true, Ordering::SeqCst);
            }
            usable = self.overlay_rule.load(Ordering::SeqCst);
        }
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
        if cli.paste_last {
            let me = self.clone();
            std::thread::spawn(move || me.paste_last());
            return;
        }
        if let Some(action) = cli.meeting.clone() {
            // Opening the devices or finishing takes a moment: not on this thread.
            let me = self.clone();
            std::thread::spawn(move || me.meeting_key(&action));
            return;
        }
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

    /// The meeting key (or `--meeting`): start, stop or toggle the recording.
    fn meeting_key(self: &Arc<Self>, action: &str) {
        if self.meeting_starting.load(Ordering::SeqCst) {
            // A tap to stop while the devices open: remembered, done right after the start.
            if matches!(action, "toggle" | "stop") {
                self.meeting_stop_pending.store(true, Ordering::SeqCst);
            }
            return;
        }
        if self.meeting_stopping.load(Ordering::SeqCst) {
            return; // it is being saved already
        }
        let recording = self.meeting.lock().is_some();
        match (action, recording) {
            ("toggle" | "start", false) => match meeting_start_inner(self, "") {
                Ok(live) => {
                    emit_logged(&self.handle, "meeting-start", json!({"result": {"Ok": live.meeting.id}}));
                }
                Err(e) if e == ALREADY_RECORDING => {} // another tap won the start
                Err(e) => {
                    // The strip says why for a moment (it reads the meeting-start event).
                    emit_logged(&self.handle, "meeting-start", json!({"result": {"Err": e}}));
                    self.want_overlay(1, true);
                    output::notify(&format!("Meeting not started: {e}"));
                    self.hide_meeting_strip_later(4);
                }
            },
            ("toggle" | "stop", true) => {
                let _ = stop_meeting(self);
            }
            ("start" | "stop", _) => {} // already as asked
            other => eprintln!("  unknown --meeting action {other:?}"),
        }
    }

    /// The paste-last key: type the last dictation again into the app that has the focus.
    fn paste_last(&self) {
        match self.history.last() {
            Some(text) => {
                let delivered = output::deliver(&text, "");
                emit_logged(&self.handle, "paste-last", json!({"text": text, "delivered": delivered}));
            }
            None => {
                emit_logged(&self.handle, "paste-last", json!({"text": null, "delivered": []}));
                output::notify("No dictation to paste yet.");
            }
        }
    }

    /// After `secs`, the strip goes unless a meeting records again. The check and the change
    /// happen under the meeting's lock, so a meeting that starts meanwhile keeps its strip.
    fn hide_meeting_strip_later(self: &Arc<Self>, secs: u64) {
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            let slot = me.meeting.lock();
            if slot.is_none() && !me.meeting_starting.load(Ordering::SeqCst) {
                me.want_overlay(1, false);
            }
            drop(slot);
        });
    }

    fn show_main(&self) {
        if let Some(w) = self.handle.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    fn shutdown(&self) {
        if let Some(m) = self.meeting.lock().take() {
            m.abandon(); // keep what is transcribed
        }
        if let Some(h) = self.hotkey.lock().take() {
            h.uninstall();
        }
        if let Some(h) = self.meeting_hotkey.lock().take() {
            h.uninstall();
        }
        if let Some(h) = self.paste_hotkey.lock().take() {
            h.uninstall();
        }
        if self.overlay_rule.load(Ordering::SeqCst) {
            hyprland::remove_overlay_rule();
        }
        self.ai_cancel.store(true, Ordering::SeqCst);
        llm::stop_builtin();
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
    // Before the queue: a file that is too large never waits behind another job.
    if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) > crate::audio::MAX_FILE_BYTES {
        return Err(format!("{name} is larger than 2 GB."));
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
    preview_engine: Option<String>,
    start_at_login: Option<bool>,
    ai_engine: Option<String>,
    ai_builtin_model: Option<String>,
    ai_url: Option<String>,
    ai_model: Option<String>,
    /// Some("") removes the key.
    ai_key: Option<String>,
    ai_auto: Option<bool>,
    vocabulary: Option<Vec<crate::vocabulary::Term>>,
    history_keep: Option<String>,
    microphone: Option<String>,
    avoid_bluetooth_mic: Option<bool>,
}

#[tauri::command]
fn set_options(app: State<'_, AppRef>, options: OptionsIn) -> Result<Value, String> {
    let model_changed;
    let preview_on = options.live_preview == Some(true) || options.preview_engine.is_some();
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
        if let Some(e) = options.preview_engine.clone() {
            if !crate::preview::CHOICES.contains(&e.as_str()) {
                return Err(format!("Unknown preview engine {e:?}"));
            }
            cfg.preview_engine = e;
        }
        if let Some(e) = options.ai_engine {
            if !["builtin", "server"].contains(&e.as_str()) {
                return Err(format!("Unknown AI notes engine {e:?}"));
            }
            cfg.ai_engine = e;
        }
        if let Some(m) = options.ai_builtin_model {
            if !llm::MODELS.iter().any(|x| x.id == m) {
                return Err(format!("Unknown AI notes model {m:?}"));
            }
            cfg.ai_builtin_model = m;
        }
        if let Some(u) = options.ai_url {
            let u = u.trim().to_string();
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err("The server address starts with http:// or https://, e.g. http://localhost:11434/v1".into());
            }
            cfg.ai_url = u;
        }
        if let Some(m) = options.ai_model {
            cfg.ai_model = m.trim().to_string();
        }
        if let Some(k) = options.ai_key {
            cfg.ai_key = k.trim().to_string();
        }
        if let Some(on) = options.ai_auto {
            cfg.ai_auto = on;
        }
        if let Some(m) = options.microphone {
            cfg.microphone = m.trim().to_string();
        }
        if let Some(on) = options.avoid_bluetooth_mic {
            cfg.avoid_bluetooth_mic = on;
        }
        if let Some(k) = options.history_keep {
            if !crate::history::KEEP_CHOICES.contains(&k.as_str()) {
                return Err(format!("Unknown history setting {k:?}"));
            }
            app.history.set_keep(&k);
            cfg.history_keep = k;
        }
        if let Some(v) = options.vocabulary {
            cfg.vocabulary = crate::vocabulary::normalize(&v);
            app.engines.set_vocabulary(&cfg.vocabulary);
        }
        model_changed = cfg.model != before;
        cfg.save().map_err(|e| e.to_string())?;
    }
    // After the other options are valid and saved.
    if let Some(on) = options.start_at_login {
        set_start_at_login(app.inner(), on)?;
    }
    if preview_on && app.config.lock().live_preview {
        app.prepare_preview(); // turned on (again), or another engine: get it ready
    } else if options.live_preview == Some(false) {
        app.preview.off(app.status_emitter()); // frees its engine (Parakeet: ~1.1 GB)
    }
    if model_changed {
        let app2 = app.inner().clone();
        let model = app.current_model();
        std::thread::spawn(move || {
            let _ = app2.engines.prepare(&model);
            // "auto": a model just downloaded (Parakeet) can now serve the preview too.
            if app2.config.lock().live_preview {
                app2.prepare_preview();
            }
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

// ------------------------------------------------------------------ meetings
/// Start recording a meeting: the mic ("Me") and the computer's sound ("Them").
#[tauri::command]
async fn meeting_start(app: State<'_, AppRef>, title: String) -> Result<meeting::Live, String> {
    let app = app.inner().clone();
    // Off the window's thread: opening the devices can take seconds.
    let a2 = app.clone();
    let r = tauri::async_runtime::spawn_blocking(move || meeting_start_inner(&a2, &title)).await.map_err(|e| e.to_string()).and_then(|r| r);
    emit_logged(&app.handle, "meeting-start", json!({"result": r.as_ref().map(|l| l.meeting.id.clone()).map_err(|e| e.clone())}));
    r
}

const ALREADY_RECORDING: &str = "A meeting is already being recorded.";

/// Clears a flag when dropped: also when the code under it panics.
struct ClearOnDrop<'a>(&'a AtomicBool);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn meeting_start_inner(app: &AppRef, title: &str) -> Result<meeting::Live, String> {
    // Claim the start without holding the slot's lock while the devices open.
    if app.meeting.lock().is_some() || app.meeting_starting.swap(true, Ordering::SeqCst) {
        return Err(ALREADY_RECORDING.into());
    }
    app.meeting_stop_pending.store(false, Ordering::SeqCst);
    let r = {
        let _starting = ClearOnDrop(&app.meeting_starting);
        meeting_open(app, title)
    };
    if r.is_ok() && app.meeting_stop_pending.swap(false, Ordering::SeqCst) {
        let a2 = app.clone();
        std::thread::spawn(move || {
            let _ = stop_meeting(&a2); // the key was tapped again while it started
        });
    }
    r
}

fn meeting_open(app: &AppRef, title: &str) -> Result<meeting::Live, String> {
    let model = app.current_model();
    let language = app.current_language();
    let engines = app.engines.clone();
    let m = model.clone();
    let transcribe: meeting::TranscribeFn = Arc::new(move |pcm| {
        engines.transcribe(&m, Input { path: None, pcm: Some(pcm), language: language.clone() }, Arc::new(|_| {})).map(|o| o.segments)
    });
    let h = app.handle.clone();
    let emit: meeting::EmitFn = Arc::new(move |ev| match ev {
        meeting::Event::Live(live) => emit_logged(&h, "meeting", live),
        // To the window and the strip.
        meeting::Event::Levels { me, them } => {
            let _ = h.emit("meeting-level", json!({"me": me, "them": them}));
        }
    });
    let doc = meeting::Meeting::new(title, &model);
    let id = doc.id.clone();
    let session = meeting::Session::new(meeting::dir(), doc, transcribe, emit).map_err(|e| format!("Could not save the meeting: {e:#}"))?;
    if let Err(e) = session.open_sources() {
        session.abandon();
        let _ = meeting::delete_in(&meeting::dir(), &id);
        return Err(format!("{e:#}"));
    }
    *app.meeting.lock() = Some(session.clone());
    app.want_overlay(1, true); // the strip shows the recording, in every app
    app.emit_status();
    // The first piece comes after a few seconds: have the model loaded by then.
    let e2 = app.engines.clone();
    std::thread::spawn(move || {
        let _ = e2.prepare(&model);
    });
    Ok(session.live())
}

/// Stop, transcribe the last seconds, save. Returns the saved meeting.
#[tauri::command]
async fn meeting_stop(app: State<'_, AppRef>) -> Result<meeting::Meeting, String> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || stop_meeting(&app)).await.map_err(|e| e.to_string())?
}

/// Stop, transcribe the last seconds, save (blocking). The page's Stop and the meeting key.
fn stop_meeting(app: &AppRef) -> Result<meeting::Meeting, String> {
    let session = app.meeting.lock().clone().ok_or("No meeting is being recorded.")?;
    if app.meeting_stopping.swap(true, Ordering::SeqCst) {
        return Err("The meeting is being saved.".into());
    }
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.finish()));
    app.meeting.lock().take(); // only now: no second meeting while this one finishes
    app.meeting_stopping.store(false, Ordering::SeqCst);
    app.emit_status();
    // The strip says "saved" for a moment, then goes (unless a new meeting started).
    app.hide_meeting_strip_later(3);
    // AI notes right away. The built-in model downloads first when it is not there yet
    // (progress in the meeting; Cancel stops it).
    if let Ok(m) = &r {
        let cfg = app.config.lock().clone();
        let ready = if cfg.ai_engine == "server" { !cfg.ai_url.is_empty() } else { llm::builtin_status(&cfg.ai_builtin_model).supported };
        if cfg.ai_auto && ready && !m.lines.is_empty() {
            if let Err(why) = start_ai_notes(app, m.id.clone()) {
                // e.g. another meeting's notes are being written: say so in this meeting.
                emit_logged(&app.handle, "ai-notes", json!({"id": m.id, "state": "error", "detail": format!("Not written: {why} Then use Write notes."), "progress": 0.0, "text": ""}));
            }
        }
    }
    r.map_err(|_| {
        session.abandon(); // keep what is transcribed; the next meeting can start
        "Stopping failed; the meeting is saved as it was.".to_string()
    })
}

#[tauri::command]
fn meeting_live(app: State<'_, AppRef>) -> Option<meeting::Live> {
    app.meeting.lock().as_ref().map(|s| s.live())
}

#[tauri::command]
fn meetings_list() -> Vec<meeting::Summary> {
    meeting::list_in(&meeting::dir())
}

fn live_session(app: &AppRef, id: &str) -> Option<Arc<meeting::Session>> {
    app.meeting.lock().clone().filter(|s| s.id() == id)
}

#[tauri::command]
fn meeting_get(app: State<'_, AppRef>, id: String) -> Result<meeting::Meeting, String> {
    match live_session(app.inner(), &id) {
        Some(s) => Ok(s.meeting()),
        None => meeting::load_in(&meeting::dir(), &id).map_err(|e| format!("{e:#}")),
    }
}

#[tauri::command]
fn meeting_update(app: State<'_, AppRef>, id: String, title: Option<String>, notes: Option<String>) -> Result<(), String> {
    let title = title.map(|t| t.trim().chars().take(meeting::MAX_TITLE).collect::<String>()).map(|t| if t.is_empty() { "Meeting".to_string() } else { t });
    if notes.as_ref().is_some_and(|n| n.len() > meeting::MAX_NOTES) {
        return Err("The notes are too long (over 1 MB).".into());
    }
    if let Some(s) = live_session(app.inner(), &id) {
        s.update(title, notes);
        return Ok(());
    }
    let dir = meeting::dir();
    meeting::edit_in(&dir, &id, |m| {
        if let Some(t) = title {
            m.title = t;
        }
        if let Some(n) = notes {
            m.notes = n;
        }
    })
    .map(|_| ())
    .map_err(|e| format!("{e:#}"))
}

// ------------------------------------------------------------------ AI notes
/// Settings → Models → AI notes: the engine to use now.
fn ai_engine(app: &AppRef) -> llm::Engine {
    let cfg = app.config.lock().clone();
    if cfg.ai_engine == "server" {
        llm::Engine::Server { url: cfg.ai_url, model: cfg.ai_model, key: cfg.ai_key }
    } else {
        // Vulkan / Metal when the app found a GPU; the CPU else (slower, but it works).
        llm::Engine::Builtin { gpu: app.engines.hw.gpu.is_some(), model: cfg.ai_builtin_model }
    }
}

/// Write the AI notes of a saved meeting (in the background; "ai-notes" events follow it).
#[tauri::command]
fn meeting_ai_notes(app: State<'_, AppRef>, id: String) -> Result<(), String> {
    let app = app.inner().clone();
    if !meeting::valid_id(&id) {
        return Err(format!("unknown meeting {id:?}"));
    }
    if live_session(&app, &id).is_some() {
        return Err("Stop the recording first.".into());
    }
    start_ai_notes(&app, id)
}

#[tauri::command]
fn ai_notes_cancel(app: State<'_, AppRef>) {
    app.ai_cancel.store(true, Ordering::SeqCst);
}

fn start_ai_notes(app: &AppRef, id: String) -> Result<(), String> {
    let busy = llm::Busy::take(&app.ai_busy).ok_or_else(|| {
        if app.ai_job.lock().as_deref() == Some(id.as_str()) {
            "The AI notes of this meeting are being written.".to_string()
        } else {
            "AI notes are being written for another meeting. Wait for them, or cancel them.".to_string()
        }
    })?;
    app.ai_cancel.store(false, Ordering::SeqCst);
    *app.ai_job.lock() = Some(id.clone());
    let app = app.clone();
    std::thread::Builder::new()
        .name("ai-notes".into())
        .spawn(move || {
            let _busy = busy;
            run_ai_notes(&app, &id);
            *app.ai_job.lock() = None;
            app.emit_status();
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn run_ai_notes(app: &AppRef, id: &str) {
    let h = app.handle.clone();
    let send = |state: &str, detail: &str, progress: f64, text: &str| {
        emit_logged(&h, "ai-notes", json!({"id": id, "state": state, "detail": detail, "progress": progress, "text": text}));
    };
    send("preparing", "Getting ready…", 0.0, "");
    let result = (|| -> anyhow::Result<(String, String)> {
        let m = meeting::load_in(&meeting::dir(), id)?;
        anyhow::ensure!(!m.lines.is_empty() || !m.notes.trim().is_empty(), "this meeting has no transcript and no notes");
        let input = meeting::model_input(&m);
        // The language: the one set in Settings, else found in the transcript.
        let lang = match app.current_language().and_then(|c| (0..=whisper_rs::get_lang_max_id()).find(|&i| whisper_rs::get_lang_str(i) == Some(c.as_str()))) {
            Some(i) => {
                let name = whisper_rs::get_lang_str_full(i).unwrap_or("English");
                let mut c = name.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
            None => llm::language_of(&m.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ")).to_string(),
        };
        let engine = ai_engine(app);
        let label = match &engine {
            llm::Engine::Builtin { model, .. } => llm::model(model).label.to_string(),
            llm::Engine::Server { model, url, .. } => if model.is_empty() { url.clone() } else { model.clone() },
        };
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut progress = |what: &str, done: u64, total: u64| match what {
            "starting" => send("preparing", "Starting the model…", 0.0, ""),
            "writing" => send("writing", "Writing…", 0.0, ""),
            "checking the download" => send("preparing", "Checking the download…", 1.0, ""),
            what if last.elapsed() >= Duration::from_millis(250) || done == total => {
                last = Instant::now();
                let p = if total > 0 { done as f64 / total as f64 } else { 0.0 };
                send("downloading", &format!("Downloading {what} (once)… {}% of {} MB", (p * 100.0) as u32, total / 1_000_000), p, "");
            }
            _ => {}
        };
        let mut on_text = |t: &str| send("writing", "Writing…", 0.0, t);
        let names = crate::vocabulary::Vocabulary::new(&app.config.lock().vocabulary).names();
        let text = llm::write_notes(&engine, &input, &lang, &names, &mut progress, &mut on_text, &app.ai_cancel)?;
        Ok((text, label))
    })();
    match result {
        Ok((text, label)) => {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            match meeting::edit_in(&meeting::dir(), id, |m| {
                m.ai_notes = text.clone();
                m.ai_model = label.clone();
                m.ai_at = now;
            }) {
                Ok(_) => send("done", &label, 1.0, &text),
                Err(e) => send("error", &format!("The notes were written but not saved: {e:#}"), 0.0, &text),
            }
        }
        Err(_) if app.ai_cancel.load(Ordering::SeqCst) => send("cancelled", "Cancelled", 0.0, ""),
        Err(e) => send("error", &format!("{e:#}"), 0.0, ""),
    }
}

#[tauri::command]
fn meeting_delete(app: State<'_, AppRef>, id: String) -> Result<(), String> {
    if live_session(app.inner(), &id).is_some() {
        return Err("Stop the recording first.".into());
    }
    if app.ai_job.lock().as_deref() == Some(id.as_str()) {
        app.ai_cancel.store(true, Ordering::SeqCst); // its notes would have no meeting
    }
    meeting::delete_in(&meeting::dir(), &id).map_err(|e| format!("{e:#}"))
}

/// The meeting as Markdown (AI notes, your notes, then the transcript), for Copy and Export.
#[tauri::command]
fn meeting_markdown(app: State<'_, AppRef>, id: String, utc_offset_minutes: i32) -> Result<String, String> {
    let m = match live_session(app.inner(), &id) {
        Some(s) => s.meeting(),
        None => meeting::load_in(&meeting::dir(), &id).map_err(|e| format!("{e:#}"))?,
    };
    Ok(meeting::to_markdown(&m, utc_offset_minutes.clamp(-14 * 60, 14 * 60)))
}

/// Wayland desktops other than Hyprland, or Hyprland without a Lua config: the user binds the
/// key in the desktop's settings (apps cannot).
fn keys_are_manual() -> bool {
    let wayland_other = cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() && !hyprland::running();
    wayland_other || (hyprland::running() && !hyprland::lua_config())
}

/// Options → Keys → Dictation key → Change: the key moves now and is saved to config.toml.
#[tauri::command]
fn set_hotkey(app: State<'_, AppRef>, key: String) -> Result<Value, String> {
    let app = app.inner().clone();
    let r = set_key_inner(&app, &key, KeyKind::Dictation);
    emit_logged(&app.handle, "hotkey-change", json!({"key": key, "result": r.as_ref().map(|_| "ok").map_err(|e| e.clone())}));
    r
}

/// Settings → Shortcuts → Paste last dictation → Change.
#[tauri::command]
fn set_paste_hotkey(app: State<'_, AppRef>, key: String) -> Result<Value, String> {
    let app = app.inner().clone();
    let r = set_key_inner(&app, &key, KeyKind::PasteLast);
    emit_logged(&app.handle, "paste-key-change", json!({"key": key, "result": r.as_ref().map(|_| "ok").map_err(|e| e.clone())}));
    r
}

// ------------------------------------------------------------------ microphone
/// Settings → General → Microphone: the inputs, and the one the app uses now (with why).
#[tauri::command]
fn microphones(app: State<'_, AppRef>) -> Value {
    let cfg = app.config.lock().clone();
    let mics = crate::microphones::list();
    let using = crate::microphones::resolve(&cfg.microphone, cfg.avoid_bluetooth_mic, &mics);
    json!({"mics": mics, "using": using, "saved": cfg.microphone, "avoid_bluetooth": cfg.avoid_bluetooth_mic, "bluetooth_aware": cfg!(target_os = "linux")})
}

/// Settings → General → Microphone → Test: the chosen microphone for 4 s, its level as
/// "mic-level" events (nothing is kept).
#[tauri::command]
fn mic_test(app: State<'_, AppRef>) -> Result<(), String> {
    if app.mic_testing.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let h = app.handle.clone();
    let on_level: LevelFn = Arc::new(move |lv| {
        let _ = h.emit_to("main", "mic-level", lv);
    });
    let mut rec = mic::new_tap(on_level, Arc::new(|_| {}), Arc::new(|_: &[f32], _| {}));
    if let Err(e) = rec.start() {
        app.mic_testing.store(false, Ordering::SeqCst);
        return Err(format!("Could not open the microphone: {e}"));
    }
    let (app2, h2) = (app.inner().clone(), app.handle.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        rec.stop();
        app2.mic_testing.store(false, Ordering::SeqCst);
        let _ = h2.emit_to("main", "mic-level", -1.0f32); // the test ended
    });
    Ok(())
}

// ------------------------------------------------------------------ history
#[tauri::command]
fn history_list(app: State<'_, AppRef>, query: String) -> Vec<crate::history::Entry> {
    app.history.list(&query)
}

#[tauri::command]
fn history_delete(app: State<'_, AppRef>, id: String) -> bool {
    app.history.delete(&id)
}

#[tauri::command]
fn history_clear(app: State<'_, AppRef>) {
    app.history.clear();
}

/// Options → Keys → Meeting key → Change.
#[tauri::command]
fn set_meeting_hotkey(app: State<'_, AppRef>, key: String) -> Result<Value, String> {
    let app = app.inner().clone();
    let r = set_key_inner(&app, &key, KeyKind::Meeting);
    emit_logged(&app.handle, "meeting-key-change", json!({"key": key, "result": r.as_ref().map(|_| "ok").map_err(|e| e.clone())}));
    r
}

fn set_key_inner(app: &AppRef, key: &str, kind: KeyKind) -> Result<Value, String> {
    let app = app.clone();
    let slot = app.slot(kind);
    let new = hyprland::normalize_key(key)
        .ok_or_else(|| format!("{key} cannot be used: hold Super, Ctrl or Alt and press a letter, a digit, F1-F24 or Space."))?;
    if let Some(other) = KeyKind::ALL.into_iter().find(|&k| k != kind && app.key_of(k) == new) {
        return Err(format!("{new} is already the {} key.", other.name()));
    }
    let text = if keys_are_manual() {
        return Err(format!("This desktop does not let apps add keys: bind `open-whisper {}` in its keyboard settings.", kind.flag()));
    } else if hyprland::running() {
        let existing = slot.hotkey.lock().clone();
        match existing {
            Some(hk) => hk.change(&new).map_err(|e| format!("{e:#}"))?,
            None => {
                // Started without a key: install it (the bind and the overlay's window rule).
                let exe = std::env::current_exe().unwrap_or_default();
                let hk = match kind {
                    KeyKind::Dictation => hyprland::Hotkey::new(&new, &exe),
                    KeyKind::Meeting => hyprland::Hotkey::new_meeting(&new, &exe),
                    KeyKind::PasteLast => hyprland::Hotkey::new_paste_last(&new, &exe),
                };
                let t = hk.install(false).map_err(|e| format!("{e:#}"))?;
                if hk.source() != "app" {
                    return Err(t);
                }
                hk.watch_reloads();
                *slot.hotkey.lock() = Some(hk);
                t
            }
        }
    } else {
        use tauri_plugin_global_shortcut::GlobalShortcutExt;
        let old = slot.os_key.lock().clone();
        if old.as_deref() != Some(new.as_str()) {
            if let Some(sc) = old.as_deref().and_then(crate::config::to_plugin_shortcut) {
                let _ = app.handle.global_shortcut().unregister(sc.as_str());
            }
            let t = register_global_shortcut(&app, &new, kind);
            if t.starts_with("not added") {
                // Put the old key back, so the user is never left without one.
                if let Some(o) = old.as_deref() {
                    register_global_shortcut(&app, o, kind);
                }
                return Err(t);
            }
            t
        } else {
            slot.text.lock().clone()
        }
    };
    {
        let mut cfg = app.config.lock();
        match kind {
            KeyKind::Dictation => cfg.hotkey = new,
            KeyKind::Meeting => cfg.meeting_hotkey = new,
            KeyKind::PasteLast => cfg.paste_hotkey = new,
        }
        cfg.save().map_err(|e| e.to_string())?;
    }
    *slot.text.lock() = text;
    // A key moved: another key that could not be added (it was the same combination) may
    // work now.
    for other in [KeyKind::Meeting, KeyKind::PasteLast] {
        if other != kind && app.source_of(other) == "none" {
            let k = app.key_of(other);
            if !k.is_empty() && KeyKind::ALL.into_iter().all(|x| x == other || app.key_of(x) != k) {
                let _ = set_key_inner(&app, &k, other);
            }
        }
    }
    app.emit_status();
    Ok(app.status())
}

/// Options → "Let the app manage it": the key comes from the open-whisper block in the user's
/// Hyprland config. Remove that block (backup next to it), reload, and add the key from the app.
#[tauri::command]
async fn adopt_hotkey(app: State<'_, AppRef>) -> Result<Value, String> {
    let app = app.inner().clone();
    // Off the window's thread: it edits a file and waits for Hyprland to reload.
    tauri::async_runtime::spawn_blocking(move || adopt_hotkey_inner(&app)).await.map_err(|e| e.to_string())?
}

fn adopt_hotkey_inner(app: &AppRef) -> Result<Value, String> {
    let hk = app.hotkey.lock().clone().ok_or("No Hyprland key to take over.")?;
    let r = hk.adopt().map_err(|e| format!("{e:#}"));
    emit_logged(&app.handle, "hotkey-adopt", json!({"result": r.as_ref().map(|b| b.display().to_string()).map_err(|e| e.clone())}));
    let backup = r?;
    // The config's bind survived a logout; the app's bind needs the app: start it at login.
    let login = set_start_at_login(app, true).is_ok();
    *app.hotkey_text.lock() = format!("{} — managed by the app", hk.key());
    {
        let mut cfg = app.config.lock();
        cfg.hotkey = hk.key();
        let _ = cfg.save();
    }
    app.emit_status();
    Ok(json!({"status": app.status(), "backup": backup, "start_at_login": login}))
}

fn set_start_at_login(app: &AppRef, on: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        crate::autostart::set_in(&crate::autostart::dir(), on, &crate::autostart::launcher()).map_err(|e| format!("Start at login: {e}"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        let al = app.handle.autolaunch();
        if on { al.enable() } else { al.disable() }.map_err(|e| format!("Start at login: {e}"))
    }
}

/// X11, macOS, Windows: the OS lets a running app own a key (press and release).
fn register_global_shortcut(app: &AppRef, key: &str, kind: KeyKind) -> String {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
    let Some(sc) = crate::config::to_plugin_shortcut(key) else {
        return format!("not added: cannot read the key {key:?}");
    };
    let weak = Arc::downgrade(app);
    let result = app.handle.global_shortcut().on_shortcut(sc.as_str(), move |_h, _s, ev| {
        let Some(app) = weak.upgrade() else { return };
        if kind == KeyKind::Meeting {
            if ev.state == ShortcutState::Pressed {
                std::thread::spawn(move || app.meeting_key("toggle"));
            }
            return;
        }
        if kind == KeyKind::PasteLast {
            if ev.state == ShortcutState::Pressed {
                std::thread::spawn(move || app.paste_last());
            }
            return;
        }
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
        Ok(()) => {
            *app.slot(kind).os_key.lock() = Some(key.to_string());
            match kind {
                KeyKind::Meeting => format!("{key} — tap to start or stop a meeting recording (while open-whisper runs)"),
                KeyKind::PasteLast => format!("{key} — types the last dictation again (while open-whisper runs)"),
                KeyKind::Dictation => format!("{key} — hold to talk, tap to start/stop (while open-whisper runs)"),
            }
        }
        Err(e) => format!("not added: {key} ({e}); it may be taken by another app. Choose another key in Options."),
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
        engines.set_vocabulary(&cfg.vocabulary);
        let model = cli
            .model
            .clone()
            .filter(|m| models::spec(m).is_some())
            .or_else(|| models::spec(&cfg.model).map(|m| m.id.to_string()))
            .unwrap_or_else(|| models::auto_model(&engines.hw).to_string());
        // Load first, so "seconds" is the transcription alone (what the app does with a loaded model).
        let t_load = Instant::now();
        let _ = engines.prepare(&model);
        let load_seconds = t_load.elapsed().as_secs_f64();
        let t0 = Instant::now();
        let input = Input { path: Some(path.clone()), pcm: None, language: cli.language.clone() };
        let out = engines.transcribe(&model, input, Arc::new(|_| {}));
        match out {
            Ok(o) => println!(
                "{}",
                json!({"model": model, "device": engines.hw.gpu, "load_seconds": load_seconds, "seconds": t0.elapsed().as_secs_f64(), "language": o.language,
                       "duration": o.duration, "text": crate::formats::to_txt(&o.segments).trim(), "segments": o.segments})
            ),
            Err(e) => {
                println!("{}", json!({"model": model, "error": format!("{e:#}")}));
                std::process::exit(1);
            }
        }
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
            set_hotkey,
            set_meeting_hotkey,
            set_paste_hotkey,
            history_list,
            microphones,
            mic_test,
            history_delete,
            history_clear,
            adopt_hotkey,
            dictation_state,
            overlay_ready,
            window_dictation,
            meeting_start,
            meeting_stop,
            meeting_live,
            meetings_list,
            meeting_get,
            meeting_update,
            meeting_delete,
            meeting_markdown,
            meeting_ai_notes,
            ai_notes_cancel
        ])
        .setup(move |tauri_app| {
            let cli = first_cli.clone();
            // A key release / cancel / quit with no app running: nothing to do.
            if matches!(cli.dictate.as_deref(), Some("release" | "cancel" | "quit")) || cli.meeting.as_deref() == Some("stop") {
                std::process::exit(0);
            }
            if cli.paste_last {
                // The app was not running: type the last dictation from the history file, and end.
                let keep = Config::load().history_keep;
                match crate::history::History::open(crate::history::path(), &keep).last() {
                    Some(text) => {
                        output::deliver(&text, "");
                    }
                    None => output::notify("No dictation to paste yet."),
                }
                std::process::exit(0);
            }
            let handle = tauri_app.handle().clone();
            let config = Config::load();
            let engines = Engines::new(config.idle_unload_minutes);
            engines.set_vocabulary(&config.vocabulary);
            let h_jobs = handle.clone();
            // Every finished dictation goes to the history (the global key's jobs are
            // "dictation", the window's "Dictation"); files do not.
            let history = Arc::new(crate::history::History::open(crate::history::path(), &config.history_keep));
            let h_hist = history.clone();
            let jobs = Jobs::new(engines.clone(), Arc::new(move |s: &JobSnapshot| {
                emit_logged(&h_jobs, "job", s.clone());
                if s.state == "done" && (s.filename == "dictation" || s.filename == "Dictation") {
                    if let Some(r) = &s.result {
                        let source = if s.filename == "dictation" { "key" } else { "window" };
                        if let Some(e) = h_hist.add(&r.text, source, &s.model, r.duration) {
                            emit_logged(&h_jobs, "history", e);
                        }
                    }
                }
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
                os_key: Mutex::new(None),
                window_rec: Mutex::new(None),
                open_files: Mutex::new(vec![]),
                started: Instant::now(),
                overlay_ready: AtomicBool::new(false),
                overlay_pending: AtomicBool::new(false),
                // Parakeet in the preview is freed after the same idle time as the main model.
                preview: Preview::new({
                    let e = engines.clone();
                    Arc::new(move || match e.idle_minutes.load(Ordering::Relaxed) {
                        0 => None,
                        m => Some(Duration::from_secs(m * 60)),
                    })
                }),
                meeting: Mutex::new(None),
                meeting_starting: AtomicBool::new(false),
                meeting_stopping: AtomicBool::new(false),
                meeting_stop_pending: AtomicBool::new(false),
                overlay_rule: AtomicBool::new(false),
                meeting_hotkey: Mutex::new(None),
                meeting_hotkey_text: Mutex::new(String::new()),
                meeting_os_key: Mutex::new(None),
                paste_hotkey: Mutex::new(None),
                paste_hotkey_text: Mutex::new(String::new()),
                paste_os_key: Mutex::new(None),
                history: history.clone(),
                mic_testing: AtomicBool::new(false),
                overlay_wants: [AtomicBool::new(false), AtomicBool::new(false)],
                ai_busy: Arc::new(AtomicBool::new(false)),
                ai_cancel: Arc::new(AtomicBool::new(false)),
                ai_job: Mutex::new(None),
            });
            // A model runner that a crash left behind holds ~3 GB: stop it.
            std::thread::spawn(llm::stop_stale);
            // Every microphone open (dictation, meetings, the test) asks which one, now.
            let weak_mic = Arc::downgrade(&app);
            mic::set_resolver(Arc::new(move || {
                let app = weak_mic.upgrade()?;
                let (saved, avoid) = {
                    let c = app.config.lock();
                    (c.microphone.clone(), c.avoid_bluetooth_mic)
                };
                crate::microphones::resolve(&saved, avoid, &crate::microphones::list()).id
            }));
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
            let background = cli.background || cli.dictate.is_some() || cli.meeting.is_some();
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
                // Transparent: the page draws the rounded strip, the corners show what is below.
                .transparent(true)
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
                register_global_shortcut(&app, &key, KeyKind::Dictation)
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

            // The meeting key: the same ways as the dictation key, one bind (a tap).
            let mkey = config.meeting_hotkey.clone();
            let mtext = if cli.no_hotkey || mkey.trim().is_empty() {
                "not added".to_string()
            } else if wayland_other {
                "this Wayland desktop does not let apps add keys: bind `open-whisper --meeting toggle` in your desktop's keyboard settings".to_string()
            } else if mkey == app.hotkey_key() {
                format!("not added: {mkey} is the dictation key. Choose another key in Options.")
            } else if !hyprland::running() {
                register_global_shortcut(&app, &mkey, KeyKind::Meeting)
            } else {
                let hk = hyprland::Hotkey::new_meeting(&mkey, &std::env::current_exe().unwrap_or_default());
                let t = match hk.install(cli.meeting.is_some()) {
                    Ok(t) => t,
                    Err(e) => format!("could not add it ({e})"),
                };
                hk.watch_reloads();
                *app.meeting_hotkey.lock() = Some(hk);
                t
            };
            println!("  Meeting key: {mtext}");
            *app.meeting_hotkey_text.lock() = mtext;

            // The paste-last key: one bind, like the meeting key.
            let pkey = config.paste_hotkey.clone();
            let ptext = if cli.no_hotkey || pkey.trim().is_empty() {
                "not added".to_string()
            } else if wayland_other {
                "this Wayland desktop does not let apps add keys: bind `open-whisper --paste-last` in your desktop's keyboard settings".to_string()
            } else if pkey == app.hotkey_key() || pkey == app.key_of(KeyKind::Meeting) {
                format!("not added: {pkey} is already another open-whisper key. Choose another key in Settings → Shortcuts.")
            } else if !hyprland::running() {
                register_global_shortcut(&app, &pkey, KeyKind::PasteLast)
            } else {
                let hk = hyprland::Hotkey::new_paste_last(&pkey, &std::env::current_exe().unwrap_or_default());
                let t = match hk.install(cli.paste_last) {
                    Ok(t) => t,
                    Err(e) => format!("could not add it ({e})"),
                };
                hk.watch_reloads();
                *app.paste_hotkey.lock() = Some(hk);
                t
            };
            println!("  Paste-last key: {ptext}");
            *app.paste_hotkey_text.lock() = ptext;

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

    // Start at login on macOS/Windows (Linux: src/autostart.rs writes the XDG entry itself).
    #[cfg(not(target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--background"])));
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
