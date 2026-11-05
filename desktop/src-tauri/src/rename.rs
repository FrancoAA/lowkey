//! The app was called "open-whisper" before. At start, before any path is read, its folders
//! move to the new name: the settings (~/.config/open-whisper → ~/.config/lowkey) and the data
//! (models, meetings, history: ~/.local/share/open-whisper → ~/.local/share/lowkey), and the
//! Linux "Start at login" entry is written again for the new program. An old app that still
//! runs is asked to quit first. When a folder cannot move, this run keeps the old folders.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const NAME: &str = "lowkey";
pub const OLD_NAME: &str = "open-whisper";

static FOLDER: OnceLock<&'static str> = OnceLock::new();

/// The folder name under the config and data folders: "lowkey", or "open-whisper" for a run
/// in which the old folders could not move.
pub fn folder() -> &'static str {
    FOLDER.get().copied().unwrap_or(NAME)
}

/// Move the old folders and the autostart entry (once per run; nothing to do: no change).
pub fn migrate() {
    if FOLDER.get().is_some() {
        return;
    }
    let bases: Vec<PathBuf> = {
        let mut v: Vec<PathBuf> = [dirs::config_dir(), dirs::data_dir()].into_iter().flatten().collect();
        v.dedup(); // macOS and Windows: one folder for both
        v
    };
    let folder = if bases.iter().any(|b| b.join(OLD_NAME).is_dir()) {
        stop_old_app();
        match move_folders(&bases) {
            Ok(()) => NAME,
            Err(e) => {
                eprintln!("  could not move the open-whisper folders to lowkey ({e}); this run uses the old ones");
                OLD_NAME
            }
        }
    } else {
        NAME
    };
    let _ = FOLDER.set(folder);
    #[cfg(target_os = "linux")]
    if let Err(e) = move_autostart(&crate::autostart::dir()) {
        eprintln!("  could not update the Start at login entry: {e}");
    }
}

/// Every `<base>/open-whisper` becomes `<base>/lowkey` (a base that has both keeps them as
/// they are). All or none: one that fails puts the others back.
pub fn move_folders(bases: &[PathBuf]) -> std::io::Result<()> {
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for base in bases {
        let (old, new) = (base.join(OLD_NAME), base.join(NAME));
        if !old.is_dir() || new.exists() {
            continue;
        }
        if let Err(e) = std::fs::rename(&old, &new) {
            for (o, n) in done.iter().rev() {
                let _ = std::fs::rename(n, o);
            }
            return Err(e);
        }
        done.push((old, new));
    }
    Ok(())
}

/// An old app that runs (it writes its program and pid to its runtime folder): ask it to quit,
/// and wait for it (up to 10 s), so it does not write to the folders while they move.
fn stop_old_app() {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(OLD_NAME);
    let Some(v) = std::fs::read_to_string(dir.join("desktop.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return };
    let (Some(exe), Some(pid)) = (v["exe"].as_str(), v["pid"].as_u64()) else { return };
    // Only that pid while it is still the old app (a pid can be used again).
    let alive = || std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == OLD_NAME);
    if !cfg!(target_os = "linux") || !alive() || pid == std::process::id() as u64 {
        return;
    }
    eprintln!("  asking the open-whisper app (pid {pid}) to quit before its folders move");
    if Path::new(exe).is_file() {
        // It quits the normal way: its keys and window rule are removed.
        let _ = std::process::Command::new(exe).args(["--dictate", "quit"]).status();
    } else {
        // Its program was removed (a new install): a plain stop.
        let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    }
    let t0 = Instant::now();
    while alive() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The old entry (open-whisper.desktop that starts a program named open-whisper) becomes the
/// new one. It starts the new program next to the old one when there is one (an install in
/// ~/.local/bin), else this program.
#[cfg(target_os = "linux")]
pub fn move_autostart(dir: &Path) -> std::io::Result<()> {
    let old = dir.join(format!("{OLD_NAME}.desktop"));
    let Ok(text) = std::fs::read_to_string(&old) else { return Ok(()) };
    let Some(program) = crate::autostart::exec_program(&text).map(PathBuf::from) else { return Ok(()) };
    if program.file_name().and_then(|n| n.to_str()) != Some(OLD_NAME) {
        return Ok(()); // not ours
    }
    if !dir.join(crate::autostart::FILE).exists() {
        let next = program.with_file_name(NAME);
        let exe = if next.is_file() { next } else { crate::autostart::launcher() };
        crate::autostart::set_in(dir, true, &exe)?;
    }
    std::fs::remove_file(old)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lowkey-rename-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_old_folders_move_and_a_new_one_is_never_overwritten() {
        let root = temp("folders");
        let (config, data) = (root.join("config"), root.join("data"));
        std::fs::create_dir_all(config.join(OLD_NAME)).unwrap();
        std::fs::write(config.join(OLD_NAME).join("config.toml"), "model = \"small\"\n").unwrap();
        std::fs::create_dir_all(data.join(OLD_NAME).join("meetings")).unwrap();
        move_folders(&[config.clone(), data.clone()]).unwrap();
        assert_eq!(std::fs::read_to_string(config.join(NAME).join("config.toml")).unwrap(), "model = \"small\"\n");
        assert!(data.join(NAME).join("meetings").is_dir() && !data.join(OLD_NAME).exists());
        // Both there: nothing moves.
        std::fs::create_dir_all(config.join(OLD_NAME)).unwrap();
        move_folders(&[config.clone()]).unwrap();
        assert!(config.join(OLD_NAME).is_dir() && config.join(NAME).join("config.toml").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_autostart_entry_starts_the_new_program() {
        let root = temp("autostart");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(NAME), "").unwrap();
        let auto = root.join("autostart");
        std::fs::create_dir_all(&auto).unwrap();
        std::fs::write(auto.join("open-whisper.desktop"), crate::autostart::entry(&bin.join(OLD_NAME))).unwrap();
        move_autostart(&auto).unwrap();
        assert!(!auto.join("open-whisper.desktop").exists());
        let text = std::fs::read_to_string(auto.join(crate::autostart::FILE)).unwrap();
        assert_eq!(crate::autostart::exec_program(&text), Some(bin.join(NAME).to_string_lossy().into_owned()));
        // Someone else's entry with that file name: left alone.
        std::fs::write(auto.join("open-whisper.desktop"), crate::autostart::entry(Path::new("/usr/bin/other"))).unwrap();
        move_autostart(&auto).unwrap();
        assert!(auto.join("open-whisper.desktop").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
