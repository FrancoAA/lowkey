//! Settings. Nothing is required: every value has a default, the file is optional.
//!
//! Precedence: command line > ~/.config/lowkey/config.toml > automatic choice.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Linux: Super+Ctrl+X (Omarchy's dictation key). macOS/Windows: Ctrl+Alt+Space
/// (Win+Ctrl+X is next to the system Win+X menu; Cmd shortcuts are taken by apps).
#[cfg(target_os = "linux")]
pub const DEFAULT_HOTKEY: &str = "SUPER + CTRL + X";
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_HOTKEY: &str = "CTRL + ALT + SPACE";

/// The meeting key: Super+Ctrl+M on Linux (free in Omarchy), Ctrl+Alt+M elsewhere.
#[cfg(target_os = "linux")]
pub const DEFAULT_MEETING_HOTKEY: &str = "SUPER + CTRL + M";
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_MEETING_HOTKEY: &str = "CTRL + ALT + M";

/// Types the last dictation again: Super+Alt+V on Linux (Super+Ctrl+V is Omarchy's clipboard
/// manager), Ctrl+Alt+V elsewhere.
#[cfg(target_os = "linux")]
pub const DEFAULT_PASTE_HOTKEY: &str = "SUPER + ALT + V";
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_PASTE_HOTKEY: &str = "CTRL + ALT + V";

/// "SUPER + CTRL + X" → "super+control+KeyX" (the global-shortcut plugin's syntax).
pub fn to_plugin_shortcut(key: &str) -> Option<String> {
    let parts: Vec<String> = key.split('+').map(|p| p.trim().to_uppercase()).filter(|p| !p.is_empty()).collect();
    let (last, mods) = parts.split_last()?;
    let mut out: Vec<String> = Vec::new();
    for m in mods {
        out.push(
            match m.as_str() {
                "SUPER" | "WIN" | "LOGO" | "MOD4" | "CMD" | "COMMAND" => "super",
                "CTRL" | "CONTROL" => "control",
                "ALT" | "OPTION" => "alt",
                "SHIFT" => "shift",
                _ => return None,
            }
            .to_string(),
        );
    }
    let code = match last.as_str() {
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) => format!("Key{k}"),
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) => format!("Digit{k}"),
        "SPACE" => "Space".into(),
        k if k.starts_with('F') && k[1..].parse::<u8>().is_ok() => k.to_string(),
        _ => return None,
    };
    out.push(code);
    Some(out.join("+"))
}

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
    /// Show the words in the dictation overlay while you talk.
    pub live_preview: bool,
    /// The preview's engine: "auto" (Parakeet when it is on disk, else Whistle), "whistle"
    /// (17 MB, light) or "parakeet" (more accurate, about 1.1 GB of memory).
    pub preview_engine: String,
    /// Global key that starts or stops a meeting recording. Empty = no key.
    pub meeting_hotkey: String,
    /// Global key that types the last dictation again. Empty = no key.
    pub paste_hotkey: String,
    /// How long the dictation history keeps entries: off | day | week | month | forever.
    pub history_keep: String,
    /// The window's colours: system (follow the desktop) | light | dark.
    pub theme: String,
    /// The floating strip (dictation, meeting recording): collapsed to a small pill.
    pub overlay_collapsed: bool,
    /// Where the user left the strip: [x, y] as fractions (0..1) of the room the monitor leaves
    /// around it. Missing: bottom centre.
    pub overlay_at: Option<[f32; 2]>,
    /// The microphone (a PipeWire source name on Linux, a device name elsewhere); "" = system default.
    pub microphone: String,
    /// With the system default: a Bluetooth headset's microphone gives way to another one.
    pub avoid_bluetooth_mic: bool,
    /// Offer to record when a call app uses the microphone (Linux).
    pub call_detection: bool,
    /// Per call ("zoom", "google-meet", "browser:firefox"): ask | always | never. Missing = ask.
    /// (A table: the toml writer puts it after the plain values.)
    pub call_policies: std::collections::BTreeMap<String, String>,
    /// AI notes: "builtin" (Qwen3.5 4B, run by the app) or "server" (an OpenAI-compatible API).
    pub ai_engine: String,
    /// The built-in model: an id from llm::MODELS ("qwen3.5-4b", "qwen3.5-2b", "gemma-4-e4b").
    pub ai_builtin_model: String,
    /// The own server: base URL (".../v1"), model name, key ("" = none).
    pub ai_url: String,
    pub ai_model: String,
    pub ai_key: String,
    /// Write the AI notes when a meeting stops (the first time, the built-in model downloads).
    pub ai_auto: bool,
    /// AI agents (the MCP server, `lowkey --mcp`) may start and stop meeting recordings. Off:
    /// they only read meetings and dictations, and transcribe files.
    pub mcp_recording: bool,
    /// Names and terms the speech model gets wrong (Settings → General → Vocabulary). Last:
    /// a list of tables goes after the plain values in config.toml.
    pub vocabulary: Vec<crate::vocabulary::Term>,
}

