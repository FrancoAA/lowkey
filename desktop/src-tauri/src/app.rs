//! Tauri app: windows, tray, single instance, commands and the global key.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
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
    /// The pointer is on the strip: a finished meeting's strip stays (its link can be clicked).
    overlay_hovered: AtomicBool,
    /// Note apps: per meeting, the newest title/notes edit (its note is saved a moment after the last one).
    export_edits: Mutex<std::collections::HashMap<String, u64>>,
    /// Meetings whose voices are being told apart (after Stop).
    voices_working: Mutex<std::collections::BTreeSet<String>>,
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
    /// Finished dictations (History), and the last one for the paste-last key.
    history: Arc<crate::history::History>,
    /// Settings' microphone test is running.
    mic_testing: AtomicBool,
    /// The paste-last key is typing.
    pasting: AtomicBool,
    /// A call offered now (the strip says so; the meeting key records it with its name).
    call_offer: Mutex<Option<(crate::calls::Call, Instant)>>,
    /// The meeting recording a detected call: its id, the call, started automatically.
    call_session: Mutex<Option<(String, crate::calls::Call, bool)>>,
    /// Calls seen while the app runs (Settings lists them with their policy).
    calls_seen: Mutex<Vec<crate::calls::Call>>,
    /// Hyprland: the strip's drag while the mouse button is down.
    overlay_drag: Mutex<Option<hyprland::OverlayDrag>>,
    /// Who wants the overlay: [dictation, meeting]. It shows while either does.
    overlay_wants: [AtomicBool; 2],
    /// The strip is the meeting's capsule (a meeting records, or its "saved" shows).
    overlay_capsule: AtomicBool,
    /// AI notes and questions: one job at a time, and its cancel flag.
    ai_busy: Arc<AtomicBool>,
    ai_cancel: Arc<AtomicBool>,
    /// The meeting whose AI notes are being written.
    ai_job: Mutex<Option<String>>,
    /// The meeting whose question is being answered.
    chat_job: Mutex<Option<String>>,
    /// Exit asks what to do with the work in progress (one question at a time), or waits for
    /// that work to end (Quit when done).
    quit_asking: AtomicBool,
    quit_waiting: Arc<AtomicBool>,
    /// The tray menu as last built (it is built again only when this changes).
    tray_menu: Mutex<Vec<crate::tray::Entry>>,
    /// The dictation's state, for the tray (kept here: the dictation reports it while it
    /// holds its own lock).
    tray_dictation: Mutex<String>,
    /// The microphones, looked up now and then off the menu's thread (None: not choosable).
    tray_mics: Mutex<Option<Vec<crate::microphones::Mic>>>,
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
    test_log(event, &payload);
    let _ = handle.emit(event, payload);
}

fn test_log<S: serde::Serialize>(event: &str, payload: &S) {
    if let Some(path) = std::env::var_os("OPENWHISPER_TEST_LOG") {
        use std::io::Write;
        static LOG: Mutex<()> = Mutex::new(()); // one line at a time from all threads
        let _guard = LOG.lock();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(format!("{}\n", json!({"event": event, "payload": payload})).as_bytes());
        }
    }
}

