//! Hyprland integration (Lua config, Hyprland 0.56+): the global key and the overlay window rule.
//!
//! Only API calls verified safe on a live session are used here: `hl.bind`, `hl.unbind`,
//! `hl.window_rule` + `set_enabled`, `hyprctl reload`. (`Keybind:remove()` aborts Hyprland
//! 0.56.2.) Binds are added to the running Hyprland and re-added after a config reload. One
//! config edit exists, and only on the user's click in Options: removing the marked
//! `lowkey.dictation` block (or the older `open-whisper.dictation` one), with a backup, so the app can manage the key.

#[cfg(unix)]
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;

/// Binds that the app adds say so; the permanent config snippet says just "Dictate (Lowkey)".
pub const RUNTIME_DESC: &str = "Dictate (Lowkey, added by the app)";
pub const CONFIG_DESC: &str = "Dictate (Lowkey)";
/// The meeting key (one bind: a tap starts or stops the recording). Only the app adds it.
pub const MEETING_DESC: &str = "Meeting recording (Lowkey, added by the app)";
/// The key that types the last dictation again.
pub const PASTE_DESC: &str = "Paste last dictation (Lowkey, added by the app)";
/// The overlay window's title; the window rule matches it.
pub const OVERLAY_TITLE: &str = "lowkey-overlay";

/// A bind's description, with the app's old name ("open-whisper") read as the new one: binds
/// and config blocks from before the rename are still the app's own.
pub fn description(b: &Value) -> String {
    current_desc(b["description"].as_str().unwrap_or(""))
}

fn current_desc(d: &str) -> String {
    if let Some(head) = d.strip_suffix("(open-whisper, added by the app)") {
        format!("{head}(Lowkey, added by the app)")
    } else if let Some(head) = d.strip_suffix("(open-whisper)") {
        format!("{head}(Lowkey)")
    } else {
        d.to_string()
    }
}

/// A bind from before the rename (its command runs the old program).
fn old_name(b: &Value) -> bool {
    b["description"].as_str().is_some_and(|d| d.contains("(open-whisper"))
}

pub fn running() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() && crate::audio::which("hyprctl")
}

