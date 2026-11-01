//! Hyprland integration (Lua config, Hyprland 0.56+): the global key and the overlay window rule.
//!
//! Only API calls verified safe on a live session are used here: `hl.bind`, `hl.unbind`,
//! `hl.window_rule` + `set_enabled`. (`Keybind:remove()` aborts Hyprland 0.56.2.) Nothing edits
//! config files: binds are added to the running Hyprland and re-added after a config reload.

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

/// Binds that the app adds say so; the permanent config snippet says just "Dictate (open-whisper)".
pub const RUNTIME_DESC: &str = "Dictate (open-whisper, added by the app)";
pub const CONFIG_DESC: &str = "Dictate (open-whisper)";
/// The overlay window's title; the window rule matches it.
pub const OVERLAY_TITLE: &str = "open-whisper-overlay";

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

/// Floating, pinned, never focused, bottom centre. A restart replaces it (Lua global).
/// `decorate = false` turns off every decoration, title-bar plugins too (hyprbars draws its
/// bar on every window): no plugin-specific field, so nothing depends on a plugin being loaded.
/// `rounding = 0` and `no_blur`: the window is transparent, the page draws the rounded strip.
pub fn overlay_rule_lua() -> String {
    format!(
        "if _G.open_whisper_desktop_rule then _G.open_whisper_desktop_rule:set_enabled(false) end\n\
         _G.open_whisper_desktop_rule = hl.window_rule({{ name = \"open-whisper-desktop-overlay\", \
         match = {{ title = {} }}, float = true, pin = true, no_initial_focus = true, no_focus = true, \
         size = {{ 380, 76 }}, move = {{ \"(monitor_w/2-190)\", \"(monitor_h-150)\" }}, \
         border_size = 0, rounding = 0, no_shadow = true, no_blur = true, decorate = false, no_anim = true }})",
        lua_str(&format!("^{OVERLAY_TITLE}$"))
    )
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

/// The command a bind runs: this executable with --dictate.
pub fn dictate_command(exe: &Path) -> String {
    format!("{} --dictate", shell_quote(&exe.to_string_lossy()))
}

/// Lines for ~/.config/hypr/bindings.lua (permanent key).
pub fn permanent_snippet(key: &str, exe: &Path) -> String {
    format!(
        "-- open-whisper: global dictation (hold = talk, tap = start/stop)\n{}\n{}",
        bind_lua(key, &dictate_command(exe), CONFIG_DESC),
        overlay_rule_lua()
    )
}

/// Lines for a classic hyprland.conf (no overlay rule: see README).
pub fn conf_snippet(key: &str, exe: &Path) -> String {
    let (mods, k) = key.rsplit_once('+').map(|(m, k)| (m, k.trim())).unwrap_or(("", key.trim()));
    let mods = mods.split('+').map(str::trim).filter(|m| !m.is_empty()).collect::<Vec<_>>().join(" ");
    let cmd = dictate_command(exe);
    format!("# open-whisper: global dictation (hold = talk, tap = start/stop)\nbind = {mods}, {k}, exec, {cmd} press\nbindr = {mods}, {k}, exec, {cmd} release")
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
        .map(|b| b["description"].as_str().unwrap_or("").to_string())
        .collect()
}

/// Keys whose binds the app added and that no user bind shares (hl.unbind is per key).
pub fn removable_app_keys(binds: &[Value]) -> Vec<String> {
    let names = [(64u64, "SUPER"), (4, "CTRL"), (8, "ALT"), (1, "SHIFT")];
    let user: std::collections::HashSet<_> = binds
        .iter()
        .filter(|b| !matches!(b["description"].as_str().unwrap_or(""), "" | RUNTIME_DESC))
        .map(combo)
        .collect();
    let mut out = Vec::new();
    for b in binds {
        if b["description"].as_str() == Some(RUNTIME_DESC) && !user.contains(&combo(b)) && b["submap"].as_str().unwrap_or("").is_empty() {
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

pub struct Hotkey {
    pub key: String,
    command: String,
    state: Mutex<State>,
    stop: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    added: bool,
    rule: bool,
    from_config: bool,
}

impl Hotkey {
    pub fn new(key: &str, exe: &Path) -> Arc<Self> {
        Arc::new(Self { key: key.to_string(), command: dictate_command(exe), state: Mutex::default(), stop: Arc::default() })
    }

    /// keep_existing: the key started us, or a reload: an app-added bind is known to work.
    pub fn install(&self, keep_existing: bool) -> anyhow::Result<String> {
        if !lua_config() {
            return Ok("not added: this Hyprland has no Lua config. Run `open-whisper --print-snippet conf` and add the lines.".into());
        }
        eval(&overlay_rule_lua())?;
        let mut st = self.state.lock();
        st.rule = true;
        let owners = key_owners(&self.key);
        if owners.iter().any(|o| o == CONFIG_DESC) {
            st.from_config = true;
            return Ok(format!("{} (from your Hyprland config)", self.key));
        }
        let ours = owners.iter().any(|o| o == RUNTIME_DESC);
        if ours && keep_existing {
            st.added = true;
            return Ok(self.active_text());
        }
        if ours {
            eval(&format!("hl.unbind({})", lua_str(&self.key)))?; // from an earlier run; may point elsewhere
        } else if let Some(other) = owners.first() {
            let what = if other.is_empty() { "no description".to_string() } else { other.clone() };
            return Ok(format!("not added: {} is already bound ({what}). Use --hotkey.", self.key));
        }
        eval(&bind_lua(&self.key, &self.command, RUNTIME_DESC))?;
        st.added = true;
        Ok(self.active_text())
    }

    fn active_text(&self) -> String {
        format!("{} — hold to talk, tap to start/stop (also starts open-whisper when it is closed)", self.key)
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
                                let was = { let s = me.state.lock(); s.added || s.rule };
                                if was {
                                    eprintln!("  Hyprland config reloaded: adding the dictation key again");
                                    *me.state.lock() = State::default();
                                    let _ = me.install(true);
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
            let _ = eval("if _G.open_whisper_desktop_rule then _G.open_whisper_desktop_rule:set_enabled(false); _G.open_whisper_desktop_rule = nil end");
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
    fn binds_and_snippets() {
        let exe = Path::new("/opt/open whisper/open-whisper");
        let lua = bind_lua("SUPER + CTRL + X", &dictate_command(exe), RUNTIME_DESC);
        assert!(lua.contains("'/opt/open whisper/open-whisper' --dictate press"));
        assert!(lua.contains("release = true") && lua.contains("added by the app"));
        assert!(overlay_rule_lua().contains("no_focus = true") && overlay_rule_lua().contains("open_whisper_desktop_rule"));
        // No plugin fields (an unknown field is a config error): decorate = false covers title bars.
        assert!(overlay_rule_lua().contains("decorate = false") && !overlay_rule_lua().contains("hyprbars"));
        let conf = conf_snippet("SUPER + CTRL + X", Path::new("/usr/bin/open-whisper"));
        assert!(conf.contains("bind = SUPER CTRL, X, exec, /usr/bin/open-whisper --dictate press"));
        assert!(conf.contains("bindr = SUPER CTRL, X, exec, /usr/bin/open-whisper --dictate release"));
    }

    #[test]
    fn key_masks() {
        assert_eq!(mods_mask("SUPER + CTRL + X"), (68, "X".into()));
        assert_eq!(mods_mask("ALT + F9"), (8, "F9".into()));
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