/// The same test seam for what only the page sees (the video player): "page-…" events. The page
/// sends them only when the status says `test`.
#[tauri::command]
fn test_event(event: String, payload: Value) {
    if event.starts_with("page-") {
        test_log(&event, &payload);
    }
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
            "options": { "model": cfg.model, "language": cfg.language, "idle_unload_minutes": cfg.idle_unload_minutes, "live_preview": cfg.live_preview, "preview_engine": cfg.preview_engine, "vocabulary": cfg.vocabulary, "microphone": cfg.microphone, "theme": cfg.theme },
            "preview": self.preview.status(),
            "version": env!("CARGO_PKG_VERSION"),
            "hotkey": *self.hotkey_text.lock(),
            "hotkey_key": self.hotkey_key(),
            "hotkey_source": self.hotkey_source(),
            // Why the key is not active (source "none"): shown in Options.
            "hotkey_problem": if self.hotkey_source() == "none" && !self.hotkey_key().is_empty() { self.hotkey_text.lock().clone() } else { String::new() },
            "start_at_login": self.start_at_login(),
            "meeting_key": self.key_of(KeyKind::Meeting),
            "overlay_collapsed": self.config.lock().overlay_collapsed,
            "overlay_capsule": self.overlay_capsule.load(Ordering::SeqCst),
            "meeting_key_source": self.source_of(KeyKind::Meeting),
            "meeting_key_problem": if self.source_of(KeyKind::Meeting) == "none" && !self.key_of(KeyKind::Meeting).is_empty() { self.meeting_hotkey_text.lock().clone() } else { String::new() },
            "paste_key": self.key_of(KeyKind::PasteLast),
            "paste_key_source": self.source_of(KeyKind::PasteLast),
            "paste_key_problem": if self.source_of(KeyKind::PasteLast) == "none" && !self.key_of(KeyKind::PasteLast).is_empty() { self.paste_hotkey_text.lock().clone() } else { String::new() },
            "history_keep": cfg.history_keep,
            "calls": {
                "detect": cfg.call_detection,
                "available": cfg!(target_os = "linux"),
                "apps": self.call_apps(&cfg),
            },
            "recording": self.meeting.lock().is_some(),
            // OPENWHISPER_TEST_LOG: the page reports its player's events (`test_event`).
            "test": std::env::var_os("OPENWHISPER_TEST_LOG").is_some(),
            "ai": {
                "engine": cfg.ai_engine, "url": cfg.ai_url, "model": cfg.ai_model, "has_key": !cfg.ai_key.is_empty(),
                "auto": cfg.ai_auto,
                // The instructions for the notes: the user's ("" = none), and the built-in ones.
                "prompt": cfg.ai_prompt, "default_prompt": llm::NOTES_PROMPT,
                "builtin_model": llm::model(&cfg.ai_builtin_model).id,
                "builtin": llm::builtin_status(&cfg.ai_builtin_model),
            },
            // Settings → Connections → AI agents: the command an agent starts (`lowkey --mcp`).
            "agents": { "recording": cfg.mcp_recording, "program": crate::autostart::launcher() },
            // Settings → Meetings → Tell the other voices apart.
            "voices": {
                "on": cfg.meeting_voices, "supported": crate::voices::SUPPORTED, "present": crate::voices::present(),
                "download": crate::voices::download_state(),
                "working": *self.voices_working.lock(),
            },
            // Settings → Meetings → Screen video; `chosen`: a screen is remembered.
            "screen": {
                "supported": crate::screen::SUPPORTED, "chosen": !cfg.screen_token.is_empty(),
                "check": crate::screen::status(),
            },
            // Settings → Connections → Note apps; `vault`: the Obsidian vault the folder is in.
            "export": {
                "dir": cfg.export_dir, "auto": cfg.export_auto, "parts": cfg.export_parts,
                "vault": (!cfg.export_dir.is_empty()).then(|| crate::export::obsidian_vault(std::path::Path::new(&cfg.export_dir))).flatten()
                    .and_then(|v| v.file_name().map(|n| n.to_string_lossy().into_owned())),
            },
        })
    }

    fn emit_status(&self) {
        emit_logged(&self.handle, "status", self.status());
        self.refresh_tray();
    }

    // ------------------------------------------------------------- tray menu
    fn tray_view(&self) -> crate::tray::View {
        use crate::tray::{Meeting, MicItem, ModelItem, View};
        let cfg = self.config.lock().clone();
        let meeting = if self.meeting_stopping.load(Ordering::SeqCst) {
            Meeting::Saving
        } else if self.meeting_starting.load(Ordering::SeqCst) {
            Meeting::Starting
        } else if self.meeting.lock().is_some() {
            Meeting::Recording
        } else {
            Meeting::Idle
        };
        let auto = models::auto_model(&self.engines.hw);
        View {
            dictation: self.tray_dictation.lock().clone(),
            meeting,
            models: models::catalog()
                .map(|m| {
                    let st = self.engines.state(m.id);
                    ModelItem {
                        id: m.id.into(),
                        label: m.label.into(),
                        downloaded: matches!(st.state.as_str(), "downloaded" | "ready" | "loading"),
                        downloading: st.state == "downloading",
                        download_mb: m.download_mb,
                    }
                })
                .collect(),
            // A saved name this build does not know counts as automatic (as current_model does).
            model_choice: if models::spec(&cfg.model).is_some() { cfg.model.clone() } else { String::new() },
            auto_model: models::spec(auto).map(|m| m.label).unwrap_or(auto).to_string(),
            mics: self.tray_mics.lock().as_ref().map(|l| {
                l.iter().map(|m| MicItem { id: m.id.clone(), name: m.name.clone(), bluetooth: m.bluetooth, default: m.default }).collect()
            }),
            mic_choice: cfg.microphone.clone(),
            omarchy_shell: crate::tray::omarchy_shell(),
        }
    }

    /// Build the tray menu again when what it shows has changed.
    fn refresh_tray(&self) {
        let entries = crate::tray::entries(&self.tray_view());
        {
            let mut last = self.tray_menu.lock();
            if *last == entries {
                return;
            }
            *last = entries.clone();
        }
        let Some(tray) = self.handle.tray_by_id("main") else { return };
        match crate::tray::build(&self.handle, &entries) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(e) => eprintln!("  tray menu: {e}"),
        }
    }

    /// The microphones for the tray (pactl can take a moment: never on the menu's thread).
    fn refresh_tray_mics(&self) {
        let mics = crate::microphones::selectable().then(crate::microphones::list);
        *self.tray_mics.lock() = mics;
        self.refresh_tray();
    }

    fn on_tray(self: &Arc<Self>, id: &str) {
        use crate::tray::Action;
        let Some(action) = crate::tray::action(id) else { return };
        match action {
            Action::Dictate => {
                let d = self.dictation.lock().clone();
                if let Some(d) = d {
                    d.toggle();
                }
            }
            Action::Meeting => {
                let me = self.clone();
                std::thread::spawn(move || me.meeting_key("toggle", "", false));
            }
            Action::Model(m) => {
                let me = self.clone();
                std::thread::spawn(move || {
                    if let Err(e) = apply_options(&me, OptionsIn::only(json!({ "model": m }))) {
                        output::notify(&format!("Model not changed: {e}"));
                    }
                });
            }
            Action::Mic(m) => {
                let me = self.clone();
                std::thread::spawn(move || {
                    if let Err(e) = apply_options(&me, OptionsIn::only(json!({ "microphone": m }))) {
                        output::notify(&format!("Microphone not changed: {e}"));
                    }
                });
            }
            Action::Open => self.show_main(),
            Action::Exit => self.request_quit(),
        }
        // A click on a check item ticks it by itself: always build the menu again.
        self.tray_menu.lock().clear();
        self.refresh_tray();
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
                if !w.is_visible().unwrap_or(false) {
                    self.place_overlay_os(&w);
                }
                let _ = w.show();
                emit_logged(&self.handle, "overlay", true);
            } else if !visible {
                if w.is_visible().unwrap_or(false) {
                    self.remember_overlay_place();
                }
                let _ = w.hide();
                emit_logged(&self.handle, "overlay", false);
            }
        }
        if who == 1 && !on {
            self.set_overlay_capsule(false); // the strip again (for a dictation, or the next time)
        }
        if !visible {
            hyprland::overlay_hidden(); // the place that changed while it showed, for the next time
        }
    }

    /// The meeting's wish, and its shape: the capsule for a recording and its "saved", the strip
    /// for what needs words (a call offered, a meeting that did not start).
    fn want_meeting_overlay(&self, capsule: bool) {
        self.set_overlay_capsule(capsule);
        self.want_overlay(1, true);
    }

    // ------------------------------------------------------------- the strip's place
    fn overlay_place(&self) -> hyprland::OverlayPlace {
        let c = self.config.lock();
        hyprland::OverlayPlace { collapsed: c.overlay_collapsed, capsule: self.overlay_capsule.load(Ordering::SeqCst), at: c.overlay_at }
    }

    /// Save a new place (only when it changed): the config, and the Hyprland rule.
    fn save_overlay_place(&self, place: hyprland::OverlayPlace) {
        let round = |a: [f32; 2]| [(a[0] * 1000.0).round() / 1000.0, (a[1] * 1000.0).round() / 1000.0];
        let place = hyprland::OverlayPlace { at: place.at.map(round), ..place };
        {
            let mut c = self.config.lock();
            if c.overlay_collapsed == place.collapsed && c.overlay_at == place.at {
                return;
            }
            c.overlay_collapsed = place.collapsed;
            c.overlay_at = place.at;
            if let Err(e) = c.save() {
                eprintln!("  strip place not saved: {e}");
            }
        }
        self.publish_overlay_place();
    }

    /// The place now to the Hyprland rule (the size at the next show) and to the strip's page.
    fn publish_overlay_place(&self) {
        let place = self.overlay_place();
        hyprland::set_overlay_place(place);
        emit_logged(&self.handle, "overlay-place", json!({ "collapsed": place.collapsed, "capsule": place.capsule, "at": place.at }));
    }

    /// Where the strip is now, as fractions of the room around it (None: not on screen, or the
    /// system does not say: e.g. Wayland without Hyprland).
    fn overlay_fractions_now(&self) -> Option<[f32; 2]> {
        if hyprland::running() {
            return hyprland::overlay_geometry().map(|g| g.fractions());
        }
        let w = self.handle.get_webview_window("overlay")?;
        let pos = w.outer_position().ok()?;
        let size = w.outer_size().ok()?;
        let area = *w.current_monitor().ok()??.work_area();
        let f = |p: i32, a: i32, s: u32, room: u32| {
            let room = room as f64 - s as f64;
            if room <= 0.0 { 0.5 } else { ((p - a) as f64 / room).clamp(0.0, 1.0) as f32 }
        };
        Some([f(pos.x, area.position.x, size.width, area.size.width), f(pos.y, area.position.y, size.height, area.size.height)])
    }

    /// The user may have moved the strip: keep where it is now (if it is not where the app put it).
    fn remember_overlay_place(&self) {
        let Some(now) = self.overlay_fractions_now() else { return };
        let place = self.overlay_place();
        let expected = place.at.unwrap_or_else(|| self.default_fractions());
        if (now[0] - expected[0]).abs() < 0.004 && (now[1] - expected[1]).abs() < 0.004 {
            return; // where the app put it (a pixel or two off): nothing to save
        }
        self.save_overlay_place(hyprland::OverlayPlace { at: Some(now), ..place });
    }

    /// Bottom centre, as fractions (for comparing: the rule says it in pixels).
    fn default_fractions(&self) -> [f32; 2] {
        if let Some(g) = hyprland::overlay_geometry() {
            let (_, h) = self.overlay_place().size();
            let room = g.monitor_size.1 - h as f64;
            let y = g.monitor_size.1 - 74.0 - h as f64;
            return [0.5, if room > 0.0 { (y / room).clamp(0.0, 1.0) as f32 } else { 1.0 }];
        }
        [0.5, 0.92]
    }

    /// Not Hyprland: put the strip where the user left it (or bottom centre) on the monitor
    /// that has the mouse pointer. Hyprland: its window rule does this.
    fn place_overlay_os(&self, w: &tauri::WebviewWindow) {
        if hyprland::running() {
            return;
        }
        let monitor = self
            .handle
            .cursor_position()
            .ok()
            .and_then(|p| self.handle.monitor_from_point(p.x, p.y).ok().flatten())
            .or_else(|| self.handle.primary_monitor().ok().flatten());
        let Some(m) = monitor else { return };
        let area = *m.work_area();
        let place = self.overlay_place();
        let (lw, lh) = place.size();
        let scale = m.scale_factor();
        let (pw, ph) = ((lw as f64 * scale).round() as i32, (lh as f64 * scale).round() as i32);
        let room = |total: u32, size: i32| (total as i32 - size).max(0) as f64;
        let [fx, fy] = place.at.unwrap_or([0.5, -1.0]);
        let x = area.position.x + (room(area.size.width, pw) * fx as f64).round() as i32;
        let y = if fy < 0.0 {
            area.position.y + area.size.height as i32 - ph - (74.0 * scale).round() as i32
        } else {
            area.position.y + (room(area.size.height, ph) * fy as f64).round() as i32
        };
        let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
    }

    /// Open ⇄ collapsed: the window's size now, and for the next times. The centre stays.
    fn set_overlay_collapsed(&self, collapsed: bool) {
        let old = self.overlay_place();
        if old.collapsed == collapsed {
            return;
        }
        let new = hyprland::OverlayPlace { collapsed, ..old };
        let at = self.resize_overlay_window(old, new);
        self.save_overlay_place(hyprland::OverlayPlace { collapsed, at, ..new });
    }

    /// The meeting's capsule on or off: the window's size limits now (before it shows), its
    /// size on screen a moment later (not on this thread: it waits for GTK).
    fn set_overlay_capsule(&self, on: bool) {
        if self.overlay_capsule.swap(on, Ordering::SeqCst) == on {
            return;
        }
        let new = self.overlay_place();
        let old = hyprland::OverlayPlace { capsule: !on, ..new };
        self.publish_overlay_place();
        if old.size() == new.size() {
            return;
        }
        if let Some(win) = self.handle.get_webview_window("overlay") {
            overlay_size_limits(&win, new.size(), new.size().0 < old.size().0);
        }
        let Some(me) = self.handle.try_state::<AppRef>().map(|a| a.inner().clone()) else { return };
        std::thread::spawn(move || {
            let at = me.resize_overlay_window(old, new);
            if me.overlay_capsule.load(Ordering::SeqCst) == on {
                me.save_overlay_place(hyprland::OverlayPlace { at, ..me.overlay_place() });
            }
        });
    }

    /// The window from `old`'s size to `new`'s, its centre kept. Returns where it is then (as
    /// fractions), or `new.at` when the system does not say.
    fn resize_overlay_window(&self, old: hyprland::OverlayPlace, new: hyprland::OverlayPlace) -> Option<[f32; 2]> {
        let (w, h) = new.size();
        if old.size() == (w, h) {
            return new.at;
        }
        let Some(win) = self.handle.get_webview_window("overlay") else { return new.at };
        // Not Hyprland: where it is before (to keep its centre).
        let before = (win.outer_position().ok(), win.outer_size().ok());
        overlay_size_limits(&win, (w, h), w < old.size().0);
        let _ = win.set_size(tauri::LogicalSize::new(w as f64, h as f64));
        let mut at = new.at;
        if hyprland::running() {
            // Give GTK a moment to send the new size limits, else Hyprland keeps the old size.
            std::thread::sleep(std::time::Duration::from_millis(120));
            if let Some(g) = hyprland::overlay_geometry() {
                if let Some((x, y)) = hyprland::resize_overlay(&g, (w, h)) {
                    let moved = hyprland::OverlayGeometry { at: (x, y), size: (w as f64, h as f64), ..g };
                    at = Some(moved.fractions());
                }
            }
        } else if win.is_visible().unwrap_or(false) {
            // Keep the centre (Windows, macOS, X11). Hidden: placed when it shows.
            let scale = win.scale_factor().unwrap_or(1.0);
            if let (Some(p), Some(b)) = before {
                let (pw, ph) = ((w as f64 * scale) as i32, (h as f64 * scale) as i32);
                let _ = win.set_position(tauri::PhysicalPosition::new(p.x + (b.width as i32 - pw) / 2, p.y + (b.height as i32 - ph) / 2));
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
            at = self.overlay_fractions_now().or(at);
        }
        at
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
            let (title, agent) = (cli.title.clone().unwrap_or_default(), cli.agent);
            std::thread::spawn(move || me.meeting_key(&action, &title, agent));
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

    /// The meeting key (or `--meeting`): start, stop or toggle the recording. `title`: for a
    /// start that is not an offered call. `agent`: an AI agent asks (the MCP server); only with
    /// Settings → Connections → AI agents → "Let agents start and stop recordings", and the user is told.
    fn meeting_key(self: &Arc<Self>, action: &str, title: &str, agent: bool) {
        if agent && !self.config.lock().mcp_recording {
            output::notify("An AI agent asked to start or stop a meeting recording; that is off in Settings → Connections → AI agents.");
            return;
        }
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
            ("toggle" | "start", false) => {
                // A call is offered: the recording is that call (its name, and "ended" later).
                let offered = self.call_offer.lock().take().map(|(c, _)| c);
                let title = offered.as_ref().map(|c| format!("{} call", c.label)).unwrap_or_else(|| title.to_string());
                match meeting_start_inner(self, &title) {
                Ok(live) => {
                    if let Some(c) = offered {
                        *self.call_session.lock() = Some((live.meeting.id.clone(), c, false));
                    }
                    emit_logged(&self.handle, "meeting-start", json!({"result": {"Ok": live.meeting.id}}));
                    if agent {
                        output::notify("An AI agent started a meeting recording. Stop it on the strip or with the meeting key.");
                    }
                }
                Err(e) if e == ALREADY_RECORDING => {} // another tap won the start
                Err(e) => {
                    if let Some(c) = offered {
                        // The call stays offered: another tap can still record it by its name.
                        *self.call_offer.lock() = Some((c, Instant::now()));
                    }
                    // The strip says why for a moment (it reads the meeting-start event).
                    emit_logged(&self.handle, "meeting-start", json!({"result": {"Err": e}}));
                    self.want_meeting_overlay(false);
                    output::notify(&format!("Meeting not started: {e}"));
                    self.hide_meeting_strip_later(4);
                }
                }
            }
            ("toggle" | "stop", true) => {
                if stop_meeting(self).is_ok() && agent {
                    output::notify("An AI agent stopped the meeting recording; it is saved.");
                }
            }
            ("start" | "stop", _) => {} // already as asked
            other => eprintln!("  unknown --meeting action {other:?}"),
        }
    }

    /// Calls for Settings: the ones with a saved policy, and the ones seen now.
    fn call_apps(&self, cfg: &Config) -> Vec<Value> {
        let mut out: Vec<(String, String, String)> = cfg.call_policies.iter().map(|(k, p)| (k.clone(), crate::calls::label_for(k), p.clone())).collect();
        for c in self.calls_seen.lock().iter() {
            if !out.iter().any(|(k, _, _)| *k == c.key) {
                out.push((c.key.clone(), c.label.clone(), "ask".into()));
            }
        }
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out.into_iter().map(|(key, label, policy)| json!({"key": key, "label": label, "policy": policy})).collect()
    }

    /// Call detection (Linux): look once a second at who uses the microphone.
    #[cfg(target_os = "linux")]
    fn watch_calls(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("calls".into())
            .spawn(move || {
                let mut detector = crate::calls::Detector::default();
                loop {
                    std::thread::sleep(Duration::from_secs(2));
                    let Some(app) = weak.upgrade() else { return };
                    if !app.config.lock().call_detection {
                        continue;
                    }
                    let Some(json) = crate::microphones::mic_users_json() else { continue };
                    let users = crate::calls::parse_users(&json);
                    if users.is_empty() && detector_idle(&app) {
                        let _ = detector.step(Instant::now(), vec![]);
                        continue;
                    }
                    // Window titles only when a browser asks (one hyprctl call per look at most).
                    let titles = std::cell::OnceCell::new();
                    let calls: Vec<crate::calls::Call> =
                        users.iter().filter_map(|u| crate::calls::classify(u, &|pid| titles.get_or_init(window_titles).get(&pid).cloned())).collect();
                    for action in detector.step(Instant::now(), calls) {
                        app.on_call(action);
                    }
                }
            })
            .ok();
    }

    #[cfg(target_os = "linux")]
    fn on_call(self: &Arc<Self>, action: crate::calls::Action) {
        use crate::calls::Action;
        match action {
            Action::Offer(call) => {
                {
                    let mut seen = self.calls_seen.lock();
                    if !seen.iter().any(|c| c.key == call.key) {
                        seen.push(call.clone());
                    }
                }
                if self.meeting.lock().is_some() || self.meeting_starting.load(Ordering::SeqCst) {
                    return; // already recording
                }
                let policy = self.config.lock().call_policies.get(&call.key).cloned().unwrap_or_else(|| "ask".into());
                emit_logged(&self.handle, "call", json!({"state": "detected", "key": call.key, "label": call.label, "policy": policy}));
                match policy.as_str() {
                    "never" => {}
                    "always" => {
                        let title = format!("{} call", call.label);
                        match meeting_start_inner(self, &title) {
                            Ok(live) => {
                                *self.call_session.lock() = Some((live.meeting.id.clone(), call.clone(), true));
                                emit_logged(&self.handle, "meeting-start", json!({"result": {"Ok": live.meeting.id}}));
                                emit_logged(&self.handle, "call", json!({"state": "auto", "key": call.key, "label": call.label}));
                            }
                            Err(e) if e == ALREADY_RECORDING => {}
                            Err(e) => {
                                // Say so, and leave the call offered: the meeting key can still record it.
                                emit_logged(&self.handle, "meeting-start", json!({"result": {"Err": e}}));
                                output::notify(&format!("The {} call was not recorded: {e}", call.label));
                                *self.call_offer.lock() = Some((call.clone(), Instant::now()));
                                self.want_meeting_overlay(false);
                                self.hide_meeting_strip_later(4);
                            }
                        }
                    }
                    _ => {
                        *self.call_offer.lock() = Some((call.clone(), Instant::now()));
                        let key = self.key_of(KeyKind::Meeting);
                        emit_logged(&self.handle, "call", json!({"state": "offer", "key": call.key, "label": call.label, "meeting_key": key}));
                        self.want_meeting_overlay(false);
                        // Not taken in 20 s: the offer goes (and the strip, unless a meeting records).
                        let me = self.clone();
                        let k = call.key.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(20));
                            let still = me.call_offer.lock().as_ref().is_some_and(|(c, _)| c.key == k);
                            if still {
                                *me.call_offer.lock() = None;
                                emit_logged(&me.handle, "call", json!({"state": "gone", "key": k}));
                                me.hide_meeting_strip_later(0);
                            }
                        });
                    }
                }
            }
            Action::Ended(call) => {
                // An offer not taken: it goes.
                let offered = self.call_offer.lock().as_ref().is_some_and(|(c, _)| c.key == call.key);
                if offered {
                    *self.call_offer.lock() = None;
                    emit_logged(&self.handle, "call", json!({"state": "gone", "key": call.key}));
                    self.hide_meeting_strip_later(0);
                }
                let session = self.call_session.lock().clone();
                let live_id = self.meeting.lock().as_ref().map(|s| s.id());
                if let Some((id, c, auto)) = session {
                    if c.key == call.key && live_id.as_deref() == Some(id.as_str()) {
                        if auto {
                            // Started by itself: it stops by itself.
                            emit_logged(&self.handle, "call", json!({"state": "ended", "key": call.key, "label": call.label, "auto": true}));
                            let me = self.clone();
                            std::thread::spawn(move || {
                                let _ = stop_meeting(&me);
                            });
                        } else {
                            let key = self.key_of(KeyKind::Meeting);
                            emit_logged(&self.handle, "call", json!({"state": "ended", "key": call.key, "label": call.label, "auto": false, "meeting_key": key}));
                        }
                    }
                }
            }
        }
    }

    /// The paste-last key: type the last dictation again into the app that has the focus.
    fn paste_last(&self) {
        // One at a time: a second press while it types is dropped (no interleaved text).
        if self.pasting.swap(true, Ordering::SeqCst) {
            return;
        }
        let _done = ClearOnDrop(&self.pasting);
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
            // Not under the pointer (someone reads it, or goes for its link); at most a minute
            // more. The strip keeps showing "saved" while it is held.
            let mut held = false;
            for _ in 0..120 {
                if !me.pointer_on_overlay() {
                    break;
                }
                if !held {
                    held = true;
                    let _ = me.handle.emit("overlay-hold", true);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if held {
                let _ = me.handle.emit("overlay-hold", false);
            }
            let slot = me.meeting.lock();
            if slot.is_none() && !me.meeting_starting.load(Ordering::SeqCst) {
                me.want_overlay(1, false);
            }
            drop(slot);
        });
    }

    /// The pointer is on the strip: Hyprland says where the cursor is; elsewhere the strip's page
    /// says it (its pointer enter and leave).
    fn pointer_on_overlay(&self) -> bool {
        hyprland::cursor_on_overlay().unwrap_or_else(|| !hyprland::running() && self.overlay_hovered.load(Ordering::SeqCst))
    }

    fn show_main(&self) {
        if let Some(w) = self.handle.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }

    /// The work that quitting now would stop, one line each (with what is lost).
    fn work_in_progress(&self) -> Vec<String> {
        let title = |id: &str| {
            let t = meeting::load_in(&meeting::dir(), id).map(|m| m.title).unwrap_or_default();
            if t.trim().is_empty() { "a meeting".to_string() } else { format!("“{}”", t.trim()) }
        };
        let mut out = Vec::new();
        if self.meeting.lock().is_some() {
            out.push(if self.meeting_stopping.load(Ordering::SeqCst) {
                "Saving a meeting: its last seconds may not be transcribed.".to_string()
            } else {
                "Recording a meeting: it is saved as it is now.".to_string()
            });
        }
        for id in self.voices_working.lock().iter() {
            let last_try = crate::voices::retry_path(&meeting::dir(), id).exists();
            out.push(if last_try {
                format!("Telling the voices apart in {}: the others stay “Them”.", title(id))
            } else {
                format!("Telling the voices apart in {}: it goes on the next time Lowkey starts.", title(id))
            });
        }
        if let Some(id) = self.ai_job.lock().clone() {
            out.push(format!("Writing the notes of {}: Write notes writes them later.", title(&id)));
        }
        if let Some(id) = self.chat_job.lock().clone() {
            out.push(format!("Answering a question about {}.", title(&id)));
        }
        match self.jobs.pending() {
            0 => {}
            1 => out.push("Transcribing a file.".into()),
            n => out.push(format!("Transcribing {n} files.")),
        }
        out
    }

    /// Exit (the tray menu, the side menu): at once when nothing is at work. Else the user
    /// chooses: quit when the work is done, quit now (the work stops), or keep running.
    fn request_quit(self: &Arc<Self>) {
        let work = self.work_in_progress();
        if work.is_empty() {
            self.handle.exit(0);
            return;
        }
        if self.quit_asking.swap(true, Ordering::SeqCst) {
            return; // the question is on screen
        }
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};
        const WHEN_DONE: &str = "Quit when done";
        const NOW: &str = "Quit now";
        let text = format!(
            "Lowkey is still at work:\n\n• {}\n\nQuit when done waits for it, then quits (a meeting being recorded is stopped and saved first).",
            work.join("\n• ")
        );
        let me = self.clone();
        self.handle
            .dialog()
            .message(text)
            .title("Quit Lowkey?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::YesNoCancelCustom(WHEN_DONE.into(), NOW.into(), "Keep running".into()))
            .show_with_result(move |r| {
                me.quit_asking.store(false, Ordering::SeqCst);
                match r {
                    MessageDialogResult::Custom(b) if b == NOW => me.handle.exit(0),
                    MessageDialogResult::Custom(b) if b == WHEN_DONE => me.quit_when_done(),
                    _ => me.quit_waiting.store(false, Ordering::SeqCst), // Keep running: also no quit later
                }
            });
    }

    /// Quit when done: a meeting being recorded is stopped and saved; then, when nothing has
    /// been at work for 2 s (the notes start right after the voices), the app quits.
    fn quit_when_done(self: &Arc<Self>) {
        if self.quit_waiting.swap(true, Ordering::SeqCst) {
            return; // waiting already
        }
        output::notify("Lowkey quits when its work is done.");
        let me = self.clone();
        std::thread::spawn(move || {
            if me.meeting.lock().is_some() && !me.meeting_stopping.load(Ordering::SeqCst) {
                let _ = stop_meeting(&me);
            }
            let mut idle_since: Option<Instant> = None;
            while me.quit_waiting.load(Ordering::SeqCst) {
                if me.work_in_progress().is_empty() {
                    let at = *idle_since.get_or_insert_with(Instant::now);
                    if at.elapsed() >= Duration::from_secs(2) {
                        me.handle.exit(0);
                        return;
                    }
                } else {
                    idle_since = None;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
    }

    fn shutdown(&self) {
        if self.handle.get_webview_window("overlay").and_then(|w| w.is_visible().ok()).unwrap_or(false) {
            self.remember_overlay_place();
        }
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
        // The others' sound of a meeting whose voices were being told apart stays: the next
        // start tells them apart (`resume_voices`).
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
        if let Some(app) = self.app.upgrade() {
            let changed = {
                let mut st = app.tray_dictation.lock();
                let changed = *st != snap.state;
                *st = snap.state.clone();
                changed
            };
            if changed {
                app.refresh_tray();
            }
        }
    }
    fn clock(&self) -> f64 {
        self.app.upgrade().map(|a| a.started.elapsed().as_secs_f64()).unwrap_or(0.0)
    }
    fn model(&self) -> String {
        self.app.upgrade().map(|a| a.current_model()).unwrap_or_default()
    }
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(crate::rename::NAME)
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

#[derive(Deserialize, Default)]
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
    /// Some("") (or the built-in text) goes back to the built-in instructions.
    ai_prompt: Option<String>,
    mcp_recording: Option<bool>,
    meeting_voices: Option<bool>,
    /// Some("") turns the note-app folder off.
    export_dir: Option<String>,
    export_auto: Option<bool>,
    export_parts: Option<Vec<String>>,
    vocabulary: Option<Vec<crate::vocabulary::Term>>,
    history_keep: Option<String>,
    /// system | light | dark.
    theme: Option<String>,
    microphone: Option<String>,
    avoid_bluetooth_mic: Option<bool>,
    call_detection: Option<bool>,
    /// One call's policy: { key, policy }.
    call_policy: Option<CallPolicyIn>,
}

impl OptionsIn {
    /// Some options from JSON (the tray menu's choices); the others stay as they are.
    fn only(v: Value) -> Self {
        serde_json::from_value(v).unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct CallPolicyIn {
    key: String,
    policy: String,
}

#[tauri::command]
fn set_options(app: State<'_, AppRef>, options: OptionsIn) -> Result<Value, String> {
    apply_options(app.inner(), options)
}

/// Save options from the window or the tray menu, and act on them (a new model is prepared).
fn apply_options(app: &AppRef, options: OptionsIn) -> Result<Value, String> {
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
        if let Some(p) = options.ai_prompt {
            let p = p.trim();
            if p.chars().count() > MAX_PROMPT_CHARS {
                return Err(format!("The instructions are too long (at most {MAX_PROMPT_CHARS} characters)."));
            }
            // The built-in text, unchanged: "" (so it follows the app's next improvements).
            cfg.ai_prompt = if p == llm::NOTES_PROMPT.trim() { String::new() } else { p.to_string() };
        }
        if let Some(on) = options.mcp_recording {
            cfg.mcp_recording = on;
        }
        if let Some(on) = options.meeting_voices {
            if on && !crate::voices::SUPPORTED {
                return Err("This build cannot tell voices apart (Linux x86_64 only).".into());
            }
            cfg.meeting_voices = on;
        }
        if let Some(d) = options.export_dir {
            let d = d.trim().to_string();
            if !d.is_empty() && !(std::path::Path::new(&d).is_absolute() && std::path::Path::new(&d).is_dir()) {
                return Err(format!("{d} is not a folder."));
            }
            cfg.export_dir = d;
        }
        if let Some(on) = options.export_auto {
            cfg.export_auto = on;
        }
        if let Some(p) = options.export_parts {
            if p.iter().any(|x| !crate::export::PARTS.contains(&x.as_str())) {
                return Err(format!("Unknown parts {p:?}"));
            }
            cfg.export_parts = p;
        }
        if let Some(on) = options.call_detection {
            cfg.call_detection = on;
        }
        if let Some(cp) = options.call_policy {
            if !crate::calls::POLICIES.contains(&cp.policy.as_str()) || cp.key.is_empty() || cp.key.len() > 64 {
                return Err(format!("Unknown call setting {:?}", cp.policy));
            }
            cfg.call_policies.insert(cp.key, cp.policy);
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
        if let Some(t) = options.theme {
            if !THEMES.contains(&t.as_str()) {
                return Err(format!("Unknown appearance {t:?}"));
            }
            set_window_theme(&app.handle, &t);
            cfg.theme = t;
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
        set_start_at_login(app, on)?;
    }
    if options.meeting_voices == Some(true) && !crate::voices::present() {
        let a2 = app.clone();
        std::thread::spawn(move || crate::voices::download_models(|| a2.emit_status()));
    }
    if preview_on && app.config.lock().live_preview {
        app.prepare_preview(); // turned on (again), or another engine: get it ready
    } else if options.live_preview == Some(false) {
        app.preview.off(app.status_emitter()); // frees its engine (Parakeet: ~1.1 GB)
    }
    if model_changed {
        let app2 = app.clone();
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

/// Settings → Meetings → Notes: the longest instructions (the built-in ones are about 2,600).
const MAX_PROMPT_CHARS: usize = 20_000;

/// Two key strings name the same combination ("super+alt+v" = "SUPER + ALT + V").
fn same_key(a: &str, b: &str) -> bool {
    let n = |k: &str| hyprland::normalize_key(k).unwrap_or_else(|| k.split_whitespace().collect::<String>().to_uppercase());
    !a.trim().is_empty() && n(a) == n(b)
}

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
    if app.config.lock().meeting_voices && crate::voices::SUPPORTED {
        if !crate::voices::present() {
            session.notice("The voice models are not downloaded yet (Settings → Meetings → Who is speaking): this meeting keeps \"Them\".".into());
        } else if let Err(e) = session.keep_their_sound(&crate::voices::tape_path(&meeting::dir(), &id)) {
            session.notice(format!("The others' voices will not be told apart in this meeting: {e:#}."));
        }
    }
    if let Err(e) = session.open_sources() {
        session.abandon();
        let _ = meeting::delete_in(&meeting::dir(), &id);
        return Err(format!("{e:#}"));
    }
    *app.meeting.lock() = Some(session.clone());
    app.want_meeting_overlay(true); // the capsule shows the recording, in every app
    app.emit_status();
    // The first piece comes after a few seconds: have the model loaded by then.
    let e2 = app.engines.clone();
    std::thread::spawn(move || {
        let _ = e2.prepare(&model);
    });
    Ok(session.live())
}

/// Record screen (the live meeting, the strip): the screen video joins the meeting when the
/// screen is chosen (the picker can take seconds; the audio records meanwhile). The portal's new
/// token is kept for next time. Stop screen: the video so far is saved.
#[tauri::command]
fn meeting_screen(app: State<'_, AppRef>, on: bool) -> Result<(), String> {
    let session = app.meeting.lock().clone().ok_or("No meeting is being recorded.")?;
    if !on {
        // The encoder closes its file: up to 5 s.
        std::thread::spawn(move || session.stop_screen());
        return Ok(());
    }
    if !crate::screen::SUPPORTED {
        return Err("The screen video is on Linux only for now.".into());
    }
    let stem = session.screen_begin()?;
    let a2 = app.inner().clone();
    std::thread::spawn(move || {
        let token = a2.config.lock().screen_token.clone();
        match crate::screen::start(&meeting::dir(), &stem, Some(&token), session.screen_end_fn(&stem)) {
            Ok(started) => {
                if let Some(t) = started.token {
                    let mut cfg = a2.config.lock();
                    if cfg.screen_token != t {
                        cfg.screen_token = t;
                        let _ = cfg.save();
                    }
                }
                if let Err(rec) = session.attach_video(started.recording) {
                    rec.discard(); // the meeting (or the screen) stopped before it was chosen
                }
                a2.emit_status();
            }
            Err(e) => session.screen_failed(&stem, &format!("{e:#}")),
        }
    });
    Ok(())
}

/// Settings → Meetings → Choose the screen again: the next Record screen shows the screen picker.
#[tauri::command]
fn screen_forget(app: State<'_, AppRef>) -> Result<(), String> {
    {
        let mut cfg = app.config.lock();
        cfg.screen_token.clear();
        cfg.save().map_err(|e| e.to_string())?;
    }
    app.emit_status();
    Ok(())
}

/// Settings → Meetings → Check again: what this computer has for the screen video (GStreamer may have been
/// installed since). Returns when it is known (seconds: each encoder is tried).
#[tauri::command]
async fn screen_check(app: State<'_, AppRef>) -> Result<(), String> {
    let a2 = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::screen::check(true);
        a2.emit_status();
    })
    .await
    .map_err(|e| e.to_string())
}

/// Where the page plays screen video `file` of meeting `id` from (src/video_server.rs).
#[tauri::command]
fn meeting_video_url(id: String, file: String) -> Result<Option<String>, String> {
    let Some(path) = meeting::video_path_in(&meeting::dir(), &id, &file) else { return Ok(None) };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    crate::video_server::url(&name).map(Some).map_err(|e| format!("The video cannot be played: {e:#}"))
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
    *app.call_session.lock() = None;
    app.meeting_stopping.store(false, Ordering::SeqCst);
    app.emit_status();
    // The strip says "saved" with a link to the meeting for a while, then goes (unless a new
    // meeting started, or the pointer is on it).
    app.hide_meeting_strip_later(8);
    if let Ok(m) = &r {
        let theirs = session.their_sound();
        let them = m.lines.iter().any(|l| l.speaker == meeting::Speaker::Them);
        match theirs {
            // The voices first (about 2 min for an hour); the note app and the AI notes then
            // get the speakers.
            // (Settings → Meetings may have turned it off while it recorded.)
            Some(tape) if them && app.config.lock().meeting_voices => {
                // Marked before the thread starts: Export, Write notes and fixes wait for it.
                app.voices_working.lock().insert(m.id.clone());
                let (a2, id) = (app.clone(), m.id.clone());
                std::thread::spawn(move || {
                    tell_voices_apart(&a2, &id, &tape);
                    // Deleted meanwhile: nothing to save or write.
                    if let Ok(m) = meeting::load_in(&meeting::dir(), &id) {
                        after_meeting(&a2, &m);
                    }
                });
            }
            other => {
                if let Some(tape) = other {
                    let _ = std::fs::remove_file(tape); // nobody else spoke
                }
                after_meeting(app, m);
            }
        }
    }
    r.map_err(|_| {
        session.abandon(); // keep what is transcribed; the next meeting can start
        "Stopping failed; the meeting is saved as it was.".to_string()
    })
}

/// Who of the others said each line, from the sound kept during the meeting; the file is
/// deleted after. Progress as "meeting-voices" {id, state: working | done | error, detail}.
fn tell_voices_apart(app: &AppRef, id: &str, tape: &std::path::Path) {
    /// The meeting is "working" until this is dropped, also after a panic.
    struct Working<'a>(&'a AppRef, &'a str);
    impl Drop for Working<'_> {
        fn drop(&mut self) {
            self.0.voices_working.lock().remove(self.1);
            self.0.emit_status();
        }
    }
    let working = Working(app, id);
    app.voices_working.lock().insert(id.to_string());
    emit_logged(&app.handle, "meeting-voices", json!({"id": id, "state": "working", "detail": ""}));
    app.emit_status();
    let r = std::panic::catch_unwind(|| crate::voices::read_tape(tape).and_then(|s| crate::voices::turns(&s)))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("it stopped with an internal error")));
    let _ = std::fs::remove_file(tape);
    if r.is_err() {
        crate::voices::verify(); // a broken model downloads again when the setting is turned on
    }
    let r = r.and_then(|turns| meeting::edit_in(&meeting::dir(), id, |m| meeting::set_people(m, &turns)));
    drop(working);
    match r {
        Ok(m) => emit_logged(&app.handle, "meeting-voices", json!({"id": id, "state": "done", "detail": "", "meeting": m})),
        Err(e) => emit_logged(&app.handle, "meeting-voices", json!({"id": id, "state": "error", "detail": format!("The voices could not be told apart: {e:#}")})),
    }
}

/// At start: the meetings whose voices were not told apart (the app quit or crashed first), one
/// after the other, then what comes after Stop (the note app; the AI notes if it has none). A
/// file whose meeting is gone, has no "Them", or has its people already, is deleted; with Who
/// is speaking on but its models not downloaded yet, it waits for a later start.
fn resume_voices(app: &AppRef, left: Vec<(String, PathBuf)>) {
    let on = app.config.lock().meeting_voices && crate::voices::SUPPORTED;
    let mut todo = Vec::new();
    for (id, tape) in left {
        let wanted = on
            && meeting::load_in(&meeting::dir(), &id)
                .is_ok_and(|m| m.people.is_empty() && m.lines.iter().any(|l| l.speaker == meeting::Speaker::Them));
        if !wanted {
            let _ = std::fs::remove_file(&tape);
        } else if crate::voices::present() {
            // Its last try (see `voices::retry_path`).
            let retry = crate::voices::retry_path(&meeting::dir(), &id);
            if std::fs::rename(&tape, &retry).is_ok() {
                app.voices_working.lock().insert(id.clone()); // Export, Write notes and fixes wait
                todo.push((id, retry));
            }
        }
    }
    if todo.is_empty() {
        return;
    }
    let a2 = app.clone();
    std::thread::spawn(move || {
        for (id, tape) in todo {
            tell_voices_apart(&a2, &id, &tape);
            if let Ok(m) = meeting::load_in(&meeting::dir(), &id) {
                if m.ai_notes.trim().is_empty() {
                    after_meeting(&a2, &m);
                } else {
                    auto_export(&a2, &id);
                }
            }
        }
    });
}

/// The voices of meeting `id` are being told apart (after Stop): its text is not final yet.
fn voices_pending(app: &AppRef, id: &str) -> bool {
    app.voices_working.lock().contains(id)
}

const VOICES_PENDING: &str = "Wait until the voices are told apart (about 2 minutes for an hour of meeting).";

/// After a meeting is saved: the note app, and the AI notes right away. The built-in model
/// downloads first when it is not there yet (progress in the meeting; Cancel stops it).
fn after_meeting(app: &AppRef, m: &meeting::Meeting) {
    auto_export(app, &m.id);
    let cfg = app.config.lock().clone();
    let ready = if cfg.ai_engine == "server" { !cfg.ai_url.is_empty() } else { llm::builtin_status(&cfg.ai_builtin_model).supported };
    if cfg.ai_auto && ready && !m.lines.is_empty() {
        if let Err(why) = start_ai_notes(app, m.id.clone()) {
            // e.g. another meeting's notes are being written: say so in this meeting.
            emit_logged(&app.handle, "ai-notes", json!({"id": m.id, "state": "error", "detail": format!("Not written: {why} Then use Write notes."), "progress": 0.0, "text": ""}));
        }
    }
}

/// The side menu's Exit: the same as the tray menu's (a meeting is stopped and saved by the page
/// first; `shutdown` keeps what a meeting left running has).
/// The end of the process, after `shutdown` saved what there is. `_exit` on Unix: the C++
/// libraries' global destructors (onnxruntime of Who is speaking, ggml) must not run while
/// their own worker threads still do: telling the voices apart when the app quits ended
/// with std::terminate (SIGABRT) in exit(). Nothing is left to flush but stdout and stderr.
fn exit_now() -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    unsafe {
        libc::_exit(0)
    }
    #[cfg(not(unix))]
    std::process::exit(0)
}

/// The window's size limits (min = max holds its size). Shrinking: the min size first; growing:
/// the max size first (never min > max: the window refuses it).
fn overlay_size_limits(win: &tauri::WebviewWindow, (w, h): (u32, u32), shrinking: bool) {
    let size = tauri::LogicalSize::new(w as f64, h as f64);
    if shrinking {
        let _ = win.set_min_size(Some(size));
        let _ = win.set_max_size(Some(size));
    } else {
        let _ = win.set_max_size(Some(size));
        let _ = win.set_min_size(Some(size));
    }
    #[cfg(target_os = "linux")]
    {
        let _ = win.with_webview(move |wv| {
            use gtk::prelude::WidgetExt;
            wv.inner().set_size_request(w as i32, h as i32);
        });
    }
}

/// The strip's collapse button and its pill (a click opens it again).
#[tauri::command]
fn overlay_collapse(app: State<'_, AppRef>, collapsed: bool) {
    let app = app.inner().clone();
    // Not on the main thread: the window's new size limits must reach GTK first.
    std::thread::spawn(move || app.set_overlay_collapsed(collapsed));
}

/// A press on the strip: on Hyprland it took the focus; give it back at once.
#[tauri::command]
fn overlay_unfocus() {
    std::thread::spawn(hyprland::focus_back_from_overlay);
}

/// The strip's microphone button: mute or unmute your microphone in the meeting being recorded.
// ------------------------------------------------------------------ note apps
/// Save meeting `id` to the note-app folder. `manual`: the meeting's Save to folder (a file
/// edited outside is an error then; by itself it is only reported).
fn export_meeting(app: &AppRef, id: &str, manual: bool) -> Result<Value, String> {
    let cfg = app.config.lock().clone();
    if cfg.export_dir.is_empty() {
        return Err("Choose a folder first: Settings → Connections → Note apps.".into());
    }
    if live_session(app, id).is_some() {
        return Err("Save it when the recording stops.".into());
    }
    if voices_pending(app, id) {
        return Err(VOICES_PENDING.into()); // saved by itself after them
    }
    let dir = PathBuf::from(&cfg.export_dir);
    let result = crate::export::save_meeting(&meeting::dir(), id, &dir, crate::export::parts(&cfg.export_parts), &crate::export::local_offset_at);
    let (state, file, detail) = match &result {
        Ok(crate::export::Outcome::Written(f)) => ("saved", f.clone(), String::new()),
        Ok(crate::export::Outcome::KeptEdited(f)) => ("kept", f.clone(), format!("{f} was changed outside the app: it is not overwritten. Rename or delete it to save again.")),
        Err(e) => ("error", String::new(), format!("Not saved to {}: {e:#}", cfg.export_dir)),
    };
    emit_logged(&app.handle, "meeting-export", json!({"id": id, "state": state, "file": file, "detail": detail}));
    match state {
        "saved" => Ok(json!({"file": file, "obsidian": crate::export::obsidian_link(&dir, &file)})),
        "kept" if !manual => Ok(json!({"file": file, "obsidian": crate::export::obsidian_link(&dir, &file)})),
        _ => Err(detail),
    }
}

/// When a meeting stops and when its AI notes are written (Settings → Connections → Note apps → by itself).
fn auto_export(app: &AppRef, id: &str) {
    let cfg = app.config.lock().clone();
    if cfg.export_dir.is_empty() || !cfg.export_auto {
        return;
    }
    let (app, id) = (app.clone(), id.to_string());
    std::thread::spawn(move || {
        let _ = export_meeting(&app, &id, false);
    });
}

/// The meeting's Save to folder.
#[tauri::command]
async fn meeting_save_to_folder(app: State<'_, AppRef>, id: String) -> Result<Value, String> {
    let app = app.inner().clone();
    tauri::async_runtime::spawn_blocking(move || export_meeting(&app, &id, true)).await.map_err(|e| e.to_string())?
}

/// The meeting's Open in Obsidian (its file in the vault the folder is in).
#[tauri::command]
fn meeting_open_in_obsidian(app: State<'_, AppRef>, id: String) -> Result<(), String> {
    let dir = PathBuf::from(&app.config.lock().export_dir);
    let m = meeting::load_in(&meeting::dir(), &id).map_err(|e| format!("{e:#}"))?;
    if m.export_file.is_empty() || !dir.join(&m.export_file).is_file() {
        return Err("Save it to the folder first.".into());
    }
    let link = crate::export::obsidian_link(&dir, &m.export_file).ok_or("The folder is not in an Obsidian vault.")?;
    crate::export::open_link(&link).map_err(|e| format!("Could not open Obsidian: {e}"))
}

/// Settings → Connections → Note apps → Choose folder: the system's folder dialog; saved at once.
#[tauri::command]
async fn choose_export_folder(app: State<'_, AppRef>, handle: tauri::AppHandle) -> Result<Option<Value>, String> {
    use tauri_plugin_dialog::DialogExt;
    let Some(folder) = handle.dialog().file().set_title("The folder for your meeting notes").blocking_pick_folder() else {
        return Ok(None);
    };
    let path = folder.into_path().map_err(|e| e.to_string())?;
    let app = app.inner().clone();
    apply_options(&app, OptionsIn { export_dir: Some(path.to_string_lossy().into_owned()), ..Default::default() }).map(Some)
}

/// The pointer came onto the strip, or left it.
#[tauri::command]
fn overlay_hover(app: State<'_, AppRef>, on: bool) {
    app.overlay_hovered.store(on, Ordering::SeqCst);
}

/// The strip's "Open the notes" (a saved meeting): the window opens on that meeting, and the
/// strip goes (unless a meeting records).
#[tauri::command]
fn open_meeting(app: State<'_, AppRef>, id: String) -> Result<(), String> {
    if !meeting::valid_id(&id) {
        return Err(format!("Unknown meeting {id:?}"));
    }
    app.show_main();
    emit_logged(&app.handle, "open-meeting", &id); // the main page opens it (the e2e reads it too)
    // Hyprland: in front even without misc:focus_on_activate (a moment for it to map first).
    if hyprland::running() {
        std::thread::spawn(|| {
            for _ in 0..10 {
                if hyprland::focus_own_window("Lowkey") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }
    app.overlay_hovered.store(false, Ordering::SeqCst);
    if app.meeting.lock().is_none() && !app.meeting_starting.load(Ordering::SeqCst) {
        app.want_overlay(1, false);
    }
    Ok(())
}

#[tauri::command]
fn meeting_mute(app: State<'_, AppRef>, muted: bool) -> Result<(), String> {
    let session = app.meeting.lock().clone().ok_or("No meeting is being recorded.")?;
    session.set_muted(muted);
    Ok(())
}

/// The mouse button went down on the strip. Hyprland: the app moves the strip with the pointer
/// (true). Elsewhere: false, and the page asks the system to drag the window.
#[tauri::command]
async fn overlay_drag_start(app: State<'_, AppRef>) -> Result<bool, String> {
    if !hyprland::running() {
        return Ok(false);
    }
    // The focus goes back on the release: given back now, the page would lose the button
    // (WebKit cancels the press) and never hear the release.
    let drag = hyprland::OverlayDrag::start();
    let started = drag.is_some();
    if let Some(old) = std::mem::replace(&mut *app.overlay_drag.lock(), drag) {
        old.end();
    }
    Ok(started)
}

/// The mouse button went up: true when the strip moved (its new place is saved), false for a
/// click.
#[tauri::command]
async fn overlay_drag_end(app: State<'_, AppRef>) -> Result<bool, String> {
    let Some(drag) = app.overlay_drag.lock().take() else { return Ok(false) };
    let moved = drag.end();
    hyprland::focus_back_from_overlay();
    if moved {
        std::thread::sleep(std::time::Duration::from_millis(30)); // the last move is applied
        app.remember_overlay_place();
    }
    Ok(moved)
}

#[tauri::command]
fn app_quit(app: State<'_, AppRef>) {
    app.request_quit();
}

const THEMES: &[&str] = &["system", "light", "dark"];

/// The window's own parts (Windows' and macOS' title bar) in the chosen colours; the page sets
/// its colours itself (frontend/public/theme.js).
fn set_window_theme(handle: &AppHandle, theme: &str) {
    if let Some(w) = handle.get_webview_window("main") {
        let _ = w.set_theme(match theme {
            "light" => Some(tauri::Theme::Light),
            "dark" => Some(tauri::Theme::Dark),
            _ => None,
        });
    }
}

#[tauri::command]
fn meeting_live(app: State<'_, AppRef>) -> Option<meeting::Live> {
    app.meeting.lock().as_ref().map(|s| s.live())
}

/// The meetings, newest first; `query`: only those with all of its words (Meetings → Search).
#[tauri::command]
async fn meetings_list(query: Option<String>) -> Vec<meeting::Summary> {
    // Off the window's thread: a search reads every meeting.
    tauri::async_runtime::spawn_blocking(move || meeting::search_in(&meeting::dir(), query.as_deref().unwrap_or("")))
        .await
        .unwrap_or_default()
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
    .map_err(|e| format!("{e:#}"))?;
    auto_export_after_edits(app.inner(), &id);
    Ok(())
}

/// A transcript paragraph fixed by the user (Transcript tab: a click on it). Returns the meeting
/// and, when the fix looks like a name or term the model gets wrong, a Vocabulary suggestion.
#[tauri::command]
fn meeting_edit_paragraph(app: State<'_, AppRef>, id: String, from: usize, to: usize, before: String, text: String, speaker: String, person: Option<u32>) -> Result<Value, String> {
    if live_session(app.inner(), &id).is_some() {
        return Err("Fix the transcript when the recording stops.".into());
    }
    if voices_pending(app.inner(), &id) {
        return Err(VOICES_PENDING.into());
    }
    let speaker = match speaker.as_str() {
        "me" => meeting::Speaker::Me,
        "them" => meeting::Speaker::Them,
        other => return Err(format!("Unknown speaker {other:?}")),
    };
    if text.len() > meeting::MAX_NOTES {
        return Err("The text is too long.".into());
    }
    let mut result = Ok(());
    let m = meeting::edit_in(&meeting::dir(), &id, |m| result = meeting::edit_paragraph(m, from, to, &before, &text, speaker, person.unwrap_or(0))).map_err(|e| format!("{e:#}"))?;
    result.map_err(|e| format!("{e:#}"))?;
    auto_export_after_edits(app.inner(), &id);
    let suggestion = crate::vocabulary::suggestion(&before, &text, &app.config.lock().vocabulary);
    Ok(json!({"meeting": m, "suggestion": suggestion}))
}

/// A name for one of the others found by voice (`person` 1, 2, …); the name of another one
/// merges the two. Returns the meeting.
#[tauri::command]
fn meeting_name_speaker(app: State<'_, AppRef>, id: String, person: u32, name: String) -> Result<meeting::Meeting, String> {
    if live_session(app.inner(), &id).is_some() || voices_pending(app.inner(), &id) {
        return Err(VOICES_PENDING.into());
    }
    let mut result = Ok(());
    let m = meeting::edit_in(&meeting::dir(), &id, |m| result = meeting::name_speaker(m, person, &name)).map_err(|e| format!("{e:#}"))?;
    result.map_err(|e| format!("{e:#}"))?;
    auto_export_after_edits(app.inner(), &id);
    Ok(m)
}

/// The transcript's "Add … to Vocabulary": the term, or one more spelling of it.
#[tauri::command]
fn vocabulary_add(app: State<'_, AppRef>, term: crate::vocabulary::Term) -> Result<Value, String> {
    let list = crate::vocabulary::with_term(&app.config.lock().vocabulary, term.clone());
    if !crate::vocabulary::has(&list, &term) {
        return Err(format!(
            "“{}” was not added: the Vocabulary is full ({} terms, {} spellings each). Remove some in Settings → Speech.",
            term.term,
            crate::vocabulary::MAX_TERMS,
            crate::vocabulary::MAX_SPELLINGS
        ));
    }
    apply_options(app.inner(), OptionsIn { vocabulary: Some(list), ..Default::default() })
}

/// The note follows title and notes edits: saved 3 s after the last one (not on every key).
fn auto_export_after_edits(app: &AppRef, id: &str) {
    let gen = {
        let mut edits = app.export_edits.lock();
        let g = edits.entry(id.to_string()).or_insert(0);
        *g += 1;
        *g
    };
    let (app, id) = (app.clone(), id.to_string());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let last = {
            let mut edits = app.export_edits.lock();
            let last = edits.get(&id) == Some(&gen);
            if last {
                edits.remove(&id);
            }
            last
        };
        if last {
            auto_export(&app, &id);
        }
    });
}

// ------------------------------------------------------------------ AI notes
/// Settings → Meetings → Notes: the engine to use now.
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
    if voices_pending(&app, &id) {
        return Err(VOICES_PENDING.into());
    }
    start_ai_notes(&app, id)
}

#[tauri::command]
fn ai_notes_cancel(app: State<'_, AppRef>) {
    app.ai_cancel.store(true, Ordering::SeqCst);
}

/// Why the AI cannot start now: it writes notes or answers a question (one job at a time).
fn ai_busy_reason(app: &AppRef, id: &str) -> String {
    if app.chat_job.lock().is_some() {
        "The AI is answering a question. Wait for the answer, or stop it.".to_string()
    } else if app.ai_job.lock().as_deref() == Some(id) {
        "The notes of this meeting are being written.".to_string()
    } else {
        "Notes are being written for another meeting. Wait for them, or cancel them.".to_string()
    }
}

fn start_ai_notes(app: &AppRef, id: String) -> Result<(), String> {
    let busy = llm::Busy::take(&app.ai_busy).ok_or_else(|| ai_busy_reason(app, &id))?;
    app.ai_cancel.store(false, Ordering::SeqCst);
    *app.ai_job.lock() = Some(id.clone());
    let job_slot = app.clone();
    let app = app.clone();
    std::thread::Builder::new()
        .name("ai-notes".into())
        .spawn(move || {
            let _busy = busy;
            run_ai_notes(&app, &id);
            *app.ai_job.lock() = None;
            app.emit_status();
        })
        .map_err(|e| {
            *job_slot.ai_job.lock() = None;
            e.to_string()
        })?;
    Ok(())
}

fn run_ai_notes(app: &AppRef, id: &str) {
    let h = app.handle.clone();
    let send = |state: &str, detail: &str, progress: f64, text: &str| {
        emit_logged(&h, "ai-notes", json!({"id": id, "state": state, "detail": detail, "progress": progress, "text": text}));
    };
    send("preparing", "Getting ready…", 0.0, "");
    let mut read_version = 0;
    let result = (|| -> anyhow::Result<(String, String)> {
        let m = meeting::load_in(&meeting::dir(), id)?;
        read_version = m.transcript_version;
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
        let prompt = app.config.lock().ai_prompt.clone();
        let text = llm::write_notes(&engine, &input, &lang, &names, &prompt, &mut progress, &mut on_text, &app.ai_cancel)?;
        // Nothing written (an empty answer): the notes there stay.
        anyhow::ensure!(!text.trim().is_empty(), "the model wrote no notes; the notes did not change");
        Ok((text, label))
    })();
    match result {
        Ok((text, label)) => {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            match meeting::edit_in(&meeting::dir(), id, |m| {
                m.ai_notes = text.clone();
                m.ai_model = label.clone();
                m.ai_at = now;
                m.ai_version = read_version; // a fix made meanwhile: still "write again"
            }) {
                Ok(_) => {
                    emit_logged(&h, "ai-notes", json!({"id": id, "state": "done", "detail": label, "progress": 1.0, "text": text, "ai_version": read_version}));
                    auto_export(app, id);
                }
                Err(e) => send("error", &format!("The notes were written but not saved: {e:#}"), 0.0, &text),
            }
        }
        Err(_) if app.ai_cancel.load(Ordering::SeqCst) => send("cancelled", "Cancelled", 0.0, ""),
        Err(e) => send("error", &format!("{e:#}"), 0.0, ""),
    }
}

// ------------------------------------------------------------------ Ask about this meeting
/// Ask the AI about a saved meeting (in the background; "ai-chat" events follow it). The
/// question and the answer are saved with the meeting when the answer is complete.
#[tauri::command]
fn meeting_chat(app: State<'_, AppRef>, id: String, question: String) -> Result<(), String> {
    let app = app.inner().clone();
    if !meeting::valid_id(&id) {
        return Err(format!("unknown meeting {id:?}"));
    }
    if live_session(&app, &id).is_some() {
        return Err("Ask when the recording stops.".into());
    }
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("Type a question.".into());
    }
    if question.chars().count() > meeting::MAX_QUESTION {
        return Err(format!("The question is too long (over {} characters).", meeting::MAX_QUESTION));
    }
    let busy = llm::Busy::take(&app.ai_busy).ok_or_else(|| ai_busy_reason(&app, &id))?;
    app.ai_cancel.store(false, Ordering::SeqCst);
    *app.chat_job.lock() = Some(id.clone());
    let job_slot = app.clone();
    std::thread::Builder::new()
        .name("ai-chat".into())
        .spawn(move || {
            let _busy = busy;
            run_chat(&app, &id, &question);
            *app.chat_job.lock() = None;
            app.emit_status();
        })
        .map_err(|e| {
            *job_slot.chat_job.lock() = None; // no thread: no job (the busy flag went with it)
            e.to_string()
        })?;
    Ok(())
}

