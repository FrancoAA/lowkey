//! Names from the call: while a meeting records, the window of the call app (Zoom, Teams, Google
//! Meet in a browser) is read through the Linux accessibility bus (AT-SPI over D-Bus) about every
//! 400 ms. Each reading says who the app shows in the call, who it marks as talking, and which one
//! is the person using Lowkey. After the meeting, each "Them" line (and each voice found by
//! `voices.rs`) gets the name the app showed while it was said.
//!
//! Never a guess: a name only when the app marked one person as talking for most of the line, or
//! when the call has just one other person. Anything else is Them. Read-only: nothing in the call
//! app is clicked or changed, and names stay on the computer (the log gets counts only).
//!
//! The idea, the tree rules for each app and the timing come from Yogurt (MIT): its
//! `callNames/parse.ts`, `callNames/attribute.ts`, `callNames/index.ts` and the Linux helper
//! `yogurt-system` (`atspi.rs`, `calls.rs`, `call.rs`).
//!
//! The pure parts (tree rules, `name_for`, `name_people`) compile on every OS. The AT-SPI reader
//! is Linux only; elsewhere `read_call` gives an error and the `Watcher` reads nothing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use regex::Regex;

// ---------------------------------------------------------------------------------------------
// Readings and the tree
// ---------------------------------------------------------------------------------------------

/// What the call app showed at one moment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SampleData {
    /// Who the app marks as talking now.
    pub speaking: Vec<String>,
    /// Everyone the app shows in the call, the person using Lowkey too.
    pub participants: Vec<String>,
    /// The person using Lowkey, when the app says which one they are ("(me)", "You").
    pub me: Option<String>,
}

/// One reading of the call app on the meeting clock.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Sample {
    /// Seconds on the meeting clock, with the app's lag already taken off.
    pub at: f64,
    pub speaking: Vec<String>,
    pub participants: Vec<String>,
    pub me: Option<String>,
    /// False: the app could not be read at this moment (no window, no accessibility bus).
    pub ok: bool,
}

impl Sample {
    pub fn new(at: f64, data: SampleData) -> Self {
        Sample { at, speaking: data.speaking, participants: data.participants, me: data.me, ok: true }
    }

    /// A moment when nothing could be read: it counts as not knowing.
    pub fn unknown(at: f64) -> Self {
        Sample { at, ok: false, ..Default::default() }
    }
}

/// One element of an accessibility tree: role, description, name, text, id, DOM classes, children.
/// The short field names are Yogurt's (`AxNode`), so its rules port line by line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    /// The AT-SPI role name ("push button", "label", "tree item", ...).
    pub r: String,
    pub d: Option<String>,
    pub t: Option<String>,
    pub v: Option<String>,
    pub i: Option<String>,
    pub c: Vec<String>,
    pub k: Vec<Node>,
}

/// One top-level window of the call app and its tree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    pub title: Option<String>,
    pub tree: Node,
}

/// What kind of window the call app has. A browser is read as Meet, or as Teams when its window
/// title says Teams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zoom,
    Teams,
    Meet,
}

/// The kind from a program name or Flatpak id (Yogurt's call.rs).
pub fn kind_of(id: &str) -> Kind {
    let id = id.to_ascii_lowercase();
    if id.contains("zoom") {
        Kind::Zoom
    } else if id.contains("teams") {
        Kind::Teams
    } else {
        Kind::Meet
    }
}

/// What the call app shows now, from its windows.
pub fn parse(kind: Kind, windows: &[Window]) -> SampleData {
    match kind {
        Kind::Zoom => read_zoom(windows),
        Kind::Teams => read_teams(windows),
        Kind::Meet => read_meet(windows, MEET_TALKING_CLASSES),
    }
}

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

fn re(pattern: &str) -> Regex {
    // The patterns are constants and the tests compile each one. A bad one must not stop the
    // app, so it becomes a pattern that never matches.
    Regex::new(pattern).unwrap_or_else(|_| Regex::new(r"[^\s\S]").expect("a constant pattern"))
}

/// A small part of Unicode NFKC: full-width forms and odd spaces become plain ones. Zoom writes
/// "（自分）" and "：" in Japanese and Chinese; Yogurt reads them after NFKC.
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' | '\u{00A0}' | '\u{202F}' => ' ',
            _ => c,
        })
        .collect()
}

fn low(s: &str) -> String {
    fold(s).to_lowercase().trim().to_string()
}

/// Each name once, case-insensitive: in the order first seen, with the spelling seen last (the
/// way a JavaScript Map keeps keys).
fn uniq(xs: &[String]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for x in xs {
        let key = x.to_lowercase();
        match seen.get(&key) {
            Some(&i) => order[i] = x.clone(),
            None => {
                seen.insert(key, order.len());
                order.push(x.clone());
            }
        }
    }
    order
}

fn walk<'a>(n: &'a Node, up: &mut Vec<&'a Node>, f: &mut dyn FnMut(&'a Node, &[&'a Node])) {
    f(n, up);
    up.push(n);
    for k in &n.k {
        walk(k, up, f);
    }
    up.pop();
}

fn walk_all<'a>(n: &'a Node, f: &mut dyn FnMut(&'a Node, &[&'a Node])) {
    walk(n, &mut Vec::new(), f);
}

fn any_node(n: &Node, pred: &dyn Fn(&Node) -> bool) -> bool {
    pred(n) || n.k.iter().any(|k| any_node(k, pred))
}

fn texts(n: &Node) -> impl Iterator<Item = &str> {
    [&n.d, &n.t, &n.v].into_iter().filter_map(|x| x.as_deref()).filter(|x| !x.is_empty())
}

fn non_empty(x: &Option<String>) -> Option<&str> {
    x.as_deref().filter(|s| !s.is_empty())
}

// AT-SPI role names. Yogurt also knows the macOS names; Lowkey reads only AT-SPI.
const TAB_GROUP: &[&str] = &["page tab"];
const STATIC_TEXT: &[&str] = &["static", "label", "text", "paragraph", "heading", "caption"];
const ROW: &[&str] = &["table row", "list item", "row"];
const OUTLINE: &[&str] = &["tree table", "table", "list", "list box", "outline", "tree"];

fn is(role: &str, set: &[&str]) -> bool {
    set.contains(&role)
}

// ---------------------------------------------------------------------------------------------
// Zoom
// ---------------------------------------------------------------------------------------------
// From Zoom's own strings and tree (Yogurt measured them): a video tile is a "page tab"
// described "Name, Computer audio unmuted, Video off", with ", Active speaker" on the person
// talking. The participants panel is a list "Participants list" with rows "Name (me)",
// "Name (Host)". The small floating window says "Talking: Name". Zoom Workplace on Linux draws
// each tile as a push button "video item <name>", with " Active Speaker" on the shown one.

/// Zoom's words by language: audio on, audio off, active speaker, talking label, the list, "(me)".
const ZOOM: &[[&str; 6]] = &[
    ["Computer audio unmuted", "Computer audio muted", "Active speaker", "Talking:", "Participants list", "(me)"],
    ["Bei Computeraudio Stummschaltung deaktiviert", "Bei Computeraudio Stummschaltung aktiviert", "Aktiver Sprecher", "Sprechen:", "Teilnehmerliste", "(ich)"],
    ["Audio de la computadora no silenciado", "Audio de la computadora silenciado", "Orador activo", "Hablando:", "Lista de participantes", "(yo)"],
    ["Son de l'ordinateur rétabli", "Ordinateur mis en sourdine", "Intervenant actif", "La parole est à :", "Liste des participants", "(moi)"],
    ["Audio computer attivato", "Audio computer disattivato", "Oratore attivo", "Conversazione in corso:", "Elenco dei partecipanti", "(io)"],
    ["Áudio do computador ativado", "Áudio do computador desativado", "Orador ativo", "Falando:", "Lista de participantes", "(eu)"],
    ["Computeraudio ingeschakeld", "Computeraudio gedempt", "Actieve spreker", "Aan het spreken:", "Deelnemerslijst", "(ik)"],
    ["Datorljudet aktiverades", "Datorljudet inaktiverades", "Aktiv talare", "Talar:", "Deltagarlista", "(jag)"],
    ["ミュート解除されたコンピュータ オーディオ", "ミュートされたコンピュータ オーディオ", "アクティブ スピーカー", "発言中:", "参加者リスト", "（自分）"],
    ["电脑音频开启", "电脑音频静音", "当前演讲者", "正在发言：", "参会者列表", "（我）"],
];

struct ZoomWords {
    audio: HashSet<String>,
    active: HashSet<String>,
    /// The talking labels without their colon.
    talking: Vec<String>,
    lists: HashSet<String>,
    me: HashSet<String>,
}

static ZW: LazyLock<ZoomWords> = LazyLock::new(|| ZoomWords {
    audio: ZOOM.iter().flat_map(|l| [low(l[0]), low(l[1])]).collect(),
    active: ZOOM.iter().map(|l| low(l[2])).collect(),
    talking: ZOOM.iter().map(|l| fold(l[3]).trim_end_matches(':').trim_end().to_string()).collect(),
    lists: ZOOM.iter().map(|l| low(l[4])).collect(),
    me: ZOOM.iter().map(|l| low(l[5]).replace(['(', ')'], "")).collect(),
});

