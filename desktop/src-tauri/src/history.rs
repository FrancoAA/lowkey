//! History: every finished dictation (the global key and the window), searchable, kept for a
//! while (7 days by default), in one private JSON file. The last dictation is also kept in
//! memory for "Paste last dictation", even with the history off. (Idea from Hex,
//! anomalyco/hex, MIT.)

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// "h-" and the time in milliseconds.
    pub id: String,
    /// Unix seconds.
    pub at: u64,
    pub text: String,
    /// "key" (the global dictation key) or "window" (dictation in this window).
    pub source: String,
    pub model: String,
    /// Seconds of audio.
    pub seconds: f64,
}

/// How long entries stay: off | day | week | month | forever.
pub const KEEP_CHOICES: &[&str] = &["off", "day", "week", "month", "forever"];
pub const DEFAULT_KEEP: &str = "week";
/// Never more than this many entries (the oldest go first).
pub const MAX_ENTRIES: usize = 5_000;

fn keep_seconds(keep: &str) -> Option<u64> {
    match keep {
        "day" => Some(86_400),
        "week" => Some(7 * 86_400),
        "month" => Some(30 * 86_400),
        "forever" => None,
        _ => Some(0), // off: nothing is kept
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn path() -> PathBuf {
    crate::config::data_dir().join("history.json")
}

pub struct History {
    file: PathBuf,
    keep: Mutex<String>,
    entries: Mutex<Vec<Entry>>,
    last: Mutex<Option<String>>,
    /// One save at a time, each with the entries as they are when it starts: an older state
    /// never lands after a newer one (a delete is never undone by a dictation saved meanwhile).
    saving: Mutex<()>,
}

impl History {
    /// Reads the file (a missing or broken one is an empty history) and drops what is too old.
    pub fn open(file: PathBuf, keep: &str) -> Self {
        let entries: Vec<Entry> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let h = Self { file, keep: Mutex::new(keep.into()), entries: Mutex::new(entries), last: Mutex::new(None), saving: Mutex::new(()) };
        h.prune(now());
        // After the pruning: a dictation that is gone is never typed again.
        *h.last.lock() = h.entries.lock().last().map(|e| e.text.clone());
        h
    }

    pub fn set_keep(&self, keep: &str) {
        *self.keep.lock() = keep.into();
        self.prune(now());
    }

    /// A finished dictation. Kept (unless the history is off) and remembered for Paste last.
    pub fn add(&self, text: &str, source: &str, model: &str, seconds: f64) -> Option<Entry> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        *self.last.lock() = Some(text.to_string());
        if self.keep.lock().as_str() == "off" {
            return None;
        }
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let entry = Entry { id: format!("h-{ms}"), at: now(), text: text.into(), source: source.into(), model: model.into(), seconds };
        {
            let mut e = self.entries.lock();
            // Two in the same millisecond: a distinct id all the same.
            let mut id = entry.id.clone();
            let mut n = 1;
            while e.iter().any(|x| x.id == id) {
                id = format!("{}-{n}", entry.id);
                n += 1;
            }
            e.push(Entry { id, ..entry.clone() });
            let over = e.len().saturating_sub(MAX_ENTRIES);
            e.drain(..over);
        }
        self.save();
        self.entries.lock().last().cloned()
    }

    /// The last dictation's text (also with the history off).
    pub fn last(&self) -> Option<String> {
        self.last.lock().clone()
    }

    /// Newest first; `query` matches words in any order, any case.
    pub fn list(&self, query: &str) -> Vec<Entry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let e = self.entries.lock();
        e.iter()
            .rev()
            .filter(|x| {
                let t = x.text.to_lowercase();
                words.iter().all(|w| t.contains(w.as_str()))
            })
            .cloned()
            .collect()
    }

    pub fn delete(&self, id: &str) -> bool {
        let removed = {
            let mut e = self.entries.lock();
            let gone: Option<Entry> = e.iter().find(|x| x.id == id).cloned();
            e.retain(|x| x.id != id);
            // A deleted dictation is not typed by Paste last any more.
            if let Some(g) = &gone {
                let mut last = self.last.lock();
                if last.as_deref() == Some(g.text.as_str()) {
                    *last = e.last().map(|x| x.text.clone());
                }
            }
            gone.is_some()
        };
        if removed {
            self.save();
        }
        removed
    }

    pub fn clear(&self) {
        self.entries.lock().clear();
        *self.last.lock() = None;
        self.save();
    }

    /// Drop what is older than the setting (and everything when it is off).
    pub fn prune(&self, now: u64) {
        let Some(max_age) = keep_seconds(&self.keep.lock()) else { return };
        let changed = {
            let mut e = self.entries.lock();
            let before = e.len();
            e.retain(|x| now.saturating_sub(x.at) < max_age);
            e.len() != before
        };
        if changed {
            self.save();
        }
    }

    /// Written whole to a temporary file, then renamed; readable by you only.
    fn save(&self) {
        let _one = self.saving.lock();
        let body = serde_json::to_vec(&*self.entries.lock()).unwrap_or_default();
        if let Err(e) = write_private(&self.file, &body) {
            eprintln!("  history: could not save {}: {e}", self.file.display());
        }
    }
}