/// Delete the questions and answers of a meeting.
#[tauri::command]
fn meeting_chat_clear(app: State<'_, AppRef>, id: String) -> Result<(), String> {
    if app.chat_job.lock().as_deref() == Some(id.as_str()) {
        return Err("Wait for the answer, or stop it.".into());
    }
    meeting::edit_in(&meeting::dir(), &id, |m| m.chat.clear()).map(|_| ()).map_err(|e| format!("{e:#}"))
}

fn run_chat(app: &AppRef, id: &str, question: &str) {
    let h = app.handle.clone();
    let send = |state: &str, detail: &str, progress: f64, text: &str| {
        emit_logged(&h, "ai-chat", json!({"id": id, "state": state, "detail": detail, "progress": progress, "text": text}));
    };
    send("preparing", "Getting ready…", 0.0, "");
    let result = (|| -> anyhow::Result<String> {
        let m = meeting::load_in(&meeting::dir(), id)?;
        anyhow::ensure!(!m.lines.is_empty() || !m.notes.trim().is_empty() || !m.ai_notes.trim().is_empty(), "this meeting has no transcript and no notes");
        let history: Vec<(String, String)> = m.chat.iter().map(|c| (c.role.clone(), c.text.clone())).collect();
        let engine = ai_engine(app);
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut progress = |what: &str, done: u64, total: u64| match what {
            "starting" => send("preparing", "Starting the model…", 0.0, ""),
            "writing" => send("writing", "", 0.0, ""),
            "checking the download" => send("preparing", "Checking the download…", 1.0, ""),
            what if last.elapsed() >= Duration::from_millis(250) || done == total => {
                last = Instant::now();
                let p = if total > 0 { done as f64 / total as f64 } else { 0.0 };
                send("downloading", &format!("Downloading {what} (once)… {}% of {} MB", (p * 100.0) as u32, total / 1_000_000), p, "");
            }
            _ => {}
        };
        let mut on_text = |t: &str| send("writing", "", 0.0, t);
        let names = crate::vocabulary::Vocabulary::new(&app.config.lock().vocabulary).names();
        llm::chat(&engine, &meeting::chat_input(&m), &history, question, &names, &mut progress, &mut on_text, &app.ai_cancel)
    })();
    match result {
        Ok(text) => {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let saved = meeting::edit_in(&meeting::dir(), id, |m| {
                m.chat.push(meeting::ChatMessage { role: "user".into(), text: question.to_string(), at: now });
                m.chat.push(meeting::ChatMessage { role: "assistant".into(), text: text.clone(), at: now });
            });
            match saved {
                Ok(_) => send("done", "", 1.0, &text),
                Err(e) => send("error", &format!("The answer was not saved: {e:#}"), 0.0, &text),
            }
        }
        Err(_) if app.ai_cancel.load(Ordering::SeqCst) => send("cancelled", "Stopped", 0.0, ""),
        Err(e) => send("error", &format!("{e:#}"), 0.0, ""),
    }
}

