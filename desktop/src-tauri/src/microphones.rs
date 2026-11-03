//! Which microphone to record: the saved one, else the system default, and not a Bluetooth
//! headset when another microphone exists (its microphone switches the headset to call mode:
//! mono, low-quality sound for everything else). (Idea from Hex, anomalyco/hex, MIT.)
//!
//! Linux: the sources PipeWire/PulseAudio know (`pactl -f json list sources`); the recording
//! then opens that exact source (system_audio::record_source). macOS and Windows: the input
//! devices cpal lists, by name; no Bluetooth detection there yet.

use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Mic {
    /// Linux: the source name; elsewhere: the device name.
    pub id: String,
    /// For people ("G733 Gaming Headset Mono").
    pub name: String,
    pub bluetooth: bool,
    /// A real device (not a virtual source such as a noise filter).
    pub hardware: bool,
    /// The system's default input.
    pub default: bool,
}

/// The microphone to open, and why (shown in Settings).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Choice {
    /// None: the backend's own default (no list was available).
    pub id: Option<String>,
    pub name: String,
    pub note: Option<String>,
}

/// `saved`: Settings' choice ("" = system default). `avoid_bluetooth`: with the system default,
/// a Bluetooth default gives way to another microphone.
pub fn resolve(saved: &str, avoid_bluetooth: bool, mics: &[Mic]) -> Choice {
    let mut note = None;
    if !saved.is_empty() {
        if let Some(m) = mics.iter().find(|m| m.id == saved) {
            return Choice { id: Some(m.id.clone()), name: m.name.clone(), note: None };
        }
        if !mics.is_empty() {
            note = Some(format!("{saved} is not connected; using the system default."));
        }
    }
    let Some(default) = mics.iter().find(|m| m.default).or(mics.first()) else {
        return Choice { id: None, name: "System default".into(), note };
    };
    if avoid_bluetooth && default.bluetooth {
        // A real device only: a virtual source is never chosen for you.
        if let Some(other) = mics.iter().find(|m| !m.bluetooth && m.hardware) {
            let why = format!("{} is Bluetooth, so open-whisper listens to {}.", default.name, other.name);
            return Choice { id: Some(other.id.clone()), name: other.name.clone(), note: Some(note.map_or(why.clone(), |n| format!("{n} {why}"))) };
        }
    }
    Choice { id: Some(default.id.clone()), name: default.name.clone(), note }
}

/// The microphones now (cached for 2 s: a dictation start asks too).
pub fn list() -> Vec<Mic> {
    static CACHE: Mutex<Option<(Instant, Vec<Mic>)>> = Mutex::new(None);
    let mut c = CACHE.lock();
    if let Some((at, mics)) = c.as_ref() {
        if at.elapsed() < Duration::from_secs(2) {
            return mics.clone();
        }
    }
    let mics = platform::list();
    *c = Some((Instant::now(), mics.clone()));
    mics
}

