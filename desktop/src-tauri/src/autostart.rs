//! "Start at login". Linux: an XDG autostart entry in $XDG_CONFIG_HOME/autostart (written here,
//! so it follows XDG_CONFIG_HOME: tests never touch the real one). macOS and Windows: the Tauri
//! autostart plugin (a launch agent / the Run registry key).

use std::path::{Path, PathBuf};

pub const FILE: &str = "open-whisper.desktop";

pub fn dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("autostart")
}

/// The entry: the app hidden (`--background`), so the dictation key works after login.
pub fn entry(exe: &Path) -> String {
    // Desktop Entry spec: quote the program; escape ", `, $ and \ inside the quotes.
    let mut quoted = String::new();
    for c in exe.to_string_lossy().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=open-whisper\nComment=Local speech to text: the dictation key, in the background\nExec=\"{quoted}\" --background\nIcon=open-whisper\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
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

pub fn enabled_in(dir: &Path) -> bool {
    dir.join(FILE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_written_quoted_and_removed() {
        let root = std::env::temp_dir().join(format!("ow-autostart-{}", std::process::id()));
        let dir = root.join("autostart");
        let exe = Path::new("/opt/my apps/open-whisper");
        assert!(!enabled_in(&dir));
        set_in(&dir, true, exe).unwrap();
        assert!(enabled_in(&dir));
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(text.contains("Exec=\"/opt/my apps/open-whisper\" --background"), "{text}");
        set_in(&dir, false, exe).unwrap();
        set_in(&dir, false, exe).unwrap(); // already off: fine
        assert!(!enabled_in(&dir));
        assert!(entry(Path::new("/a\"b$c")).contains(r#"Exec="/a\"b\$c" --background"#));
        let _ = std::fs::remove_dir_all(root);
    }
}
