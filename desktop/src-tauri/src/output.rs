//! Text output for global dictation: clipboard + typing into the focused app, per platform.
//!
//! Linux Wayland: wl-copy and wtype (virtual keyboard; Hyprland, sway and other wlroots).
//! Linux X11: xclip/xsel and xdotool. macOS/Windows: arboard + enigo.
//! The text is given on stdin, never argv, so other processes cannot read it.

use std::io::Write;
use std::process::{Command, Stdio};

/// Two dictations within this many seconds (end of the new recording vs. the last typed text),
/// into the same window, are one flow of text: the second gets a leading space.
pub const JOIN_WINDOW_S: f64 = 15.0;
/// No leading space before marks that close or continue a sentence (quotes/dashes can open one).
const NO_SPACE_BEFORE: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '…', '%', '»', '”', '’'];

pub fn joining_space(text: &str, now: f64, last_at: Option<f64>, same_window: bool) -> &'static str {
    match last_at {
        Some(t) if now - t <= JOIN_WINDOW_S && same_window => {}
        _ => return "",
    }
    match text.chars().next() {
        Some(c) if !c.is_whitespace() && !NO_SPACE_BEFORE.contains(&c) => " ",
        _ => "",
    }
}

fn pipe(cmd: &str, args: &[&str], input: &str) -> bool {
    let Ok(mut child) = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Copy `text` and type `typed_prefix + text`. Returns what worked: "copied", "typed".
pub fn deliver(text: &str, typed_prefix: &str) -> Vec<&'static str> {
    let mut done = Vec::new();
    if copy(text) {
        done.push("copied");
    }
    if type_text(&format!("{typed_prefix}{text}")) {
        done.push("typed");
    }
    done
}

#[cfg(target_os = "linux")]
pub fn copy(text: &str) -> bool {
    if wayland() {
        pipe("wl-copy", &[], text)
    } else {
        pipe("xclip", &["-selection", "clipboard"], text) || pipe("xsel", &["--clipboard", "--input"], text)
    }
}

#[cfg(target_os = "linux")]
fn type_text(text: &str) -> bool {
    if wayland() {
        pipe("wtype", &["-"], text)
    } else {
        pipe("xdotool", &["type", "--clearmodifiers", "--file", "-"], text)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn copy(text: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn type_text(text: &str) -> bool {
    use enigo::{Enigo, Keyboard, Settings};
    // macOS asks once for Accessibility permission; until then this fails and the
    // text is still on the clipboard.
    match Enigo::new(&Settings::default()) {
        Ok(mut e) => e.text(text).is_ok(),
        Err(_) => false,
    }
}

pub fn notify(message: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("notify-send").args(["-a", "Lowkey", "Lowkey", message]).spawn();
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("lowkey: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_rule() {
        for (text, gap, same, want) in [
            ("Hello", 5.0, true, " "),
            ("Hello", 16.0, true, ""),
            ("Hello", 5.0, false, ""),
            (", and more", 5.0, true, ""),
            ("? Really", 5.0, true, ""),
            ("…well", 5.0, true, ""),
            ("\"Quoted\" words", 5.0, true, " "),
            ("-5 degrees", 5.0, true, " "),
            ("» y sigue", 5.0, true, ""),
            (" Hello", 5.0, true, ""),
        ] {
            assert_eq!(joining_space(text, 100.0 + gap, Some(100.0), same), want, "{text}");
            assert_eq!(joining_space(text, 100.0, None, same), "");
        }
    }
}