#[tauri::command]
fn meeting_delete(app: State<'_, AppRef>, id: String) -> Result<(), String> {
    if live_session(app.inner(), &id).is_some() {
        return Err("Stop the recording first.".into());
    }
    if app.ai_job.lock().as_deref() == Some(id.as_str()) || app.chat_job.lock().as_deref() == Some(id.as_str()) {
        app.ai_cancel.store(true, Ordering::SeqCst); // its notes or answer would have no meeting
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

/// One part of the meeting, for the Copy button of its tab: "notes" (the AI notes, as the model
/// wrote them) or "transcript" (one paragraph per speaker turn). No title or date.
#[tauri::command]
fn meeting_text(app: State<'_, AppRef>, id: String, part: String) -> Result<String, String> {
    let m = match live_session(app.inner(), &id) {
        Some(s) => s.meeting(),
        None => meeting::load_in(&meeting::dir(), &id).map_err(|e| format!("{e:#}"))?,
    };
    match part.as_str() {
        "notes" if m.ai_notes.trim().is_empty() => Err("This meeting has no notes yet.".into()),
        "notes" => Ok(m.ai_notes.trim().to_string() + "\n"),
        "transcript" => Ok(meeting::transcript_markdown(&m)),
        other => Err(format!("unknown part: {other}")),
    }
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

/// Settings → Dictation → Paste last dictation → Change.
#[tauri::command]
fn set_paste_hotkey(app: State<'_, AppRef>, key: String) -> Result<Value, String> {
    let app = app.inner().clone();
    let r = set_key_inner(&app, &key, KeyKind::PasteLast);
    emit_logged(&app.handle, "paste-key-change", json!({"key": key, "result": r.as_ref().map(|_| "ok").map_err(|e| e.clone())}));
    r
}

/// No call is followed now (nothing to time): an empty look is cheap.
#[cfg(target_os = "linux")]
fn detector_idle(app: &AppRef) -> bool {
    app.call_offer.lock().is_none() && app.call_session.lock().is_none()
}

/// Window titles by process id (Hyprland), to name a browser's call.
#[cfg(target_os = "linux")]
/// The title of each process's most recently used window (Hyprland's focus history).
fn window_titles() -> std::collections::HashMap<u32, String> {
    let mut best: std::collections::HashMap<u32, (i64, String)> = std::collections::HashMap::new();
    if !hyprland::running() {
        return Default::default();
    }
    if let Ok(Value::Array(clients)) = serde_json::from_str::<Value>(&hyprland::hyprctl(&["clients", "-j"])) {
        for c in clients {
            if let (Some(pid), Some(title)) = (c["pid"].as_u64(), c["title"].as_str()) {
                // focusHistoryID: 0 = focused now, higher = longer ago, -1 = never focused.
                let rank = c["focusHistoryID"].as_i64().filter(|r| *r >= 0).unwrap_or(i64::MAX);
                let e = best.entry(pid as u32).or_insert((i64::MAX, String::new()));
                if rank <= e.0 {
                    *e = (rank, title.to_string());
                }
            }
        }
    }
    best.into_iter().map(|(pid, (_, t))| (pid, t)).collect()
}

// ------------------------------------------------------------------ microphone
/// Settings → Speech → Microphone: the inputs, and the one the app uses now (with why).
#[tauri::command]
async fn microphones(app: State<'_, AppRef>) -> Result<Value, String> {
    let app = app.inner().clone();
    // Off the window's thread: pactl can take a moment.
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = app.config.lock().clone();
        let mics = crate::microphones::list();
        let selectable = crate::microphones::selectable();
        let mut using = crate::microphones::resolve(&cfg.microphone, cfg.avoid_bluetooth_mic, &mics);
        if !selectable {
            using = crate::microphones::Choice { id: None, name: "System default".into(), note: Some("Choosing a microphone needs PipeWire or PulseAudio (libpulse-simple): the system default is used.".into()) };
        }
        json!({"mics": mics, "using": using, "saved": cfg.microphone, "avoid_bluetooth": cfg.avoid_bluetooth_mic,
               "bluetooth_aware": cfg!(target_os = "linux") && selectable, "selectable": selectable})
    })
    .await
    .map_err(|e| e.to_string())
}

/// Settings → Speech → Microphone → Test: the chosen microphone for 4 s, its level as
/// "mic-level" events (nothing is kept).
#[tauri::command]
async fn mic_test(app: State<'_, AppRef>) -> Result<(), String> {
    let app = app.inner().clone();
    // Off the window's thread: opening a microphone can take seconds.
    tauri::async_runtime::spawn_blocking(move || mic_test_inner(&app)).await.map_err(|e| e.to_string())?
}

fn mic_test_inner(app: &AppRef) -> Result<(), String> {
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
    let (app2, h2) = (app.clone(), app.handle.clone());
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
    // The newest 500 (search reaches every entry).
    app.history.list(&query).into_iter().take(500).collect()
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
    if let Some(other) = KeyKind::ALL.into_iter().find(|&k| k != kind && same_key(&app.key_of(k), &new)) {
        return Err(format!("{new} is already the {} key.", other.name()));
    }
    let text = if keys_are_manual() {
        return Err(format!("This desktop does not let apps add keys: bind `lowkey {}` in its keyboard settings.", kind.flag()));
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
            if !k.is_empty() && KeyKind::ALL.into_iter().all(|x| x == other || !same_key(&app.key_of(x), &k)) {
                let _ = set_key_inner(&app, &k, other);
            }
        }
    }
    app.emit_status();
    Ok(app.status())
}