pub fn hyprctl(args: &[&str]) -> String {
    Command::new("hyprctl")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn eval(lua: &str) -> anyhow::Result<()> {
    let out = hyprctl(&["eval", lua]);
    if out.trim() == "ok" {
        Ok(())
    } else {
        anyhow::bail!("hyprctl eval: {}", out.trim())
    }
}

/// `hyprctl reload`, then wait until Hyprland answers again (the config is applied).
fn reload() -> anyhow::Result<()> {
    let out = hyprctl(&["reload"]);
    if !out.trim().eq_ignore_ascii_case("ok") {
        anyhow::bail!("hyprctl reload: {}", out.trim());
    }
    std::thread::sleep(Duration::from_millis(300));
    for _ in 0..20 {
        if lua_config() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("Hyprland did not answer after the reload")
}

pub fn lua_config() -> bool {
    hyprctl(&["eval", "return true"]).trim() == "ok"
}

/// Lua string literal (Lua has no \uXXXX escape, so json escaping is not enough).
pub fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            c if (c as u32) < 32 || c as u32 == 127 => out.push_str(&format!("\\{:03}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The strip's sizes (logical px): open, and collapsed to a small pill.
pub const OVERLAY_SIZE: (u32, u32) = (380, 76);
pub const OVERLAY_PILL: (u32, u32) = (150, 44);

/// Where the strip shows and how: collapsed or not, and where the user left it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OverlayPlace {
    pub collapsed: bool,
    /// [x, y] as fractions (0..1) of the room the monitor leaves around the strip
    /// (0,0 = top left, 1,1 = bottom right). None: bottom centre.
    pub at: Option<[f32; 2]>,
}

impl OverlayPlace {
    pub fn size(&self) -> (u32, u32) {
        if self.collapsed { OVERLAY_PILL } else { OVERLAY_SIZE }
    }
}

/// The place the app's rule uses now (set from the config, and when the user moves the strip).
static PLACE: parking_lot::Mutex<OverlayPlace> = parking_lot::Mutex::new(OverlayPlace { collapsed: false, at: None });

/// Floating, pinned, bottom centre (or where the user left it). A restart replaces it (Lua
/// global; each rule gets a new name: Hyprland keeps the first rule of a name). Not `no_focus`:
/// Hyprland sends such a window no pointer at all, and the strip is dragged and clicked. So:
/// `no_initial_focus` and `no_follow_mouse` (the pointer over it never focuses it), and a
/// click gives the focus straight back (`focus_back_from_overlay`). `decorate = false` turns off every decoration, title-bar plugins
/// too (hyprbars draws its bar on every window): no plugin-specific field, so nothing depends
/// on a plugin being loaded. `rounding = 0` and `no_blur`: the window is transparent, the page
/// draws the rounded strip.
pub fn overlay_rule_lua() -> String {
    overlay_rule_lua_at(&PLACE.lock())
}

static RULE_NO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn overlay_rule_lua_at(place: &OverlayPlace) -> String {
    let n = RULE_NO.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let (w, h) = place.size();
    let (x, y) = match place.at {
        // Fractions of the free room: on any monitor size the strip stays on screen.
        Some([fx, fy]) => (format!("((monitor_w-{w})*{:.4})", fx.clamp(0.0, 1.0)), format!("((monitor_h-{h})*{:.4})", fy.clamp(0.0, 1.0))),
        None => (format!("(monitor_w/2-{})", w / 2), format!("(monitor_h-{})", 74 + h)),
    };
    format!(
        "if _G.lowkey_desktop_rule then _G.lowkey_desktop_rule:set_enabled(false) end\n\
         _G.lowkey_desktop_rule = hl.window_rule({{ name = \"lowkey-desktop-overlay-{n}\", \
         match = {{ title = {} }}, float = true, pin = true, no_initial_focus = true, no_follow_mouse = true, \
         size = {{ {w}, {h} }}, move = {{ \"{x}\", \"{y}\" }}, \
         border_size = 0, rounding = 0, no_shadow = true, no_blur = true, decorate = false, no_anim = true }})",
        lua_str(&format!("^{OVERLAY_TITLE}$"))
    )
}

/// A new place for the strip: the app's rule follows it (if the app's rule is in use), so the
/// strip shows there the next time it opens.
pub fn set_overlay_place(place: OverlayPlace) {
    let changed = {
        let mut p = PLACE.lock();
        let changed = *p != place;
        *p = place;
        changed
    };
    if changed && running() && lua_config() {
        let _ = eval(&overlay_rule_lua());
    }
}

/// The strip as it is on screen: its top-left and size, and its monitor's top-left and size
/// (layout px). None when it is not mapped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayGeometry {
    pub address: u64,
    pub at: (f64, f64),
    pub size: (f64, f64),
    pub monitor_at: (f64, f64),
    pub monitor_size: (f64, f64),
}

pub fn overlay_geometry() -> Option<OverlayGeometry> {
    if !running() {
        return None;
    }
    let clients: Value = serde_json::from_str(&hyprctl(&["clients", "-j"])).ok()?;
    let monitors: Value = serde_json::from_str(&hyprctl(&["monitors", "-j"])).ok()?;
    geometry_from(&clients, &monitors)
}

pub fn geometry_from(clients: &Value, monitors: &Value) -> Option<OverlayGeometry> {
    let c = clients.as_array()?.iter().find(|c| c["title"] == OVERLAY_TITLE && c["mapped"].as_bool().unwrap_or(true) && c["hidden"] != true)?;
    let pair = |v: &Value| Some((v[0].as_f64()?, v[1].as_f64()?));
    let m = monitors.as_array()?.iter().find(|m| m["id"] == c["monitor"])?;
    let scale = m["scale"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0);
    let address = u64::from_str_radix(c["address"].as_str()?.trim_start_matches("0x"), 16).ok()?;
    // A monitor turned 90° or 270° (transform 1, 3, 5, 7) is taller than wide.
    let (mut mw, mut mh) = (m["width"].as_f64()? / scale, m["height"].as_f64()? / scale);
    if m["transform"].as_i64().unwrap_or(0) % 2 == 1 {
        std::mem::swap(&mut mw, &mut mh);
    }
    Some(OverlayGeometry { address, at: pair(&c["at"])?, size: pair(&c["size"])?, monitor_at: (m["x"].as_f64()?, m["y"].as_f64()?), monitor_size: (mw, mh) })
}

impl OverlayGeometry {
    /// Where it is, as fractions of the room its monitor leaves around it.
    pub fn fractions(&self) -> [f32; 2] {
        let f = |pos: f64, mon: f64, size: f64, mon_size: f64| {
            let room = mon_size - size;
            if room <= 0.0 { 0.5 } else { ((pos - mon) / room).clamp(0.0, 1.0) as f32 }
        };
        [f(self.at.0, self.monitor_at.0, self.size.0, self.monitor_size.0), f(self.at.1, self.monitor_at.1, self.size.1, self.monitor_size.1)]
    }
}

/// One request on Hyprland's socket (as hyprctl sends it: "j/cursorpos", "/dispatch …").
/// Much faster than starting hyprctl: the strip's drag asks ~60 times a second.
#[cfg(unix)]
fn request(msg: &str) -> Option<String> {
    use std::io::{Read, Write};
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let base = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let mut s = std::os::unix::net::UnixStream::connect(PathBuf::from(base).join("hypr").join(sig).join(".socket.sock")).ok()?;
    s.write_all(msg.as_bytes()).ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(not(unix))]
fn request(_msg: &str) -> Option<String> {
    None
}

/// The mouse pointer (layout px).
pub fn cursor_pos() -> Option<(f64, f64)> {
    let v: Value = serde_json::from_str(&request("j/cursorpos")?).ok()?;
    Some((v["x"].as_f64()?, v["y"].as_f64()?))
}

/// Put a window's top-left at x, y (layout px).
pub fn move_window(address: u64, x: i64, y: i64) -> bool {
    let lua = format!("hl.dsp.window.move({{ x = {x}, y = {y}, window = {} }})", lua_str(&format!("address:0x{address:x}")));
    request(&format!("/dispatch {lua}")).is_some_and(|r| r.trim() == "ok")
}

/// The strip's drag on Hyprland, done by the app: the window follows the pointer while the
/// button is down (the page says when it goes up). Not Hyprland's own drag (xdg move): it
/// ends in a strip that keeps the focus, and giving the focus back stops it.
pub struct OverlayDrag {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    moved: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl OverlayDrag {
    pub fn start() -> Option<OverlayDrag> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let g = overlay_geometry()?;
        let start = cursor_pos()?;
        let grab = (start.0 - g.at.0, start.1 - g.at.1);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let moved = std::sync::Arc::new(AtomicBool::new(false));
        let (stop2, moved2) = (stop.clone(), moved.clone());
        std::thread::Builder::new()
            .name("strip-drag".into())
            .spawn(move || {
                let mut last = start;
                // At most 30 s (a lost "up": the strip never stays glued to the pointer).
                for _ in 0..3000 {
                    if stop2.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let Some(p) = cursor_pos() else { continue };
                    if p == last {
                        continue;
                    }
                    // A few pixels are still a click.
                    if !moved2.load(Ordering::SeqCst) && (p.0 - start.0).hypot(p.1 - start.1) < 4.0 {
                        continue;
                    }
                    last = p;
                    moved2.store(true, Ordering::SeqCst);
                    // On its monitor.
                    let x = (p.0 - grab.0).clamp(g.monitor_at.0, (g.monitor_at.0 + g.monitor_size.0 - g.size.0).max(g.monitor_at.0));
                    let y = (p.1 - grab.1).clamp(g.monitor_at.1, (g.monitor_at.1 + g.monitor_size.1 - g.size.1).max(g.monitor_at.1));
                    move_window(g.address, x.round() as i64, y.round() as i64);
                }
            })
            .ok()?;
        Some(OverlayDrag { stop, moved })
    }

    /// The button is up: stop; true when the strip moved (else it was a click).
    pub fn end(self) -> bool {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.moved.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A click on the strip focused it (Hyprland focuses the window under a click): give the
/// focus back to the window that had it, so a dictation types where the user was typing.
pub fn focus_back_from_overlay() {
    if !running() {
        return;
    }
    let Ok(clients) = serde_json::from_str::<Value>(&hyprctl(&["clients", "-j"])) else { return };
    let Some(active) = focused_window() else { return };
    let list = clients.as_array().cloned().unwrap_or_default();
    if !list.iter().any(|c| c["title"] == OVERLAY_TITLE && c["address"] == active.as_str()) {
        return;
    }
    let prev = list
        .iter()
        .filter(|c| c["title"] != OVERLAY_TITLE && c["mapped"] != false && c["hidden"] != true && c["focusHistoryID"].as_i64().unwrap_or(-1) >= 0)
        .min_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX));
    if let Some(addr) = prev.and_then(|c| c["address"].as_str()) {
        let _ = hyprctl(&["dispatch", &format!("hl.dsp.focus({{ window = {} }})", lua_str(&format!("address:{addr}")))]);
    }
}

/// Resize the strip now and keep its centre where it is (open ⇄ collapsed). Returns its new
/// top-left, or None when Hyprland did not take it.
pub fn resize_overlay(g: &OverlayGeometry, (w, h): (u32, u32)) -> Option<(f64, f64)> {
    let cx = g.at.0 + g.size.0 / 2.0;
    let cy = g.at.1 + g.size.1 / 2.0;
    let mut x = (cx - w as f64 / 2.0).round();
    let mut y = (cy - h as f64 / 2.0).round();
    // On its monitor.
    x = x.clamp(g.monitor_at.0, (g.monitor_at.0 + g.monitor_size.0 - w as f64).max(g.monitor_at.0));
    y = y.clamp(g.monitor_at.1, (g.monitor_at.1 + g.monitor_size.1 - h as f64).max(g.monitor_at.1));
    let win = lua_str(&format!("address:0x{:x}", g.address));
    let ok = |lua: String| hyprctl(&["dispatch", &lua]).trim() == "ok";
    (ok(format!("hl.dsp.window.resize({{ x = {w}, y = {h}, window = {win} }})"))
        && ok(format!("hl.dsp.window.move({{ x = {}, y = {}, window = {win} }})", x as i64, y as i64)))
    .then_some((x, y))
}

pub fn bind_lua(key: &str, command: &str, description: &str) -> String {
    let press = lua_str(&format!("{command} press"));
    let release = lua_str(&format!("{command} release"));
    let k = lua_str(key);
    format!(
        "hl.bind({k}, hl.dsp.exec_cmd({press}), {{ description = {} }})\nhl.bind({k}, hl.dsp.exec_cmd({release}), {{ release = true }})",
        lua_str(description)
    )
}

/// One bind, on press only (the meeting key).
pub fn bind_once_lua(key: &str, command: &str, description: &str) -> String {
    format!("hl.bind({}, hl.dsp.exec_cmd({}), {{ description = {} }})", lua_str(key), lua_str(command), lua_str(description))
}

/// The overlay's window rule without a key (a meeting from the window, no keys at all).
pub fn install_overlay_rule() -> bool {
    lua_config() && eval(&overlay_rule_lua()).is_ok()
}

pub fn remove_overlay_rule() {
    let _ = eval("if _G.lowkey_desktop_rule then _G.lowkey_desktop_rule:set_enabled(false); _G.lowkey_desktop_rule = nil end");
}

/// The paste-last key's command: this executable with --paste-last.
pub fn paste_command(exe: &Path) -> String {
    format!("{} --paste-last", shell_quote(&exe.to_string_lossy()))
}

/// The meeting key's command: this executable with --meeting toggle.
pub fn meeting_command(exe: &Path) -> String {
    format!("{} --meeting toggle", shell_quote(&exe.to_string_lossy()))
}

/// The command a bind runs: this executable with --dictate.
pub fn dictate_command(exe: &Path) -> String {
    format!("{} --dictate", shell_quote(&exe.to_string_lossy()))
}

/// Lines for ~/.config/hypr/bindings.lua (permanent key).
pub fn permanent_snippet(key: &str, exe: &Path) -> String {
    format!(
        "-- Lowkey: global dictation (hold = talk, tap = start/stop)\n{}\n{}",
        bind_lua(key, &dictate_command(exe), CONFIG_DESC),
        overlay_rule_lua_at(&OverlayPlace::default())
    )
}

/// Lines for a classic hyprland.conf (no overlay rule: see README).
pub fn conf_snippet(key: &str, exe: &Path) -> String {
    let (mods, k) = key.rsplit_once('+').map(|(m, k)| (m, k.trim())).unwrap_or(("", key.trim()));
    let mods = mods.split('+').map(str::trim).filter(|m| !m.is_empty()).collect::<Vec<_>>().join(" ");
    let cmd = dictate_command(exe);
    format!("# Lowkey: global dictation (hold = talk, tap = start/stop)\nbind = {mods}, {k}, exec, {cmd} press\nbindr = {mods}, {k}, exec, {cmd} release")
}

pub fn mods_mask(key: &str) -> (u64, String) {
    let parts: Vec<String> = key.split('+').map(|p| p.trim().to_uppercase()).filter(|p| !p.is_empty()).collect();
    let mask = parts[..parts.len().saturating_sub(1)]
        .iter()
        .map(|p| match p.as_str() {
            "SHIFT" => 1,
            "CTRL" | "CONTROL" => 4,
            "ALT" => 8,
            "SUPER" | "WIN" | "LOGO" | "MOD4" => 64,
            _ => 0,
        })
        .sum();
    (mask, parts.last().cloned().unwrap_or_default())
}

fn binds() -> Vec<Value> {
    serde_json::from_str::<Value>(&hyprctl(&["binds", "-j"]))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn combo(b: &Value) -> (u64, String) {
    (b["modmask"].as_u64().unwrap_or(0), b["key"].as_str().unwrap_or("").to_uppercase())
}

/// Descriptions of keyboard binds on `key` in the default submap.
pub fn key_owners(key: &str) -> Vec<String> {
    owners_in(&binds(), key)
}

pub fn owners_in(binds: &[Value], key: &str) -> Vec<String> {
    let want = mods_mask(key);
    binds
        .iter()
        .filter(|b| combo(b) == want && !b["mouse"].as_bool().unwrap_or(false) && b["submap"].as_str().unwrap_or("").is_empty())
        .map(description)
        .collect()
}

/// Keys whose binds the app added and that no user bind shares (hl.unbind is per key).
pub fn removable_app_keys(binds: &[Value]) -> Vec<String> {
    let names = [(64u64, "SUPER"), (4, "CTRL"), (8, "ALT"), (1, "SHIFT")];
    let user: std::collections::HashSet<_> = binds
        .iter()
        .filter(|b| !matches!(description(b).as_str(), "" | RUNTIME_DESC | MEETING_DESC | PASTE_DESC))
        .map(combo)
        .collect();
    let mut out = Vec::new();
    for b in binds {
        if matches!(description(b).as_str(), RUNTIME_DESC | MEETING_DESC | PASTE_DESC) && !user.contains(&combo(b)) && b["submap"].as_str().unwrap_or("").is_empty() {
            let (mask, key) = combo(b);
            let mut parts: Vec<String> = names.iter().filter(|(m, _)| mask & m != 0).map(|(_, n)| n.to_string()).collect();
            parts.push(key);
            let s = parts.join(" + ");
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

pub fn remove_app_binds() -> Vec<String> {
    let keys = removable_app_keys(&binds());
    for k in &keys {
        let _ = eval(&format!("hl.unbind({})", lua_str(k)));
    }
    keys
}

pub const BLOCK_BEGIN: &str = "-- BEGIN lowkey.dictation";
pub const BLOCK_END: &str = "-- END lowkey.dictation";
/// The markers from before the rename: such a block is the app's too.
const OLD_BLOCK_BEGIN: &str = "-- BEGIN open-whisper.dictation";
const OLD_BLOCK_END: &str = "-- END open-whisper.dictation";

/// "SUPER + CTRL + X" written the same way every time; None when it is not a usable key: at
/// least one of Super, Ctrl or Alt (a plain key would be taken from every app), and one main
/// key (A-Z, 0-9, F1-F24, SPACE).
pub fn normalize_key(key: &str) -> Option<String> {
    let parts: Vec<String> = key.split('+').map(|p| p.trim().to_uppercase()).filter(|p| !p.is_empty()).collect();
    let (main, mods) = parts.split_last()?;
    let mut out: Vec<&str> = Vec::new();
    for (names, canon) in [(&["SUPER", "WIN", "LOGO", "MOD4", "META", "CMD"][..], "SUPER"), (&["CTRL", "CONTROL"][..], "CTRL"), (&["ALT", "OPTION"][..], "ALT"), (&["SHIFT"][..], "SHIFT")] {
        if mods.iter().any(|m| names.contains(&m.as_str())) {
            out.push(canon);
        }
    }
    let known = |m: &String| ["SUPER", "WIN", "LOGO", "MOD4", "META", "CMD", "CTRL", "CONTROL", "ALT", "OPTION", "SHIFT"].contains(&m.as_str());
    if !mods.iter().all(known) || !out.iter().any(|m| *m != "SHIFT") {
        return None;
    }
    let ok_main = (main.len() == 1 && main.chars().all(|c| c.is_ascii_alphanumeric()))
        || main == "SPACE"
        || (main.starts_with('F') && main[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)));
    if !ok_main {
        return None;
    }
    let mut v: Vec<String> = out.into_iter().map(String::from).collect();
    v.push(main.clone());
    Some(v.join(" + "))
}

/// The one config file that holds the marked block: in the running Hyprland's config folder
/// (the folder of its `-c` file, else $XDG_CONFIG_HOME/hypr), backups and copies skipped. More
/// than one file with the block: an error naming them (the app does not guess which one counts).
pub fn find_config_block() -> anyhow::Result<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    // The running instance's own config, when it was started with -c/--config.
    if let Ok(v) = serde_json::from_str::<Value>(&hyprctl(&["instances", "-j"])) {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").unwrap_or_default();
        for inst in v.as_array().into_iter().flatten().filter(|i| i["instance"].as_str() == Some(sig.as_str())) {
            if let Some(pid) = inst["pid"].as_u64() {
                let args: Vec<String> = std::fs::read(format!("/proc/{pid}/cmdline"))
                    .map(|b| b.split(|&c| c == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
                    .unwrap_or_default();
                if let Some(i) = args.iter().position(|a| a == "-c" || a == "--config") {
                    if let Some(dir) = args.get(i + 1).map(PathBuf::from).and_then(|p| p.parent().map(Path::to_path_buf)) {
                        dirs.push(dir);
                    }
                }
            }
        }
    }
    dirs.push(dirs_config().join("hypr"));
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| config_candidate(p))
            .filter(|f| std::fs::read_to_string(f).is_ok_and(|t| without_block(&t).is_some()))
            .collect();
        match found.len() {
            0 => continue,
            1 => return Ok(found.into_iter().next().unwrap()),
            _ => anyhow::bail!(
                "the app's block is in several files ({}); remove the extra copies first",
                found.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(", ")
            ),
        }
    }
    anyhow::bail!("could not find the app's block in your Hyprland config")
}

/// A `.lua` config file, not a backup or a copy ("bindings.lua.bak.…", "bindings copy.lua").
pub fn config_candidate(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name.ends_with(".lua") && !name.contains(".bak") && !name.contains("copy") && !name.contains("old") && !name.starts_with('.')
}

fn dirs_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".config"))).unwrap_or_default()
}

