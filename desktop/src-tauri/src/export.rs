//! Note apps: each meeting as a Markdown file in a folder the user chooses: an Obsidian vault (or
//! a folder in one), Logseq's pages, or any folder a sync program keeps (Google Drive for desktop,
//! Dropbox, iCloud Drive, Syncthing). Nothing leaves the computer here.
//!
//! The file has YAML front matter (title, date, length, tags, the meeting's id), then the parts
//! the user chose. It is written again when the AI notes come, but never over the user's own
//! edits: the meeting keeps the hash of what was written, and a file that differs from it stays.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::meeting::{self, Meeting, Parts};

/// What `export_parts` may name, in the file's order.
pub const PARTS: &[&str] = &["ai_notes", "notes", "transcript"];

pub fn parts(names: &[String]) -> Parts {
    let has = |p: &str| names.iter().any(|n| n.trim().eq_ignore_ascii_case(p));
    Parts { ai_notes: has("ai_notes"), notes: has("notes"), transcript: has("transcript") }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Written (or rewritten): the file's name in the folder.
    Written(String),
    /// The file was changed outside (in Obsidian…): it stays as it is.
    KeptEdited(String),
}

/// Write meeting `id` (from `meetings`) to `dir`, and remember the file in the meeting.
/// `offset_at`: the time zone (minutes east of UTC) at a unix time.
pub fn save_meeting(meetings: &Path, id: &str, dir: &Path, parts: Parts, offset_at: &dyn Fn(u64) -> i32) -> anyhow::Result<Outcome> {
    // One save at a time (after Stop and after the AI notes they can meet): each one reads the
    // file and the hash that the one before left.
    static ONE_EXPORT: Mutex<()> = Mutex::new(());
    let _one = ONE_EXPORT.lock();
    anyhow::ensure!(dir.is_dir(), "The folder {} is not there (a drive that is not connected?).", dir.display());
    let m = meeting::load_in(meetings, id)?;
    let offset_min = offset_at(m.started_at);
    let text = file_text(&m, parts, offset_min);
    let hash = hash(&text);
    let wanted = file_name(&m, offset_min);
    let dir_text = dir.to_string_lossy().into_owned();
    // The file it was saved as: only a name in this same folder (another folder: a new file).
    let saved_as = Path::new(&m.export_file).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let old = (!saved_as.is_empty() && (m.export_dir.is_empty() || m.export_dir == dir_text)).then(|| dir.join(&saved_as)).filter(|p| p.is_file());
    let target = match &old {
        Some(path) => {
            let on_disk = std::fs::read_to_string(path).unwrap_or_default();
            if self::hash(&on_disk) != m.export_hash {
                return Ok(Outcome::KeptEdited(saved_as));
            }
            if on_disk == text {
                return Ok(Outcome::Written(saved_as)); // nothing new
            }
            // The title changed: the file follows it (when that name is free).
            if wanted != saved_as && !dir.join(&wanted).exists() {
                std::fs::rename(path, dir.join(&wanted))?;
                wanted
            } else {
                saved_as
            }
        }
        None => free_name(dir, &wanted),
    };
    write_atomic(&dir.join(&target), &text)?;
    meeting::edit_in(meetings, id, |m| {
        m.export_file = target.clone();
        m.export_hash = hash.clone();
        m.export_dir = dir_text.clone();
    })?;
    Ok(Outcome::Written(target))
}

/// The file: front matter, then the chosen parts (title and time always).
pub fn file_text(m: &Meeting, parts: Parts, offset_min: i32) -> String {
    let date = format!("{}:00{}", meeting::local_time(m.started_at, offset_min).replace(' ', "T"), offset_text(offset_min));
    // The screen video stays in the app's folder: the note links to it (a big file in a synced
    // vault is not wanted).
    let video = match &m.video {
        Some(v) if v.bytes > 0 => format!("video: {}\n", yaml_str(&meeting::dir().join(&v.file).to_string_lossy())),
        _ => String::new(),
    };
    let front = format!(
        "---\ntitle: {}\ndate: {date}\nduration_min: {}\ntags: [meeting]\nlowkey_id: {}\n{video}---\n\n",
        yaml_str(&m.title),
        (m.duration / 60.0).round() as u64,
        m.id
    );
    front + &meeting::to_markdown_parts(m, offset_min, parts)
}

