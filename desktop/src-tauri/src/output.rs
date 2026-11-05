//! Text output for global dictation: clipboard + typing into the focused app, per platform.
//!
//! Linux Wayland: wl-copy and wtype (virtual keyboard; Hyprland, sway and other wlroots).
//! Linux X11: xclip/xsel and xdotool. macOS/Windows: arboard + enigo.
//! The text is given on stdin, never argv, so other processes cannot read it.
//!
//! A line break is typed as Shift+Enter, never as Enter: in a chat box (Slack, Teams, a web
//! chat) Enter sends the message, and the rest of the dictation would go into a new one.
//! Shift+Enter is a new line there and in editors. The clipboard keeps the text as it is.

#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
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

/// What to type: text, and the line breaks between it.
#[derive(Debug, PartialEq)]
enum Piece<'a> {
    Text(&'a str),
    /// One line break (Shift+Enter).
    Break,
}

/// `text` cut at its line breaks (\n, \r\n, \r). Each break is one `Break`, so a blank line
/// between paragraphs stays.
fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(['\n', '\r']) {
        if at > 0 {
            out.push(Piece::Text(&rest[..at]));
        }
        out.push(Piece::Break);
        let len = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
        rest = &rest[at + len..];
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest));
    }
    out
}

/// Type `text`: its parts with `type_part`, a Shift+Enter with `soft_break` between them.
fn type_pieces(text: &str, type_part: &mut dyn FnMut(&str) -> bool, soft_break: &mut dyn FnMut() -> bool) -> bool {
    // A tab would move the focus to another field: a space instead.
    let text = text.replace('\t', " ");
    pieces(&text).into_iter().all(|p| match p {
        Piece::Text(t) => type_part(t),
        Piece::Break => soft_break(),
    })
}

/// Run `cmd` with `input` on its stdin (Linux: the clipboard and typing tools).
#[cfg(target_os = "linux")]
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
    let run = |cmd: &str, args: &[&str]| Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if wayland() {
        type_pieces(text, &mut |t| pipe("wtype", &["-"], t), &mut || run("wtype", &["-M", "shift", "-k", "Return", "-m", "shift"]))
    } else {
        type_pieces(
            text,
            &mut |t| pipe("xdotool", &["type", "--clearmodifiers", "--file", "-"], t),
            &mut || run("xdotool", &["key", "--clearmodifiers", "shift+Return"]),
        )
    }
}

#[cfg(not(target_os = "linux"))]
pub fn copy(text: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn type_text(text: &str) -> bool {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    // macOS asks once for Accessibility permission; until then this fails and the
    // text is still on the clipboard.
    let Ok(e) = Enigo::new(&Settings::default()) else { return false };
    let e = std::cell::RefCell::new(e);
    type_pieces(text, &mut |t| e.borrow_mut().text(t).is_ok(), &mut || {
        let mut e = e.borrow_mut();
        let ok = e.key(Key::Shift, Direction::Press).is_ok() && e.key(Key::Return, Direction::Click).is_ok();
        e.key(Key::Shift, Direction::Release).is_ok() && ok
    })
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
    fn line_breaks_are_typed_as_shift_enter() {
        use Piece::*;
        assert_eq!(pieces("one line"), [Text("one line")]);
        assert_eq!(pieces("First.\n\nSecond.\r\nThird.\rEnd"), [Text("First."), Break, Break, Text("Second."), Break, Text("Third."), Break, Text("End")]);
        assert_eq!(pieces("\nhi\n"), [Break, Text("hi"), Break]);
        assert_eq!(pieces("¿Sí?\n¡No!"), [Text("¿Sí?"), Break, Text("¡No!")], "cut at bytes of ASCII breaks only");
        let mut typed = String::new();
        let typed_ref = std::cell::RefCell::new(&mut typed);
        let ok = type_pieces(
            "Hello\tthere.\n\nNew paragraph.",
            &mut |t| {
                typed_ref.borrow_mut().push_str(t);
                true
            },
            &mut || {
                typed_ref.borrow_mut().push_str("<shift+enter>");
                true
            },
        );
        assert!(ok);
        assert_eq!(typed, "Hello there.<shift+enter><shift+enter>New paragraph.", "never a bare Enter; a tab is a space");
        // A part that fails stops the rest (the text is on the clipboard).
        let mut n = 0;
        assert!(!type_pieces("a\nb", &mut |_| false, &mut || {
            n += 1;
            true
        }));
        assert_eq!(n, 0);
    }

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