/// Options → "Let the app manage it": the key comes from the Lowkey block in the user's
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
                std::thread::spawn(move || app.meeting_key("toggle", "", false));
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
                KeyKind::Meeting => format!("{key} — tap to start or stop a meeting recording (while the app runs)"),
                KeyKind::PasteLast => format!("{key} — types the last dictation again (while the app runs)"),
                KeyKind::Dictation => format!("{key} — hold to talk, tap to start/stop (while the app runs)"),
            }
        }
        Err(e) => format!("not added: {key} ({e}); it may be taken by another app. Choose another key in Options."),
    }
}

// ------------------------------------------------------------------ entry
/// Headless commands that need no window. Returns true when handled.
pub fn headless(cli: &Cli) -> bool {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("lowkey"));
    if cli.help {
        print!("{HELP}");
        return true;
    }
    if cli.version {
        println!("Lowkey {}", env!("CARGO_PKG_VERSION"));
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
    if cli.mcp {
        crate::rename::migrate();
        crate::mcp::stdio();
        return true;
    }
    if let Some(path) = &cli.transcribe {
        crate::rename::migrate();
        let cfg = Config::load();
        let engines = Engines::new(cfg.idle_unload_minutes);
        engines.set_vocabulary(&cfg.vocabulary);
        match transcribe_json(&engines, &cfg, path, cli.model.as_deref(), cli.language.clone(), true) {
            (_, Ok(out)) => println!("{out}"),
            (model, Err(e)) => {
                println!("{}", json!({"model": model, "error": format!("{e:#}")}));
                std::process::exit(1);
            }
        }
        return true;
    }
    false
}