/// The text without every marked block (BEGIN to END lines, both included; CRLF and a last line
/// without a newline are kept as they are). None: no complete block.
pub fn without_block(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut removed) = (0, false);
    while i < lines.len() {
        let line = lines[i].trim_start();
        if line.starts_with(BLOCK_BEGIN) || line.starts_with(OLD_BLOCK_BEGIN) {
            let is_end = |l: &&str| l.trim_start().starts_with(BLOCK_END) || l.trim_start().starts_with(OLD_BLOCK_END);
            let end = i + lines[i..].iter().position(is_end)?; // never cut to EOF
            i = end + 1;
            removed = true;
            continue;
        }
        out.push_str(lines[i]);
        i += 1;
    }
    removed.then_some(out)
}

/// Remove the blocks from `file` (a symlink: its target is edited, so dotfile managers keep
/// one file), keeping a copy next to it; returns the backup's path. The new text is written to
/// a temporary file with the same permissions, synced, and renamed over the old one.
pub fn remove_config_block(file: &Path) -> anyhow::Result<PathBuf> {
    let file = std::fs::canonicalize(file)?;
    let text = std::fs::read_to_string(&file)?;
    let new = without_block(&text).ok_or_else(|| anyhow::anyhow!("no complete block of the app in {}", file.display()))?;
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let backup = file.with_file_name(format!("{name}.bak.lowkey.{stamp}"));
    std::fs::copy(&file, &backup)?;
    write_atomically(&file, &new)?;
    Ok(backup)
}

