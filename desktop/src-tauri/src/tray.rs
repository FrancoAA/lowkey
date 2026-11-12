//! The tray icon's menu: open the window, start or stop a dictation or a meeting recording,
//! choose the microphone, see the speech and notes models (chosen in Settings → Models), exit.
//!
//! The menu is a pure description (`entries`) of the app's state (`View`); the app builds it
//! again only when that description changes (a recording starts, a model is chosen, a
//! microphone is plugged in).

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Runtime};

/// What the menu shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    /// The dictation's state: idle | starting | listening | transcribing | error.
    pub dictation: String,
    pub meeting: Meeting,
    /// The model that transcribes now: "Parakeet v3 · this computer", "gpt-4o-transcribe · OpenAI".
    pub speech: String,
    /// The model that writes the notes: "Qwen3.5 4B · this computer", "llama3 · your server".
    pub notes: String,
    /// None: the microphone cannot be chosen here (no PipeWire/PulseAudio).
    pub mics: Option<Vec<MicItem>>,
    /// The saved choice ("" = the system default).
    pub mic_choice: String,
    /// The tray is drawn by Omarchy's shell, which hides a divider in the first two rows.
    pub omarchy_shell: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Meeting {
    #[default]
    Idle,
    Starting,
    Recording,
    Saving,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MicItem {
    pub id: String,
    pub name: String,
    pub bluetooth: bool,
    pub default: bool,
}

/// One menu entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Item { id: String, label: String, enabled: bool },
    Check { id: String, label: String, checked: bool },
    Sub { label: String, entries: Vec<Entry> },
    Separator,
}

/// What a click asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Dictate,
    Meeting,
    /// Open the window on Settings → Models.
    Models,
    /// "" = the system default.
    Mic(String),
    Open,
    Exit,
}

pub fn action(id: &str) -> Option<Action> {
    Some(match id {
        "dictate" => Action::Dictate,
        "meeting" => Action::Meeting,
        "open" => Action::Open,
        "models" => Action::Models,
        "exit" => Action::Exit,
        _ => {
            if let Some(m) = id.strip_prefix("mic:") {
                Action::Mic(m.into())
            } else {
                return None;
            }
        }
    })
}

fn item(id: &str, label: &str, enabled: bool) -> Entry {
    Entry::Item { id: id.into(), label: label.into(), enabled }
}

fn check(id: String, label: String, checked: bool) -> Entry {
    Entry::Check { id, label, checked }
}

pub fn entries(v: &View) -> Vec<Entry> {
    let dictate = match v.dictation.as_str() {
        "listening" => item("dictate", "Stop dictation", true),
        "starting" => item("dictate", "Cancel dictation", true),
        "transcribing" => item("dictate", "Transcribing…", false),
        _ => item("dictate", "Start dictation", true),
    };
    let meeting = match v.meeting {
        Meeting::Idle => item("meeting", "Start meeting recording", true),
        Meeting::Starting => item("meeting", "Starting the recording…", false),
        Meeting::Recording => item("meeting", "Stop meeting recording", true),
        Meeting::Saving => item("meeting", "Saving the recording…", false),
    };

    let mics = match &v.mics {
        None => vec![item("mic:", "System default (choosing one needs PipeWire or PulseAudio)", false)],
        Some(list) => {
            let def = list.iter().find(|m| m.default).map(|m| format!("System default — {}", m.name)).unwrap_or_else(|| "System default".into());
            let mut out = vec![check("mic:".into(), def, v.mic_choice.is_empty())];
            for m in list {
                let label = if m.bluetooth { format!("{} · Bluetooth", m.name) } else { m.name.clone() };
                out.push(check(format!("mic:{}", m.id), label, v.mic_choice == m.id));
            }
            // A saved microphone that is not connected: shown, so the menu says what is saved.
            if !v.mic_choice.is_empty() && !list.iter().any(|m| m.id == v.mic_choice) {
                out.push(check(format!("mic:{}", v.mic_choice), format!("{} · not connected", v.mic_choice), true));
            }
            out
        }
    };

    let mut out = vec![item("open", "Open", true), Entry::Separator];
    if v.omarchy_shell {
        // Omarchy's shell hides a divider in row 1 or 2 (it takes it for the line under a
        // menu title): a second one, in row 3, is the one it shows.
        out.push(Entry::Separator);
    }
    out.extend([
        dictate,
        meeting,
        Entry::Separator,
        Entry::Sub { label: "Microphone".into(), entries: mics },
        Entry::Separator,
        // The models: one place to choose them (Settings → Models), seen here.
        item("speech-model", &format!("Speech: {}", v.speech), false),
        item("notes-model", &format!("Notes: {}", v.notes), false),
        item("models", "Change in Settings…", true),
        Entry::Separator,
        item("exit", "Exit", true),
    ]);
    out
}