/// One file → its text as JSON (`--transcribe` and the agents' transcribe_file). The model: the
/// one asked for, else the saved one, else the automatic one. Loaded first, so "seconds" is the
/// transcription alone (what the app does with a loaded model). `download`: a model that is not
/// on disk downloads first (else it is an error: an agent never starts a download). Returns the
/// model used too.
pub fn transcribe_json(engines: &Engines, cfg: &Config, path: &std::path::Path, model: Option<&str>, language: Option<String>, download: bool) -> (String, anyhow::Result<Value>) {
    let model = model
        .filter(|m| models::spec(m).is_some())
        .map(String::from)
        .or_else(|| models::spec(&cfg.model).map(|m| m.id.to_string()))
        .unwrap_or_else(|| models::auto_model(&engines.hw).to_string());
    if !download && !models::spec(&model).is_some_and(models::present) {
        return (model.clone(), Err(anyhow::anyhow!("The model {model} is not on this computer. Download it in Lowkey (Settings → Speech), or name a downloaded one.")));
    }
    let t_load = Instant::now();
    let _ = engines.prepare(&model);
    let load_seconds = t_load.elapsed().as_secs_f64();
    let t0 = Instant::now();
    let input = Input { path: Some(path.to_path_buf()), pcm: None, language };
    let out = engines.transcribe(&model, input, Arc::new(|_| {})).map(|o| {
        json!({"model": model, "device": engines.hw.gpu, "load_seconds": load_seconds, "seconds": t0.elapsed().as_secs_f64(), "language": o.language,
               "duration": o.duration, "text": crate::formats::to_txt(&o.segments).trim(), "segments": o.segments})
    });
    (model, out)
}