impl Default for Config {
    fn default() -> Self {
        Self { model: String::new(), language: "auto".into(), idle_unload_minutes: 15, hotkey: DEFAULT_HOTKEY.into(), live_preview: true, preview_engine: "auto".into(), meeting_hotkey: DEFAULT_MEETING_HOTKEY.into(),
            paste_hotkey: DEFAULT_PASTE_HOTKEY.into(),
            history_keep: crate::history::DEFAULT_KEEP.into(),
            theme: "system".into(),
            overlay_collapsed: false,
            overlay_at: None,
            microphone: String::new(),
            avoid_bluetooth_mic: true,
            call_detection: true,
            call_policies: Default::default(),
            ai_engine: "builtin".into(),
            ai_builtin_model: crate::llm::DEFAULT_MODEL.into(),
            ai_url: "http://localhost:11434/v1".into(),
            ai_model: String::new(),
            ai_key: String::new(),
            ai_auto: true,
            mcp_recording: false,
            vocabulary: Vec::new(),
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(crate::rename::folder()).join("config.toml")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join(crate::rename::folder())
}

impl Config {
    /// Load the file; a missing or broken file gives the defaults (with a warning for broken).
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let mut cfg: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                eprintln!("  Ignoring {}: {err}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        // A key typed into an older file (0644): readable by you only.
        #[cfg(unix)]
        if !cfg.ai_key.is_empty() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        // A model that is gone (e.g. the faster-whisper "fw:" models): back to the default.
        if !cfg.model.is_empty() && crate::models::spec(&cfg.model).is_none() {
            eprintln!("  The model {:?} in {} is not available any more; using the default.", cfg.model, path.display());
            cfg.model.clear();
        }
        cfg
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let body = format!(
            "# Lowkey settings. Every line is optional; delete the file to go back to defaults.\n{}",
            toml::to_string_pretty(self)?
        );
        // It can hold an API key: readable by you only, from its first byte (a new file, then a rename).
        let tmp = path.with_extension("toml.tmp");
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?; // an old tmp file kept its mode
            }
            f.write_all(body.as_bytes())?;
        }
        std::fs::rename(&tmp, &path)?;
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
    pub meeting: Option<String>,
    /// The title of a meeting that `--meeting start` starts.
    pub title: Option<String>,
    /// The command comes from an AI agent (the MCP server): the app says so in a notification.
    pub agent: bool,
    /// Serve the Model Context Protocol on stdin/stdout, for AI agents.
    pub mcp: bool,
    /// Type the last dictation again (the paste-last key).
    pub paste_last: bool,
    pub background: bool,
    pub transcribe: Option<PathBuf>,
    pub language: Option<String>,
    pub files: Vec<PathBuf>,
    pub help: bool,
    pub version: bool,
    pub print_snippet: Option<String>,
    pub remove_hotkey: bool,
}

pub const HELP: &str = "Lowkey — local speech to text

Usage: lowkey [options] [audio or video files…]