/// What else Zoom appends to a tile: it says nothing about who it is.
static OTHER_STATUS: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(video (on|off)|no audio|telephone.*|screen sharing.*|pinned|spotlighted)$"));
const TAG: &str = r"me|host|co-host|guest|interpreter|participant id: [^),]*|ich|yo|moi|io|eu|ik|jag|自分|我";
/// Zoom's tags after a name, alone or together ("(Host, me)"): never part of the name.
static ZOOM_TAGS: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?i)\s*[(（]\s*(?:{TAG})(?:\s*[,，、]\s*(?:{TAG}))*\s*[)）]\s*$")));
static ZOOM_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r",\s*|、"));
static PARTICIPANTS_COUNT: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^participants \("));
static LINUX_TILE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^video item\s+(.+)$"));
static LINUX_ACTIVE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\s+active speaker$"));

/// "Ana Ruiz (Host, me)" gives host, me.
fn zoom_tags(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut name = fold(raw).trim().to_string();
    while let Some(m) = ZOOM_TAGS.find(&name) {
        let inner = m.as_str().trim_start();
        let inner = inner.strip_prefix('(').unwrap_or(inner).trim_end();
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        out.extend(inner.split([',', '、']).map(low));
        name.truncate(m.start());
    }
    out
}

fn clean_zoom(raw: &str) -> Option<String> {
    let mut name = fold(raw).trim().to_string();
    while ZOOM_TAGS.is_match(&name) {
        name = ZOOM_TAGS.replace(&name, "").into_owned();
    }
    (!name.is_empty() && !PARTICIPANTS_COUNT.is_match(&name)).then_some(name)
}

/// "Ana Ruiz, Computer audio unmuted, Video off, Active speaker" gives (Ana Ruiz, talking).
/// None when the words are not a tile.
pub fn zoom_tile(description: &str) -> Option<(String, bool)> {
    let folded = fold(description);
    let mut fields: Vec<&str> = ZOOM_SPLIT.split(&folded).collect();
    let (mut talking, mut known) = (false, false);
    while fields.len() > 1 {
        let f = low(fields[fields.len() - 1]);
        if ZW.active.contains(&f) {
            talking = true;
            known = true;
        } else if ZW.audio.contains(&f) {
            known = true;
        } else if !OTHER_STATUS.is_match(&f) {
            break;
        }
        fields.pop();
    }
    if !known {
        return None;
    }
    clean_zoom(&fields.join(", ")).map(|name| (name, talking))
}

/// "video item Ana Ruiz Active Speaker" gives (Ana Ruiz, talking). None when it is not a tile.
pub fn zoom_linux_tile(name: &str) -> Option<(String, bool)> {
    let folded = fold(name);
    let caps = LINUX_TILE.captures(folded.trim())?;
    let rest = caps.get(1)?.as_str();
    let talking = LINUX_ACTIVE.is_match(rest);
    let person = clean_zoom(&LINUX_ACTIVE.replace(rest, ""))?;
    (person.chars().count() <= 60).then_some((person, talking))
}

fn read_zoom(windows: &[Window]) -> SampleData {
    let mut speaking: Vec<String> = Vec::new();
    let mut participants: Vec<String> = Vec::new();
    let mut linux_tiles: Vec<(String, bool)> = Vec::new();
    let mut me: Option<String> = None;
    for w in windows {
        walk_all(&w.tree, &mut |n, up| {
            // The tile's words are its description on Linux, its name under UI Automation.
            if is(&n.r, TAB_GROUP) {
                if let Some((name, talking)) = non_empty(&n.d).or(non_empty(&n.t)).and_then(zoom_tile) {
                    participants.push(name.clone());
                    if talking {
                        speaking.push(name);
                    }
                }
            }
            if n.r == "push button" || n.r == "toggle button" {
                for raw in [&n.t, &n.d].into_iter().filter_map(non_empty) {
                    if let Some(tile) = zoom_linux_tile(raw) {
                        participants.push(tile.0.clone());
                        linux_tiles.push(tile);
                    }
                }
            }
            // The floating window's "Talking: Name". Yogurt also takes the words without the
            // colon; here the colon is needed, so a label like "Talking points" is never a name.
            if is(&n.r, STATIC_TEXT) {
                for raw in [&n.v, &n.t].into_iter().filter_map(non_empty) {
                    let v = fold(raw);
                    for key in &ZW.talking {
                        let Some(rest) = v.strip_prefix(key.as_str()) else { continue };
                        let Some(rest) = rest.trim_start().strip_prefix(':') else { continue };
                        if let Some(name) = clean_zoom(rest) {
                            speaking.push(name);
                        }
                    }
                }
            }
            // A row of the participants list: its first text is the name; "(me)" is the user.
            let in_list = || {
                up.iter().any(|a| {
                    is(&a.r, OUTLINE) && ZW.lists.contains(&low(non_empty(&a.d).or(non_empty(&a.t)).unwrap_or("")))
                })
            };
            if is(&n.r, ROW) && in_list() {
                let mut text: Option<&str> = None;
                walk_all(n, &mut |x, _| {
                    if text.is_none() && is(&x.r, STATIC_TEXT) {
                        text = non_empty(&x.v).or(non_empty(&x.t));
                    }
                });
                let Some(text) = text else { return };
                let Some(name) = clean_zoom(text) else { return };
                participants.push(name.clone());
                if zoom_tags(text).iter().any(|tag| ZW.me.contains(tag)) {
                    me = Some(name);
                }
            }
        });
    }
    // The Linux view says "Active Speaker" on the tile it shows even in a call of one, so the
    // mark is trusted only with three or more tiles.
    let tile_names: Vec<String> = linux_tiles.iter().map(|t| t.0.clone()).collect();
    if uniq(&tile_names).len() >= 3 {
        speaking.extend(linux_tiles.into_iter().filter(|t| t.1).map(|t| t.0));
    }
    SampleData { speaking: uniq(&speaking), participants: uniq(&participants), me }
}

// ---------------------------------------------------------------------------------------------
// Google Meet in a browser
// ---------------------------------------------------------------------------------------------
// Meet marks nobody as talking in words. Each tile shows the name as text, and the talking tile
// gets a CSS class that the browser's tree carries. That class is a build name Google changes:
// as of October 2026 it is "v5h6Xc". When it is gone, nobody is marked, and only a call of two
// can be named. A tile is the element seven levels above a name's text, holding just one name.

pub const MEET_TALKING_CLASSES: &[&str] = &["v5h6Xc"];
const TILE_UP: usize = 7;

/// Text in Meet's window that is never a person: times, the meeting code, chat.
static NOT_A_NAME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^\d{1,2}[:.]\d{2}(\s?[AP]M)?$|^[a-z]{3}-[a-z]{4}-[a-z]{3}$|says in chat|^(AM|PM)$|^\d+$")
});
static PRESENTING: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\s*\((presenting|presentation|you)[^)]*\)\s*$"));
static A_LETTER: LazyLock<Regex> = LazyLock::new(|| re(r"\p{L}"));
static NOT_IN_A_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"\d|[?!…:/⌘]|\.\.|\s[-–—|]\s"));
static LOWER_START: LazyLock<Regex> = LazyLock::new(|| re(r"^\p{Ll}"));
static MEET_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"\bMeet\b"));
static TITLE_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"\s+[-–—]\s+"));

/// Meet's own words that stand alone like a name.
const MEET_WORDS: &[&str] =
    &["meet", "reframe", "captions", "chat", "people", "activities", "host controls", "present", "raise hand", "reactions"];
/// Lowercase words inside real names: Ana María de la Cruz, Ludwig van Beethoven.
const PARTICLES: &[&str] = &[
    "de", "del", "della", "der", "den", "di", "da", "das", "do", "dos", "du", "la", "le", "van", "von", "ter", "ten", "y", "e",
    "bin", "binti", "al", "el", "ibn", "zu",
];
/// A toast, banner or other live area: Meet's messages, never a tile.
const LIVE: &[&str] = &["alert", "notification", "status bar", "status", "log", "marquee", "timer"];

/// A tile's name, or None for anything else Meet writes in a box of its own: lobby sentences,
/// toasts, times, a shared tab's title, control labels. A name has no digits, no sentence end,
/// at most five words, and in a name of several words each word starts with a capital (or is a
/// particle like "de", or is from a script without capitals). A miss means Them, never a wrong name.
fn meet_name(text: &str) -> Option<String> {
    let folded = fold(text);
    let name = PRESENTING.replace(&folded, "").trim().to_string();
    let len = name.chars().count();
    if !(2..=60).contains(&len) || !A_LETTER.is_match(&name) || NOT_A_NAME.is_match(&name) {
        return None;
    }
    if NOT_IN_A_NAME.is_match(&name) || MEET_WORDS.contains(&low(&name).as_str()) {
        return None;
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    // "Jr." and initials end with a period; a sentence does too, but with a longer last word.
    if let Some(last) = words.last() {
        if name.ends_with('.') && last.strip_suffix('.').unwrap_or(last).chars().count() > 2 {
            return None;
        }
    }
    if words.len() > 5 {
        return None;
    }
    if words.len() > 1 && words.iter().any(|w| LOWER_START.is_match(w) && !PARTICLES.contains(w)) {
        return None;
    }
    Some(name)
}

fn read_meet(windows: &[Window], talking_classes: &[&str]) -> SampleData {
    // The tiles in the order first seen, each with the names found under it.
    let mut groups: Vec<(&Node, Vec<String>)> = Vec::new();
    let mut index: HashMap<*const Node, usize> = HashMap::new();
    for w in windows {
        if let Some(title) = &w.title {
            if !MEET_TITLE.is_match(title) {
                continue;
            }
        }
        // The parts of the title ("Meet", the meeting code, the browser) are never people.
        let skip: HashSet<String> = TITLE_SPLIT.split(w.title.as_deref().unwrap_or("")).map(low).collect();
        walk_all(&w.tree, &mut |n, up| {
            if !is(&n.r, STATIC_TEXT) || up.len() < TILE_UP || up.iter().any(|a| is(&a.r, LIVE)) {
                return;
            }
            let Some(text) = non_empty(&n.v).or(non_empty(&n.t)) else { return };
            let Some(name) = meet_name(text) else { return };
            if skip.contains(&low(&name)) {
                return;
            }
            let root = up[up.len() - TILE_UP];
            let at = *index.entry(root as *const Node).or_insert_with(|| {
                groups.push((root, Vec::new()));
                groups.len() - 1
            });
            groups[at].1.push(name);
        });
    }
    let mut speaking = Vec::new();
    let mut participants = Vec::new();
    let mut me = None;
    for (root, names) in groups {
        let one = uniq(&names);
        if one.len() != 1 {
            continue;
        }
        let name = one[0].clone();
        participants.push(name.clone());
        // Meet labels the person's own tile "You".
        if low(&name) == "you" {
            me = Some(name.clone());
        }
        if any_node(root, &|x| x.c.iter().any(|c| talking_classes.contains(&c.as_str()))) {
            speaking.push(name);
        }
    }
    SampleData { speaking: uniq(&speaking), participants: uniq(&participants), me }
}

// ---------------------------------------------------------------------------------------------
// Microsoft Teams
// ---------------------------------------------------------------------------------------------
// Measured by Yogurt on Teams for Linux 2.28.0 (AT-SPI): the People panel's "Attendees" tree has
// rows "Name, Role, Muted|Unmuted", and the row's section repeats it. Teams marks the active
// speaker only with a ring: no word and no class. What is read is words only: those roster rows
// (inside a list named Participants/Attendees/People, so a chat line that reads like one is
// never a person), "Name, speaking" / "Name is speaking" / "Active speaker: Name" (other builds),
// and "(You)" after the person's own name.

static TEAMS_SPEAKING: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        re(r"(?i)^(.{2,60}?)(?:,\s*|\s+is\s+)(?:speaking|talking)\b"),
        re(r"(?i)\b(?:active speaker|now speaking|speaking now)[:,]?\s+(.{2,60})$"),
    ]
});
/// The state Teams puts after a person in the roster.
static TEAMS_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:muted|unmuted|mic(?:rophone)? (?:on|off)|video (?:is )?(?:on|off)|camera (?:is )?(?:on|off))$")
});
/// The roles Teams puts between the name and the state.
static TEAMS_ROLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:organizer|presenter|guest|external|co-?organizer|attendee|host|co-?host|interpreter)$"));
static TEAMS_SAYS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:speaking|talking|active speaker|now speaking)$"));
static TEAMS_ME: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(.{2,60}?)\s*\((?:you|me)\)"));
static TEAMS_TAGS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\s*\((?:organizer|presenter|guest|external|you|me)\)"));
static TRAILING_PUNCT: LazyLock<Regex> = LazyLock::new(|| re(r"[,.:;]+$"));
static YOU_OR_ME: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(you|me)\b"));
static TEAMS_LIST_NAME: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:meeting )?(?:participants|attendees|people|in (?:this|the) meeting)\b"));
static COMMA_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"\s*,\s*"));
/// A browser window that shows Teams.
static TEAMS_TITLE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bMicrosoft Teams\b|\|\s*Teams\b"));

const TEAMS_ROW: &[&str] = &["tree item", "table row", "list item", "row"];
const TEAMS_LIST: &[&str] = &["tree", "tree table", "table", "list", "list box", "outline"];

fn clean_teams(s: &str) -> String {
    let s = TEAMS_TAGS.replace_all(s, "");
    TRAILING_PUNCT.replace(&s, "").trim().to_string()
}

/// Whether an element sits in the People panel: a row, inside a list named Participants/Attendees/People.
fn in_teams_roster(n: &Node, up: &[&Node]) -> bool {
    if !is(&n.r, TEAMS_ROW) && !up.iter().any(|a| is(&a.r, TEAMS_ROW)) {
        return false;
    }
    up.iter().any(|a| {
        is(&a.r, TEAMS_LIST) && TEAMS_LIST_NAME.is_match(non_empty(&a.d).or(non_empty(&a.t)).unwrap_or("").trim())
    })
}

/// "Dana Smith, Organizer, Unmuted" gives Dana Smith; "Smith, Dana, Organizer, Unmuted" gives "Smith, Dana".
fn teams_persona(label: &str) -> Option<(String, bool)> {
    let folded = fold(label);
    let fields: Vec<&str> = COMMA_SPLIT.split(&folded).collect();
    let state = fields.iter().position(|f| TEAMS_STATUS.is_match(f))?;
    if state < 1 {
        return None;
    }
    let mut before = fields[..state].to_vec();
    while before.len() > 1 && before.last().is_some_and(|f| TEAMS_ROLE.is_match(f)) {
        before.pop();
    }
    let name = clean_teams(&before.join(", "));
    let len = name.chars().count();
    if !(2..=60).contains(&len) {
        return None;
    }
    Some((name, fields.iter().any(|f| TEAMS_SAYS.is_match(f))))
}