/// Omarchy's shell (Quickshell) draws the tray menus on this computer.
pub fn omarchy_shell() -> bool {
    cfg!(target_os = "linux") && std::path::Path::new("/usr/share/omarchy/shell").is_dir()
}

/// The Tauri menu for `entries`.
pub fn build<R: Runtime>(app: &AppHandle<R>, entries: &[Entry]) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for e in entries {
        append(app, e, &|i| menu.append(i))?;
    }
    Ok(menu)
}

fn append<R: Runtime>(app: &AppHandle<R>, e: &Entry, add: &dyn Fn(&dyn IsMenuItem<R>) -> tauri::Result<()>) -> tauri::Result<()> {
    match e {
        Entry::Item { id, label, enabled } => add(&MenuItem::with_id(app, id, label, *enabled, None::<&str>)?),
        Entry::Check { id, label, checked } => add(&CheckMenuItem::with_id(app, id, label, true, *checked, None::<&str>)?),
        Entry::Separator => add(&PredefinedMenuItem::separator(app)?),
        Entry::Sub { label, entries } => {
            let sub = Submenu::new(app, label, true)?;
            for e in entries {
                append(app, e, &|i| sub.append(i))?;
            }
            add(&sub)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View {
            dictation: "idle".into(),
            meeting: Meeting::Idle,
            speech: "Parakeet v3 · this computer".into(),
            notes: "Qwen3.5 4B · this computer".into(),
            mics: Some(vec![
                MicItem { id: "alsa.usb".into(), name: "USB mic".into(), bluetooth: false, default: true },
                MicItem { id: "bluez.g733".into(), name: "G733".into(), bluetooth: true, default: false },
            ]),
            mic_choice: String::new(),
            omarchy_shell: false,
        }
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                Entry::Item { label, enabled, .. } => format!("{label}{}", if *enabled { "" } else { " (off)" }),
                Entry::Check { label, checked, .. } => format!("[{}] {label}", if *checked { "x" } else { " " }),
                Entry::Sub { label, .. } => format!("{label} >"),
                Entry::Separator => "--".into(),
            })
            .collect()
    }

    fn sub(entries: &[Entry], name: &str) -> Vec<String> {
        entries
            .iter()
            .find_map(|e| match e {
                Entry::Sub { label, entries } if label == name => Some(labels(entries)),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn the_menu_shows_the_state_and_the_choices() {
        let e = entries(&view());
        assert_eq!(
            labels(&e),
            vec![
                "Open",
                "--",
                "Start dictation",
                "Start meeting recording",
                "--",
                "Microphone >",
                "--",
                "Speech: Parakeet v3 · this computer (off)",
                "Notes: Qwen3.5 4B · this computer (off)",
                "Change in Settings…",
                "--",
                "Exit"
            ]
        );
        assert_eq!(sub(&e, "Microphone"), vec!["[x] System default — USB mic", "[ ] USB mic", "[ ] G733 · Bluetooth"]);
    }

    #[test]
    fn recording_and_choices_change_the_labels() {
        let mut v = view();
        v.dictation = "listening".into();
        v.meeting = Meeting::Recording;
        v.mic_choice = "bluez.old".into();
        let e = entries(&v);
        assert_eq!(labels(&e)[2..4], ["Stop dictation", "Stop meeting recording"]);
        assert_eq!(sub(&e, "Microphone").last().unwrap(), "[x] bluez.old · not connected");
        v.dictation = "transcribing".into();
        v.meeting = Meeting::Saving;
        assert_eq!(labels(&entries(&v))[2..4], ["Transcribing… (off)", "Saving the recording… (off)"]);
        v.mics = None;
        assert_eq!(sub(&entries(&v), "Microphone"), vec!["System default (choosing one needs PipeWire or PulseAudio) (off)"]);
        // A provider transcribes, your server writes the notes: the menu says so.
        v.speech = "gpt-4o-transcribe · OpenAI".into();
        v.notes = "llama3 · your server".into();
        assert_eq!(labels(&entries(&v))[7..9], ["Speech: gpt-4o-transcribe · OpenAI (off)", "Notes: llama3 · your server (off)"]);
    }

    #[test]
    fn omarchy_shell_gets_a_divider_it_shows_under_open() {
        let mut v = view();
        v.omarchy_shell = true;
        assert_eq!(labels(&entries(&v))[..3], ["Open", "--", "--"]);
    }

    #[test]
    fn clicks_are_read_back() {
        assert_eq!(action("dictate"), Some(Action::Dictate));
        assert_eq!(action("exit"), Some(Action::Exit));
        assert_eq!(action("models"), Some(Action::Models));
        assert_eq!(action("model:small"), None, "the models are chosen in Settings");
        assert_eq!(action("speech-model"), None, "the model lines do nothing");
        assert_eq!(action("mic:alsa_input.usb-x:y"), Some(Action::Mic("alsa_input.usb-x:y".into())));
        assert_eq!(action("other"), None);
    }
}
