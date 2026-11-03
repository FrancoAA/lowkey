//! Call detection (Linux): when a call app uses the microphone, offer to record the meeting.
//! (Idea from Hex, anomalyco/hex, MIT; its rules for timing, adapted to PipeWire.)
//!
//! Who uses the microphone: PipeWire/PulseAudio's source outputs (`pactl -f json list
//! source-outputs`: application name, binary, pid). A browser's call is named by its window
//! title on Hyprland ("Meet - …" is Google Meet). Timing:
//! - a known call app, or a browser on a known call page: 2 s on the microphone;
//! - a browser on another page: 20 s (voice notes and dictation fields hold it shortly);
//! - offered once per call; the same call again only after 60 s off the microphone;
//! - "ended" after 3 s off the microphone;
//! - a gap of over 10 s between looks (sleep) starts the timing again.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Call {
    /// Stable key for the policy ("zoom", "google-meet", "browser:firefox").
    pub key: String,
    /// For people ("Zoom", "Google Meet", "Firefox").
    pub label: String,
    /// A known call app or call page (2 s), else a browser page (20 s).
    pub known: bool,
}

/// One source output of `pactl -f json list source-outputs`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MicUser {
    pub app: String,
    pub binary: String,
    pub pid: Option<u32>,
}

pub fn parse_users(json: &str) -> Vec<MicUser> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    items
        .iter()
        .map(|o| {
            let p = &o["properties"];
            let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
            MicUser { app: s("application.name"), binary: s("application.process.binary"), pid: s("application.process.id").parse().ok() }
        })
        .collect()
}

/// Native call apps: (substring of the binary or app name, key, label).
const APPS: &[(&str, &str, &str)] = &[
    ("zoom", "zoom", "Zoom"),
    ("teams", "teams", "Microsoft Teams"),
    ("slack", "slack", "Slack"),
    ("discord", "discord", "Discord"),
    ("vesktop", "discord", "Discord"),
    ("webcord", "discord", "Discord"),
    ("skype", "skype", "Skype"),
    ("signal", "signal", "Signal"),
    ("telegram", "telegram", "Telegram"),
    ("whatsapp", "whatsapp", "WhatsApp"),
    ("element", "element", "Element"),
    ("mumble", "mumble", "Mumble"),
];

const BROWSERS: &[(&str, &str)] = &[
    ("firefox", "Firefox"),
    ("librewolf", "LibreWolf"),
    ("zen", "Zen"),
    ("chromium", "Chromium"),
    ("chrome", "Chrome"),
    ("brave", "Brave"),
    ("vivaldi", "Vivaldi"),
    ("msedge", "Edge"),
    ("opera", "Opera"),
];

/// Where a call page's mark is in the window title.
#[derive(Clone, Copy)]
enum At {
    Start,
    Anywhere,
}

/// Call pages, found in a browser window's title (lower case): (where, text, key, label).
/// Strict where a word is common: "Meet - …" only at the start (a calendar title "… meet - …"
/// is not a call).
const PAGES: &[(At, &str, &str, &str)] = &[
    (At::Start, "meet - ", "google-meet", "Google Meet"),
    (At::Start, "meet – ", "google-meet", "Google Meet"),
    (At::Start, "zoom meeting", "zoom", "Zoom"),
    (At::Anywhere, "| microsoft teams", "teams", "Microsoft Teams"),
    (At::Anywhere, "jitsi meet", "jitsi", "Jitsi Meet"),
    (At::Anywhere, "| whereby", "whereby", "Whereby"),
    (At::Anywhere, "| discord", "discord", "Discord"),
    (At::Anywhere, "huddle", "slack", "Slack"),
];

fn page_of(title: &str) -> Option<(&'static str, &'static str)> {
    let t = title.to_lowercase();
    PAGES
        .iter()
        .find(|(at, p, _, _)| match at {
            At::Start => t.starts_with(p),
            At::Anywhere => t.contains(p),
        })
        .map(|(_, _, k, l)| (*k, *l))
}