/// Who uses the microphone now (Linux: pactl's source outputs as JSON).
#[cfg(target_os = "linux")]
pub fn mic_users_json() -> Option<String> {
    platform::pactl(&["-f", "json", "list", "source-outputs"])
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    /// OPENWHISPER_PACTL: another pactl (tests); default "pactl".
    pub fn pactl(args: &[&str]) -> Option<String> {
        let bin = std::env::var("OPENWHISPER_PACTL").unwrap_or_else(|_| "pactl".into());
        let out = std::process::Command::new(bin).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn list() -> Vec<Mic> {
        let default = pactl(&["get-default-source"]).map(|s| s.trim().to_string()).unwrap_or_default();
        let Some(json) = pactl(&["-f", "json", "list", "sources"]) else { return vec![] };
        parse(&json, &default)
    }

    /// Real inputs only: no monitors (the sound of an output), no virtual sources.
    pub fn parse(json: &str, default: &str) -> Vec<Mic> {
        let Ok(serde_json::Value::Array(sources)) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
        sources
            .iter()
            .filter_map(|s| {
                let id = s["name"].as_str()?.to_string();
                let p = &s["properties"];
                let class = p["device.class"].as_str().unwrap_or("");
                if class == "monitor" || id.ends_with(".monitor") {
                    return None;
                }
                let bluetooth = p["device.bus"].as_str() == Some("bluetooth") || p["device.api"].as_str() == Some("bluez5") || id.starts_with("bluez_input.");
                let name = s["description"].as_str().unwrap_or(&id).to_string();
                let hardware = !p["device.api"].is_null();
                Some(Mic { default: id == default, id, name, bluetooth, hardware })
            })
            .collect()
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};

    pub fn list() -> Vec<Mic> {
        let host = cpal::default_host();
        let default = host.default_input_device().and_then(|d| d.name().ok()).unwrap_or_default();
        host.input_devices()
            .map(|it| it.filter_map(|d| d.name().ok()).map(|n| Mic { default: n == default, id: n.clone(), name: n, bluetooth: false, hardware: true }).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mic(id: &str, bt: bool, default: bool) -> Mic {
        Mic { id: id.into(), name: format!("{id} name"), bluetooth: bt, hardware: true, default }
    }

    #[test]
    fn the_saved_microphone_else_the_default_else_not_bluetooth() {
        let usb = mic("usb", false, false);
        let pci = mic("pci", false, false);
        let bt = mic("bt", true, true);
        let all = vec![bt.clone(), usb.clone(), pci.clone()];
        // Saved and connected: always that one, Bluetooth too.
        assert_eq!(resolve("bt", true, &all).id.as_deref(), Some("bt"));
        assert_eq!(resolve("pci", true, &all), Choice { id: Some("pci".into()), name: "pci name".into(), note: None });
        // System default is Bluetooth: the first other microphone, and the reason.
        let c = resolve("", true, &all);
        assert_eq!(c.id.as_deref(), Some("usb"));
        assert_eq!(c.note.as_deref(), Some("bt name is Bluetooth, so open-whisper listens to usb name."));
        // The switch off: the default as it is.
        assert_eq!(resolve("", false, &all).id.as_deref(), Some("bt"));
        // A virtual source is never the stand-in.
        let virt = Mic { hardware: false, ..mic("filter", false, false) };
        assert_eq!(resolve("", true, &[bt.clone(), virt.clone(), pci.clone()]).id.as_deref(), Some("pci"));
        assert_eq!(resolve("", true, &[bt.clone(), virt.clone()]).id.as_deref(), Some("bt"));
        assert_eq!(resolve("filter", true, &[bt.clone(), virt]).id.as_deref(), Some("filter"), "chosen by you: used");
        // Only Bluetooth: it is used.
        assert_eq!(resolve("", true, &[bt.clone()]).id.as_deref(), Some("bt"));
        // Saved but gone: the default rules apply, and the note says so.
        let gone = resolve("shure", true, &all);
        assert_eq!(gone.id.as_deref(), Some("usb"));
        assert!(gone.note.unwrap().starts_with("shure is not connected; using the system default."));
        // No list (no pactl): the backend's default.
        assert_eq!(resolve("", true, &[]).id, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pactl_sources_become_microphones() {
        let json = r#"[
          {"name":"alsa_output.pci.analog-stereo.monitor","description":"Monitor of Speakers","properties":{"device.class":"monitor","device.api":"alsa","device.bus":"pci"}},
          {"name":"alsa_input.pci.analog-stereo","description":"Built-in Mic","properties":{"device.class":"sound","device.api":"alsa","device.bus":"pci"}},
          {"name":"bluez_input.14_14_7D","description":"WH-1000XM4","properties":{"device.api":"bluez5","device.bus":"bluetooth"}},
          {"name":"virtual_source","description":"Noise canceller","properties":{"media.class":"Audio/Source"}}
        ]"#;
        let mics = platform::parse(json, "bluez_input.14_14_7D");
        assert_eq!(mics.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["alsa_input.pci.analog-stereo", "bluez_input.14_14_7D", "virtual_source"], "no monitor");
        assert!(mics[1].bluetooth && mics[1].default && !mics[0].bluetooth);
        assert!(mics[0].hardware && !mics[2].hardware);
        assert_eq!(resolve("", true, &mics).name, "Built-in Mic");
        assert!(platform::parse("not json", "").is_empty());
    }
}