/// The app touches `desktop.json` this often while it runs; older than `ALIVE_S`: it does not run
/// (a crash leaves the file).
const HEARTBEAT_S: u64 = 10;
const ALIVE_S: u64 = 3 * HEARTBEAT_S;

/// Since when (unix seconds) the desktop app runs, from the file it writes at start and touches
/// every `HEARTBEAT_S` (`desktop.json`, removed when it ends); None when it does not run.
pub fn running_since() -> Option<u64> {
    let mut dirs = vec![runtime_dir()];
    // A process without the session's XDG_RUNTIME_DIR (a service, a bare shell) looks where the
    // session keeps it.
    #[cfg(target_os = "linux")]
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        dirs.push(PathBuf::from(format!("/run/user/{}", unsafe { libc_getuid() })).join(crate::rename::NAME));
    }
    dirs.iter().find_map(|d| alive_since(&d.join("desktop.json")))
}

fn alive_since(f: &std::path::Path) -> Option<u64> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
    let touched = std::fs::metadata(f).ok()?.modified().ok()?.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    if touched > ALIVE_S {
        return None;
    }
    // Linux: also the process itself (its number may be another program's after a crash).
    #[cfg(target_os = "linux")]
    {
        let pid = v["pid"].as_u64()?;
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        let exe = v["exe"].as_str().and_then(|e| std::path::Path::new(e).file_name()?.to_str().map(String::from)).unwrap_or_default();
        // The kernel keeps 15 bytes of the name.
        if exe.is_empty() || !exe.starts_with(comm.trim()) {
            return None;
        }
    }
    v["started"].as_u64()
}