/// What call (if any) a microphone user is. `title`: the title of the pid's most recently
/// used window (a browser's other windows say nothing about which one holds the microphone).
pub fn classify(u: &MicUser, title: &dyn Fn(u32) -> Option<String>) -> Option<Call> {
    let hay = format!("{} {}", u.binary, u.app).to_lowercase();
    // Our own streams (dictation, meetings) never count.
    if hay.contains("open-whisper") || hay.contains("open_whisper") {
        return None;
    }
    if let Some((_, key, label)) = BROWSERS.iter().map(|(n, l)| (n, n, l)).find(|(n, _, _)| hay.contains(*n)) {
        let page = u.pid.and_then(title).as_deref().and_then(page_of).map(|(k, l)| (k.to_string(), l.to_string()));
        return Some(match page {
            Some((k, l)) => Call { key: k, label: l, known: true },
            None => Call { key: format!("browser:{key}"), label: label.to_string(), known: false },
        });
    }
    APPS.iter().find(|(n, _, _)| hay.contains(n)).map(|(_, k, l)| Call { key: k.to_string(), label: l.to_string(), known: true })
}

/// The label of a saved policy's key (the call may not be on the microphone now).
pub fn label_for(key: &str) -> String {
    if let Some(b) = key.strip_prefix("browser:") {
        return BROWSERS.iter().find(|(n, _)| *n == b).map(|(_, l)| l.to_string()).unwrap_or_else(|| b.to_string());
    }
    APPS.iter().map(|(_, k, l)| (k, l)).chain(PAGES.iter().map(|(_, _, k, l)| (k, l))).find(|(k, _)| **k == key).map(|(_, l)| l.to_string()).unwrap_or_else(|| key.to_string())
}

pub const POLICIES: &[&str] = &["ask", "always", "never"];

/// What the app should do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A call is on the microphone long enough: offer (or record, or nothing: its policy).
    Offer(Call),
    /// An offered call left the microphone.
    Ended(Call),
}

struct Run {
    call: Call,
    since: Instant,
    last_seen: Instant,
    offered: bool,
}

#[derive(Default)]
pub struct Detector {
    runs: HashMap<String, Run>,
    /// Offered calls: not again until off the microphone for SNOOZE (from their last sighting).
    snoozed: HashMap<String, Instant>,
    last_look: Option<Instant>,
}

pub const KNOWN_AFTER: Duration = Duration::from_secs(2);
pub const UNKNOWN_AFTER: Duration = Duration::from_secs(20);
pub const ENDED_AFTER: Duration = Duration::from_secs(3);
pub const SNOOZE: Duration = Duration::from_secs(60);
const GAP: Duration = Duration::from_secs(10);