/// Put a backup made by `remove_config_block` back in place.
pub fn restore_config(file: &Path, backup: &Path) -> anyhow::Result<()> {
    let file = std::fs::canonicalize(file)?;
    write_atomically(&file, &std::fs::read_to_string(backup)?)
}

fn write_atomically(file: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let part = file.with_file_name(format!(".{name}.lowkey.part"));
    {
        let mut f = std::fs::File::create(&part)?;
        f.write_all(text.as_bytes())?;
        f.set_permissions(std::fs::metadata(file)?.permissions())?;
        f.sync_all()?;
    }
    std::fs::rename(&part, file)?;
    if let Some(dir) = file.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// One bind on a key: its description, whether it fires on release, and whether it is from
/// before the rename.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BindInfo {
    pub description: String,
    pub release: bool,
    pub old_name: bool,
}

pub fn binds_on_key(binds: &[Value], key: &str) -> Vec<BindInfo> {
    let want = mods_mask(key);
    binds
        .iter()
        .filter(|b| combo(b) == want && !b["mouse"].as_bool().unwrap_or(false) && b["submap"].as_str().unwrap_or("").is_empty())
        .map(|b| BindInfo { description: description(b), release: b["release"].as_bool().unwrap_or(false), old_name: old_name(b) })
        .collect()
}