pub fn run() {
    let cli = Cli::parse(std::env::args());
    if headless(&cli) {
        return;
    }
    // Before any path is read: the folders from the old name move to the new one.
    crate::rename::migrate();
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
            overlay_collapse,
            overlay_unfocus,
            meeting_mute,
            overlay_hover,
            open_meeting,
            meeting_save_to_folder,
            meeting_edit_paragraph,
            meeting_name_speaker,
            vocabulary_add,
            meeting_open_in_obsidian,
            choose_export_folder,
            overlay_drag_start,
            overlay_drag_end,
            window_dictation,
            meeting_start,
            meeting_stop,
            meeting_live,
            meetings_list,
            meeting_get,
            meeting_update,
            meeting_delete,
            meeting_video_url,
            test_event,
            screen_forget,
            screen_check,
            meeting_screen,
            meeting_markdown,
            meeting_text,
            meeting_ai_notes,
            ai_notes_cancel,
            meeting_chat,
            meeting_chat_clear,
            app_quit
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
            // The others' sound of meetings whose voices were not told apart (the app quit or
            // crashed first): listed now, before a meeting can record; told apart below.
            crate::voices::delete_retries(&meeting::dir());
            let voices_left = crate::voices::leftovers(&meeting::dir());
            // Its screen video: kept, made whole (seconds for a long one: not in the way).
            let left = crate::screen::leftovers(&meeting::dir()); // now: no meeting records yet
            std::thread::spawn(move || meeting::recover_videos(&meeting::dir(), left));
            let config = Config::load();
            set_window_theme(&handle, &config.theme);
            // The strip opens where the user left it (the rule that keys add uses this place).
            let place = hyprland::OverlayPlace { collapsed: config.overlay_collapsed, capsule: false, at: config.overlay_at };
            hyprland::set_overlay_place(place);
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
                overlay_hovered: AtomicBool::new(false),
                export_edits: Mutex::new(Default::default()),
                voices_working: Mutex::new(Default::default()),
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
                pasting: AtomicBool::new(false),
                call_offer: Mutex::new(None),
                call_session: Mutex::new(None),
                calls_seen: Mutex::new(Vec::new()),
                overlay_wants: [AtomicBool::new(false), AtomicBool::new(false)],
                overlay_capsule: AtomicBool::new(false),
                overlay_drag: Mutex::new(None),
                ai_busy: Arc::new(AtomicBool::new(false)),
                ai_cancel: Arc::new(AtomicBool::new(false)),
                ai_job: Mutex::new(None),
                chat_job: Mutex::new(None),
                quit_asking: AtomicBool::new(false),
                quit_waiting: Arc::new(AtomicBool::new(false)),
                tray_menu: Mutex::new(Vec::new()),
                tray_dictation: Mutex::new("idle".into()),
                tray_mics: Mutex::new(Some(Vec::new())),
            });
            #[cfg(target_os = "linux")]
            app.watch_calls();
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
            // Settings → Meetings → Screen video: what this computer has (a second or two; once).
            if crate::screen::SUPPORTED {
                let a2 = app.clone();
                std::thread::spawn(move || {
                    crate::screen::check(false);
                    a2.emit_status();
                });
            }

            // Windows: the main window, and the dictation overlay (hidden; never focused).
            let background = cli.background || cli.dictate.is_some() || cli.meeting.is_some();
            let main = WebviewWindowBuilder::new(tauri_app, "main", WebviewUrl::App("index.html".into()))
                .title("Lowkey")
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
            let (ow, oh) = place.size();
            WebviewWindowBuilder::new(tauri_app, "overlay", WebviewUrl::App("overlay.html".into()))
                .title(hyprland::OVERLAY_TITLE)
                .inner_size(ow as f64, oh as f64)
                .min_inner_size(ow as f64, oh as f64)
                .max_inner_size(ow as f64, oh as f64)
                .decorations(false)
                // Transparent: the page draws the rounded strip, the corners show what is below.
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                // Resizable, but its min = max size holds it: a window that is not resizable
                // keeps its first size in GTK, and the strip collapses to a pill.
                .resizable(true)
                .focused(false)
                // A click on it (drag, collapse) never takes the focus from the app you type in.
                .focusable(false)
                .visible(false)
                .build()?;
            // WebKitGTK asks for at least 200 px of height; the overlay is a 76 px strip.
            #[cfg(target_os = "linux")]
            {
                let overlay = tauri_app.get_webview_window("overlay").expect("overlay");
                let _ = overlay.with_webview(move |wv| {
                    use gtk::prelude::WidgetExt;
                    wv.inner().set_size_request(ow as i32, oh as i32);
                });
            }

            // The tray menu (src/tray.rs): built now, and again whenever what it shows changes.
            let entries = crate::tray::entries(&app.tray_view());
            let menu = crate::tray::build(&handle, &entries)?;
            *app.tray_menu.lock() = entries;
            let tray_app = app.clone();
            TrayIconBuilder::with_id("main")
                .icon(tauri_app.default_window_icon().cloned().expect("app icon"))
                .tooltip("Lowkey")
                .menu(&menu)
                .on_menu_event(move |_, ev| tray_app.on_tray(ev.id().as_ref()))
                .build(tauri_app)?;
            // The microphones now and every 3 s (plugged in, removed); the recording states
            // once a second (a meeting starts and stops on other threads).
            let weak_tray = Arc::downgrade(&app);
            std::thread::Builder::new()
                .name("tray".into())
                .spawn(move || {
                    for tick in 0u64.. {
                        let Some(app) = weak_tray.upgrade() else { return };
                        if tick % HEARTBEAT_S == 0 {
                            // "The app runs" for the MCP server (`running_since`).
                            let f = runtime_dir().join("desktop.json");
                            if let Ok(file) = std::fs::File::options().write(true).open(&f) {
                                let _ = file.set_modified(std::time::SystemTime::now());
                            }
                        }
                        if tick % 3 == 0 {
                            app.refresh_tray_mics();
                        } else {
                            app.refresh_tray();
                        }
                        drop(app);
                        std::thread::sleep(Duration::from_secs(1));
                    }
                })
                .ok();

            // The ow-dictate shim (used by the bindings.lua block) finds us here.
            match private_runtime_dir() {
                Ok(dir) => {
                    let exe = std::env::current_exe().unwrap_or_default();
                    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let _ = std::fs::write(dir.join("desktop.json"), json!({"exe": exe, "pid": std::process::id(), "started": started}).to_string());
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
                "this Wayland desktop does not let apps add keys: bind `lowkey --dictate press` (and `--dictate release` on key release, or `--dictate toggle`) in your desktop's keyboard settings".to_string()
            } else if !hyprland::running() {
                register_global_shortcut(&app, &key, KeyKind::Dictation)
            } else if cli.no_hotkey || key.trim().is_empty() {
                let removed = hyprland::remove_app_binds();
                let mut t = "not added (--no-hotkey)".to_string();
                if !removed.is_empty() {
                    t += &format!("; removed {}", removed.join(", "));
                }
                if hyprland::key_owners(crate::config::DEFAULT_HOTKEY).iter().any(|o| o == hyprland::CONFIG_DESC) {
                    t += ". The key in your Hyprland config (the block the app added) still works; delete that block to turn it off";
                }
                t
            } else {
                let exe = std::env::current_exe().unwrap_or_default();
                let hk = hyprland::Hotkey::new(&key, &exe);
                let t = match hk.install(cli.dictate.is_some()) {
                    Ok(t) => t,
                    Err(e) => format!("could not add it ({e}); see `lowkey --print-snippet`"),
                };
                hk.watch_reloads();
                *app.hotkey.lock() = Some(hk);
                t
            };
            println!("  Lowkey {}: {}, model {}", env!("CARGO_PKG_VERSION"), app.engines.hw.gpu.clone().map(|g| format!("GPU {g}")).unwrap_or("CPU".into()), app.current_model());
            println!("  Global dictation key: {text}");
            *app.hotkey_text.lock() = text;

            // The meeting key: the same ways as the dictation key, one bind (a tap).
            let mkey = config.meeting_hotkey.clone();
            let mtext = if cli.no_hotkey || mkey.trim().is_empty() {
                "not added".to_string()
            } else if wayland_other {
                "this Wayland desktop does not let apps add keys: bind `lowkey --meeting toggle` in your desktop's keyboard settings".to_string()
            } else if same_key(&mkey, &app.hotkey_key()) {
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
                "this Wayland desktop does not let apps add keys: bind `lowkey --paste-last` in your desktop's keyboard settings".to_string()
            } else if same_key(&pkey, &app.hotkey_key()) || same_key(&pkey, &app.key_of(KeyKind::Meeting)) {
                format!("not added: {pkey} is already used by another key here. Choose another key in Settings → Dictation or Settings → Meetings.")
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
            // Who is speaking is on but its models are not there (deleted, or a config from
            // another computer): download them again.
            if config.meeting_voices && crate::voices::SUPPORTED && !crate::voices::present() {
                let a2 = app.clone();
                std::thread::spawn(move || crate::voices::download_models(|| a2.emit_status()));
            }
            resume_voices(&app, voices_left);
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
    let app = builder.build(context).expect("Lowkey failed to start");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(a) = handle.try_state::<AppRef>() {
                a.shutdown();
            }
            exit_now();
        }
    });
}