fn read_teams(windows: &[Window]) -> SampleData {
    let mut speaking = Vec::new();
    let mut participants = Vec::new();
    let mut me = None;
    for w in windows {
        walk_all(&w.tree, &mut |n, up| {
            let roster = in_teams_roster(n, up);
            for label in texts(n) {
                for r in TEAMS_SPEAKING.iter() {
                    if let Some(m) = r.captures(label).and_then(|c| c.get(1)) {
                        let name = clean_teams(m.as_str());
                        if name.chars().count() >= 2 && !YOU_OR_ME.is_match(&name) {
                            speaking.push(name);
                        }
                    }
                }
                if !roster {
                    continue;
                }
                if let Some((name, talking)) = teams_persona(label) {
                    participants.push(name.clone());
                    if talking {
                        speaking.push(name);
                    }
                }
                if let Some(m) = TEAMS_ME.captures(label).and_then(|c| c.get(1)) {
                    let mine = clean_teams(m.as_str());
                    participants.push(mine.clone());
                    me = Some(mine);
                }
            }
        });
    }
    participants.retain(|p: &String| p.chars().count() >= 2);
    SampleData { speaking: uniq(&speaking), participants: uniq(&participants), me }
}

// ---------------------------------------------------------------------------------------------
// Which windows hold the call
// ---------------------------------------------------------------------------------------------

/// The controls only a call window has: Teams's calendar and chat views have none.
const CALL_CONTROLS: &[&str] = &["leave", "leave call", "hang up", "end call", "mute mic", "unmute mic", "close participants pane"];

/// Whether a tree has a call control (Teams keeps its calendar and chat in windows shaped like
/// the call's; only the call has Leave or Mute).
pub fn is_call_tree(tree: &Node) -> bool {
    any_node(tree, &|n| {
        matches!(n.r.as_str(), "button" | "push button" | "toggle button" | "menu item")
            && [&n.t, &n.d].into_iter().filter_map(|x| x.as_deref()).any(|s| CALL_CONTROLS.contains(&s.trim().to_lowercase().as_str()))
    })
}

/// Whether a window may hold the call: Zoom only a meeting window, a browser only when its front
/// tab is a Meet call, Teams any window (`is_call_tree` tells its windows apart).
pub fn call_window(kind: Kind, title: &str) -> bool {
    match kind {
        Kind::Teams => true,
        Kind::Zoom => title.starts_with("Zoom Meeting") || title.starts_with("Zoom Webinar") || title == "Meeting",
        Kind::Meet => has_word(title, "meet"),
    }
}

/// "Meet - abc-defg-hij" has the word; "Meeting notes" does not.
pub fn has_word(title: &str, word: &str) -> bool {
    title.split(|c: char| !c.is_alphanumeric()).any(|w| w.eq_ignore_ascii_case(word))
}

/// The kind a browser window is read as: Teams when its title says so, Meet when it has the
/// word "Meet", else none (another tab is in front).
pub fn browser_window_kind(title: &str) -> Option<Kind> {
    if TEAMS_TITLE.is_match(title) {
        Some(Kind::Teams)
    } else if has_word(title, "meet") {
        Some(Kind::Meet)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// The call apps and the hint
// ---------------------------------------------------------------------------------------------

/// The call apps on Linux: a display name, the reader they use (None: a browser, read by its
/// window title) and their program names and Flatpak ids. Chromium is not in Yogurt's list; it
/// has Chrome's tree.
pub const APPS: &[(&str, Option<Kind>, &[&str])] = &[
    ("Zoom", Some(Kind::Zoom), &["zoom", "us.zoom.Zoom"]),
    ("Teams", Some(Kind::Teams), &["teams-for-linux", "com.github.IsmaelMartinez.teams_for_linux"]),
    ("Chrome", None, &["chrome", "google-chrome", "com.google.Chrome"]),
    ("Chromium", None, &["chromium", "org.chromium.Chromium"]),
    ("Brave", None, &["brave", "com.brave.Browser"]),
    ("Edge", None, &["msedge", "com.microsoft.Edge"]),
    ("Firefox", None, &["firefox", "firefox-bin", "org.mozilla.firefox"]),
];

/// Which apps to read and which kinds of call window to accept.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    /// Indexes into `APPS`.
    pub apps: Vec<usize>,
    pub zoom: bool,
    pub teams: bool,
    pub meet: bool,
}

impl Want {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn accepts(&self, kind: Kind) -> bool {
        match kind {
            Kind::Zoom => self.zoom,
            Kind::Teams => self.teams,
            Kind::Meet => self.meet,
        }
    }
}

/// What to read for a hint: a key of `calls.rs` ("zoom", "teams", "google-meet",
/// "browser:firefox") or an app name of `APPS`. No hint (or one not known): every call app, and
/// the first with a call window is read.
pub fn want_for(hint: Option<&str>) -> Want {
    let all = || Want { apps: (0..APPS.len()).collect(), zoom: true, teams: true, meet: true };
    let browsers = || (0..APPS.len()).filter(|&i| APPS[i].1.is_none());
    let Some(hint) = hint.map(|h| h.trim().to_lowercase()).filter(|h| !h.is_empty()) else { return all() };
    match hint.as_str() {
        "zoom" => return Want { apps: vec![0], zoom: true, teams: false, meet: false },
        // Teams in a browser tab is read as Teams too.
        "teams" | "microsoft teams" => {
            return Want { apps: std::iter::once(1).chain(browsers()).collect(), zoom: false, teams: true, meet: false }
        }
        "google-meet" | "google meet" | "meet" => {
            return Want { apps: browsers().collect(), zoom: false, teams: false, meet: true }
        }
        _ => {}
    }
    let name = hint.strip_prefix("browser:").unwrap_or(&hint);
    let found: Vec<usize> = (0..APPS.len())
        .filter(|&i| APPS[i].0.eq_ignore_ascii_case(name) || APPS[i].2.iter().any(|id| id.eq_ignore_ascii_case(name)))
        .collect();
    match found.first().map(|&i| APPS[i].1) {
        None if hint.starts_with("browser:") => Want { apps: browsers().collect(), zoom: false, teams: true, meet: true },
        None => all(),
        Some(None) => Want { apps: found, zoom: false, teams: true, meet: true },
        Some(Some(kind)) => Want { apps: found, zoom: kind == Kind::Zoom, teams: kind == Kind::Teams, meet: false },
    }
}

/// The `APPS` entry one of these names belongs to (a program name or a Flatpak id).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn app_of(names: &[String], want: &Want) -> Option<usize> {
    want.apps.iter().copied().find(|&i| APPS[i].2.iter().any(|id| names.iter().any(|n| n == id)))
}