/// Lua binds do not show their command, so the app's binds are: the press bind with the app's
/// description, and a release bind without description on the same key. Anything else on the
/// key (an undescribed press bind included) is someone else's.
pub fn foreign_binds(on_key: &[BindInfo]) -> Vec<String> {
    foreign_binds_for(on_key, RUNTIME_DESC, true)
}

/// The same for the key whose binds carry `ours` (the dictation or the meeting key). The other
/// key of the app counts as someone else's: the two keys never share a combination.
/// `release_bind`: the key has an undescribed release bind of ours (the dictation key only).
pub fn foreign_binds_for(on_key: &[BindInfo], ours: &str, release_bind: bool) -> Vec<String> {
    let ours_here = release_bind && on_key.iter().any(|b| b.description == ours);
    on_key
        .iter()
        .filter(|b| !(b.description == ours || (ours_here && b.release && b.description.is_empty())))
        .map(|b| if b.description.is_empty() { "another shortcut".to_string() } else { b.description.clone() })
        .collect()
}

pub struct Hotkey {
    key: Mutex<String>,
    command: String,
    /// The description on the app's binds for this key.
    desc: &'static str,
    /// Dictation: press and release binds, and the user's config block can hold the key.
    /// Meeting: one bind on press.
    dictation: bool,
    state: Mutex<State>,
    stop: Arc<AtomicBool>,
    /// Held by change, install and adopt: one bind operation at a time (the reload watcher too).
    op: Mutex<()>,
}

#[derive(Default)]
struct State {
    added: bool,
    rule: bool,
    from_config: bool,
}

impl Hotkey {
    pub fn new(key: &str, exe: &Path) -> Arc<Self> {
        Arc::new(Self {
            key: Mutex::new(key.to_string()),
            command: dictate_command(exe),
            desc: RUNTIME_DESC,
            dictation: true,
            state: Mutex::default(),
            stop: Arc::default(),
            op: Mutex::new(()),
        })
    }

    /// The meeting key: a tap starts or stops a meeting recording.
    pub fn new_meeting(key: &str, exe: &Path) -> Arc<Self> {
        Self::new_single(key, meeting_command(exe), MEETING_DESC)
    }

    /// The paste-last key: a tap types the last dictation again.
    pub fn new_paste_last(key: &str, exe: &Path) -> Arc<Self> {
        Self::new_single(key, paste_command(exe), PASTE_DESC)
    }

    /// A key with one bind, on press.
    fn new_single(key: &str, command: String, desc: &'static str) -> Arc<Self> {
        Arc::new(Self {
            key: Mutex::new(key.to_string()),
            command,
            desc,
            dictation: false,
            state: Mutex::default(),
            stop: Arc::default(),
            op: Mutex::new(()),
        })
    }

    fn bind(&self, key: &str) -> String {
        if self.dictation {
            bind_lua(key, &self.command, self.desc)
        } else {
            bind_once_lua(key, &self.command, self.desc)
        }
    }