impl Detector {
    /// One look: the calls on the microphone now.
    pub fn step(&mut self, now: Instant, calls: Vec<Call>) -> Vec<Action> {
        if self.last_look.is_some_and(|t| now.duration_since(t) > GAP) {
            // A sleep or a stalled loop: the gap is not time on the microphone.
            for r in self.runs.values_mut() {
                r.since = now;
            }
        }
        self.last_look = Some(now);
        let mut out = Vec::new();
        for c in calls {
            if let Some(t) = self.snoozed.get_mut(&c.key) {
                *t = now; // still around: the snooze counts from the last sighting
            }
            let run = self.runs.entry(c.key.clone()).or_insert(Run { call: c.clone(), since: now, last_seen: now, offered: false });
            run.last_seen = now;
            // A browser page that turned into a known call page: the known timing from now on.
            if c.known && !run.call.known {
                run.call = c.clone();
            }
            let after = if run.call.known { KNOWN_AFTER } else { UNKNOWN_AFTER };
            if !run.offered && now.duration_since(run.since) >= after && !self.snoozed.contains_key(&c.key) {
                run.offered = true;
                self.snoozed.insert(c.key.clone(), now);
                out.push(Action::Offer(run.call.clone()));
            }
        }
        let gone: Vec<String> = self.runs.iter().filter(|(_, r)| now.duration_since(r.last_seen) >= ENDED_AFTER).map(|(k, _)| k.clone()).collect();
        for k in gone {
            if let Some(r) = self.runs.remove(&k) {
                if r.offered {
                    out.push(Action::Ended(r.call));
                }
            }
        }
        self.snoozed.retain(|_, t| now.duration_since(*t) < SNOOZE);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(app: &str, binary: &str, pid: u32) -> MicUser {
        MicUser { app: app.into(), binary: binary.into(), pid: Some(pid) }
    }

    #[test]
    fn mic_users_are_named() {
        let none = |_: u32| None::<String>;
        let meet = |_: u32| Some("Meet - Weekly sync — Mozilla Firefox".to_string());
        let calendar = |_: u32| Some("Calendar – meet - Thursday — Mozilla Firefox".to_string());
        assert_eq!(classify(&user("ZOOM VoiceEngine", "zoom", 1), &none).unwrap().label, "Zoom");
        assert_eq!(classify(&user("teams-for-linux", "teams-for-linux", 1), &none).unwrap().key, "teams");
        assert_eq!(classify(&user("Firefox", "firefox", 7), &meet), Some(Call { key: "google-meet".into(), label: "Google Meet".into(), known: true }));
        assert_eq!(classify(&user("Firefox", "firefox", 7), &none), Some(Call { key: "browser:firefox".into(), label: "Firefox".into(), known: false }));
        assert_eq!(classify(&user("Firefox", "firefox", 7), &calendar).unwrap().known, false, "'meet - ' inside a title is not a call");
        assert_eq!(page_of("Daily | Microsoft Teams"), Some(("teams", "Microsoft Teams")));
        assert_eq!(classify(&user("open-whisper", "open-whisper", 9), &none), None, "our own streams never count");
        assert_eq!(classify(&user("ALSA plug-in [obs]", "obs", 3), &none), None, "not a call app");
        let json = r#"[{"properties":{"application.name":"ZOOM VoiceEngine","application.process.binary":"zoom","application.process.id":"4242"}},{"properties":{}}]"#;
        assert_eq!(parse_users(json)[0], user("ZOOM VoiceEngine", "zoom", 4242));
        assert!(parse_users("nope").is_empty());
    }

    fn zoom() -> Call {
        Call { key: "zoom".into(), label: "Zoom".into(), known: true }
    }

    fn page() -> Call {
        Call { key: "browser:firefox".into(), label: "Firefox".into(), known: false }
    }

    #[test]
    fn a_known_call_is_offered_after_2_s_once_and_ends_after_3_s_off() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        let mut d = Detector::default();
        assert!(d.step(at(0.0), vec![zoom()]).is_empty());
        assert!(d.step(at(1.0), vec![zoom()]).is_empty());
        assert_eq!(d.step(at(2.0), vec![zoom()]), vec![Action::Offer(zoom())]);
        assert!(d.step(at(3.0), vec![zoom()]).is_empty(), "offered once");
        assert!(d.step(at(4.0), vec![]).is_empty());
        assert!(d.step(at(5.0), vec![]).is_empty());
        assert_eq!(d.step(at(6.0), vec![]), vec![Action::Ended(zoom())], "3 s off the microphone");
        // Back within the snooze: not offered again.
        for s in 7..20 {
            assert!(d.step(at(s as f64), vec![zoom()]).is_empty());
        }
        // Off for over 60 s: a new call is offered again.
        for s in (20..90).step_by(5) {
            d.step(at(s as f64), vec![]);
        }
        assert!(d.step(at(90.0), vec![zoom()]).is_empty());
        assert_eq!(d.step(at(92.0), vec![zoom()]), vec![Action::Offer(zoom())]);
    }

    #[test]
    fn a_browser_page_needs_20_s_and_short_holds_or_sleep_never_count() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        let mut d = Detector::default();
        // A 5-s voice note: never offered, never "ended".
        for s in 0..5 {
            assert!(d.step(at(s as f64), vec![page()]).is_empty());
        }
        for s in 5..10 {
            assert!(d.step(at(s as f64), vec![]).is_empty());
        }
        // A long stay on an unknown page: after 20 s.
        let mut offered = None;
        for s in 10..40 {
            if !d.step(at(s as f64), vec![page()]).is_empty() {
                offered = Some(s);
                break;
            }
        }
        assert_eq!(offered, Some(30));
        // A sleep of 30 s between looks restarts the timing.
        let mut d = Detector::default();
        d.step(at(0.0), vec![zoom()]);
        assert!(d.step(at(1.0), vec![zoom()]).is_empty());
        assert!(d.step(at(31.0), vec![zoom()]).is_empty(), "the gap is not time on the microphone");
        assert_eq!(d.step(at(33.0), vec![zoom()]), vec![Action::Offer(zoom())]);
    }
}