fn write_private(file: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
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
        // A temporary file left by an older build keeps its mode: private again.
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(body)?;
    f.sync_all()?;
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ow-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("history.json")
    }

    #[test]
    fn dictations_are_kept_found_deleted_and_saved() {
        let f = temp("basic");
        let h = History::open(f.clone(), "week");
        assert!(h.add("  ", "key", "small", 1.0).is_none(), "nothing said: nothing kept");
        h.add("Ship the new version on Friday", "key", "parakeet-v3", 3.2).unwrap();
        h.add("Call Lisandro tomorrow", "window", "small", 2.0).unwrap();
        assert_eq!(h.last().as_deref(), Some("Call Lisandro tomorrow"));
        let all = h.list("");
        assert_eq!(all.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), vec!["Call Lisandro tomorrow", "Ship the new version on Friday"], "newest first");
        assert_eq!(h.list("friday SHIP").len(), 1, "words in any order and case");
        assert!(h.list("nothing").is_empty());
        // A second open reads the file back.
        let again = History::open(f.clone(), "week");
        assert_eq!(again.list(""), all);
        assert!(again.delete(&all[0].id));
        assert!(!again.delete("h-0"));
        assert_eq!(History::open(f.clone(), "week").list("").len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
        again.clear();
        assert!(History::open(f.clone(), "week").list("").is_empty());
        assert_eq!(again.last(), None, "Clear all: nothing to paste");
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn a_deleted_or_pruned_dictation_is_never_pasted() {
        let f = temp("last");
        let h = History::open(f.clone(), "week");
        let a = h.add("first words", "key", "small", 1.0).unwrap();
        let b = h.add("secret words", "key", "small", 1.0).unwrap();
        assert!(h.delete(&b.id));
        assert_eq!(h.last().as_deref(), Some("first words"), "the newest left");
        assert!(h.delete(&a.id));
        assert_eq!(h.last(), None);
        // An old file opened with a short retention: the pruned text is not the last one.
        let mut old = Entry { id: "h-1".into(), at: 1, text: "very old".into(), source: "key".into(), model: "small".into(), seconds: 1.0 };
        std::fs::write(&f, serde_json::to_vec(&vec![old.clone()]).unwrap()).unwrap();
        assert_eq!(History::open(f.clone(), "day").last(), None);
        old.at = now();
        std::fs::write(&f, serde_json::to_vec(&vec![old]).unwrap()).unwrap();
        assert_eq!(History::open(f.clone(), "day").last().as_deref(), Some("very old"));
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn off_keeps_nothing_but_the_last_text_and_old_entries_go() {
        let f = temp("keep");
        let h = History::open(f.clone(), "off");
        assert!(h.add("private words", "key", "small", 1.0).is_none());
        assert_eq!(h.last().as_deref(), Some("private words"), "Paste last still works");
        assert!(h.list("").is_empty());
        h.set_keep("day");
        h.add("today", "key", "small", 1.0).unwrap();
        h.prune(now() + 86_400 + 1);
        assert!(h.list("").is_empty(), "older than a day: gone");
        h.set_keep("forever");
        h.add("kept", "key", "small", 1.0).unwrap();
        h.prune(now() + 10 * 365 * 86_400);
        assert_eq!(h.list("").len(), 1);
        h.set_keep("off");
        assert!(h.list("").is_empty(), "turning it off deletes what was kept");
        // A broken file is an empty history, never an error.
        std::fs::write(&f, "not json").unwrap();
        assert!(History::open(f.clone(), "week").list("").is_empty());
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }
}