/// "2026-11-04 14.05 Weekly sync.md": sorted by time, and no character that Obsidian, Windows
/// or macOS refuse in a name.
pub fn file_name(m: &Meeting, offset_min: i32) -> String {
    let when = meeting::local_time(m.started_at, offset_min).replace(':', ".");
    // The app's own title ("Meeting · Nov 4, 2:05 PM") says the time again: "Meeting" is enough.
    let title = if m.title.starts_with("Meeting · ") { "Meeting" } else { m.title.as_str() };
    let title: String = title.chars().map(|c| if "/\\:*?\"<>|#^[]".contains(c) || c.is_control() { ' ' } else { c }).collect();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    // Names are limited in bytes (255 on most file systems): room for the date, " (99)" and ".md".
    let mut bytes = 0;
    let title: String = title.trim_matches(['.', ' ']).chars().take_while(|c| {
        bytes += c.len_utf8();
        bytes <= 200
    }).collect();
    format!("{when} {}.md", if title.is_empty() { "Meeting" } else { title.trim_end_matches(['.', ' ']) })
}

/// `name`, or "name (2).md"… when another file has it.
fn free_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let stem = name.strip_suffix(".md").unwrap_or(name);
    (2..).map(|n| format!("{stem} ({n}).md")).find(|n| !dir.join(n).exists()).unwrap()
}