/// A Flatpak app runs in its own cgroup: app-flatpak-com.example.App-<instance>.scope.
pub fn flatpak_id_from_cgroup(cgroup: &str) -> Option<String> {
    for line in cgroup.lines() {
        let Some(rest) = line.split("app-flatpak-").nth(1) else { continue };
        let id = rest.rsplit_once('-').map(|(id, _)| id).unwrap_or(rest);
        let id = id.strip_suffix(".scope").unwrap_or(id);
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

pub fn parent_pid_from_status(status: &str) -> Option<u32> {
    status.lines().find_map(|line| line.strip_prefix("PPid:").and_then(|rest| rest.trim().parse().ok()))
}

/// argv[0]'s file name: /app/teams-for-linux/teams-for-linux, /usr/lib/firefox/firefox.
pub fn program_name_of(cmdline: &[u8]) -> Option<String> {
    let first = cmdline.split(|b| *b == 0).next()?;
    let first = String::from_utf8_lossy(first);
    let base = first.rsplit('/').next().unwrap_or("");
    (!base.is_empty()).then(|| base.to_string())
}

/// The program from /proc/<pid>/exe ("/opt/google/chrome/chrome (deleted)" after an update is still "chrome").
pub fn exe_base(target: &str) -> String {
    let t = target.strip_suffix(" (deleted)").unwrap_or(target);
    t.rsplit('/').next().unwrap_or(t).to_string()
}

// ---------------------------------------------------------------------------------------------
// Who said a line
// ---------------------------------------------------------------------------------------------

/// A reading stands until the next one, but never longer than this: a stuck app never keeps naming someone.
pub const HOLD: f64 = 2.0;
/// A name needs more than this share of the line.
const MOST: f64 = 0.5;

fn norm(s: &str) -> String {
    fold(s).split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn is_me(name: &str, s: &Sample, my_names: &[String]) -> bool {
    let n = norm(name);
    s.me.as_deref().is_some_and(|m| norm(m) == n) || my_names.iter().any(|m| !m.trim().is_empty() && norm(m) == n)
}

/// Who the other side was at one reading, or None when it cannot be told.
fn who_at(s: &Sample, my_names: &[String]) -> Option<String> {
    if !s.ok {
        return None;
    }
    // Each person once (by normalized name), the spelling seen last.
    let mut people: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for p in &s.participants {
        let k = norm(p);
        match keys.iter().position(|x| *x == k) {
            Some(i) => people[i] = p.clone(),
            None => {
                keys.push(k);
                people.push(p.clone());
            }
        }
    }
    // A floating speaker tile can name someone before the roster shows them. Then the roster is
    // no proof of a call of two, so their speech is not given to the one other roster entry.
    let outside = s.speaking.iter().any(|p| !is_me(p, s, my_names) && !keys.contains(&norm(p)));
    // One other person in the call: the call's sound can only be them, whoever the app marks.
    if people.len() == 2 && !outside {
        let mine = people.iter().filter(|p| is_me(p, s, my_names)).count();
        if mine == 1 {
            return people.iter().find(|p| !is_me(p, s, my_names)).cloned();
        }
    }
    let mut others: Vec<&String> = Vec::new();
    for p in &s.speaking {
        if !is_me(p, s, my_names) && !others.contains(&p) {
            others.push(p);
        }
    }
    (others.len() == 1).then(|| others[0].clone())
}

fn sorted(samples: &[Sample]) -> Vec<&Sample> {
    let mut v: Vec<&Sample> = samples.iter().collect();
    v.sort_by(|a, b| a.at.total_cmp(&b.at));
    v
}

/// The time span one reading stands for.
fn span_of(s: &[&Sample], i: usize) -> (f64, f64) {
    let a = s[i].at;
    let next = s.get(i + 1).map_or(f64::INFINITY, |x| x.at);
    (a, next.min(a + HOLD))
}

fn name_for_sorted(start: f64, end: f64, s: &[&Sample], my_names: &[String]) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    // A line too short to measure: whoever it was the moment it started.
    if end <= start {
        for i in (0..s.len()).rev() {
            let (a, b) = span_of(s, i);
            if a <= start {
                return if start < b { who_at(s[i], my_names) } else { None };
            }
        }
        return None;
    }
    // Readings that end before the line cannot overlap it.
    let first = s.partition_point(|x| x.at + HOLD <= start);
    let mut time: Vec<(String, f64)> = Vec::new();
    for i in first..s.len() {
        let (a, b) = span_of(s, i);
        if a >= end {
            break;
        }
        let overlap = b.min(end) - a.max(start);
        if overlap <= 0.0 {
            continue;
        }
        if let Some(who) = who_at(s[i], my_names) {
            match time.iter_mut().find(|(w, _)| *w == who) {
                Some(entry) => entry.1 += overlap,
                None => time.push((who, overlap)),
            }
        }
    }
    // The first of the longest; two cannot both have more than half.
    let best = time.into_iter().fold(None, |best: Option<(String, f64)>, (w, t)| match best {
        Some((_, b)) if b >= t => best,
        _ => Some((w, t)),
    });
    best.filter(|(_, t)| *t > MOST * (end - start)).map(|(w, _)| w)
}

/// The name for a line of the other side spoken from `start` to `end` (seconds on the meeting
/// clock), or None for Them. A name only when the app marked one person (not the user) for more
/// than half of the line, or when the call has just one other person.
pub fn name_for(start: f64, end: f64, samples: &[Sample], my_names: &[String]) -> Option<String> {
    name_for_sorted(start, end, &sorted(samples), my_names)
}

/// Everyone seen in the call but the person using Lowkey, each once, in the order first seen.
pub fn others_in(samples: &[Sample], my_names: &[String]) -> Vec<String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for s in samples {
        for p in &s.participants {
            let k = norm(p);
            if !is_me(p, s, my_names) && !out.iter().any(|(x, _)| *x == k) {
                out.push((k, p.clone()));
            }
        }
    }
    // Someone marked "me" in a later reading is never one of the others.
    let me: HashSet<String> = samples.iter().filter_map(|s| s.me.as_deref().map(norm)).collect();
    out.into_iter().filter(|(k, _)| !me.contains(k)).map(|(_, v)| v).collect()
}

/// A person needs at least this much of their speech carrying one name.
pub const PERSON_MIN_SECONDS: f64 = 5.0;
/// And that name on at least this share of their named speech.
pub const PERSON_SHARE: f64 = 0.6;

/// One name a person had: its key (`norm`), its spelling, and the seconds it covered.
type Named = (String, String, f64);

/// A name for each voice that `voices.rs` found. `lines` are the "Them" lines as (start, end,
/// person), person 0 being unknown. Each line gets `name_for`; a person gets the name that
/// covers at least 60% of their named seconds and at least 5 s. When two persons win the same
/// name, only the one with more seconds keeps it (a tie: neither). Sorted by person number.
pub fn name_people(lines: &[(f64, f64, u32)], samples: &[Sample], my_names: &[String]) -> Vec<(u32, String)> {
    let s = sorted(samples);
    // Each person's names, in the order first named.
    let mut by_person: Vec<(u32, Vec<Named>)> = Vec::new();
    for &(start, end, person) in lines {
        if person == 0 || end <= start {
            continue;
        }
        let Some(name) = name_for_sorted(start, end, &s, my_names) else { continue };
        let key = norm(&name);
        let at = match by_person.iter().position(|(p, _)| *p == person) {
            Some(i) => i,
            None => {
                by_person.push((person, Vec::new()));
                by_person.len() - 1
            }
        };
        let names = &mut by_person[at].1;
        match names.iter_mut().find(|(k, _, _)| *k == key) {
            Some(e) => e.2 += end - start,
            None => names.push((key, name, end - start)),
        }
    }
    // Each person's winner, when it is clear.
    let mut winners: Vec<(u32, String, String, f64)> = Vec::new();
    for (person, names) in by_person {
        let total: f64 = names.iter().map(|n| n.2).sum();
        let mut best: Option<&Named> = None;
        for n in &names {
            if best.map_or(true, |b| n.2 > b.2) {
                best = Some(n);
            }
        }
        let Some((key, name, secs)) = best.cloned() else { continue };
        if secs >= PERSON_MIN_SECONDS && secs >= PERSON_SHARE * total {
            winners.push((person, key, name, secs));
        }
    }
    // One name, one person.
    let mut out: Vec<(u32, String)> = winners
        .iter()
        .filter(|(p, key, _, secs)| winners.iter().all(|(q, k2, _, s2)| q == p || k2 != key || s2 < secs))
        .map(|(p, _, name, _)| (*p, name.clone()))
        .collect();
    out.sort_by_key(|(p, _)| *p);
    out
}

// ---------------------------------------------------------------------------------------------
// The AT-SPI reader (Linux)
// ---------------------------------------------------------------------------------------------

/// How long one D-Bus call may wait for an app that stopped answering.
#[cfg(target_os = "linux")]
const CALL_TIMEOUT: Duration = Duration::from_millis(300);
/// One reading stops walking after this long, so an app that stopped answering cannot hold it.
#[cfg(target_os = "linux")]
const READ_DEADLINE: Duration = Duration::from_millis(1500);
/// How much of a Teams window to read before trusting it is a call (its Leave button is within ~200).
#[cfg(target_os = "linux")]
const CALL_PROBE: i32 = 1200;
/// The longest text copied from one element.
#[cfg(target_os = "linux")]
const MAX_TEXT: usize = 300;

#[cfg(target_os = "linux")]
mod atspi {
    use super::*;
    use zbus::blocking::{connection::Builder, Connection};
    use zbus::zvariant::{DynamicDeserialize, DynamicType, OwnedObjectPath, OwnedValue};

    const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
    const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
    const TEXT: &str = "org.a11y.atspi.Text";

    /// Roles that hold text worth reading. What someone types (entries) is never read.
    pub(super) fn text_role(role: &str) -> bool {
        matches!(role, "static" | "label" | "text" | "paragraph" | "heading" | "caption" | "link" | "list item" | "table cell")
    }

    /// AT-SPI has more than one word for a window.
    pub(super) fn window_role(role: &str) -> bool {
        matches!(role, "frame" | "window" | "dialog")
    }

    /// The AT-SPI role numbers (AtspiRole) of the roles the rules read; other roles stay "".
    pub(super) fn role_name(n: u32) -> &'static str {
        match n {
            2 => "alert",
            16 => "dialog",
            20 => "filler",
            23 => "frame",
            27 => "image",
            29 => "label",
            31 => "list",
            32 => "list item",
            35 => "menu item",
            37 => "page tab",
            39 => "panel",
            43 => "push button",
            49 => "scroll pane",
            54 => "status bar",
            55 => "table",
            56 => "table cell",
            61 => "text",
            62 => "toggle button",
            65 => "tree",
            66 => "tree table",
            69 => "window",
            73 => "paragraph",
            81 => "caption",
            83 => "heading",
            85 => "section",
            88 => "link",
            90 => "table row",
            91 => "tree item",
            95 => "document web",
            98 => "list box",
            101 => "notification",
            110 => "landmark",
            111 => "log",
            112 => "marquee",
            115 => "timer",
            116 => "static",
            _ => "",
        }
    }

    pub(super) fn clip(s: &str) -> String {
        let s = s.trim();
        match s.char_indices().nth(MAX_TEXT) {
            Some((i, _)) => s[..i].to_string(),
            None => s.to_string(),
        }
    }

    pub struct A11y {
        conn: Connection,
    }

    pub struct App {
        pub bus: String,
        pub path: OwnedObjectPath,
    }

    impl A11y {
        /// The accessibility bus: the session bus says where it is (org.a11y.Bus).
        pub fn connect() -> Result<Self, String> {
            let session = Builder::session()
                .map_err(|e| format!("no session bus: {e}"))?
                .method_timeout(CALL_TIMEOUT)
                .build()
                .map_err(|e| format!("no session bus: {e}"))?;
            let addr: String = session
                .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
                .map_err(|e| format!("no accessibility bus: {e}"))?
                .body()
                .deserialize()
                .map_err(|e| format!("bad accessibility address: {e}"))?;
            let conn = Builder::address(addr.as_str())
                .map_err(|e| format!("bad accessibility address: {e}"))?
                .method_timeout(CALL_TIMEOUT)
                .build()
                .map_err(|e| format!("cannot reach the accessibility bus: {e}"))?;
            Ok(Self { conn })
        }

        /// The applications on the bus now.
        pub fn apps(&self) -> Result<Vec<App>, String> {
            let children: Vec<(String, OwnedObjectPath)> = self
                .call("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root", ACCESSIBLE, "GetChildren", &())
                .map_err(|e| format!("cannot list the accessibility apps: {e}"))?;
            Ok(children.into_iter().map(|(bus, path)| App { bus, path }).collect())
        }

        /// The process behind an application's bus name.
        pub fn pid(&self, bus: &str) -> Option<u32> {
            self.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetConnectionUnixProcessID", &(bus,))
                .ok()
        }

        pub fn name(&self, bus: &str, path: &OwnedObjectPath) -> Option<String> {
            self.prop(bus, path, "Name")
        }

        /// The role name. WebKitGTK gives "" for it (measured): then the role's number says it.
        pub fn role(&self, bus: &str, path: &OwnedObjectPath) -> Option<String> {
            let name: String = self.call(bus, path.as_str(), ACCESSIBLE, "GetRoleName", &()).ok()?;
            if !name.is_empty() {
                return Some(name);
            }
            let n: u32 = self.call(bus, path.as_str(), ACCESSIBLE, "GetRole", &()).ok()?;
            Some(role_name(n).to_string())
        }

        pub fn children(&self, bus: &str, path: &OwnedObjectPath) -> Vec<(String, OwnedObjectPath)> {
            self.call(bus, path.as_str(), ACCESSIBLE, "GetChildren", &()).unwrap_or_default()
        }

        /// One element and what is under it. It stops at `deadline` and when `budget` is used up,
        /// so a big page or an app that stopped answering cannot hold the reading.
        pub fn node(&self, bus: &str, path: &OwnedObjectPath, depth: usize, max_depth: usize, budget: &mut i32, deadline: Instant) -> Node {
            let mut n = Node::default();
            if Instant::now() >= deadline {
                return n;
            }
            *budget -= 1;
            n.r = self.role(bus, path).unwrap_or_default();
            let name = self.prop(bus, path, "Name").filter(|s| !s.is_empty());
            n.d = self.prop(bus, path, "Description").filter(|s| !s.is_empty()).map(|x| clip(&x));
            n.t = name.as_deref().map(clip);
            // What someone types is never copied: only the roles of labels and static text.
            if text_role(&n.r) {
                n.v = self.text(bus, path).filter(|x| !x.is_empty() && Some(x) != name.as_ref()).map(|x| clip(&x));
            }
            let attrs = self.attrs(bus, path);
            n.i = attrs.get("id").filter(|s| !s.is_empty()).cloned();
            if let Some(x) = attrs.get("class") {
                n.c = x.split_whitespace().take(20).map(str::to_string).collect();
            }
            if depth < max_depth && *budget > 0 && Instant::now() < deadline {
                for (b, p) in self.children(bus, path) {
                    if *budget <= 0 || Instant::now() >= deadline {
                        break;
                    }
                    n.k.push(self.node(&b, &p, depth + 1, max_depth, budget, deadline));
                }
            }
            n
        }

        /// One property at a time: Qt's AT-SPI adaptor (Zoom) does not implement GetAll and
        /// crashes when asked (Yogurt found this).
        fn prop(&self, bus: &str, path: &OwnedObjectPath, key: &str) -> Option<String> {
            let v: OwnedValue = self.call(bus, path.as_str(), PROPERTIES, "Get", &(ACCESSIBLE, key)).ok()?;
            v.downcast_ref::<&str>().ok().map(str::to_string)
        }

        fn attrs(&self, bus: &str, path: &OwnedObjectPath) -> HashMap<String, String> {
            self.call(bus, path.as_str(), ACCESSIBLE, "GetAttributes", &()).unwrap_or_default()
        }

        fn text(&self, bus: &str, path: &OwnedObjectPath) -> Option<String> {
            self.call(bus, path.as_str(), TEXT, "GetText", &(0i32, -1i32)).ok()
        }

        fn call<B, R>(&self, dest: &str, path: &str, iface: &str, method: &str, body: &B) -> Result<R, String>
        where
            B: serde::ser::Serialize + DynamicType,
            R: for<'de> DynamicDeserialize<'de>,
        {
            let reply = self.conn.call_method(Some(dest), path, Some(iface), method, body).map_err(|e| e.to_string())?;
            reply.body().deserialize().map_err(|e| e.to_string())
        }
    }

    /// Every name a process goes by: its Flatpak id, argv[0], its program file, its short name.
    pub fn names_of(pid: u32) -> Vec<String> {
        let dir = format!("/proc/{pid}");
        let mut names = Vec::new();
        if let Some(id) = std::fs::read_to_string(format!("{dir}/cgroup")).ok().and_then(|c| flatpak_id_from_cgroup(&c)) {
            names.push(id);
        }
        if let Some(argv0) = std::fs::read(format!("{dir}/cmdline")).ok().and_then(|c| program_name_of(&c)) {
            names.push(argv0);
        }
        if let Ok(target) = std::fs::read_link(format!("{dir}/exe")) {
            names.push(exe_base(&target.to_string_lossy()));
        }
        if let Ok(comm) = std::fs::read_to_string(format!("{dir}/comm")) {
            names.push(comm.trim_end().to_string());
        }
        names
    }

    /// The call app a process is. A sandboxed app's tree can come from its xdg-dbus-proxy,
    /// whose name is not the app's: then one level up is the app's own process.
    pub fn app_of_pid(pid: u32, want: &Want) -> Option<usize> {
        app_of(&names_of(pid), want).or_else(|| {
            let parent = parent_pid_from_status(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)?;
            app_of(&names_of(parent), want)
        })
    }
}

/// A connection to the accessibility bus, kept between readings: a reading every 400 ms must
/// not pay for a D-Bus handshake each time.
pub struct Reader {
    #[cfg(target_os = "linux")]
    a11y: atspi::A11y,
}