  --model <id>            model to use (see Options in the app; default: picked for this machine)
  --language <code>       force a language instead of auto-detect
  --hotkey \"SUPER + CTRL + X\"   global dictation key (Hyprland)
  --no-hotkey             do not add the global keys (dictation and meeting)
  --dictate <action>      press | release | toggle | cancel | quit (used by the key bind)
  --meeting <action>      toggle | start | stop a meeting recording (used by the meeting key)
  --title <text>          with --meeting start: the meeting's title
  --mcp                   serve AI agents (Claude Code, Codex, Gemini CLI…) over MCP on stdin/stdout
  --paste-last            type the last dictation again (used by the paste-last key)
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
                "--meeting" => cli.meeting = it.next(),
                "--title" => cli.title = it.next(),
                "--agent" => cli.agent = true,
                "--mcp" => cli.mcp = true,
                "--paste-last" => cli.paste_last = true,
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
        std::iter::once("lowkey").chain(s.split_whitespace()).map(String::from).collect()
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
    fn a_removed_model_falls_back_to_the_default() {
        let f = std::env::temp_dir().join(format!("ow-cfg-fw-{}.toml", std::process::id()));
        std::fs::write(&f, "model = \"fw:large-v3-turbo\"\nidle_unload_minutes = 7\n").unwrap();
        let c = Config::load_from(&f);
        assert_eq!(c.model, "");
        assert_eq!(c.idle_unload_minutes, 7);
        let _ = std::fs::remove_file(f);
    }

    #[test]
    fn save_writes_the_file_the_app_reads() {
        let dir = std::env::temp_dir().join(format!("ow-cfgsave-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let c = Config { model: "small".into(), idle_unload_minutes: 60, ..Config::default() };
        c.save().unwrap();
        let path = config_path();
        assert!(path.starts_with(&dir));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Lowkey settings"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(Config::load_from(&path), c);
        // The vocabulary (a list of tables) round-trips, after the plain values.
        let mut policies = std::collections::BTreeMap::new();
        policies.insert("zoom".to_string(), "always".to_string());
        policies.insert("browser:firefox".to_string(), "never".to_string());
        let v = Config {
            vocabulary: vec![crate::vocabulary::Term { term: "Lisandro".into(), sounds_like: vec!["Lisando".into()] }],
            call_policies: policies,
            ..c.clone()
        };
        v.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[vocabulary]]") && text.contains("term = \"Lisandro\""), "{text}");
        assert!(text.contains("[call_policies]") && text.contains("zoom = \"always\""), "{text}");
        assert_eq!(Config::load_from(&path), v);
    }

    #[test]
    fn plugin_shortcut_syntax() {
        assert_eq!(to_plugin_shortcut("SUPER + CTRL + X").as_deref(), Some("super+control+KeyX"));
        assert_eq!(to_plugin_shortcut("CTRL + ALT + SPACE").as_deref(), Some("control+alt+Space"));
        assert_eq!(to_plugin_shortcut("ALT + F9").as_deref(), Some("alt+F9"));
        assert_eq!(to_plugin_shortcut("HYPER + X"), None);
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
        assert_eq!(Cli::parse(argv("--meeting toggle")).meeting.as_deref(), Some("toggle"));
        assert!(Cli::parse(argv("--paste-last")).paste_last);
        let c = Cli::parse(vec!["lowkey".into(), "--meeting".into(), "start".into(), "--title".into(), "Weekly sync".into(), "--agent".into()]);
        assert_eq!((c.meeting.as_deref(), c.title.as_deref(), c.agent), (Some("start"), Some("Weekly sync"), true));
        assert!(Cli::parse(argv("--mcp")).mcp);
    }
}
