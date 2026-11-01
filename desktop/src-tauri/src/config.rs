//! Settings. Nothing is required: every value has a default, the file is optional.
//!
//! Precedence: command line > ~/.config/open-whisper/config.toml > automatic choice.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_HOTKEY: &str = "SUPER + CTRL + X";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Model id from the catalog (see models.rs). Empty = pick automatically for this machine.
    pub model: String,
    /// "auto" or a language code.
    pub language: String,
    /// Unload the model after this many idle minutes (0 = keep it loaded).
    pub idle_unload_minutes: u64,
    /// Global dictation key (Hyprland syntax, e.g. "SUPER + CTRL + X"). Empty = no key.
    pub hotkey: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { model: String::new(), language: "auto".into(), idle_unload_minutes: 15, hotkey: DEFAULT_HOTKEY.into() }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("open-whisper").join("config.toml")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("open-whisper")
}

impl Config {
    /// Load the file; a missing or broken file gives the defaults (with a warning for broken).
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                eprintln!("  Ignoring {}: {err}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let body = format!(
            "# open-whisper settings. Every line is optional; delete the file to go back to defaults.\n{}",
            toml::to_string_pretty(self)?
        );
        std::fs::write(&path, body)?;
        Ok(())
    }
}

/// Command line. Unknown arguments that are existing files are opened for transcription.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Cli {
    pub model: Option<String>,
    pub hotkey: Option<String>,
    pub no_hotkey: bool,
    pub dictate: Option<String>,
    pub background: bool,
    pub transcribe: Option<PathBuf>,
    pub language: Option<String>,
    pub files: Vec<PathBuf>,
    pub help: bool,
    pub version: bool,
    pub print_snippet: Option<String>,
    pub remove_hotkey: bool,
}

pub const HELP: &str = "open-whisper — local speech to text

Usage: open-whisper [options] [audio or video files…]

  --model <id>            model to use (see Options in the app; default: picked for this machine)
  --language <code>       force a language instead of auto-detect
  --hotkey \"SUPER + CTRL + X\"   global dictation key (Hyprland)
  --no-hotkey             do not add the global dictation key
  --dictate <action>      press | release | toggle | cancel | quit (used by the key bind)
  --background            start without opening the window
  --transcribe <file>     transcribe a file without a window and print JSON
  --print-snippet [lua|conf]  print the Hyprland config lines for the key
  --remove-hotkey         remove a key that the app added to the running Hyprland
  -h, --help / -V, --version
";

impl Cli {
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Self {
        let mut cli = Cli::default();
        let mut it = args.into_iter().skip(1).peekable();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--model" => cli.model = it.next(),
                "--language" => cli.language = it.next(),
                "--hotkey" => cli.hotkey = it.next(),
                "--no-hotkey" => cli.no_hotkey = true,
                "--dictate" => cli.dictate = it.next(),
                "--background" => cli.background = true,
                "--transcribe" => cli.transcribe = it.next().map(PathBuf::from),
                "--remove-hotkey" => cli.remove_hotkey = true,
                "--print-snippet" => {
                    let kind = match it.peek().map(String::as_str) {
                        Some("lua") | Some("conf") => it.next().unwrap(),
                        _ => "lua".into(),
                    };
                    cli.print_snippet = Some(kind);
                }
                "-h" | "--help" => cli.help = true,
                "-V" | "--version" => cli.version = true,
                other if other.starts_with("--model=") => cli.model = Some(other[8..].into()),
                other if !other.starts_with('-') => cli.files.push(PathBuf::from(other)),
                _ => {}
            }
        }
        cli
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        std::iter::once("open-whisper").chain(s.split_whitespace()).map(String::from).collect()
    }

    #[test]
    fn defaults_need_no_file() {
        let c = Config::load_from(std::path::Path::new("/nonexistent/config.toml"));
        assert_eq!(c, Config::default());
        assert_eq!(c.language, "auto");
        assert!(c.model.is_empty());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let dir = std::env::temp_dir().join(format!("ow-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, "model = \"small\"\nidle_unload_minutes = 0\n").unwrap();
        let c = Config::load_from(&p);
        assert_eq!(c.model, "small");
        assert_eq!(c.idle_unload_minutes, 0);
        assert_eq!(c.hotkey, DEFAULT_HOTKEY);
        std::fs::write(&p, "model = [broken").unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
    }

    #[test]
    fn cli_flags() {
        let c = Cli::parse(argv("--model large-v3-turbo-q5_0 --dictate press --background a.mp3"));
        assert_eq!(c.model.as_deref(), Some("large-v3-turbo-q5_0"));
        assert_eq!(c.dictate.as_deref(), Some("press"));
        assert!(c.background);
        assert_eq!(c.files, vec![PathBuf::from("a.mp3")]);
        assert_eq!(Cli::parse(argv("--model=small")).model.as_deref(), Some("small"));
        assert_eq!(Cli::parse(argv("--print-snippet conf")).print_snippet.as_deref(), Some("conf"));
        assert_eq!(Cli::parse(argv("--print-snippet")).print_snippet.as_deref(), Some("lua"));
    }
}