    fn foreign(&self, on_key: &[BindInfo]) -> Vec<String> {
        foreign_binds_for(on_key, self.desc, self.dictation)
    }

    pub fn key(&self) -> String {
        self.key.lock().clone()
    }

    /// "config" (the marked block in the user's Hyprland config), "app" (added by the app) or
    /// "none" (not active).
    pub fn source(&self) -> &'static str {
        let st = self.state.lock();
        if st.from_config {
            "config"
        } else if st.added {
            "app"
        } else {
            "none"
        }
    }

    /// Move the app's key to `new` now. Refuses a key that another bind uses, and a key that
    /// comes from the user's config (adopt it first).
    pub fn change(&self, new: &str) -> anyhow::Result<String> {
        let new = normalize_key(new).ok_or_else(|| anyhow::anyhow!("{new:?} is not a usable key: use Super, Ctrl or Alt with a letter, a digit, F1-F24 or Space"))?;
        let _op = self.op.lock();
        if self.state.lock().from_config {
            anyhow::bail!("the key comes from your Hyprland config: let the app manage it first");
        }
        let old = self.key();
        let all = binds();
        if new == old && self.state.lock().added {
            return Ok(self.active_text());
        }
        if let Some(other) = self.foreign(&binds_on_key(&all, &new)).into_iter().next() {
            anyhow::bail!("{new} is already used by: {other}");
        }
        // A leftover of ours on the new key (an earlier run): replace it, never doubled.
        if !binds_on_key(&all, &new).is_empty() {
            eval(&format!("hl.unbind({})", lua_str(&new)))?;
        }
        eval(&self.bind(&new))?;
        // The old key: only when every bind on it is ours (hl.unbind removes all binds of a key).
        if new != old && self.foreign(&binds_on_key(&all, &old)).is_empty() && !binds_on_key(&all, &old).is_empty() {
            let _ = eval(&format!("hl.unbind({})", lua_str(&old)));
        }
        *self.key.lock() = new;
        self.state.lock().added = true;
        Ok(self.active_text())
    }

    /// Take over a key from the user's config: remove the marked block (with a backup), reload
    /// Hyprland, and check that the config's key is gone and the app's key works. If not, the
    /// backup is put back and Hyprland reloaded again. Returns the backup's path.
    pub fn adopt(&self) -> anyhow::Result<PathBuf> {
        let _op = self.op.lock();
        let file = find_config_block()?;
        let backup = remove_config_block(&file)?;
        let result = (|| -> anyhow::Result<()> {
            reload()?;
            let key = self.key();
            if binds_on_key(&binds(), &key).iter().any(|b| b.description == CONFIG_DESC) {
                anyhow::bail!("{key} is still bound by your Hyprland config after the reload (another file has it?)");
            }
            let text = self.install_locked(false)?;
            if !self.state.lock().added {
                anyhow::bail!("{text}");
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(backup),
            Err(e) => {
                let restored = restore_config(&file, &backup).and_then(|_| reload());
                let _ = self.install_locked(true);
                match restored {
                    Ok(()) => anyhow::bail!("{e:#}. Your config was put back as it was (a copy is also in {}).", backup.display()),
                    Err(r) => anyhow::bail!("{e:#}. Putting your config back failed ({r:#}): restore it from {}.", backup.display()),
                }
            }
        }
    }

    /// keep_existing: the key started us, or a reload: an app-added bind is known to work.
    pub fn install(&self, keep_existing: bool) -> anyhow::Result<String> {
        let _op = self.op.lock();
        self.install_locked(keep_existing)
    }

    /// The state is computed first and written once at the end, so a status read meanwhile
    /// (or the reload watcher running this again) never sees the key as gone.
    fn install_locked(&self, keep_existing: bool) -> anyhow::Result<String> {
        if !lua_config() {
            *self.state.lock() = State::default();
            return Ok("not added: this Hyprland has no Lua config. Run `lowkey --print-snippet conf` and add the lines.".into());
        }
        eval(&overlay_rule_lua())?;
        let mut st = State { rule: true, ..State::default() };
        let key = self.key();
        let on_key = binds_on_key(&binds(), &key);
        let foreign = self.foreign(&on_key);
        let ours = on_key.iter().any(|b| b.description == self.desc);
        let text = if self.dictation && on_key.iter().any(|b| b.description == CONFIG_DESC) {
            st.from_config = true;
            format!("{key} (from your Hyprland config)")
        } else if ours && keep_existing && foreign.is_empty() && !on_key.iter().any(|b| b.old_name) {
            st.added = true;
            self.active_text()
        } else if let Some(other) = foreign.first() {
            format!("not added: {key} is already used by: {other}. Choose another key in Options.")
        } else {
            if ours {
                eval(&format!("hl.unbind({})", lua_str(&key)))?; // from an earlier run; may point elsewhere
            }
            eval(&self.bind(&key))?;
            st.added = true;
            self.active_text()
        };
        *self.state.lock() = st;
        Ok(text)
    }

    fn active_text(&self) -> String {
        match self.desc {
            RUNTIME_DESC => format!("{} — hold to talk, tap to start/stop (also starts the app when it is closed)", self.key()),
            PASTE_DESC => format!("{} — types the last dictation again", self.key()),
            _ => format!("{} — tap to start or stop a meeting recording (also starts the app when it is closed)", self.key()),
        }
    }

    pub fn rule_installed(&self) -> bool {
        self.state.lock().rule
    }

    #[cfg(not(unix))]
    pub fn watch_reloads(self: &Arc<Self>) {}

    /// Re-install after `configreloaded` events from Hyprland's event socket.
    #[cfg(unix)]
    pub fn watch_reloads(self: &Arc<Self>) {
        let Some(sock) = event_socket() else { return };
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            while !me.stop.load(Ordering::Relaxed) {
                if let Ok(stream) = UnixStream::connect(&sock) {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    while !me.stop.load(Ordering::Relaxed) {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) => break,
                            Ok(_) if line.starts_with("configreloaded>>") => {
                                std::thread::sleep(Duration::from_millis(300));
                                let _op = me.op.lock(); // after a running change/adopt
                                let was = { let s = me.state.lock(); s.added || s.rule };
                                if was {
                                    eprintln!("  Hyprland config reloaded: adding the {} key again", me.desc);
                                    let _ = me.install_locked(true);
                                }
                            }
                            Ok(_) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
                            Err(_) => break,
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }

    /// On exit the bind stays (it starts the app again); our overlay rule goes.
    pub fn uninstall(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut st = self.state.lock();
        if st.rule {
            let _ = eval("if _G.lowkey_desktop_rule then _G.lowkey_desktop_rule:set_enabled(false); _G.lowkey_desktop_rule = nil end");
            st.rule = false;
        }
    }
}

#[cfg(unix)]
fn event_socket() -> Option<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let base = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let p = PathBuf::from(base).join("hypr").join(sig).join(".socket2.sock");
    p.exists().then_some(p)
}

/// Address of the focused window (None off Hyprland or on error).
pub fn focused_window() -> Option<String> {
    if !running() {
        return None;
    }
    serde_json::from_str::<Value>(&hyprctl(&["activewindow", "-j"])).ok()?["address"].as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lua_strings() {
        assert_eq!(lua_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(lua_str("Año/ñ"), "\"Año/ñ\"");
        assert_eq!(lua_str("x\ny"), "\"x\\010y\"");
    }

    #[test]
    fn the_strips_place_and_size() {
        let open = overlay_rule_lua_at(&OverlayPlace::default());
        assert!(open.contains("size = { 380, 76 }") && open.contains(r#"move = { "(monitor_w/2-190)", "(monitor_h-150)" }"#), "{open}");
        let pill = overlay_rule_lua_at(&OverlayPlace { collapsed: true, at: Some([0.25, 1.5]) });
        assert!(pill.contains("size = { 150, 44 }") && pill.contains(r#"move = { "((monitor_w-150)*0.2500)", "((monitor_h-44)*1.0000)" }"#), "{pill}");
        // Where it is on screen, read back from hyprctl's JSON (a 2x monitor at 1920,0).
        let clients = json!([
            { "title": "other", "address": "0x1", "at": [0, 0], "size": [10, 10], "monitor": 1 },
            { "title": OVERLAY_TITLE, "address": "0xabc", "mapped": true, "at": [2200, 900], "size": [380, 76], "monitor": 1 }
        ]);
        let monitors = json!([{ "id": 0, "x": 0, "y": 0, "width": 1920, "height": 1080, "scale": 1.0 },
                              { "id": 1, "x": 1920, "y": 0, "width": 3840, "height": 2160, "scale": 2.0 }]);
        let g = geometry_from(&clients, &monitors).unwrap();
        assert_eq!((g.address, g.monitor_at, g.monitor_size), (0xabc, (1920.0, 0.0), (1920.0, 1080.0)));
        let [fx, fy] = g.fractions();
        assert!((fx - 280.0 / 1540.0).abs() < 1e-4 && (fy - 900.0 / 1004.0).abs() < 1e-4, "{fx} {fy}");
        assert_eq!(geometry_from(&json!([]), &monitors), None);
    }

    #[test]
    fn binds_and_snippets() {
        let exe = Path::new("/opt/low key/lowkey");
        let lua = bind_lua("SUPER + CTRL + X", &dictate_command(exe), RUNTIME_DESC);
        assert!(lua.contains("'/opt/low key/lowkey' --dictate press"));
        assert!(lua.contains("release = true") && lua.contains("added by the app"));
        // Pointer yes (drag, collapse), focus no: no_focus would take the pointer away too.
        let rule = overlay_rule_lua();
        assert!(rule.contains("no_follow_mouse = true") && rule.contains("no_initial_focus = true") && !rule.contains("no_focus = true"));
        assert!(rule.contains("lowkey_desktop_rule"));
        assert_ne!(overlay_rule_lua(), overlay_rule_lua(), "a new name each time (Hyprland keeps the first rule of a name)");
        // No plugin fields (an unknown field is a config error): decorate = false covers title bars.
        assert!(overlay_rule_lua().contains("decorate = false") && !overlay_rule_lua().contains("hyprbars"));
        let conf = conf_snippet("SUPER + CTRL + X", Path::new("/usr/bin/lowkey"));
        assert!(conf.contains("bind = SUPER CTRL, X, exec, /usr/bin/lowkey --dictate press"));
        assert!(conf.contains("bindr = SUPER CTRL, X, exec, /usr/bin/lowkey --dictate release"));
    }

    #[test]
    fn key_masks() {
        assert_eq!(mods_mask("SUPER + CTRL + X"), (68, "X".into()));
        assert_eq!(mods_mask("ALT + F9"), (8, "F9".into()));
    }

    #[test]
    fn keys_are_normalized_and_plain_keys_refused() {
        assert_eq!(normalize_key("ctrl+super+x").as_deref(), Some("SUPER + CTRL + X"));
        assert_eq!(normalize_key("Meta + Alt + d").as_deref(), Some("SUPER + ALT + D"));
        assert_eq!(normalize_key("ALT + F9").as_deref(), Some("ALT + F9"));
        assert_eq!(normalize_key("ctrl + alt + space").as_deref(), Some("CTRL + ALT + SPACE"));
        for bad in ["X", "SHIFT + X", "CTRL + HYPER + X", "CTRL + F30", "CTRL + ENTER", "", "CTRL +"] {
            assert_eq!(normalize_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_the_marked_block_is_removed() {
        let text = "a = 1\n-- BEGIN open-whisper.dictation\nhl.bind(...)\nhl.bind(...)\n-- END open-whisper.dictation\nb = 2\n";
        assert_eq!(without_block(text).unwrap(), "a = 1\nb = 2\n");
        assert_eq!(without_block("a = 1\n"), None);
        assert_eq!(without_block("-- BEGIN open-whisper.dictation\nno end\n"), None, "never cut to the end of the file");
        // CRLF, two blocks, the last line without a newline.
        let crlf = "a\r\n-- BEGIN open-whisper.dictation\r\nx\r\n-- END open-whisper.dictation\r\nb\r\n-- BEGIN open-whisper.dictation\r\ny\r\n-- END open-whisper.dictation";
        assert_eq!(without_block(crlf).unwrap(), "a\r\nb\r\n");
        assert!(config_candidate(Path::new("/h/bindings.lua")));
        for skip in ["/h/bindings.lua.bak.123", "/h/bindings copy.lua", "/h/bindings-old.lua", "/h/.bindings.lua.lowkey.part", "/h/hyprland.conf"] {
            assert!(!config_candidate(Path::new(skip)), "{skip}");
        }
        let dir = std::env::temp_dir().join(format!("ow-block-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("bindings.lua");
        std::fs::write(&f, text).unwrap();
        // A symlinked config (dotfile managers): the target is edited, the link stays a link.
        let target = dir.join("real-bindings.lua");
        std::fs::rename(&f, &target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &f).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&target, &f).unwrap();
        let backup = remove_config_block(&f).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a = 1\nb = 2\n");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), text, "the backup is the old file");
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(&f).unwrap().file_type().is_symlink(), "still a link");
        restore_config(&f, &backup).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text, "restored");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn binds_and_blocks_from_before_the_rename_are_still_ours() {
        let old = json!({"modmask": 68, "key": "X", "description": "Dictate (open-whisper, added by the app)", "submap": ""});
        assert_eq!(description(&old), RUNTIME_DESC);
        assert_eq!(description(&json!({"description": "Dictate (open-whisper)"})), CONFIG_DESC);
        assert_eq!(description(&json!({"description": "Paste last dictation (open-whisper, added by the app)"})), PASTE_DESC);
        assert_eq!(description(&json!({"description": "Screenshot"})), "Screenshot");
        let on_key = binds_on_key(&[old.clone()], "SUPER + CTRL + X");
        assert!(on_key[0].old_name, "re-added, so the key runs the new program");
        assert_eq!(removable_app_keys(&[old]), vec!["SUPER + CTRL + X"]);
        let text = "a\n-- BEGIN open-whisper.dictation\nx\n-- END open-whisper.dictation\nb\n-- BEGIN lowkey.dictation\ny\n-- END lowkey.dictation\n";
        assert_eq!(without_block(text).as_deref(), Some("a\nb\n"));
    }

    #[test]
    fn the_apps_binds_are_told_apart_from_others() {
        let b = |d: &str, r: bool| BindInfo { description: d.into(), release: r, ..Default::default() };
        // The app's press + release: nothing foreign.
        assert!(foreign_binds(&[b(RUNTIME_DESC, false), b("", true)]).is_empty());
        // A release bind without our press bind, an undescribed press bind, a described one: foreign.
        assert_eq!(foreign_binds(&[b("", true)]), vec!["another shortcut"]);
        assert_eq!(foreign_binds(&[b(RUNTIME_DESC, false), b("", true), b("", false)]), vec!["another shortcut"]);
        assert_eq!(foreign_binds(&[b("Screenshot", false)]), vec!["Screenshot"]);
        assert_eq!(foreign_binds(&[b(CONFIG_DESC, false), b("", true)]), vec![CONFIG_DESC.to_string(), "another shortcut".into()]);
    }

    #[test]
    fn the_meeting_key_and_the_dictation_key_never_share_a_combination() {
        let dictation = BindInfo { description: RUNTIME_DESC.into(), ..Default::default() };
        let release = BindInfo { release: true, ..Default::default() };
        let meeting = BindInfo { description: MEETING_DESC.into(), ..Default::default() };
        assert!(foreign_binds_for(&[meeting.clone()], MEETING_DESC, false).is_empty());
        assert_eq!(foreign_binds_for(&[dictation.clone(), release.clone()], MEETING_DESC, false), vec![RUNTIME_DESC.to_string(), "another shortcut".into()]);
        // A user's release bind next to the meeting key is theirs (the meeting key has none).
        assert_eq!(foreign_binds_for(&[meeting.clone(), release.clone()], MEETING_DESC, false), vec!["another shortcut".to_string()]);
        assert_eq!(foreign_binds(&[meeting]), vec![MEETING_DESC.to_string()]);
        assert!(foreign_binds(&[dictation, release]).is_empty());
        assert_eq!(
            bind_once_lua("SUPER + CTRL + M", &meeting_command(Path::new("/opt/o w/lowkey")), MEETING_DESC),
            "hl.bind(\"SUPER + CTRL + M\", hl.dsp.exec_cmd(\"'/opt/o w/lowkey' --meeting toggle\"), { description = \"Meeting recording (Lowkey, added by the app)\" })"
        );
        let binds = vec![json!({"modmask": 68, "key": "M", "description": MEETING_DESC, "submap": ""})];
        assert_eq!(removable_app_keys(&binds), vec!["SUPER + CTRL + M".to_string()], "--remove-hotkey removes the meeting key too");
    }

    #[test]
    fn never_unbind_a_user_key() {
        let binds = vec![
            json!({"modmask": 68, "key": "X", "description": CONFIG_DESC, "submap": ""}),
            json!({"modmask": 68, "key": "X", "description": RUNTIME_DESC, "submap": ""}),
            json!({"modmask": 72, "key": "D", "description": RUNTIME_DESC, "submap": ""}),
            json!({"modmask": 72, "key": "D", "description": "", "release": true, "submap": ""}),
        ];
        assert_eq!(removable_app_keys(&binds), vec!["SUPER + ALT + D".to_string()]);
        assert_eq!(owners_in(&binds, "SUPER + CTRL + X"), vec![CONFIG_DESC.to_string(), RUNTIME_DESC.to_string()]);
    }
}