impl Reader {
    #[cfg(target_os = "linux")]
    pub fn connect() -> Result<Self, String> {
        Ok(Reader { a11y: atspi::A11y::connect()? })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn connect() -> Result<Self, String> {
        Err("names from the call app are read on Linux only".into())
    }

    /// One reading: the first wanted app with a call window, its name (of `APPS`) and what it
    /// shows. Ok(None): no such app or window now. Err: the bus failed (connect again).
    #[cfg(target_os = "linux")]
    pub fn read(&self, want: &Want) -> Result<Option<(String, SampleData)>, String> {
        let deadline = Instant::now() + READ_DEADLINE;
        let me = std::process::id();
        for app in self.a11y.apps()? {
            if Instant::now() >= deadline {
                break;
            }
            let Some(pid) = self.a11y.pid(&app.bus) else { continue };
            // Lowkey's own window is on the bus too; it is never the call.
            if pid == me {
                continue;
            }
            let Some(at) = atspi::app_of_pid(pid, want) else { continue };
            let (kind, windows) = self.windows(&app, APPS[at].1, want, deadline);
            if let Some(kind) = kind.filter(|_| !windows.is_empty()) {
                return Ok(Some((APPS[at].0.to_string(), parse(kind, &windows))));
            }
        }
        Ok(None)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn read(&self, _want: &Want) -> Result<Option<(String, SampleData)>, String> {
        Err("names from the call app are read on Linux only".into())
    }

    /// The call windows of one app (at most 4), and the kind to read them as.
    #[cfg(target_os = "linux")]
    fn windows(&self, app: &atspi::App, app_kind: Option<Kind>, want: &Want, deadline: Instant) -> (Option<Kind>, Vec<Window>) {
        let zoom = app_kind == Some(Kind::Zoom);
        let mut budget: i32 = if zoom { 1500 } else { 4000 };
        let max_depth = if zoom { 14 } else { 40 };
        let mut kind: Option<Kind> = None;
        let mut out = Vec::new();
        for (bus, path) in self.a11y.children(&app.bus, &app.path) {
            if out.len() == 4 || budget <= 0 || Instant::now() >= deadline {
                break;
            }
            let Some(role) = self.a11y.role(&bus, &path) else { continue };
            if !atspi::window_role(&role) {
                continue;
            }
            let title = self.a11y.name(&bus, &path).unwrap_or_default();
            let this = match app_kind {
                Some(k) => Some(k).filter(|&k| call_window(k, &title)),
                None => browser_window_kind(&title),
            };
            // One app's windows are read with one reader: the first call window decides it.
            let Some(this) = this.filter(|&k| want.accepts(k) && kind.map_or(true, |x| x == k)) else { continue };
            // Teams keeps its calendar and chat in windows shaped like the call's: read a little
            // of the tree first, and keep it only when the call controls are there.
            let tree = if this == Kind::Teams {
                let mut probe = CALL_PROBE;
                let first = self.a11y.node(&bus, &path, 0, max_depth, &mut probe, deadline);
                if !is_call_tree(&first) {
                    continue;
                }
                if probe > 0 {
                    first
                } else {
                    let mut more = 4000;
                    self.a11y.node(&bus, &path, 0, max_depth, &mut more, deadline)
                }
            } else {
                self.a11y.node(&bus, &path, 0, max_depth, &mut budget, deadline)
            };
            kind = Some(this);
            out.push(Window { title: Some(title), tree });
        }
        (kind, out)
    }
}

/// One reading now, with a fresh connection: for a check or a test. The `Watcher` keeps its
/// connection instead. Ok(None): no call app with a call window.
pub fn read_call(app_hint: Option<&str>) -> Result<Option<(String, SampleData)>, String> {
    Reader::connect()?.read(&want_for(app_hint))
}

// ---------------------------------------------------------------------------------------------
// The watch while a meeting records
// ---------------------------------------------------------------------------------------------

/// How often the app is read.
pub const WATCH_EVERY: Duration = Duration::from_millis(400);
/// The apps mark the person talking about this late (Zoom: about 0.75 to 1.2 s after they start).
pub const APP_LAG: f64 = 0.75;
/// No reading for this long: the reader is stuck (Yogurt saw it once: 11 minutes of Them), so a new one starts.
pub const QUIET: Duration = Duration::from_secs(5);
/// At most this many new readers per meeting, so an app that never answers cannot start readers forever.
pub const MAX_RESTARTS: u32 = 20;
/// A bus that is not there is asked again only after this long.
const RECONNECT_EVERY: Duration = Duration::from_secs(2);

/// The meeting-clock time of a reading that started at `started`, less the app's lag.
pub fn sample_at(clock: Instant, started: Instant) -> f64 {
    let secs = match started.checked_duration_since(clock) {
        Some(d) => d.as_secs_f64(),
        None => -(clock.duration_since(started).as_secs_f64()),
    };
    secs - APP_LAG
}

type ReadFn = Box<dyn FnMut() -> Result<Option<(String, SampleData)>, String> + Send>;
type MakeReader = Arc<dyn Fn() -> ReadFn + Send + Sync>;

#[derive(Clone, Copy)]
struct Timing {
    every: Duration,
    quiet: Duration,
    max_restarts: u32,
}

struct Shared {
    clock: Instant,
    timing: Timing,
    samples: Mutex<Vec<Sample>>,
    stopped: AtomicBool,
    /// The reader that may still add samples; a stuck one that wakes up later sees it is old.
    generation: AtomicU64,
    heard: Mutex<Instant>,
    app: Mutex<Option<String>>,
    restarts: AtomicU64,
}

/// Reads the call app on its own thread while a meeting records.
pub struct Watcher {
    shared: Arc<Shared>,
    supervisor: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Starts reading. `clock` is the moment of meeting time 0; `app_hint` is the call's key
    /// (calls.rs) or app name, None to read every call app.
    pub fn start(clock: Instant, app_hint: Option<&str>) -> Watcher {
        let want = want_for(app_hint);
        let make: MakeReader = Arc::new(move || live_reader(want.clone()));
        let timing = Timing { every: WATCH_EVERY, quiet: QUIET, max_restarts: MAX_RESTARTS };
        if cfg!(target_os = "linux") {
            Self::start_with(clock, make, timing)
        } else {
            // Nothing to read on this OS: no threads.
            Watcher { shared: Arc::new(Shared::new(clock, timing)), supervisor: None }
        }
    }

    fn start_with(clock: Instant, make: MakeReader, timing: Timing) -> Watcher {
        let shared = Arc::new(Shared::new(clock, timing));
        spawn_reader(shared.clone(), make.clone(), 0);
        let sh = shared.clone();
        let supervisor = std::thread::Builder::new()
            .name("callnames-watch".into())
            .spawn(move || supervise(sh, make))
            .map_err(|e| log::warn!("call names: the watch did not start: {e}"))
            .ok();
        Watcher { shared, supervisor }
    }

    /// The readings so far (to name a line just heard, while the meeting records).
    pub fn samples(&self) -> Vec<Sample> {
        self.shared.samples.lock().clone()
    }

    /// The call app last read, by its `APPS` name.
    pub fn app(&self) -> Option<String> {
        self.shared.app.lock().clone()
    }

    /// Stops reading and gives every reading. It does not wait for a reader stuck in a D-Bus
    /// call: that one ends by itself and adds nothing more.
    pub fn stop(mut self) -> Vec<Sample> {
        let samples = self.end();
        if let Some(h) = self.supervisor.take() {
            let _ = h.join();
        }
        let named = samples.iter().filter(|s| s.ok && (!s.speaking.is_empty() || !s.participants.is_empty())).count();
        log::info!(
            "call names: {} readings, {} with people, {} new readers",
            samples.len(),
            named,
            self.shared.restarts.load(Ordering::Relaxed)
        );
        samples
    }

    fn end(&self) -> Vec<Sample> {
        let mut s = self.shared.samples.lock();
        self.shared.stopped.store(true, Ordering::SeqCst);
        std::mem::take(&mut *s)
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
    }
}

impl Shared {
    fn new(clock: Instant, timing: Timing) -> Shared {
        Shared {
            clock,
            timing,
            samples: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            heard: Mutex::new(Instant::now()),
            app: Mutex::new(None),
            restarts: AtomicU64::new(0),
        }
    }

    fn live(&self, generation: u64) -> bool {
        !self.stopped.load(Ordering::SeqCst) && self.generation.load(Ordering::SeqCst) == generation
    }
}

/// The real reader: one connection, made again after an error.
fn live_reader(want: Want) -> ReadFn {
    let mut reader: Option<Reader> = None;
    let mut next_connect = Instant::now();
    Box::new(move || {
        if reader.is_none() {
            if Instant::now() < next_connect {
                return Err("no accessibility bus".into());
            }
            next_connect = Instant::now() + RECONNECT_EVERY;
            reader = Some(Reader::connect()?);
        }
        let Some(r) = reader.as_ref() else { return Err("no accessibility bus".into()) };
        let got = r.read(&want);
        if got.is_err() {
            reader = None;
        }
        got
    })
}

fn spawn_reader(shared: Arc<Shared>, make: MakeReader, generation: u64) {
    let spawned = std::thread::Builder::new().name("callnames-reader".into()).spawn(move || {
        let mut read = make();
        let mut said: HashSet<String> = HashSet::new();
        while shared.live(generation) {
            let started = Instant::now();
            let got = read();
            let at = sample_at(shared.clock, started);
            let sample = match got {
                Ok(Some((app, data))) => {
                    let mut last = shared.app.lock();
                    if last.as_deref() != Some(app.as_str()) {
                        // Counts only: the names belong to the people in the call.
                        log::info!("call names: reading {app}: {} in the call, {} talking", data.participants.len(), data.speaking.len());
                        *last = Some(app);
                    }
                    Sample::new(at, data)
                }
                Ok(None) => {
                    if said.insert("no call window".into()) {
                        log::info!("call names: no call window to read yet");
                    }
                    Sample::unknown(at)
                }
                Err(e) => {
                    if said.insert(e.clone()) {
                        log::info!("call names: {e}; the other side stays Them for now");
                    }
                    Sample::unknown(at)
                }
            };
            {
                let mut samples = shared.samples.lock();
                if !shared.live(generation) {
                    break;
                }
                samples.push(sample);
            }
            *shared.heard.lock() = Instant::now();
            // A slow app (a big page) is read less often, so reading never takes much of the computer.
            let took = started.elapsed();
            let wait = shared.timing.every.max(took * 2).saturating_sub(took);
            let until = Instant::now() + wait;
            while shared.live(generation) && Instant::now() < until {
                std::thread::sleep((until - Instant::now()).min(Duration::from_millis(50)));
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("call names: the reader did not start: {e}");
    }
}

/// Starts a new reader when the current one has been quiet too long.
fn supervise(shared: Arc<Shared>, make: MakeReader) {
    let step = Duration::from_millis(50).min(shared.timing.quiet / 4);
    let mut restarts = 0u32;
    while !shared.stopped.load(Ordering::SeqCst) {
        std::thread::sleep(step);
        let quiet = shared.heard.lock().elapsed();
        if quiet <= shared.timing.quiet || restarts >= shared.timing.max_restarts {
            continue;
        }
        restarts += 1;
        shared.restarts.store(restarts.into(), Ordering::Relaxed);
        log::info!("call names: the reader went quiet for {} s; starting it again", quiet.as_secs());
        let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *shared.heard.lock() = Instant::now();
        spawn_reader(shared.clone(), make.clone(), generation);
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn node(r: &str) -> Node {
        Node { r: r.into(), ..Default::default() }
    }
    impl Node {
        fn t(mut self, s: &str) -> Self {
            self.t = Some(s.into());
            self
        }
        fn d(mut self, s: &str) -> Self {
            self.d = Some(s.into());
            self
        }
        fn v(mut self, s: &str) -> Self {
            self.v = Some(s.into());
            self
        }
        fn c(mut self, cs: &[&str]) -> Self {
            self.c = cs.iter().map(|x| x.to_string()).collect();
            self
        }
        fn k(mut self, kids: Vec<Node>) -> Self {
            self.k = kids;
            self
        }
    }
    fn win(tree: Node, title: Option<&str>) -> Window {
        Window { title: title.map(str::to_string), tree }
    }
    fn strs(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }
    fn data(speaking: &[&str], participants: &[&str], me: Option<&str>) -> SampleData {
        SampleData { speaking: strs(speaking), participants: strs(participants), me: me.map(str::to_string) }
    }

    #[test]
    fn every_pattern_compiles() {
        let all: Vec<&Regex> = vec![
            &OTHER_STATUS, &ZOOM_TAGS, &ZOOM_SPLIT, &PARTICIPANTS_COUNT, &LINUX_TILE, &LINUX_ACTIVE, &NOT_A_NAME, &PRESENTING,
            &A_LETTER, &NOT_IN_A_NAME, &LOWER_START, &MEET_TITLE, &TITLE_SPLIT, &TEAMS_STATUS, &TEAMS_ROLE, &TEAMS_SAYS, &TEAMS_ME,
            &TEAMS_TAGS, &TRAILING_PUNCT, &YOU_OR_ME, &TEAMS_LIST_NAME, &COMMA_SPLIT, &TEAMS_TITLE,
        ];
        for r in all.into_iter().chain(TEAMS_SPEAKING.iter()) {
            assert_ne!(r.as_str(), r"[^\s\S]", "a pattern did not compile");
        }
    }

    // ---- Zoom (callParse.test.ts, with the AT-SPI roles) ----

    fn zoom_row(text: &str) -> Node {
        node("table row").k(vec![node("table cell").k(vec![node("label").t(text), node("image").d("Computer audio unmuted")])])
    }
    fn zoom_window(tiles: &[&str], rows: &[&str]) -> Window {
        let mut kids = vec![node("panel").k(tiles.iter().map(|d| node("page tab").d(d)).collect())];
        if !rows.is_empty() {
            kids.push(node("scroll pane").k(vec![node("tree table").d("Participants list").k(rows.iter().map(|r| zoom_row(r)).collect())]));
        }
        kids.push(node("push button").d("Mute my audio"));
        win(node("frame").t("Zoom Meeting").k(kids), Some("Zoom Meeting"))
    }

    #[test]
    fn zoom_reads_a_tile_name_and_mark() {
        let t = |s: &str| zoom_tile(s);
        assert_eq!(t("Ana Ruiz, Computer audio unmuted, Video off, Active speaker"), Some(("Ana Ruiz".into(), true)));
        assert_eq!(t("Ana Ruiz, Computer audio muted, Video on"), Some(("Ana Ruiz".into(), false)));
        // a comma in a name stays; a role tag goes
        assert_eq!(t("Smith, Dana (Host), Computer audio unmuted"), Some(("Smith, Dana".into(), false)));
        assert_eq!(t("Ana Ruiz, Audio de la computadora no silenciado, Orador activo"), Some(("Ana Ruiz".into(), true)));
        assert_eq!(t("Mute my audio"), None);
        assert_eq!(t("Share screen, Alt+S"), None);
    }

    #[test]
    fn zoom_call_of_three_marked_tile_talks_and_me_from_the_list() {
        let r = parse(
            Kind::Zoom,
            &[zoom_window(
                &[
                    "Alex Morgan, Computer audio unmuted, Video on",
                    "Dana Smith, Computer audio unmuted, Video off, Active speaker",
                    "Ann Lee, Computer audio muted, Video off",
                ],
                &["Alex Morgan (me)", "Dana Smith (Host)", "Ann Lee"],
            )],
        );
        assert_eq!(r, data(&["Dana Smith"], &["Alex Morgan", "Dana Smith", "Ann Lee"], Some("Alex Morgan")));
    }

    #[test]
    fn zoom_host_and_me_is_one_tag_group() {
        let r = parse(Kind::Zoom, &[zoom_window(&["Alex Morgan, Computer audio unmuted"], &["Alex Morgan (Host, me)", "Ann Lee (Co-host)"])]);
        assert_eq!(r, data(&[], &["Alex Morgan", "Ann Lee"], Some("Alex Morgan")));
    }

    #[test]
    fn zoom_me_in_japanese_with_full_width_brackets() {
        let r = parse(Kind::Zoom, &[zoom_window(&[], &["Ana Ruiz（自分）", "Ann Lee"])]);
        assert_eq!(r, data(&[], &["Ana Ruiz", "Ann Lee"], Some("Ana Ruiz")));
    }

    #[test]
    fn zoom_list_closed_and_nobody_marked() {
        let r = parse(Kind::Zoom, &[zoom_window(&["Alex Morgan, Computer audio unmuted", "Dana Smith, Computer audio unmuted"], &[])]);
        assert_eq!(r, data(&[], &["Alex Morgan", "Dana Smith"], None));
    }

    #[test]
    fn zoom_floating_window_says_who_talks() {
        let r = parse(Kind::Zoom, &[win(node("frame").k(vec![node("label").t("Talking: Dana Smith")]), None)]);
        assert_eq!(r.speaking, strs(&["Dana Smith"]));
        let r = parse(Kind::Zoom, &[win(node("frame").k(vec![node("label").v("正在发言：李雷")]), None)]);
        assert_eq!(r.speaking, strs(&["李雷"]));
        // without the colon it is not the label
        let r = parse(Kind::Zoom, &[win(node("frame").k(vec![node("label").t("Talking points")]), None)]);
        assert!(r.speaking.is_empty());
    }

    #[test]
    fn zoom_changed_window_reads_as_nothing() {
        let tree = node("frame").k(vec![node("panel").d("Dana Smith"), node("page tab").d("Chat"), node("label").t("Dana Smith")]);
        assert_eq!(parse(Kind::Zoom, &[win(tree, None)]), SampleData::default());
    }

    #[test]
    fn zoom_reads_atspi_roles() {
        let tree = node("frame").t("Zoom Meeting").k(vec![
            node("page tab").d("Ana Ruiz, Computer audio unmuted, Active speaker"),
            node("label").t("Talking: Ann Lee"),
            node("tree table").t("Participants list").k(vec![node("table row").k(vec![node("label").t("Alex Morgan (me)")])]),
        ]);
        assert_eq!(parse(Kind::Zoom, &[win(tree, None)]), data(&["Ana Ruiz", "Ann Lee"], &["Ana Ruiz", "Alex Morgan"], Some("Alex Morgan")));
    }

    #[test]
    fn zoom_tile_words_in_its_name() {
        let tree = node("frame").t("Zoom Meeting").k(vec![node("page tab").t("Ana Ruiz, Computer audio unmuted, Active speaker"), node("page tab").t("Chat")]);
        assert_eq!(parse(Kind::Zoom, &[win(tree, None)]), data(&["Ana Ruiz"], &["Ana Ruiz"], None));
    }

    #[test]
    fn zoom_linux_tile_words() {
        assert_eq!(zoom_linux_tile("video item Ana Ruiz Active Speaker"), Some(("Ana Ruiz".into(), true)));
        assert_eq!(zoom_linux_tile("video item Ana Ruiz"), Some(("Ana Ruiz".into(), false)));
        assert_eq!(zoom_linux_tile("video item Madonna"), Some(("Madonna".into(), false)));
        assert_eq!(zoom_linux_tile("share content item"), None);
        assert_eq!(zoom_linux_tile("video item"), None);
    }

    #[test]
    fn zoom_linux_mark_trusted_only_with_three_tiles() {
        let tile = |name: &str, active: bool| node("push button").t(&format!("video item {name}{}", if active { " Active Speaker" } else { "" }));
        let meeting = |tiles: Vec<Node>| win(node("frame").t("Meeting").k(vec![node("panel").k(tiles)]), Some("Meeting"));
        assert_eq!(
            parse(Kind::Zoom, &[meeting(vec![tile("Alex Morgan", true), tile("Dana Smith", false), tile("Ann Lee", false)])]),
            data(&["Alex Morgan"], &["Alex Morgan", "Dana Smith", "Ann Lee"], None)
        );
        assert_eq!(parse(Kind::Zoom, &[meeting(vec![tile("Alex Morgan", true)])]), data(&[], &["Alex Morgan"], None));
    }

    // ---- Google Meet ----

    /// A tile: the name text seven levels down, the class marks on the tile itself.
    fn meet_tile(name: &str, classes: &[&str]) -> Node {
        let mut n = node("static").t(name);
        for i in 0..6 {
            n = if i == 3 { node("section").c(&["notranslate"]).k(vec![n]) } else { node("section").k(vec![n]) };
        }
        let mut cs = vec!["oZRSLe"];
        cs.extend_from_slice(classes);
        node("section").c(&cs).k(vec![node("section").d(&format!("More options for {name}")), n])
    }
    fn deep(n: Node, levels: usize) -> Node {
        (0..levels).fold(n, |n, _| node("section").k(vec![n]))
    }
    fn meet_window(tiles: Vec<Node>, title: &str) -> Window {
        // the meeting code and the clock are text too, never people
        let code = deep(node("section").k(vec![node("static").t("abc-defg-hij"), node("static").t("10:42")]), 6);
        win(
            node("frame").t(title).k(vec![node("panel").k(vec![node("document web").t(title).k(vec![
                node("section").k(vec![node("section").k(tiles)]),
                code,
            ])])]),
            Some(title),
        )
    }
    const MEET: &str = "Meet - abc-defg-hij";

    #[test]
    fn meet_reads_each_tile_and_the_talking_class() {
        let r = parse(Kind::Meet, &[meet_window(vec![meet_tile("You", &[]), meet_tile("Dana Smith", &["v5h6Xc"]), meet_tile("Ann Lee", &[])], MEET)]);
        assert_eq!(r, data(&["Dana Smith"], &["You", "Dana Smith", "Ann Lee"], Some("You")));
    }

    #[test]
    fn meet_renamed_class_marks_nobody_and_presenting_is_still_the_person() {
        let r = parse(Kind::Meet, &[meet_window(vec![meet_tile("You", &["zz9"]), meet_tile("Dana Smith (Presenting)", &["Abc12"])], MEET)]);
        assert_eq!(r, data(&[], &["You", "Dana Smith"], Some("You")));
        // a class found later can be given
        let r = read_meet(&[meet_window(vec![meet_tile("Dana Smith", &["Abc12"])], MEET)], &["Abc12"]);
        assert_eq!(r.speaking, strs(&["Dana Smith"]));
    }

    #[test]
    fn meet_own_words_are_never_people() {
        let ui = [
            "Ready to join?", "In 2 mins", "Looking for others in the call...", "Mon, Nov 16", "Your camera is on.",
            "Background is now blurred", "Reducing noise", "Continuous chat is turned off", "Messages will not be saved",
            "Presentation is starting...", "Select a tab to share", "Share with tab audio", "Quarterly plan - Google Docs",
            "Alex Doe has left the meeting", "No one else is in this meeting", "Returning to home screen in 60 seconds.", "Meet",
            "Reframe", "Turn off camera (⌘ + e)", "Learn more about Gemini in Meetings.", "Chat / People panel is open",
        ];
        let mut tiles = vec![meet_tile("Dana Smith", &[])];
        tiles.extend(ui.iter().map(|u| meet_tile(u, &[])));
        assert_eq!(parse(Kind::Meet, &[meet_window(tiles, MEET)]).participants, strs(&["Dana Smith"]));
    }

    #[test]
    fn meet_name_in_a_toast_or_status_area_is_not_a_tile() {
        let toast = node("status bar").k(vec![meet_tile("Ann Lee", &[])]);
        let alert = node("alert").k(vec![meet_tile("Bo Park", &[])]);
        assert_eq!(parse(Kind::Meet, &[meet_window(vec![meet_tile("Dana Smith", &[]), toast, alert], MEET)]).participants, strs(&["Dana Smith"]));
    }

    #[test]
    fn meet_real_names_of_every_shape_count() {
        let names = ["Dana Smith", "Ana María de la Cruz", "Mary-Jane O'Neil", "J. R. Smith", "Ludwig van Beethoven", "李雷", "jiwon", "Ann Lee (Presenting)"];
        let r = parse(Kind::Meet, &[meet_window(names.iter().map(|n| meet_tile(n, &[])).collect(), MEET)]);
        assert_eq!(
            r.participants,
            strs(&["Dana Smith", "Ana María de la Cruz", "Mary-Jane O'Neil", "J. R. Smith", "Ludwig van Beethoven", "李雷", "jiwon", "Ann Lee"])
        );
    }

    #[test]
    fn meet_only_windows_showing_a_meet_call() {
        let r = parse(Kind::Meet, &[meet_window(vec![meet_tile("Dana Smith", &["v5h6Xc"])], "Inbox - Gmail")]);
        assert!(r.participants.is_empty());
    }

    // ---- Teams ----

    #[test]
    fn teams_speaking_roster_and_you() {
        let tree = node("frame").k(vec![
            node("section").d("Dana Smith, speaking"),
            node("tree").d("Participants").k(vec![
                node("tree item").d("Dana Smith, unmuted, video on").k(vec![node("static").t("Dana Smith")]),
                node("tree item").d("Ann Lee, muted"),
                node("tree item").d("Alex Morgan (You)"),
            ]),
            node("push button").d("Leave"),
        ]);
        assert_eq!(
            parse(Kind::Teams, &[win(tree, Some("Weekly sync | Microsoft Teams"))]),
            data(&["Dana Smith"], &["Dana Smith", "Ann Lee", "Alex Morgan"], Some("Alex Morgan"))
        );
    }

    #[test]
    fn teams_never_takes_you_or_its_own_words() {
        let tree = node("frame").k(vec![node("static").t("You are speaking"), node("push button").d("Mic on")]);
        assert_eq!(parse(Kind::Teams, &[win(tree, None)]), SampleData::default());
    }

    #[test]
    fn teams_reads_names_as_well_as_descriptions() {
        let tree = node("frame").k(vec![
            node("section").t("Dana Smith, speaking"),
            node("tree").t("Participants").k(vec![node("tree item").k(vec![node("section").t("Ann Lee, muted")])]),
        ]);
        assert_eq!(parse(Kind::Teams, &[win(tree, None)]), data(&["Dana Smith"], &["Ann Lee"], None));
    }

    #[test]
    fn teams_chat_or_calendar_line_is_never_a_person() {
        let tree = node("frame").k(vec![
            node("list").t("Chat").k(vec![node("list item").t("Dana Smith, muted"), node("list item").t("Dana Smith (you)")]),
            node("section").t("Ann Lee, Organizer, Unmuted"),
        ]);
        assert_eq!(parse(Kind::Teams, &[win(tree, None)]), SampleData::default());
    }

    #[test]
    fn teams_people_panel_roster() {
        let tree = node("frame").t("Meeting | Microsoft Teams").k(vec![node("landmark").t("Participants").k(vec![node("tree").t("Attendees").k(vec![
            node("tree item").t("Dana Smith, Organizer, Unmuted").k(vec![node("section").t("Dana Smith, Organizer, Unmuted").k(vec![
                node("section").t("Dana Smith").k(vec![node("static").t("Dana Smith"), node("static").t("Organizer"), node("image").t("Unmuted")]),
            ])]),
        ])])]);
        assert_eq!(parse(Kind::Teams, &[win(tree, None)]), data(&[], &["Dana Smith"], None));
    }

    #[test]
    fn teams_every_roster_row_never_the_group_header() {
        let tree = node("frame").k(vec![node("tree").t("Attendees").k(vec![
            node("tree item").t("Dana Smith, Organizer, Unmuted"),
            node("tree item").t("Alex Morgan, Presenter, Muted"),
            node("tree item").t("Sam Lee, Guest, Unmuted"),
            node("tree item").t("In this meeting, 3 total"),
        ])]);
        assert_eq!(parse(Kind::Teams, &[win(tree, None)]), data(&[], &["Dana Smith", "Alex Morgan", "Sam Lee"], None));
    }

    #[test]
    fn teams_keeps_a_comma_in_a_roster_name() {
        let tree = node("frame").k(vec![node("tree").t("Attendees").k(vec![node("tree item").t("Smith, Dana, Organizer, Unmuted")])]);
        assert_eq!(parse(Kind::Teams, &[win(tree, None)]), data(&[], &["Smith, Dana"], None));
    }

    #[test]
    fn teams_roster_row_marked_you_is_me() {
        let tree = node("frame").k(vec![node("tree").t("Attendees").k(vec![
            node("tree item").t("Dana Smith (You), Organizer, Unmuted"),
            node("tree item").t("Alex Morgan, Presenter, Unmuted"),
        ])]);
        assert_eq!(parse(Kind::Teams, &[win(tree, None)]), data(&[], &["Dana Smith", "Alex Morgan"], Some("Dana Smith")));
    }

    // ---- Which windows and apps (call.rs, calls.rs, index.ts) ----

    #[test]
    fn kind_comes_from_the_id() {
        assert_eq!(kind_of("us.zoom.Zoom"), Kind::Zoom);
        assert_eq!(kind_of("zoom"), Kind::Zoom);
        assert_eq!(kind_of("teams-for-linux"), Kind::Teams);
        assert_eq!(kind_of("com.github.IsmaelMartinez.teams_for_linux"), Kind::Teams);
        assert_eq!(kind_of("com.google.Chrome"), Kind::Meet);
        assert_eq!(kind_of("firefox"), Kind::Meet);
    }

    #[test]
    fn a_browser_window_is_a_call_only_with_the_word_meet_or_teams() {
        assert!(call_window(Kind::Meet, "Meet - abc-defg-hij - Google Chrome"));
        assert!(call_window(Kind::Meet, "meet.google.com"));
        assert!(call_window(Kind::Meet, "Meet – abc-defg-hij and 1 more page - Work - Microsoft Edge"));
        assert!(!call_window(Kind::Meet, "Meeting notes - Google Chrome"));
        assert!(call_window(Kind::Zoom, "Zoom Meeting"));
        assert!(call_window(Kind::Zoom, "Meeting"));
        assert!(call_window(Kind::Zoom, "Zoom Webinar - 123"));
        assert!(!call_window(Kind::Zoom, "Zoom Workplace"));
        // Teams in a browser tab is read as Teams, not Meet
        assert_eq!(browser_window_kind("Weekly sync | Microsoft Teams - Google Chrome"), Some(Kind::Teams));
        assert_eq!(browser_window_kind("Meet - abc-defg-hij - Chromium"), Some(Kind::Meet));
        assert_eq!(browser_window_kind("Inbox - Gmail - Firefox"), None);
    }

    #[test]
    fn only_a_tree_with_call_controls_is_a_call() {
        let call = node("frame").k(vec![
            node("static").t("Calendar | Meeting with Dana Smith | Microsoft Teams"),
            node("button").t("Mute mic"),
            node("button").t("Leave"),
        ]);
        assert!(is_call_tree(&call));
        let calendar = node("frame").k(vec![
            node("static").t("Calendar | Meeting with Dana Smith | Microsoft Teams"),
            node("button").t("Go to today November 12, 2026"),
        ]);
        assert!(!is_call_tree(&calendar));
        assert!(!is_call_tree(&Node::default()));
    }

    #[test]
    fn process_names_flatpak_ids_and_parents() {
        let line = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-com.github.IsmaelMartinez.teams_for_linux-1512112203.scope";
        assert_eq!(flatpak_id_from_cgroup(line).as_deref(), Some("com.github.IsmaelMartinez.teams_for_linux"));
        assert_eq!(flatpak_id_from_cgroup("0::/user.slice/user-1000.slice/app-flatpak-com.spotify.Client-42.scope").as_deref(), Some("com.spotify.Client"));
        assert_eq!(flatpak_id_from_cgroup("0::/user.slice/user-1000.slice/session-3.scope"), None);
        assert_eq!(parent_pid_from_status("Name:\tbash\nPPid:\t1234\n"), Some(1234));
        assert_eq!(parent_pid_from_status("Name:\tbash\n"), None);
        assert_eq!(parent_pid_from_status("PPid:\tabc\n"), None);
        assert_eq!(program_name_of(b"/app/teams-for-linux/teams-for-linux\0--ozone-platform=wayland\0").as_deref(), Some("teams-for-linux"));
        assert_eq!(program_name_of(b"/usr/lib/firefox/firefox\0").as_deref(), Some("firefox"));
        assert_eq!(program_name_of(b"\0"), None);
        assert_eq!(exe_base("/opt/google/chrome/chrome (deleted)"), "chrome");
    }

    #[test]
    fn the_hint_picks_the_apps_and_kinds() {
        let names = |w: &Want| w.apps.iter().map(|&i| APPS[i].0).collect::<Vec<_>>();
        let all = want_for(None);
        assert_eq!(all.apps.len(), APPS.len());
        assert!(all.zoom && all.teams && all.meet);
        let z = want_for(Some("zoom"));
        assert_eq!(names(&z), vec!["Zoom"]);
        assert!(z.zoom && !z.teams && !z.meet);
        let t = want_for(Some("teams"));
        assert_eq!(names(&t)[0], "Teams");
        assert!(names(&t).contains(&"Firefox") && t.teams && !t.meet);
        let m = want_for(Some("google-meet"));
        assert!(!names(&m).contains(&"Zoom") && m.meet && !m.teams);
        let f = want_for(Some("browser:firefox"));
        assert_eq!(names(&f), vec!["Firefox"]);
        assert!(f.meet && f.teams && !f.zoom);
        assert_eq!(names(&want_for(Some("Chromium"))), vec!["Chromium"]);
        assert_eq!(want_for(Some("FaceTime")), all);
        assert_eq!(names(&want_for(Some("browser:vivaldi"))).len(), 5);
        // a Flatpak id or a program name is found by its app
        assert_eq!(app_of(&strs(&["bash", "org.mozilla.firefox"]), &all).map(|i| APPS[i].0), Some("Firefox"));
        assert_eq!(app_of(&strs(&["zoom"]), &want_for(Some("google-meet"))), None);
    }

    // ---- Who said a line (callAttribute.test.ts) ----

    /// A reading every 0.5 s from `from` to `to`, all the same.
    fn run(from: f64, to: f64, s: &Sample) -> Vec<Sample> {
        let n = ((to - from) / 0.5).floor() as usize;
        (0..n).map(|i| Sample { at: from + i as f64 * 0.5, ..s.clone() }).collect()
    }
    fn reading(participants: &[&str], me: Option<&str>, speaking: &[&str]) -> Sample {
        Sample { at: 0.0, speaking: strs(speaking), participants: strs(participants), me: me.map(str::to_string), ok: true }
    }
    const GROUP: &[&str] = &["Alex Morgan", "Dana Smith", "Ann Lee"];
    fn of3(speaking: &[&str]) -> Sample {
        reading(GROUP, Some("Alex Morgan"), speaking)
    }
    fn cat(parts: Vec<Vec<Sample>>) -> Vec<Sample> {
        parts.into_iter().flatten().collect()
    }
    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn names_the_person_marked_for_the_whole_line() {
        let tl = run(0.0, 10.0, &of3(&["Dana Smith"]));
        assert_eq!(name_for(2.0, 6.0, &tl, &[]), some("Dana Smith"));
    }

    #[test]
    fn takes_the_one_who_talked_most_of_the_line() {
        let tl = cat(vec![run(0.0, 3.0, &of3(&["Ann Lee"])), run(3.0, 10.0, &of3(&["Dana Smith"]))]);
        assert_eq!(name_for(2.0, 7.0, &tl, &[]), some("Dana Smith"));
    }

    #[test]
    fn them_when_nobody_has_most_of_it() {
        let tl = cat(vec![run(0.0, 4.0, &of3(&["Ann Lee"])), run(4.0, 6.0, &of3(&[])), run(6.0, 10.0, &of3(&["Dana Smith"]))]);
        assert_eq!(name_for(2.0, 8.0, &tl, &[]), None);
    }

    #[test]
    fn them_when_two_are_marked_at_once() {
        assert_eq!(name_for(1.0, 5.0, &run(0.0, 10.0, &of3(&["Ann Lee", "Dana Smith"])), &[]), None);
    }

    #[test]
    fn them_when_only_the_user_is_marked() {
        assert_eq!(name_for(1.0, 5.0, &run(0.0, 10.0, &of3(&["Alex Morgan"])), &[]), None);
        // and when they are told apart only by the name in Settings
        let tl = run(0.0, 10.0, &reading(GROUP, None, &["Alex Morgan"]));
        assert_eq!(name_for(1.0, 5.0, &tl, &strs(&["alex morgan"])), None);
    }

    #[test]
    fn them_with_no_readings_or_readings_long_before() {
        assert_eq!(name_for(1.0, 5.0, &[], &[]), None);
        // a reading counts for 2 s at most: a stuck app never keeps naming someone
        assert_eq!(name_for(4.0, 8.0, &run(0.0, 1.0, &of3(&["Dana Smith"])), &[]), None);
    }

    #[test]
    fn an_unreadable_moment_counts_as_not_knowing() {
        let tl = cat(vec![run(0.0, 2.0, &of3(&["Dana Smith"])), run(2.0, 10.0, &Sample::unknown(0.0))]);
        assert_eq!(name_for(0.0, 6.0, &tl, &[]), None);
    }

    #[test]
    fn a_call_with_one_other_person_is_theirs() {
        let pair = |speaking: &[&str]| reading(&["Alex Morgan", "Dana Smith"], Some("Alex Morgan"), speaking);
        assert_eq!(name_for(1.0, 5.0, &run(0.0, 10.0, &pair(&[])), &[]), some("Dana Smith"));
        assert_eq!(name_for(1.0, 5.0, &run(0.0, 10.0, &pair(&["Alex Morgan"])), &[]), some("Dana Smith"));
    }

    #[test]
    fn no_two_person_fallback_when_the_roster_is_stale() {
        let label = |speaking: &[&str]| {
            name_for(1.0, 5.0, &run(0.0, 10.0, &reading(&["Alex Morgan", "Dana Smith"], Some("Alex Morgan"), speaking)), &[])
        };
        assert_eq!(label(&["Dana Smith"]), some("Dana Smith"));
        assert_eq!(label(&["Ann Lee"]), some("Ann Lee"));
        assert_eq!(label(&["Alex Morgan", "Ann Lee"]), some("Ann Lee"));
        assert_eq!(label(&["Dana Smith", "Ann Lee"]), None);
        assert_eq!(label(&["Ann Lee", "Bo Park"]), None);
    }

    #[test]
    fn a_call_of_two_without_me_names_nobody() {
        let tl = run(0.0, 10.0, &reading(&["Ann Lee", "Dana Smith"], None, &[]));
        assert_eq!(name_for(1.0, 5.0, &tl, &[]), None);
        // the name in Settings tells them apart
        assert_eq!(name_for(1.0, 5.0, &tl, &strs(&["Ann  Lee"])), some("Dana Smith"));
    }

    #[test]
    fn a_very_short_line_is_named_by_the_moment_it_starts() {
        assert_eq!(name_for(3.0, 3.0, &run(0.0, 10.0, &of3(&["Ann Lee"])), &[]), some("Ann Lee"));
    }

    #[test]
    fn readings_out_of_order_are_sorted() {
        let mut tl = run(0.0, 10.0, &of3(&["Dana Smith"]));
        tl.reverse();
        assert_eq!(name_for(2.0, 6.0, &tl, &[]), some("Dana Smith"));
    }

    #[test]
    fn others_in_lists_each_other_name_once() {
        let tl = cat(vec![run(0.0, 1.0, &of3(&[])), run(1.0, 2.0, &reading(&["Alex Morgan", "Bo Park", "Dana Smith"], Some("Alex Morgan"), &["Bo Park"]))]);
        assert_eq!(others_in(&tl, &[]), strs(&["Dana Smith", "Ann Lee", "Bo Park"]));
        assert_eq!(others_in(&run(0.0, 1.0, &reading(&["Alex Morgan", "Dana Smith"], None, &[])), &strs(&["Alex Morgan"])), strs(&["Dana Smith"]));
    }

    // ---- The timeline (callNames.test.ts) ----

    #[test]
    fn readings_are_placed_on_the_meeting_clock_less_the_lag() {
        let clock = Instant::now();
        let t = sample_at(clock, clock + Duration::from_millis(2750));
        assert!((t - 2.0).abs() < 1e-9);
        // a reading from before meeting time 0 is before 0
        assert!(sample_at(clock + Duration::from_secs(1), clock) < -1.7);
    }

    #[test]
    fn a_line_gets_the_name_talking_then_with_the_lag_taken_off() {
        // Meeting time 0 at clock; Zoom reads Dana from 1 s to 5 s on the wall, then Ann.
        let clock = Instant::now();
        let mut tl = Vec::new();
        let mut ms = 1000;
        while ms < 9000 {
            let who = if ms < 5000 { "Dana Smith" } else { "Ann Lee" };
            tl.push(Sample::new(sample_at(clock, clock + Duration::from_millis(ms)), data(&[who], GROUP, Some("Alex Morgan"))));
            ms += 500;
        }
        // Zoom marks a little late: 0.25 s to 4.25 s of the meeting is Dana's
        assert_eq!(name_for(0.5, 3.0, &tl, &[]), some("Dana Smith"));
        assert_eq!(name_for(5.0, 7.0, &tl, &[]), some("Ann Lee"));
    }

    #[test]
    fn the_name_in_settings_is_never_given_to_the_other_side() {
        let tl = run(0.0, 5.0, &reading(GROUP, None, &["Alex Morgan"]));
        assert_eq!(name_for(1.0, 3.0, &tl, &strs(&["Alex Morgan"])), None);
        let pair = run(0.0, 5.0, &reading(&["Alex Morgan", "Dana Smith"], None, &[]));
        assert_eq!(name_for(1.0, 3.0, &pair, &strs(&["Alex Morgan"])), some("Dana Smith"));
    }

    // ---- Names for the voices ----

    #[test]
    fn each_voice_gets_the_name_of_most_of_its_named_speech() {
        let tl = cat(vec![run(0.0, 20.0, &of3(&["Dana Smith"])), run(20.0, 40.0, &of3(&["Ann Lee"])), run(40.0, 60.0, &of3(&[]))]);
        let lines = [(1.0, 6.0, 1), (7.0, 12.0, 1), (21.0, 28.0, 2), (41.0, 50.0, 2), (30.0, 33.0, 0)];
        assert_eq!(name_people(&lines, &tl, &[]), vec![(1, "Dana Smith".to_string()), (2, "Ann Lee".to_string())]);
    }

    #[test]
    fn a_voice_without_a_clear_name_gets_none() {
        let tl = cat(vec![run(0.0, 10.0, &of3(&["Dana Smith"])), run(10.0, 20.0, &of3(&["Ann Lee"]))]);
        // 6 s Dana, 5 s Ann: 55%, under 60%
        assert_eq!(name_people(&[(1.0, 7.0, 1), (12.0, 17.0, 1)], &tl, &[]), vec![]);
        // under 5 s of named speech
        assert_eq!(name_people(&[(1.0, 4.0, 1)], &tl, &[]), vec![]);
        // 8 s Dana, 3 s Ann (73%): Dana
        assert_eq!(name_people(&[(1.0, 9.0, 1), (12.0, 15.0, 1)], &tl, &[]), vec![(1, "Dana Smith".to_string())]);
    }

    #[test]
    fn two_voices_with_one_name_only_the_bigger_keeps_it() {
        let tl = run(0.0, 30.0, &of3(&["Dana Smith"]));
        assert_eq!(name_people(&[(1.0, 7.0, 1), (10.0, 20.0, 2)], &tl, &[]), vec![(2, "Dana Smith".to_string())]);
        // a tie: neither
        assert_eq!(name_people(&[(1.0, 7.0, 1), (10.0, 16.0, 2)], &tl, &[]), vec![]);
    }

    #[test]
    fn the_user_is_never_a_voice_of_the_other_side() {
        let tl = run(0.0, 30.0, &reading(GROUP, None, &["Alex Morgan"]));
        assert_eq!(name_people(&[(1.0, 20.0, 1)], &tl, &strs(&["Alex Morgan"])), vec![]);
    }

    // ---- The watch ----

    fn fast() -> Timing {
        Timing { every: Duration::from_millis(20), quiet: Duration::from_millis(200), max_restarts: 3 }
    }

    #[test]
    fn the_watch_collects_readings_until_stopped() {
        let make: MakeReader = Arc::new(|| -> ReadFn { Box::new(|| Ok(Some(("Zoom".to_string(), data(&["Dana Smith"], GROUP, None))))) });
        let w = Watcher::start_with(Instant::now(), make, fast());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(w.app().as_deref(), Some("Zoom"));
        let got = w.stop();
        assert!(got.len() >= 3, "{} readings", got.len());
        assert!(got.iter().all(|s| s.ok && s.speaking == strs(&["Dana Smith"])));
        assert!(got.windows(2).all(|p| p[0].at < p[1].at));
    }

    #[test]
    fn a_reader_that_goes_quiet_is_started_again() {
        use std::sync::atomic::AtomicUsize;
        let made = Arc::new(AtomicUsize::new(0));
        let m = made.clone();
        let make: MakeReader = Arc::new(move || -> ReadFn {
            // the first reader gets stuck after one reading; the next ones work
            let first = m.fetch_add(1, Ordering::SeqCst) == 0;
            let mut n = 0;
            Box::new(move || {
                n += 1;
                if first && n > 1 {
                    std::thread::sleep(Duration::from_millis(800));
                }
                Ok(None)
            })
        });
        let w = Watcher::start_with(Instant::now(), make, fast());
        std::thread::sleep(Duration::from_millis(600));
        assert!(made.load(Ordering::SeqCst) >= 2, "a new reader started");
        let got = w.stop();
        assert!(got.len() >= 3);
        assert!(got.iter().all(|s| !s.ok), "nothing read is not knowing");
    }

    #[test]
    fn restarts_stop_at_the_limit() {
        use std::sync::atomic::AtomicUsize;
        let made = Arc::new(AtomicUsize::new(0));
        let m = made.clone();
        let make: MakeReader = Arc::new(move || -> ReadFn {
            m.fetch_add(1, Ordering::SeqCst);
            Box::new(|| {
                std::thread::sleep(Duration::from_secs(2));
                Ok(None)
            })
        });
        let w = Watcher::start_with(Instant::now(), make, fast());
        std::thread::sleep(Duration::from_millis(1500));
        let _ = w.stop();
        assert_eq!(made.load(Ordering::SeqCst), 4, "the first reader and 3 restarts");
    }

    // ---- The AT-SPI reader's small rules ----

    #[cfg(target_os = "linux")]
    #[test]
    fn inputs_are_never_read_but_labels_are() {
        for role in ["static", "label", "text", "paragraph", "heading", "link"] {
            assert!(atspi::text_role(role), "{role}");
        }
        for role in ["entry text", "password text", "text field", "combo box", "document web", "push button"] {
            assert!(!atspi::text_role(role), "{role}");
        }
        for role in ["frame", "window", "dialog"] {
            assert!(atspi::window_role(role));
        }
        assert!(!atspi::window_role("panel"));
        // the numbers WebKitGTK answered with (GetRole), and their names
        assert_eq!([20, 49, 85, 95].map(atspi::role_name), ["filler", "scroll pane", "section", "document web"]);
        assert_eq!(atspi::role_name(9999), "");
        assert_eq!(atspi::clip("  hi  "), "hi");
        assert_eq!(atspi::clip(&"a".repeat(400)).chars().count(), MAX_TEXT);
    }

    /// The live session, read-only: every app on the bus by its program name, and a small walk of
    /// the first window found (role counts only, no text). `cargo test -- --ignored live --nocapture`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn live_walk_any_window() {
        let a = atspi::A11y::connect().expect("the accessibility bus");
        let apps = a.apps().expect("the apps");
        let mut walked = false;
        for app in &apps {
            let pid = a.pid(&app.bus);
            let names = pid.map(atspi::names_of).unwrap_or_default();
            eprintln!("app pid={pid:?} names={:?}", names.iter().take(2).collect::<Vec<_>>());
            if walked || pid == Some(std::process::id()) {
                continue;
            }
            for (bus, path) in a.children(&app.bus, &app.path) {
                let Some(role) = a.role(&bus, &path) else { continue };
                if !atspi::window_role(&role) {
                    continue;
                }
                let mut budget = 300;
                let started = Instant::now();
                let tree = a.node(&bus, &path, 0, 40, &mut budget, Instant::now() + Duration::from_millis(1500));
                let mut roles: HashMap<String, usize> = HashMap::new();
                let mut with_text = 0;
                walk_all(&tree, &mut |n, _| {
                    *roles.entry(n.r.clone()).or_default() += 1;
                    with_text += usize::from(texts(n).next().is_some());
                });
                eprintln!("  walked a {role}: {} nodes ({with_text} with text) in {:?}; roles {:?}", 300 - budget, started.elapsed(), roles);
                walked = true;
                break;
            }
        }
        assert!(walked || apps.is_empty());
    }

    /// The live session: read-only. `cargo test -- --ignored live --nocapture`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn live_read_call_does_not_fail_badly() {
        let started = Instant::now();
        let got = read_call(None);
        eprintln!("read_call(None) in {:?}: {:?}", started.elapsed(), got.as_ref().map(|x| x.as_ref().map(|(app, d)| (app.clone(), d.participants.len(), d.speaking.len(), d.me.is_some()))));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
