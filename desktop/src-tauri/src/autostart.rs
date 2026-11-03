//! "Start at login". Linux: an XDG autostart entry in $XDG_CONFIG_HOME/autostart (written here,
//! so it follows XDG_CONFIG_HOME: tests never touch the real one). macOS and Windows: the Tauri
//! autostart plugin (a launch agent / the Run registry key).

use std::path::{Path, PathBuf};

pub const FILE: &str = "lowkey.desktop";

pub fn dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("autostart")
}

/// The program the entry starts: the AppImage itself when the app runs from one (its own path
/// is a temporary mount), else this executable.
pub fn launcher() -> PathBuf {
    std::env::var_os("APPIMAGE").map(PathBuf::from).unwrap_or_else(|| std::env::current_exe().unwrap_or_default())
}

/// The entry: the app hidden (`--background`), so the dictation key works after login.
pub fn entry(exe: &Path) -> String {
    // Desktop Entry spec: quote the program; escape ", `, $ and \ inside the quotes.
    let mut quoted = String::new();
    for c in exe.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"), // % starts a field code
            c => quoted.push(c),
        }
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=Lowkey\nComment=Local speech to text: the dictation key, in the background\nExec=\"{quoted}\" --background\nIcon=lowkey\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

pub fn set_in(dir: &Path, on: bool, exe: &Path) -> std::io::Result<()> {
    let f = dir.join(FILE);
    if on {
        std::fs::create_dir_all(dir)?;
        std::fs::write(f, entry(exe))
    } else {
        match std::fs::remove_file(f) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
}

/// On when the entry exists and the program it starts is still there (a moved or deleted
/// build would start nothing at login).
pub fn enabled_in(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else { return false };
    exec_program(&text).is_some_and(|p| Path::new(&p).is_file())
}

/// The program in the entry's `Exec="…"` line, unescaped.
pub fn exec_program(text: &str) -> Option<String> {
    let line = text.lines().find_map(|l| l.strip_prefix("Exec=\""))?;
    let (mut out, mut chars) = (String::new(), line.chars());
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out.replace("%%", "%")),
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_written_quoted_and_removed() {
        let root = std::env::temp_dir().join(format!("ow-autostart-{}", std::process::id()));
        let dir = root.join("autostart");
        let exe = Path::new("/opt/my apps/lowkey");
        assert!(!enabled_in(&dir));
        set_in(&dir, true, exe).unwrap();
        assert!(!enabled_in(&dir), "the program does not exist: the entry would start nothing");
        let real = std::env::current_exe().unwrap();
        set_in(&dir, true, &real).unwrap();
        assert!(enabled_in(&dir));
        set_in(&dir, true, exe).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(text.contains("Exec=\"/opt/my apps/lowkey\" --background"), "{text}");
        set_in(&dir, false, exe).unwrap();
        set_in(&dir, false, exe).unwrap(); // already off: fine
        assert!(!enabled_in(&dir));
        assert!(entry(Path::new("/a\"b$c")).contains(r#"Exec="/a\"b\$c" --background"#));
        let odd = Path::new("/x/100% \"real\"/lowkey");
        assert!(entry(odd).contains("100%%"));
        assert_eq!(exec_program(&entry(odd)).as_deref(), Some("/x/100% \"real\"/lowkey"));
        let _ = std::fs::remove_dir_all(root);
    }
}