/// A hidden temporary file (on disk before the rename), then a rename: a sync program or
/// Obsidian never sees half a file, and a power cut never leaves an empty one.
fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("meeting.md");
    let tmp = path.with_file_name(format!(".{name}.{}-{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A YAML double-quoted string: a new line or another control character never breaks the
/// front matter.
fn yaml_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn offset_text(offset_min: i32) -> String {
    let sign = if offset_min < 0 { '-' } else { '+' };
    let m = offset_min.unsigned_abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// The Obsidian vault `dir` is in: the nearest folder with a `.obsidian` folder.
pub fn obsidian_vault(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".obsidian").is_dir()).map(Path::to_path_buf)
}

/// "obsidian://open?vault=…&file=…" for `file` in `dir`.
pub fn obsidian_link(dir: &Path, file: &str) -> Option<String> {
    let vault = obsidian_vault(dir)?;
    let name = vault.file_name()?.to_str()?;
    let rel = dir.join(file).strip_prefix(&vault).ok()?.to_str()?.replace('\\', "/");
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20");
    Some(format!("obsidian://open?vault={}&file={}", enc(name), enc(&rel)))
}

/// The time zone at unix time `at` (summer time included), in minutes east of UTC: the files
/// say the user's local time of the meeting. Windows: UTC for now (meetings record on Linux only).
pub fn local_offset_at(at: u64) -> i32 {
    #[cfg(target_os = "linux")]
    let out = std::process::Command::new("date").args(["-d", &format!("@{at}"), "+%z"]).output().ok();
    #[cfg(target_os = "macos")]
    let out = std::process::Command::new("date").args(["-r", &at.to_string(), "+%z"]).output().ok();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let out: Option<std::process::Output> = {
        let _ = at;
        None
    };
    out.and_then(|o| parse_offset(String::from_utf8_lossy(&o.stdout).trim())).unwrap_or(0)
}

/// "-0300" → -180.
fn parse_offset(z: &str) -> Option<i32> {
    let sign = z.chars().next()?;
    let (h, m) = (z.get(1..3)?.parse::<i32>().ok()?, z.get(3..5)?.parse::<i32>().ok()?);
    match sign {
        '-' => Some(-(h * 60 + m)),
        '+' => Some(h * 60 + m),
        _ => None,
    }
}

/// Open a link with the system (obsidian://…).
pub fn open_link(link: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(windows)]
    let mut cmd = {
        // Quoted by hand: cmd splits an unquoted link at "&".
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "\"\""]).raw_arg(format!("\"{link}\""));
        c
    };
    #[cfg(not(windows))]
    cmd.arg(link);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::{Line, Speaker};

    fn dirs(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("ow-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (meetings, vault) = (root.join("meetings"), root.join("My Vault"));
        std::fs::create_dir_all(&meetings).unwrap();
        std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
        std::fs::create_dir_all(vault.join("Meetings")).unwrap();
        (meetings, vault)
    }

    fn doc() -> Meeting {
        Meeting {
            id: "m-1793800000000".into(),
            title: "Weekly sync: \"Q4\"/plans".into(),
            started_at: 1_793_800_000, // 2026-11-04 13:46 UTC
            duration: 1830.0,
            state: "done".into(),
            notes: "Ask about the budget".into(),
            lines: vec![Line { speaker: Speaker::Me, start: 0.0, end: 2.0, text: "Hello everyone.".into(), person: 0 }],
            ..Default::default()
        }
    }

    #[test]
    fn the_file_has_front_matter_and_the_chosen_parts() {
        let t = file_text(&doc(), parts(&["notes".into(), "transcript".into()]), -180);
        assert!(t.starts_with("---\ntitle: \"Weekly sync: \\\"Q4\\\"/plans\"\ndate: 2026-11-04T10:46:00-03:00\nduration_min: 31\ntags: [meeting]\nlowkey_id: m-1793800000000\n---\n\n# Weekly sync"), "{t}");
        assert!(t.contains("## Notes\n\nAsk about the budget") && t.contains("## Transcript") && !t.contains("AI notes"), "{t}");
        let only_notes = file_text(&doc(), parts(&["notes".into()]), 0);
        assert!(!only_notes.contains("## Transcript"), "{only_notes}");
        // The screen video: its path, once it is saved.
        let mut m = doc();
        m.video = Some(crate::meeting::Video { file: "m-1793800000000.webm".into(), bytes: 0, ..Default::default() });
        assert!(!file_text(&m, parts(&["notes".into()]), 0).contains("video:"), "not while it records");
        m.video.as_mut().unwrap().bytes = 9;
        let path = meeting::dir().join("m-1793800000000.webm");
        let t = file_text(&m, parts(&["notes".into()]), 0);
        assert!(t.contains(&format!("lowkey_id: m-1793800000000\nvideo: \"{}\"\n---", path.display())), "{t}");
    }

    #[test]
    fn names_are_safe_and_sorted_by_time() {
        assert_eq!(file_name(&doc(), -180), "2026-11-04 10.46 Weekly sync Q4 plans.md");
        assert_eq!(file_name(&Meeting { title: " ..#[]".into(), ..doc() }, 0), "2026-11-04 13.46 Meeting.md");
        assert_eq!(file_name(&Meeting { title: "Meeting · Nov 4, 1:46 PM".into(), ..doc() }, 0), "2026-11-04 13.46 Meeting.md");
        assert_eq!(offset_text(330), "+05:30");
        assert_eq!(offset_text(-180), "-03:00");
    }

    #[test]
    fn saved_again_but_never_over_the_users_edits() {
        let (meetings, vault) = dirs("save");
        let dir = vault.join("Meetings");
        meeting::save_in(&meetings, &doc()).unwrap();
        let all = parts(&PARTS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let first = save_meeting(&meetings, "m-1793800000000", &dir, all, &|_| 0).unwrap();
        let Outcome::Written(name) = first.clone() else { panic!("{first:?}") };
        assert!(dir.join(&name).is_file());
        // The AI notes come: written again, same file.
        meeting::edit_in(&meetings, "m-1793800000000", |m| m.ai_notes = "## Plans\n- **Me** sends the plan".into()).unwrap();
        assert_eq!(save_meeting(&meetings, "m-1793800000000", &dir, all, &|_| 0).unwrap(), Outcome::Written(name.clone()));
        assert!(std::fs::read_to_string(dir.join(&name)).unwrap().contains("**Me** sends the plan"));
        // The title changes: the file follows it.
        meeting::edit_in(&meetings, "m-1793800000000", |m| m.title = "Planning".into()).unwrap();
        let renamed = save_meeting(&meetings, "m-1793800000000", &dir, all, &|_| 0).unwrap();
        assert_eq!(renamed, Outcome::Written("2026-11-04 13.46 Planning.md".into()));
        assert!(!dir.join(&name).exists());
        // Edited in Obsidian: kept as it is.
        std::fs::write(dir.join("2026-11-04 13.46 Planning.md"), "my own words").unwrap();
        meeting::edit_in(&meetings, "m-1793800000000", |m| m.ai_notes = "## Other".into()).unwrap();
        assert_eq!(save_meeting(&meetings, "m-1793800000000", &dir, all, &|_| 0).unwrap(), Outcome::KeptEdited("2026-11-04 13.46 Planning.md".into()));
        assert_eq!(std::fs::read_to_string(dir.join("2026-11-04 13.46 Planning.md")).unwrap(), "my own words");
        // Another file has the name of a new meeting: a free one.
        let other = Meeting { id: "m-1793800000001".into(), title: "Planning".into(), ..doc() };
        meeting::save_in(&meetings, &other).unwrap();
        assert_eq!(save_meeting(&meetings, "m-1793800000001", &dir, all, &|_| 0).unwrap(), Outcome::Written("2026-11-04 13.46 Planning (2).md".into()));
        assert!(save_meeting(&meetings, "m-1793800000001", &vault.join("gone"), all, &|_| 0).is_err(), "a missing folder is not made");
        let _ = std::fs::remove_dir_all(vault.parent().unwrap());
    }

    #[test]
    fn two_saves_at_once_leave_the_note_and_its_hash_in_step() {
        let (meetings, vault) = dirs("race");
        let dir = vault.join("Meetings");
        meeting::save_in(&meetings, &doc()).unwrap();
        let all = parts(&PARTS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        save_meeting(&meetings, "m-1793800000000", &dir, all, &|_| 0).unwrap();
        meeting::edit_in(&meetings, "m-1793800000000", |m| m.ai_notes = "## Plans\n- **Me** sends the plan".into()).unwrap();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (m, d) = (meetings.clone(), dir.clone());
                std::thread::spawn(move || save_meeting(&m, "m-1793800000000", &d, all, &|_| 0).unwrap())
            })
            .collect();
        for t in threads {
            assert!(matches!(t.join().unwrap(), Outcome::Written(_)));
        }
        let m = meeting::load_in(&meetings, "m-1793800000000").unwrap();
        assert_eq!(hash(&std::fs::read_to_string(dir.join(&m.export_file)).unwrap()), m.export_hash, "never \"changed outside\" by itself");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left");
        let _ = std::fs::remove_dir_all(vault.parent().unwrap());
    }

    #[test]
    fn another_folder_gets_its_own_note() {
        let (meetings, vault) = dirs("other");
        meeting::save_in(&meetings, &doc()).unwrap();
        let all = parts(&["AI_Notes".into(), " transcript ".into()]);
        assert!(all.ai_notes && all.transcript && !all.notes, "names in any case");
        save_meeting(&meetings, "m-1793800000000", &vault.join("Meetings"), all, &|_| 0).unwrap();
        std::fs::create_dir_all(vault.join("Other")).unwrap();
        // A file of that name in the new folder is not taken for the meeting's.
        let name = file_name(&doc(), 0);
        std::fs::write(vault.join("Other").join(&name), "someone else's").unwrap();
        assert_eq!(save_meeting(&meetings, "m-1793800000000", &vault.join("Other"), all, &|_| 0).unwrap(), Outcome::Written(name.replace(".md", " (2).md")));
        let _ = std::fs::remove_dir_all(vault.parent().unwrap());
    }

    #[test]
    fn long_titles_new_lines_and_time_zones() {
        let long = Meeting { title: "會議記錄".repeat(40), ..doc() };
        let name = file_name(&long, 0);
        assert!(name.len() + " (99)".len() <= 255 && name.ends_with(".md"), "{} bytes", name.len());
        let t = file_text(&Meeting { title: "Sync\nretro\t\u{7}".into(), ..doc() }, Parts::ALL, 0);
        assert!(t.starts_with("---\ntitle: \"Sync\\nretro\\t\\u0007\"\ndate: "), "{t}");
        assert_eq!((parse_offset("-0300"), parse_offset("+0530"), parse_offset("+0000"), parse_offset("UTC")), (Some(-180), Some(330), Some(0), None));
        #[cfg(target_os = "linux")]
        {
            // GNU date with TZ: summer and winter time differ for the same zone.
            let at = |ts: u64| {
                let o = std::process::Command::new("date").env("TZ", "Europe/Madrid").args(["-d", &format!("@{ts}"), "+%z"]).output().unwrap();
                parse_offset(String::from_utf8_lossy(&o.stdout).trim())
            };
            assert_eq!((at(1_782_900_000), at(1_793_800_000)), (Some(120), Some(60)));
        }
    }

    #[test]
    fn obsidian_links_name_the_vault_and_the_file() {
        let (_m, vault) = dirs("link");
        let link = obsidian_link(&vault.join("Meetings"), "2026-11-04 13.46 Planning.md").unwrap();
        assert_eq!(link, "obsidian://open?vault=My%20Vault&file=Meetings%2F2026-11-04%2013.46%20Planning.md");
        assert!(obsidian_link(&std::env::temp_dir().join("no-vault-here-at-all"), "x.md").is_none());
        let _ = std::fs::remove_dir_all(vault.parent().unwrap());
    }
}
