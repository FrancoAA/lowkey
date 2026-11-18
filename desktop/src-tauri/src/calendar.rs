//! Calendar links: the person pastes the private iCal address of their calendar (Google's
//! "Secret address in iCal format", Outlook's published calendar). The app reads it every few
//! minutes, straight from this computer to the calendar provider, and gets the events from
//! yesterday to a week ahead. A new meeting then gets its title and attendees from the event
//! that is on now, and the agenda shows what comes next.
//!
//! The idea and its behaviour come from Yogurt (MIT): `calendarLink/` (the address rules, the
//! errors, the iCal reading) and `detect.ts` (`matchEvent`: which event a moment belongs to).
//!
//! Recurring events are expanded in their own wall-clock time (as RFC 5545 says), and only then
//! put on the time line with the event's time zone. Thus "10:00 every Monday" stays at 10:00
//! across a daylight saving change.
//!
//! Nothing here panics on bad input: a feed that is not iCal is an error, and a part of a feed
//! that cannot be read is left out.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, TimeDelta, TimeZone};
use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

/// How long a fetched feed is used before the app asks for it again.
pub const REFRESH_SECS: i64 = 5 * 60;
/// An event is "near" when it starts at most this long before or after now.
pub const NEAR_SECS: i64 = 10 * 60;

/// A calendar is text: more than this is not a calendar a person keeps.
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// A slow provider must not hold the refresh forever.
const TIMEOUT: Duration = Duration::from_secs(20);
/// At most this many occurrences of one series are walked. A daily series runs for about 55
/// years before it reaches this. A rule that repeats each minute stops here and does not freeze
/// the app.
const MAX_STEPS: usize = 20_000;
/// An event that lasts longer than this is a broken event, not a meeting.
const MAX_LENGTH_SECS: i64 = 366 * 86_400;
const DAY_SECS: i64 = 86_400;

/// Why an address does not work. The names are the ones the UI shows text for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkError {
    /// Not a web address, or not https.
    BadUrl,
    /// No answer, an error answer, or an answer that is too big.
    Unreachable,
    /// An answer that is not iCal (for example a sign-in page).
    NotCalendar,
    /// Google's public address. It works only for a public calendar, so the person must use the
    /// secret address. Work accounts often do not have a secret address.
    PublicAddress,
}

impl LinkError {
    pub fn code(self) -> &'static str {
        match self {
            LinkError::BadUrl => "bad-url",
            LinkError::Unreachable => "unreachable",
            LinkError::NotCalendar => "not-calendar",
            LinkError::PublicAddress => "public-address",
        }
    }
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for LinkError {}

/// A person on an event. An address alone also stands for the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub name: String,
    pub email: Option<String>,
}

/// One occurrence of an event. Times are unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// The event's UID and its start: each occurrence of a series has its own id.
    pub id: String,
    /// The event's UID, the same for all occurrences of a series.
    pub uid: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    /// A whole-day event: start and end are midnight in this computer's time zone.
    pub all_day: bool,
    /// The organizer and the attendees, once each. The person the calendar belongs to is not
    /// in the list (they know they are there). Rooms are not people, so they are not in it.
    pub attendees: Vec<Person>,
    pub organizer: Option<Person>,
    /// The call link (Zoom, Meet, Teams, Webex, Jitsi, …). Other links (documents) are not one.
    pub link: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// The address

/// The address as the app keeps it: https (webcal:// is the same address), or plain http only on
/// this computer (tests). Plain http on the network would send the private address in clear.
pub fn normalize(raw: &str) -> Result<Url, LinkError> {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    let raw = ["webcal://", "webcals://"]
        .iter()
        .find(|p| lower.starts_with(*p))
        .map(|p| format!("https://{}", &raw[p.len()..]))
        .unwrap_or_else(|| raw.to_string());
    let url = Url::parse(&raw).map_err(|_| LinkError::BadUrl)?;
    if allowed(&url) {
        Ok(url)
    } else {
        Err(LinkError::BadUrl)
    }
}

fn allowed(url: &Url) -> bool {
    match url.scheme() {
        "https" => url.host_str().is_some_and(|h| !h.is_empty()),
        "http" => matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
        _ => false,
    }
}

/// Google's public address (…/public/basic.ics) answers only for a public calendar.
fn public_google(url: &Url) -> bool {
    if url.host_str() != Some("calendar.google.com") {
        return false;
    }
    let path = url.path().to_ascii_lowercase();
    let mut parts = path.rsplit('/');
    let file = parts.next().unwrap_or("");
    parts.next() == Some("public") && file.ends_with(".ics")
}

/// The one request a calendar link makes: the person's own address, from this computer to the
/// calendar provider. No other server is in between. At most 10 MB and 20 seconds.
///
/// This blocks: call it from a thread of its own, not from async code.
pub fn fetch(url: &Url) -> Result<String, LinkError> {
    if !allowed(url) {
        return Err(LinkError::BadUrl);
    }
    // A redirect must not take the private address to plain http.
    let policy = reqwest::redirect::Policy::custom(|a| {
        if a.previous().len() >= 10 {
            a.error("too many redirects")
        } else if allowed(a.url()) {
            a.follow()
        } else {
            a.stop()
        }
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .redirect(policy)
        .user_agent("Lowkey")
        .build()
        .map_err(|_| LinkError::Unreachable)?;
    let res = client
        .get(url.clone())
        .header("accept", "text/calendar, */*")
        .send()
        .map_err(|_| LinkError::Unreachable)?;
    if !res.status().is_success() {
        return Err(LinkError::Unreachable);
    }
    if res.content_length().is_some_and(|n| n > MAX_BYTES) {
        return Err(LinkError::Unreachable);
    }
    // The length in the header can be missing or wrong: the read itself stops after the limit.
    let mut body = Vec::new();
    res.take(MAX_BYTES + 1).read_to_end(&mut body).map_err(|_| LinkError::Unreachable)?;
    if body.len() as u64 > MAX_BYTES {
        return Err(LinkError::Unreachable);
    }
    let text = String::from_utf8_lossy(&body);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// A calendar the person connected: its address and the name to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    pub url: Url,
    /// The feed's own name (X-WR-CALNAME), else the provider's host.
    pub name: String,
}

/// Connect a pasted address: make sure that it is a calendar, and get its name. The error says
/// why it does not work.
pub fn connect(raw: &str) -> Result<Linked, LinkError> {
    connect_with(raw, fetch)
}

fn connect_with(raw: &str, get: impl FnOnce(&Url) -> Result<String, LinkError>) -> Result<Linked, LinkError> {
    let url = normalize(raw)?;
    let public = public_google(&url);
    let text = get(&url).map_err(|_| if public { LinkError::PublicAddress } else { LinkError::Unreachable })?;
    if !is_calendar(&text) {
        return Err(if public { LinkError::PublicAddress } else { LinkError::NotCalendar });
    }
    let name = calendar_name(&text).unwrap_or_else(|| url.host_str().unwrap_or_default().to_string());
    Ok(Linked { url, name })
}

// ---------------------------------------------------------------------------------------------
// The feed, kept

/// The events that the app reads: from the start of yesterday to the end of the 7th day ahead,
/// in this computer's time zone.
pub fn window(now: i64) -> (i64, i64) {
    let today = Zone::System.wall(now).date();
    let day = |n: i64| {
        today
            .checked_add_signed(TimeDelta::days(n))
            .map(|d| Zone::System.instant(d.and_hms_opt(0, 0, 0).unwrap_or_default()))
    };
    (day(-1).unwrap_or(now - DAY_SECS), day(8).unwrap_or(now + 8 * DAY_SECS))
}

/// One connected calendar and its last good events. When a refresh fails (no network, the
/// provider is down), the last good events stay: the agenda does not go empty.
#[derive(Debug, Clone)]
pub struct Feed {
    pub url: Url,
    pub name: Option<String>,
    pub events: Vec<Event>,
    /// When the events were last read (unix seconds). None: never.
    pub fetched_at: Option<i64>,
}

impl Feed {
    pub fn new(url: Url) -> Self {
        Feed { url, name: None, events: Vec::new(), fetched_at: None }
    }

    /// True when the events are older than five minutes (or the clock went back).
    pub fn due(&self, now: i64) -> bool {
        self.fetched_at.map_or(true, |t| now - t >= REFRESH_SECS || now < t)
    }

    /// Fetch and read the feed again. This blocks (see `fetch`).
    pub fn refresh(&mut self, now: i64) -> Result<(), LinkError> {
        self.refresh_with(now, fetch)
    }

    fn refresh_with(&mut self, now: i64, get: impl FnOnce(&Url) -> Result<String, LinkError>) -> Result<(), LinkError> {
        let text = get(&self.url)?;
        let (from, to) = window(now);
        self.events = parse(&text, from, to)?;
        self.name = calendar_name(&text);
        self.fetched_at = Some(now);
        Ok(())
    }

    /// The event this moment belongs to (see `near`).
    pub fn near(&self, now: i64) -> Option<&Event> {
        near(&self.events, now)
    }
}

// ---------------------------------------------------------------------------------------------
// Which event now

/// The event this moment belongs to: it starts at most 10 minutes before or after now, and it is
/// not over. One with a call link comes first (a call is the likely meeting), then the one that
/// starts closest to now. Whole-day events are not meetings.
pub fn near(events: &[Event], now: i64) -> Option<&Event> {
    events
        .iter()
        .filter(|e| !e.all_day && (e.start - now).abs() <= NEAR_SECS && e.end > now)
        .min_by_key(|e| (e.link.is_none(), (e.start - now).abs()))
}

// ---------------------------------------------------------------------------------------------
// Reading iCal

/// True when the text is an iCal feed, not (for example) a sign-in page.
pub fn is_calendar(ics: &str) -> bool {
    ics.as_bytes().windows(15).any(|w| w.eq_ignore_ascii_case(b"BEGIN:VCALENDAR"))
}

/// The feed's own name (X-WR-CALNAME). Google names a person's calendar after their address.
pub fn calendar_name(ics: &str) -> Option<String> {
    tree(ics).iter().find(|c| c.name == "VCALENDAR").and_then(|c| c.text("X-WR-CALNAME"))
}

/// The events that overlap [from, to), sorted by start. Cancelled events are not in it. An
/// error only when the text is not iCal.
pub fn parse(ics: &str, from: i64, to: i64) -> Result<Vec<Event>, LinkError> {
    parse_in(ics, from, to, Zone::System)
}

fn parse_in(ics: &str, from: i64, to: i64, local: Zone<'static>) -> Result<Vec<Event>, LinkError> {
    if !is_calendar(ics) {
        return Err(LinkError::NotCalendar);
    }
    let roots = tree(ics);
    let calendars: Vec<&Comp> = roots.iter().filter(|c| c.name == "VCALENDAR").collect();
    if calendars.is_empty() {
        return Err(LinkError::NotCalendar);
    }
    let mut out = Vec::new();
    for cal in calendars {
        let zones: HashMap<String, Vtz> = cal
            .subs
            .iter()
            .filter(|c| c.name == "VTIMEZONE")
            .filter_map(|c| Some((c.text("TZID")?, Vtz::read(c))))
            .collect();
        let base = Ctx { zones: &zones, floating: local, day: local };
        // A time without a zone is in the calendar's own zone (Google gives it), else in this
        // computer's zone.
        let floating = cal.text("X-WR-TIMEZONE").map(|z| base.zone(Some(&z))).unwrap_or(local);
        let ctx = Ctx { floating, ..base };
        let me = cal.text("X-WR-CALNAME").map(|n| n.to_lowercase()).filter(|n| is_address(n));
        ctx.events(cal, me.as_deref(), from, to, &mut out);
    }
    out.sort_by(|a, b| (a.start, a.end, &a.title).cmp(&(b.start, b.end, &b.title)));
    // The same event can come twice (two calendars in one feed, a feed that repeats it): once
    // is enough, and the agenda needs ids that are not the same.
    let mut seen = HashSet::new();
    out.retain(|e| seen.insert((e.uid.clone(), e.start, e.end)));
    Ok(out)
}

fn is_address(s: &str) -> bool {
    let mut parts = s.split('@');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    !a.is_empty() && !b.is_empty() && !s.chars().any(char::is_whitespace)
}

// --- content lines and components ---

struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

struct Comp {
    name: String,
    props: Vec<Prop>,
    subs: Vec<Comp>,
}

impl Comp {
    fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }

    fn props<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s Prop> + 's {
        self.props.iter().filter(move |p| p.name == name)
    }

    /// A text value, unescaped and trimmed; None when it is empty.
    fn text(&self, name: &str) -> Option<String> {
        let v = unescape(&self.prop(name)?.value);
        let v = v.trim();
        (!v.is_empty()).then(|| v.to_string())
    }
}

/// Components deeper than this are not in a real calendar. The limit keeps the tree small.
const MAX_DEPTH: usize = 16;

/// The text as a tree of components. Lines that cannot be read are left out, and a component
/// that is not closed is closed at the end.
fn tree(text: &str) -> Vec<Comp> {
    let mut roots = Vec::new();
    let mut stack: Vec<Comp> = Vec::new();
    // Components opened past MAX_DEPTH: their contents are skipped, but their END lines must
    // still pair up.
    let mut skipped = 0usize;
    for line in unfold(text) {
        let Some(p) = content_line(&line) else { continue };
        if p.name == "BEGIN" {
            if skipped > 0 || stack.len() >= MAX_DEPTH {
                skipped += 1;
            } else {
                stack.push(Comp { name: p.value.trim().to_ascii_uppercase(), props: Vec::new(), subs: Vec::new() });
            }
        } else if p.name == "END" {
            if skipped > 0 {
                skipped -= 1;
            } else if let Some(done) = stack.pop() {
                match stack.last_mut() {
                    Some(parent) => parent.subs.push(done),
                    None => roots.push(done),
                }
            }
        } else if skipped == 0 {
            if let Some(c) = stack.last_mut() {
                c.props.push(p);
            }
        }
    }
    while let Some(done) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.subs.push(done),
            None => roots.push(done),
        }
    }
    roots
}

/// The logical lines: a line that starts with a space or a tab continues the line before it.
fn unfold(text: &str) -> Vec<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match raw.as_bytes().first() {
            Some(b' ' | b'\t') if !out.is_empty() => {
                if let Some(last) = out.last_mut() {
                    last.push_str(&raw[1..]);
                }
            }
            _ => out.push(raw.to_string()),
        }
    }
    out
}

/// NAME;PARAM=VALUE;…:VALUE. A ':' or ';' inside double quotes is part of a parameter.
fn content_line(line: &str) -> Option<Prop> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
        })
        .collect();
    Some(Prop { name, params, value: value.to_string() })
}

fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == sep && !in_quotes {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&s[start..]);
    out
}

/// TEXT values: \n is a new line, and \, \; \\ are the characters themselves.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

// --- time zones ---

/// Where a wall-clock time is.
#[derive(Clone, Copy)]
enum Zone<'a> {
    Tz(chrono_tz::Tz),
    /// A zone the feed defines itself (Outlook's "Pacific Standard Time", "Customized Time Zone").
    Custom(&'a Vtz),
    /// This computer's zone.
    System,
}

impl Zone<'_> {
    /// The instant of a wall-clock time. A time that occurs twice (the clock goes back) is the
    /// first one. A time that does not occur (the clock goes forward) is one hour later, as
    /// calendar apps show it.
    fn instant(&self, wall: NaiveDateTime) -> i64 {
        fn pick<T: TimeZone>(tz: &T, wall: NaiveDateTime) -> Option<i64> {
            match tz.from_local_datetime(&wall) {
                LocalResult::Single(d) | LocalResult::Ambiguous(d, _) => Some(d.timestamp()),
                LocalResult::None => {
                    let later = wall.checked_add_signed(TimeDelta::hours(1))?;
                    tz.from_local_datetime(&later).earliest().map(|d| d.timestamp())
                }
            }
        }
        let utc = wall.and_utc().timestamp();
        match self {
            Zone::Tz(tz) => pick(tz, wall).unwrap_or(utc),
            Zone::System => pick(&Local, wall).unwrap_or(utc),
            Zone::Custom(v) => utc - i64::from(v.offset_at(wall)),
        }
    }

    /// The wall-clock time of an instant.
    fn wall(&self, t: i64) -> NaiveDateTime {
        let Some(utc) = DateTime::from_timestamp(t, 0) else { return NaiveDateTime::default() };
        match self {
            Zone::Tz(tz) => utc.with_timezone(tz).naive_local(),
            Zone::System => utc.with_timezone(&Local).naive_local(),
            Zone::Custom(v) => {
                let u = utc.naive_utc();
                let shift = |w: NaiveDateTime| u.checked_add_signed(TimeDelta::seconds(i64::from(v.offset_at(w))));
                let guess = shift(u).unwrap_or(u);
                shift(guess).unwrap_or(u)
            }
        }
    }
}

/// A VTIMEZONE: the times its offset changes, worked out once.
struct Vtz {
    /// (wall-clock time of the change, the offset after it in seconds), sorted.
    changes: Vec<(NaiveDateTime, i32)>,
    /// The offset before the first change.
    first: i32,
}

impl Vtz {
    /// Changes are worked out to this year. Outlook starts its rules in 1601, so a yearly rule
    /// gives about 500 changes.
    const LAST_YEAR: i32 = 2100;

    fn read(c: &Comp) -> Vtz {
        let mut changes = Vec::new();
        let mut earliest: Option<(NaiveDateTime, i32)> = None;
        for o in c.subs.iter().filter(|s| s.name == "STANDARD" || s.name == "DAYLIGHT") {
            let (Some(start), Some(to)) = (
                o.prop("DTSTART").and_then(|p| wall_value(&p.value)),
                o.prop("TZOFFSETTO").and_then(|p| offset(&p.value)),
            ) else {
                continue;
            };
            let from = o.prop("TZOFFSETFROM").and_then(|p| offset(&p.value)).unwrap_or(to);
            if earliest.map_or(true, |(t, _)| start < t) {
                earliest = Some((start, from));
            }
            changes.push((start, to));
            for p in o.props("RDATE") {
                changes.extend(p.value.split(',').filter_map(wall_value).map(|w| (w, to)));
            }
            for p in o.props("RRULE") {
                let end = NaiveDate::from_ymd_opt(Self::LAST_YEAR, 12, 31).and_then(|d| d.and_hms_opt(0, 0, 0));
                let Some(end) = end else { continue };
                if let Some(starts) = rule_starts(&p.value, start, &Zone::Tz(chrono_tz::UTC), end, 2_000) {
                    changes.extend(starts.into_iter().map(|w| (w, to)));
                }
            }
        }
        changes.sort();
        changes.dedup_by_key(|c| c.0);
        Vtz { changes, first: earliest.map(|e| e.1).unwrap_or(0) }
    }

    fn offset_at(&self, wall: NaiveDateTime) -> i32 {
        match self.changes.partition_point(|(t, _)| *t <= wall) {
            0 => self.first,
            i => self.changes[i - 1].1,
        }
    }
}

/// "+0100", "-0800", "+053000": seconds east of UTC.
fn offset(v: &str) -> Option<i32> {
    let v = v.trim();
    let (sign, digits) = match v.as_bytes().first()? {
        b'+' => (1, &v[1..]),
        b'-' => (-1, &v[1..]),
        _ => (1, v),
    };
    if !(digits.len() == 4 || digits.len() == 6) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| digits.get(r).and_then(|s| s.parse::<i32>().ok());
    let secs = n(0..2)? * 3600 + n(2..4)? * 60 + if digits.len() == 6 { n(4..6)? } else { 0 };
    Some(sign * secs)
}

// --- dates and times ---

/// A DTSTART-like value put on its zone's wall clock.
#[derive(Clone, Copy)]
struct At<'a> {
    wall: NaiveDateTime,
    zone: Zone<'a>,
    /// A date without a time (a whole-day event).
    day: bool,
}

impl At<'_> {
    fn instant(&self) -> i64 {
        self.zone.instant(self.wall)
    }
}

/// The time zones a calendar can use.
struct Ctx<'a> {
    zones: &'a HashMap<String, Vtz>,
    /// For a time without a zone.
    floating: Zone<'a>,
    /// For a date without a time: midnight here.
    day: Zone<'a>,
}

impl<'a> Ctx<'a> {
    /// A real (IANA) zone name first: it knows the full history. Then the feed's own VTIMEZONE.
    /// An unknown name is read as a time without a zone.
    fn zone(&self, tzid: Option<&str>) -> Zone<'a> {
        let Some(id) = tzid.map(|t| t.trim().trim_matches('"')).filter(|t| !t.is_empty()) else {
            return self.floating;
        };
        if let Some(tz) = iana(id) {
            return Zone::Tz(tz);
        }
        match self.zones.get(id) {
            Some(v) => Zone::Custom(v),
            None => self.floating,
        }
    }

    fn at(&self, p: &Prop) -> Option<At<'a>> {
        let date = p.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
        self.at_value(&p.value, p.param("TZID"), date)
    }

    fn at_value(&self, v: &str, tzid: Option<&str>, date: bool) -> Option<At<'a>> {
        let v = v.trim();
        // A period (RDATE;VALUE=PERIOD) starts at its first part.
        let v = v.split('/').next().unwrap_or(v);
        if date || v.len() == 8 {
            let d = NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d").ok()?;
            return Some(At { wall: d.and_hms_opt(0, 0, 0)?, zone: self.day, day: true });
        }
        if let Some(utc) = v.strip_suffix('Z').or_else(|| v.strip_suffix('z')) {
            return Some(At { wall: wall_value(utc)?, zone: Zone::Tz(chrono_tz::UTC), day: false });
        }
        Some(At { wall: wall_value(v)?, zone: self.zone(tzid), day: false })
    }
}

/// "America/Los_Angeles", also with a prefix ("/mozilla.org/20050126_1/America/New_York").
fn iana(id: &str) -> Option<chrono_tz::Tz> {
    if let Ok(tz) = chrono_tz::Tz::from_str(id) {
        return Some(tz);
    }
    id.match_indices('/').find_map(|(i, _)| chrono_tz::Tz::from_str(&id[i + 1..]).ok())
}

/// "20261102T100000" (a trailing Z is the caller's to remove).
fn wall_value(v: &str) -> Option<NaiveDateTime> {
    let v = v.trim();
    let v = v.strip_suffix('Z').unwrap_or(v);
    if v.len() != 15 || !v.is_char_boundary(8) {
        return None;
    }
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()
}

/// "PT30M", "P1D", "P1W", "-PT15M": seconds.
fn duration(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, v) = match v.as_bytes().first()? {
        b'-' => (-1, &v[1..]),
        b'+' => (1, &v[1..]),
        _ => (1, v),
    };
    let v = v.strip_prefix('P').or_else(|| v.strip_prefix('p'))?;
    let mut total: i64 = 0;
    let mut n: i64 = 0;
    let mut digits = false;
    for c in v.chars() {
        if let Some(d) = c.to_digit(10) {
            n = n.checked_mul(10)?.checked_add(i64::from(d))?;
            digits = true;
            continue;
        }
        let unit = match c.to_ascii_uppercase() {
            'T' => continue,
            'W' => 7 * DAY_SECS,
            'D' => DAY_SECS,
            'H' => 3600,
            'M' => 60,
            'S' => 1,
            _ => return None,
        };
        if !digits {
            return None;
        }
        total = total.checked_add(n.checked_mul(unit)?)?;
        n = 0;
        digits = false;
    }
    Some(sign * total)
}

// --- recurrence ---

/// The starts of a recurrence rule, in wall-clock time, up to `until_wall`. None when the rule
/// cannot be read: then the event is only its first occurrence.
fn rule_starts(rule: &str, start: NaiveDateTime, zone: &Zone, until_wall: NaiveDateTime, max: usize) -> Option<Vec<NaiveDateTime>> {
    // The rule is worked out on wall-clock times (as if they were UTC). UNTIL is an instant
    // when it ends in Z: put it on the same wall clock first.
    let mut until = None;
    let mut parts = Vec::new();
    for part in rule.trim().split(';').filter(|p| !p.trim().is_empty()) {
        let Some((k, v)) = part.split_once('=') else { continue };
        if k.trim().eq_ignore_ascii_case("UNTIL") {
            let v = v.trim();
            until = if v.len() == 8 {
                NaiveDate::parse_from_str(v, "%Y%m%d").ok().and_then(|d| d.and_hms_opt(23, 59, 59))
            } else if let Some(u) = v.strip_suffix('Z').or_else(|| v.strip_suffix('z')) {
                wall_value(u).map(|w| zone.wall(w.and_utc().timestamp()))
            } else {
                wall_value(v)
            };
            until?;
        } else {
            parts.push(part.trim());
        }
    }
    let text = parts.join(";");
    // The rrule crate is third-party code: a panic in it must not take down the app.
    std::panic::catch_unwind(|| {
        let utc = rrule::Tz::UTC;
        let mut r: rrule::RRule<rrule::Unvalidated> = text.parse().ok()?;
        if let Some(u) = until {
            r = r.until(utc.from_utc_datetime(&u));
        }
        let set = r.build(utc.from_utc_datetime(&start)).ok()?.limit();
        let mut out = Vec::new();
        for d in set.into_iter().take(max) {
            let w = d.naive_utc();
            if w > until_wall {
                break;
            }
            out.push(w);
        }
        Some(out)
    })
    .ok()
    .flatten()
}

// --- events ---

/// What every occurrence of one VEVENT shares.
struct Shared {
    uid: String,
    title: String,
    attendees: Vec<Person>,
    organizer: Option<Person>,
    link: Option<String>,
}

impl<'a> Ctx<'a> {
    fn events(&self, cal: &Comp, me: Option<&str>, from: i64, to: i64, out: &mut Vec<Event>) {
        let vevents: Vec<&Comp> = cal.subs.iter().filter(|c| c.name == "VEVENT").collect();
        // A moved or changed occurrence (RECURRENCE-ID) replaces the one its series gives.
        let mut moved: HashMap<String, HashSet<i64>> = HashMap::new();
        for v in &vevents {
            if let (Some(uid), Some(rid)) = (v.text("UID"), v.prop("RECURRENCE-ID").and_then(|p| self.at(p))) {
                moved.entry(uid).or_default().insert(rid.instant());
            }
        }
        let none = HashSet::new();
        for v in vevents {
            // Without a UID an occurrence cannot be told apart from the others.
            let Some(uid) = v.text("UID") else { continue };
            if v.text("STATUS").is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED")) {
                continue;
            }
            let Some(start) = v.prop("DTSTART").and_then(|p| self.at(p)) else { continue };
            let length = self.length(v, &start);
            let shared = shared(v, uid, me);
            let mut emit = |s: i64, e: i64| {
                if s < to && e > from {
                    out.push(Event {
                        id: format!("{}|{}", shared.uid, s),
                        uid: shared.uid.clone(),
                        title: shared.title.clone(),
                        start: s,
                        end: e,
                        all_day: start.day,
                        attendees: shared.attendees.clone(),
                        organizer: shared.organizer.clone(),
                        link: shared.link.clone(),
                    });
                }
            };
            let recurring = v.prop("RRULE").is_some() || v.prop("RDATE").is_some();
            if v.prop("RECURRENCE-ID").is_some() || !recurring {
                let (s, e) = occurrence(&start, start.wall, length);
                emit(s, e);
                continue;
            }
            let moved = moved.get(&shared.uid).unwrap_or(&none);
            self.expand(v, &start, length, from, to, moved, &mut emit);
        }
    }

    /// How long one occurrence lasts, in seconds of wall-clock time: DTEND, else DURATION, else
    /// one day for a whole-day event and nothing for a timed one (RFC 5545).
    fn length(&self, v: &Comp, start: &At) -> i64 {
        let secs = if let Some(end) = v.prop("DTEND").and_then(|p| self.at(p)) {
            // An end in another zone: as the start's wall clock sees it.
            let end_wall = if end.day == start.day && same_zone(&end.zone, &start.zone) {
                end.wall
            } else {
                start.zone.wall(end.instant())
            };
            (end_wall - start.wall).num_seconds()
        } else if let Some(d) = v.prop("DURATION").and_then(|p| duration(&p.value)) {
            d
        } else if start.day {
            DAY_SECS
        } else {
            0
        };
        secs.clamp(0, MAX_LENGTH_SECS)
    }

    /// The occurrences of a series that overlap [from, to).
    #[allow(clippy::too_many_arguments)]
    fn expand(
        &self,
        v: &Comp,
        start: &At<'a>,
        length: i64,
        from: i64,
        to: i64,
        moved: &HashSet<i64>,
        emit: &mut impl FnMut(i64, i64),
    ) {
        let mut skip: HashSet<i64> = moved.clone();
        let mut skip_days: HashSet<NaiveDate> = HashSet::new();
        for p in v.props("EXDATE") {
            let date = p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE"));
            for value in p.value.split(',') {
                let Some(at) = self.at_value(value, p.param("TZID"), date) else { continue };
                // A date alone takes out the whole day, also for a timed series.
                if at.day {
                    skip_days.insert(at.wall.date());
                }
                skip.insert(at.instant());
            }
        }
        // The window on the series' wall clock, with a day to spare for the zone and the
        // occurrence's length.
        let last = start
            .zone
            .wall(to)
            .checked_add_signed(TimeDelta::days(1))
            .unwrap_or(NaiveDateTime::MAX);
        // Occurrences that end before this are not looked at more closely: the zone work for
        // each one is the slow part of a series that has run for years.
        let first = start
            .zone
            .wall(from)
            .checked_sub_signed(TimeDelta::seconds(length + DAY_SECS))
            .unwrap_or(NaiveDateTime::MIN);
        let mut walls = vec![start.wall];
        for p in v.props("RRULE") {
            if let Some(starts) = rule_starts(&p.value, start.wall, &start.zone, last, MAX_STEPS) {
                walls.extend(starts);
            }
        }
        for p in v.props("RDATE") {
            let date = p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE"));
            for value in p.value.split(',') {
                if let Some(at) = self.at_value(value, p.param("TZID"), date) {
                    // As the series' wall clock sees it, so it lasts as long as the others.
                    walls.push(if same_zone(&at.zone, &start.zone) { at.wall } else { start.zone.wall(at.instant()) });
                }
            }
        }
        let mut seen = HashSet::new();
        for wall in walls {
            if wall < first || wall > last || skip_days.contains(&wall.date()) {
                continue;
            }
            let (s, e) = occurrence(start, wall, length);
            if e <= from || s >= to || skip.contains(&s) || !seen.insert(s) {
                continue;
            }
            emit(s, e);
        }
    }
}

/// The start and end instants of one occurrence that starts at `wall`.
fn occurrence(start: &At, wall: NaiveDateTime, length: i64) -> (i64, i64) {
    let s = start.zone.instant(wall);
    let e = wall
        .checked_add_signed(TimeDelta::seconds(length))
        .map(|w| start.zone.instant(w))
        .unwrap_or(s);
    (s, e.max(s))
}

fn same_zone(a: &Zone, b: &Zone) -> bool {
    match (a, b) {
        (Zone::Tz(x), Zone::Tz(y)) => x == y,
        (Zone::Custom(x), Zone::Custom(y)) => std::ptr::eq(*x, *y),
        (Zone::System, Zone::System) => true,
        _ => false,
    }
}

fn shared(v: &Comp, uid: String, me: Option<&str>) -> Shared {
    Shared {
        uid,
        title: v.text("SUMMARY").unwrap_or_default(),
        attendees: people(v, me),
        organizer: v.prop("ORGANIZER").and_then(person),
        link: ["URL", "X-GOOGLE-CONFERENCE", "LOCATION", "DESCRIPTION"]
            .iter()
            .find_map(|name| v.props(name).find_map(|p| call_link(&unescape(&p.value)))),
    }
}

/// The organizer and the attendees, once each, without the calendar's owner.
fn people(v: &Comp, me: Option<&str>) -> Vec<Person> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in v.props("ORGANIZER").chain(v.props("ATTENDEE")) {
        let Some(person) = person(p) else { continue };
        if me.is_some() && person.email.as_deref() == me {
            continue;
        }
        let key = person.email.clone().unwrap_or_else(|| person.name.to_lowercase());
        if seen.insert(key) {
            out.push(person);
        }
    }
    out
}

fn person(p: &Prop) -> Option<Person> {
    // A room or a projector is on the invite, but it is not a person.
    if p.param("CUTYPE").is_some_and(|t| t.eq_ignore_ascii_case("ROOM") || t.eq_ignore_ascii_case("RESOURCE")) {
        return None;
    }
    let value = p.value.trim();
    let address = value
        .get(..7)
        .filter(|s| s.eq_ignore_ascii_case("mailto:"))
        .map_or(value, |_| &value[7..])
        .trim()
        .to_lowercase();
    let email = is_address(&address).then_some(address);
    // Google gives a shared calendar or a room its own address: it is not a person.
    if email.as_deref().is_some_and(|e| {
        e.ends_with("@group.calendar.google.com")
            || e.ends_with("@resource.calendar.google.com")
            || e == "unknownorganizer@calendar.google.com"
    }) {
        return None;
    }
    let name = p.param("CN").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    let name = name.or_else(|| email.clone())?;
    Some(Person { name, email })
}

/// Links that start a call. Other links in an invite (documents, agendas) are not one.
fn call_link(text: &str) -> Option<String> {
    static LINK: OnceLock<Option<Regex>> = OnceLock::new();
    let re = LINK
        .get_or_init(|| {
            Regex::new(
                r#"(?i)https?://(?:[\w-]+\.)*(?:zoom\.us|zoomgov\.com|meet\.google\.com|teams\.microsoft\.com|teams\.live\.com|webex\.com|whereby\.com|facetime\.apple\.com|discord\.gg|around\.co|gotomeeting\.com|meet\.jit\.si|8x8\.vc)/[^\s<>"')\]]*"#,
            )
            .ok()
        })
        .as_ref()?;
    let m = re.find(text)?.as_str();
    // The end of a sentence is not part of the link.
    Some(m.trim_end_matches(['.', ',', ';', ':', '!', '?']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A made-up feed in the shape Google's "Secret address in iCal format" gives: its own time
    // zone, attendees, a Meet link (from Yogurt's tests).
    fn feed(events: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nX-WR-CALNAME:alex@example.com\r\n\
BEGIN:VTIMEZONE\r\nTZID:America/Los_Angeles\r\nBEGIN:STANDARD\r\nDTSTART:19701101T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\n\
TZOFFSETFROM:-0700\r\nTZOFFSETTO:-0800\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:19700308T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\n\
TZOFFSETFROM:-0800\r\nTZOFFSETTO:-0700\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n{events}\r\nEND:VCALENDAR\r\n"
        )
    }

    const SYNC: &str = "BEGIN:VEVENT
UID:sync-1@example.com
SUMMARY:Weekly sync
DTSTART;TZID=America/Los_Angeles:20261102T100000
DTEND;TZID=America/Los_Angeles:20261102T103000
RRULE:FREQ=WEEKLY;BYDAY=MO
EXDATE;TZID=America/Los_Angeles:20261109T100000
ORGANIZER;CN=Dana Smith:mailto:dana@example.com
ATTENDEE;CN=Dana Smith;PARTSTAT=ACCEPTED:mailto:dana@example.com
ATTENDEE;CN=Alex Morgan;PARTSTAT=ACCEPTED:mailto:alex@example.com
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bo@example.com
DESCRIPTION:Join: https://meet.google.com/abc-defg-hij
END:VEVENT
BEGIN:VEVENT
UID:sync-1@example.com
RECURRENCE-ID;TZID=America/Los_Angeles:20261116T100000
SUMMARY:Weekly sync (moved)
DTSTART;TZID=America/Los_Angeles:20261116T140000
DTEND;TZID=America/Los_Angeles:20261116T143000
ATTENDEE;CN=Dana Smith:mailto:dana@example.com
END:VEVENT
BEGIN:VEVENT
UID:lunch@example.com
SUMMARY:Lunch
DTSTART:20261117T200000Z
DTEND:20261117T210000Z
LOCATION:Cafe
END:VEVENT
BEGIN:VEVENT
UID:off@example.com
SUMMARY:Day off
DTSTART;VALUE=DATE:20261118
DTEND;VALUE=DATE:20261119
END:VEVENT
BEGIN:VEVENT
UID:gone@example.com
SUMMARY:Cancelled review
STATUS:CANCELLED
DTSTART:20261117T180000Z
DTEND:20261117T183000Z
END:VEVENT";

    // Daily since 2019, one occurrence moved (Yogurt #79: a work calendar with years of
    // recurring meetings).
    const YEARS: &str = "BEGIN:VEVENT
UID:standup@example.com
SUMMARY:Standup
DTSTART:20190107T170000Z
DTEND:20190107T171500Z
RRULE:FREQ=DAILY
END:VEVENT
BEGIN:VEVENT
UID:standup@example.com
RECURRENCE-ID:20261116T170000Z
SUMMARY:Standup (moved)
DTSTART:20261117T190000Z
DTEND:20261117T191500Z
END:VEVENT";

    fn at(iso: &str) -> i64 {
        DateTime::parse_from_rfc3339(iso).map(|d| d.timestamp()).unwrap_or_else(|e| panic!("{iso}: {e}"))
    }

    /// Tests read whole days in UTC, so they do not depend on this computer's zone.
    fn events(ics: &str, from: &str, to: &str) -> Vec<Event> {
        parse_in(ics, at(from), at(to), Zone::Tz(chrono_tz::UTC)).expect("a calendar")
    }

    fn starts(got: &[Event]) -> Vec<(String, i64)> {
        got.iter().map(|e| (e.title.clone(), e.start)).collect()
    }

    // --- ported from Yogurt: calendarLinkIcs.test.ts ---

    #[test]
    fn names_the_calendar_after_the_feed() {
        assert_eq!(calendar_name(&feed(SYNC)).as_deref(), Some("alex@example.com"));
        assert_eq!(calendar_name("BEGIN:VCALENDAR\nEND:VCALENDAR"), None);
    }

    #[test]
    fn expands_a_weekly_meeting_in_its_own_zone_skips_an_excluded_week_and_uses_a_moved_one() {
        let got = events(&feed(SYNC), "2026-11-02T00:00:00Z", "2026-11-24T00:00:00Z");
        let syncs: Vec<&Event> = got.iter().filter(|e| e.uid == "sync-1@example.com").collect();
        // Nov 2 and 23 at 10:00 PST (18:00Z); Nov 9 excluded; Nov 16 moved to 14:00 PST (22:00Z).
        assert_eq!(
            syncs.iter().map(|e| (e.title.as_str(), e.start)).collect::<Vec<_>>(),
            vec![
                ("Weekly sync", at("2026-11-02T18:00:00Z")),
                ("Weekly sync (moved)", at("2026-11-16T22:00:00Z")),
                ("Weekly sync", at("2026-11-23T18:00:00Z")),
            ]
        );
        // Each occurrence has its own id, so the agenda can tell them apart.
        let ids: HashSet<&str> = syncs.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn gives_attendees_by_name_and_email_and_the_call_link() {
        let got = events(&feed(SYNC), "2026-11-02T00:00:00Z", "2026-11-03T00:00:00Z");
        let first = &got[0];
        assert_eq!(first.title, "Weekly sync");
        assert_eq!(first.end, at("2026-11-02T18:30:00Z"));
        assert_eq!(first.link.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        // The person the feed belongs to (alex) is not in the list; the organizer is there once.
        assert_eq!(
            first.attendees,
            vec![
                Person { name: "Dana Smith".into(), email: Some("dana@example.com".into()) },
                Person { name: "bo@example.com".into(), email: Some("bo@example.com".into()) },
            ]
        );
        assert_eq!(first.organizer, Some(Person { name: "Dana Smith".into(), email: Some("dana@example.com".into()) }));
    }

    #[test]
    fn keeps_one_off_and_all_day_events_and_leaves_out_cancelled_ones() {
        // Yogurt leaves whole-day events out; here they are in, marked, for the agenda.
        let got = events(&feed(SYNC), "2026-11-17T00:00:00Z", "2026-11-19T00:00:00Z");
        assert_eq!(starts(&got), vec![("Lunch".into(), at("2026-11-17T20:00:00Z")), ("Day off".into(), at("2026-11-18T00:00:00Z"))]);
        assert!(!got[0].all_day);
        assert!(got[1].all_day);
        assert_eq!(got[1].end, at("2026-11-19T00:00:00Z"));
    }

    #[test]
    fn the_same_event_twice_is_kept_once() {
        let one = "BEGIN:VEVENT\nUID:twice@x\nSUMMARY:Twice\nDTSTART:20261117T150000Z\nDTEND:20261117T160000Z\nEND:VEVENT";
        let ics = format!("BEGIN:VCALENDAR\n{one}\n{one}\nEND:VCALENDAR\nBEGIN:VCALENDAR\n{one}\nEND:VCALENDAR");
        assert_eq!(events(&ics, "2026-11-17T00:00:00Z", "2026-11-18T00:00:00Z").len(), 1);
    }

    #[test]
    fn a_feed_that_is_not_a_calendar_is_an_error_not_an_empty_week() {
        assert_eq!(parse("<html>Sign in</html>", 0, 1), Err(LinkError::NotCalendar));
        assert_eq!(parse("", 0, 1), Err(LinkError::NotCalendar));
    }

    #[test]
    fn a_series_that_has_run_for_years_still_shows_and_a_moved_one_comes_into_the_window() {
        let t = std::time::Instant::now();
        let got = events(&feed(YEARS), "2026-11-17T00:00:00Z", "2026-11-18T00:00:00Z");
        assert_eq!(
            starts(&got),
            vec![("Standup".into(), at("2026-11-17T17:00:00Z")), ("Standup (moved)".into(), at("2026-11-17T19:00:00Z"))]
        );
        // The moved occurrence's own day: the series does not also give it.
        let got = events(&feed(YEARS), "2026-11-16T00:00:00Z", "2026-11-17T00:00:00Z");
        assert!(got.is_empty(), "{got:?}");
        assert!(t.elapsed() < Duration::from_secs(2), "slow: {:?}", t.elapsed());
    }

    #[test]
    fn many_events_are_read_quickly() {
        let many: String = (0..3000)
            .map(|i| {
                format!(
                    "BEGIN:VEVENT\nUID:one-{i}@example.com\nSUMMARY:One {i}\nDTSTART:2026111{d}T10{m:02}00Z\nDTEND:2026111{d}T11{m:02}00Z\nEND:VEVENT\n",
                    d = i % 10,
                    m = i % 60
                )
            })
            .collect();
        let t = std::time::Instant::now();
        let got = events(&feed(&format!("{YEARS}\n{many}")), "2026-11-17T00:00:00Z", "2026-11-18T00:00:00Z");
        assert_eq!(got.len(), 300 + 2);
        assert!(t.elapsed() < Duration::from_secs(2), "slow: {:?}", t.elapsed());
        // Sorted by start.
        assert!(got.windows(2).all(|w| w[0].start <= w[1].start));
    }

    // --- recurrence and time zones ---

    #[test]
    fn a_weekly_meeting_keeps_its_wall_clock_time_across_daylight_saving() {
        let ics = feed(
            "BEGIN:VEVENT\nUID:w@x\nSUMMARY:W\nDTSTART;TZID=America/Los_Angeles:20261019T100000\nDTEND;TZID=America/Los_Angeles:20261019T110000\nRRULE:FREQ=WEEKLY\nEND:VEVENT",
        );
        let got = events(&ics, "2026-10-19T00:00:00Z", "2026-11-10T00:00:00Z");
        // PDT (UTC-7) until Nov 1, then PST (UTC-8).
        assert_eq!(
            got.iter().map(|e| (e.start, e.end - e.start)).collect::<Vec<_>>(),
            vec![
                (at("2026-10-19T17:00:00Z"), 3600),
                (at("2026-10-26T17:00:00Z"), 3600),
                (at("2026-11-02T18:00:00Z"), 3600),
                (at("2026-11-09T18:00:00Z"), 3600),
            ]
        );
    }

    #[test]
    fn until_in_utc_and_count_end_a_series() {
        let ics = feed(
            "BEGIN:VEVENT\nUID:u@x\nSUMMARY:U\nDTSTART;TZID=America/Los_Angeles:20261102T100000\nDTEND;TZID=America/Los_Angeles:20261102T103000\nRRULE:FREQ=DAILY;UNTIL=20261104T180000Z\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:c@x\nSUMMARY:C\nDTSTART:20261102T090000Z\nDURATION:PT45M\nRRULE:FREQ=WEEKLY;COUNT=2\nEND:VEVENT",
        );
        let got = events(&ics, "2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z");
        // UNTIL is 10:00 PST on Nov 4: that day is the last one.
        let u: Vec<i64> = got.iter().filter(|e| e.title == "U").map(|e| e.start).collect();
        assert_eq!(u, vec![at("2026-11-02T18:00:00Z"), at("2026-11-03T18:00:00Z"), at("2026-11-04T18:00:00Z")]);
        let c: Vec<(i64, i64)> = got.iter().filter(|e| e.title == "C").map(|e| (e.start, e.end)).collect();
        assert_eq!(c, vec![(at("2026-11-02T09:00:00Z"), at("2026-11-02T09:45:00Z")), (at("2026-11-09T09:00:00Z"), at("2026-11-09T09:45:00Z"))]);
    }

    #[test]
    fn outlook_zone_names_use_the_feeds_own_vtimezone() {
        let ics = "BEGIN:VCALENDAR\nVERSION:2.0\nX-WR-CALNAME:Calendar\n\
BEGIN:VTIMEZONE\nTZID:Pacific Standard Time\nBEGIN:STANDARD\nDTSTART:16010101T020000\nTZOFFSETFROM:-0700\nTZOFFSETTO:-0800\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\nEND:STANDARD\n\
BEGIN:DAYLIGHT\nDTSTART:16010101T020000\nTZOFFSETFROM:-0800\nTZOFFSETTO:-0700\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\nEND:DAYLIGHT\nEND:VTIMEZONE\n\
BEGIN:VEVENT\nUID:o@x\nSUMMARY:Review\nDTSTART;TZID=Pacific Standard Time:20261026T100000\nDTEND;TZID=Pacific Standard Time:20261026T110000\nRRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=2\nEND:VEVENT\nEND:VCALENDAR";
        let got = events(ics, "2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z");
        assert_eq!(got.iter().map(|e| e.start).collect::<Vec<_>>(), vec![at("2026-10-26T17:00:00Z"), at("2026-11-02T18:00:00Z")]);
    }

    #[test]
    fn a_zone_name_with_a_prefix_and_a_floating_time_in_the_calendars_zone() {
        let ics = "BEGIN:VCALENDAR\nX-WR-TIMEZONE:Europe/Berlin\n\
BEGIN:VEVENT\nUID:m@x\nSUMMARY:Mozilla\nDTSTART;TZID=/mozilla.org/20050126_1/America/New_York:20261117T090000\nDTEND;TZID=/mozilla.org/20050126_1/America/New_York:20261117T100000\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:f@x\nSUMMARY:Floating\nDTSTART:20261117T090000\nDTEND:20261117T100000\nEND:VEVENT\nEND:VCALENDAR";
        let got = events(ics, "2026-11-17T00:00:00Z", "2026-11-18T00:00:00Z");
        assert_eq!(starts(&got), vec![("Floating".into(), at("2026-11-17T08:00:00Z")), ("Mozilla".into(), at("2026-11-17T14:00:00Z"))]);
    }

    #[test]
    fn a_whole_day_series_with_an_excluded_day() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:d@x\nSUMMARY:Gym\nDTSTART;VALUE=DATE:20261116\nRRULE:FREQ=DAILY;COUNT=4\nEXDATE;VALUE=DATE:20261117\nEND:VEVENT\nEND:VCALENDAR";
        let got = events(ics, "2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z");
        assert_eq!(
            got.iter().map(|e| (e.start, e.end, e.all_day)).collect::<Vec<_>>(),
            vec![
                (at("2026-11-16T00:00:00Z"), at("2026-11-17T00:00:00Z"), true),
                (at("2026-11-18T00:00:00Z"), at("2026-11-19T00:00:00Z"), true),
                (at("2026-11-19T00:00:00Z"), at("2026-11-20T00:00:00Z"), true),
            ]
        );
    }

    #[test]
    fn a_cancelled_occurrence_takes_one_day_out_of_a_series_and_rdate_adds_one() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:s@x\nSUMMARY:S\nDTSTART:20261116T150000Z\nDTEND:20261116T153000Z\nRRULE:FREQ=DAILY;COUNT=3\nRDATE:20261120T120000Z\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:s@x\nRECURRENCE-ID:20261117T150000Z\nSTATUS:CANCELLED\nSUMMARY:S\nDTSTART:20261117T150000Z\nDTEND:20261117T153000Z\nEND:VEVENT\nEND:VCALENDAR";
        let got = events(ics, "2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z");
        assert_eq!(
            got.iter().map(|e| (e.start, e.end)).collect::<Vec<_>>(),
            vec![
                (at("2026-11-16T15:00:00Z"), at("2026-11-16T15:30:00Z")),
                (at("2026-11-18T15:00:00Z"), at("2026-11-18T15:30:00Z")),
                (at("2026-11-20T12:00:00Z"), at("2026-11-20T12:30:00Z")),
            ]
        );
    }

    #[test]
    fn a_rule_that_cannot_be_read_gives_the_first_occurrence() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:b@x\nSUMMARY:B\nDTSTART:20261117T150000Z\nDTEND:20261117T153000Z\nRRULE:FREQ=SOMETIMES;BYDAY=XX\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:n@x\nSUMMARY:Never\nDTSTART:20261117T160000Z\nRRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30\nEND:VEVENT\n\
BEGIN:VEVENT\nUID:e@x\nSUMMARY:Ended\nDTSTART:20261117T170000Z\nRRULE:FREQ=DAILY;UNTIL=20200101T000000Z\nEND:VEVENT\nEND:VCALENDAR";
        let t = std::time::Instant::now();
        let got = events(ics, "2026-11-17T00:00:00Z", "2026-11-25T00:00:00Z");
        assert_eq!(
            starts(&got),
            vec![("B".into(), at("2026-11-17T15:00:00Z")), ("Never".into(), at("2026-11-17T16:00:00Z")), ("Ended".into(), at("2026-11-17T17:00:00Z"))]
        );
        assert!(t.elapsed() < Duration::from_secs(5), "slow: {:?}", t.elapsed());
    }

    #[test]
    fn a_rule_that_repeats_each_minute_stops_and_does_not_freeze() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:m@x\nSUMMARY:Spam\nDTSTART:20000101T000000Z\nRRULE:FREQ=MINUTELY\nEND:VEVENT\nEND:VCALENDAR";
        let t = std::time::Instant::now();
        let got = events(ics, "2026-11-17T00:00:00Z", "2026-11-18T00:00:00Z");
        assert!(got.is_empty());
        assert!(t.elapsed() < Duration::from_secs(5), "slow: {:?}", t.elapsed());
    }

    // --- reading the text ---

    #[test]
    fn folded_lines_escapes_quoted_names_rooms_and_calendar_addresses() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:t@x\r\nSUMMARY:Plan\\, budget \r\n and\\; more\r\nDTSTART:20261117T150000Z\r\nDTEND:20261117T160000Z\r\n\
ORGANIZER;CN=Team:mailto:abc123@group.calendar.google.com\r\n\
ATTENDEE;CN=\"Smith, Dana\";ROLE=REQ-PARTICIPANT:MAILTO:Dana@Example.com\r\n\
ATTENDEE;CUTYPE=ROOM;CN=Room 4:mailto:room4@example.com\r\n\
ATTENDEE;CN=Bo:mailto:\r\n\
LOCATION:https://example.zoom.us/j/123?pwd=abc.\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let got = events(ics, "2026-11-17T00:00:00Z", "2026-11-18T00:00:00Z");
        let e = &got[0];
        assert_eq!(e.title, "Plan, budget and; more");
        assert_eq!(e.organizer, None);
        assert_eq!(
            e.attendees,
            vec![Person { name: "Smith, Dana".into(), email: Some("dana@example.com".into()) }, Person { name: "Bo".into(), email: None }]
        );
        assert_eq!(e.link.as_deref(), Some("https://example.zoom.us/j/123?pwd=abc"));
    }

    #[test]
    fn finds_the_call_link_and_ignores_other_links() {
        assert_eq!(call_link("Join: https://meet.google.com/abc-defg-hij thanks").as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(call_link("Notes at https://docs.example.com/x"), None);
        assert_eq!(call_link("<https://teams.microsoft.com/l/meetup-join/19%3a>").as_deref(), Some("https://teams.microsoft.com/l/meetup-join/19%3a"));
        assert_eq!(call_link("https://acme.my.webex.com/meet/dana").as_deref(), Some("https://acme.my.webex.com/meet/dana"));
        assert_eq!(call_link("(https://meet.jit.si/Standup)").as_deref(), Some("https://meet.jit.si/Standup"));
        assert_eq!(call_link("https://notzoom.us/j/1"), None);
        assert_eq!(call_link("https://zoom.us.example.com/j/1"), None);

        // URL first, then Google's conference field, then the place, then the text.
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:l@x\nDTSTART:20261117T150000Z\nURL:https://example.com/agenda\nX-GOOGLE-CONFERENCE:https://meet.google.com/xyz\nDESCRIPTION:https://zoom.us/j/1\nEND:VEVENT\nEND:VCALENDAR";
        let got = events(ics, "2026-11-17T00:00:00Z", "2026-11-18T00:00:00Z");
        assert_eq!(got[0].link.as_deref(), Some("https://meet.google.com/xyz"));
        // An event without DTEND lasts no time; it still shows.
        assert_eq!(got[0].end, got[0].start);
    }

    #[test]
    fn bad_input_never_panics() {
        let inputs = [
            "BEGIN:VCALENDAR",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nDTSTART:2026\nEND:VEVENT",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nDTSTART;TZID=\"Nowhere:20261117T150000\nDURATION:P99999999999999999999W\nEND:VEVENT",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nDTSTART:99991231T235959Z\nDURATION:P52W\nRRULE:FREQ=DAILY;UNTIL=9\nEND:VEVENT\nEND:VCALENDAR",
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nDTSTART:20261117T150000Z\nEXDATE:garbage,,\nRDATE;VALUE=PERIOD:20261118T150000Z/PT1H\nEND:VEVENT\nEND:VCALENDAR",
            "END:VEVENT\nEND:VCALENDAR\nBEGIN:VCALENDAR\n:\n;;;\nBEGIN:\n\u{feff}é:ü\n\tcontinued",
            "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:Bad\nBEGIN:STANDARD\nDTSTART:x\nTZOFFSETTO:+99\nEND:STANDARD\nEND:VTIMEZONE\nBEGIN:VEVENT\nUID:x\nDTSTART;TZID=Bad:20261117T150000\nEND:VEVENT\nEND:VCALENDAR",
        ];
        for ics in inputs {
            let _ = parse_in(ics, 0, i64::MAX / 2, Zone::Tz(chrono_tz::UTC));
            let _ = parse_in(ics, i64::MIN / 2, i64::MAX / 2, Zone::System);
            let _ = calendar_name(ics);
        }
        let deep = "BEGIN:VCALENDAR\n".to_string() + &"BEGIN:X\n".repeat(100_000) + "END:VCALENDAR";
        let _ = parse_in(&deep, 0, 1, Zone::System);
    }

    // --- the address (Yogurt: calendarLinks.test.ts) ---

    #[test]
    fn normalizes_an_address() {
        let n = |s: &str| normalize(s).map(|u| u.to_string());
        assert_eq!(
            n("  https://calendar.google.com/calendar/ical/alex%40example.com/private-abc123/basic.ics "),
            Ok("https://calendar.google.com/calendar/ical/alex%40example.com/private-abc123/basic.ics".into())
        );
        assert_eq!(
            n("webcal://outlook.office365.com/owa/calendar/abc/reachcalendar.ics"),
            Ok("https://outlook.office365.com/owa/calendar/abc/reachcalendar.ics".into())
        );
        assert_eq!(n("WEBCALS://example.com/a.ics"), Ok("https://example.com/a.ics".into()));
        assert_eq!(n("not a link"), Err(LinkError::BadUrl));
        assert_eq!(n("ftp://example.com/cal.ics"), Err(LinkError::BadUrl));
        assert_eq!(n("http://example.com/cal.ics"), Err(LinkError::BadUrl));
        assert_eq!(n("file:///etc/passwd"), Err(LinkError::BadUrl));
        assert!(n("http://127.0.0.1:8080/cal.ics").is_ok());
        assert!(n("http://localhost/cal.ics").is_ok());
        assert!(n("http://[::1]/cal.ics").is_ok());
    }

    fn ics(name: &str) -> String {
        format!("BEGIN:VCALENDAR\nVERSION:2.0\nX-WR-CALNAME:{name}\nBEGIN:VEVENT\nUID:a@example.com\nSUMMARY:Standup\nDTSTART:20261117T170000Z\nDTEND:20261117T171500Z\nEND:VEVENT\nEND:VCALENDAR\n")
    }

    #[test]
    fn connects_an_address_and_names_it() {
        let got = connect_with("https://calendar.google.com/calendar/ical/alex%40example.com/private-abc123/basic.ics", |_| Ok(ics("alex@example.com")));
        assert_eq!(got.map(|l| l.name), Ok("alex@example.com".into()));
        let mut asked = String::new();
        let got = connect_with("webcal://outlook.office365.com/owa/calendar/abc/reachcalendar.ics", |u| {
            asked = u.to_string();
            Ok("BEGIN:VCALENDAR\nEND:VCALENDAR".into())
        });
        assert_eq!(asked, "https://outlook.office365.com/owa/calendar/abc/reachcalendar.ics");
        // No name in the feed: the provider's host.
        assert_eq!(got.map(|l| l.name), Ok("outlook.office365.com".into()));
    }

    #[test]
    fn says_why_an_address_does_not_work() {
        let fail = |_: &Url| Err(LinkError::Unreachable);
        assert_eq!(connect_with("not a link", |_| Ok(String::new())), Err(LinkError::BadUrl));
        assert_eq!(connect_with("ftp://example.com/cal.ics", |_| Ok(String::new())), Err(LinkError::BadUrl));
        assert_eq!(connect_with("https://example.com/cal.ics", fail), Err(LinkError::Unreachable));
        assert_eq!(connect_with("https://example.com/cal.ics", |_| Ok("<html>Sign in</html>".into())), Err(LinkError::NotCalendar));
    }

    #[test]
    fn googles_public_address_says_so_when_it_fails() {
        let public = "https://calendar.google.com/calendar/ical/alex%40example.com/public/basic.ics";
        assert_eq!(connect_with(public, |_| Err(LinkError::Unreachable)), Err(LinkError::PublicAddress));
        assert_eq!(connect_with(public, |_| Ok("<html>Not found</html>".into())), Err(LinkError::PublicAddress));
        assert_eq!(connect_with(public, |_| Ok(ics("Team"))).map(|l| l.name), Ok("Team".into()));
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_events() {
        let now = at("2026-11-17T16:55:00Z");
        let mut f = Feed::new(normalize("https://example.com/cal.ics").unwrap_or_else(|e| panic!("{e}")));
        assert!(f.due(now));
        assert_eq!(f.refresh_with(now, |_| Ok(ics("Work"))), Ok(()));
        assert_eq!(f.events.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), vec!["Standup"]);
        assert_eq!(f.name.as_deref(), Some("Work"));
        assert_eq!(f.near(now).map(|e| e.title.as_str()), Some("Standup"));
        assert!(!f.due(now + 60));
        assert!(f.due(now + REFRESH_SECS));
        assert_eq!(f.refresh_with(now + REFRESH_SECS, |_| Err(LinkError::Unreachable)), Err(LinkError::Unreachable));
        assert_eq!(f.refresh_with(now + REFRESH_SECS, |_| Ok("<html>".into())), Err(LinkError::NotCalendar));
        assert_eq!(f.events.len(), 1);
        assert_eq!(f.fetched_at, Some(now));
    }

    #[test]
    fn the_window_is_yesterday_to_a_week_ahead() {
        let now = at("2026-11-17T12:00:00Z");
        let (from, to) = window(now);
        assert!(from <= now - DAY_SECS && from > now - 3 * DAY_SECS);
        assert!(to >= now + 7 * DAY_SECS && to < now + 9 * DAY_SECS);
    }

    // --- which event now (Yogurt: detect.ts matchEvent) ---

    fn ev(title: &str, start: i64, end: i64, link: bool) -> Event {
        Event {
            id: format!("{title}|{start}"),
            uid: title.into(),
            title: title.into(),
            start,
            end,
            all_day: false,
            attendees: vec![],
            organizer: None,
            link: link.then(|| "https://zoom.us/j/1".into()),
        }
    }

    #[test]
    fn near_is_within_ten_minutes_either_way_and_not_over() {
        let now = 1_000_000;
        assert_eq!(near(&[ev("Soon", now + 600, now + 3600, false)], now).map(|e| e.title.as_str()), Some("Soon"));
        assert_eq!(near(&[ev("Later", now + 601, now + 3600, false)], now), None);
        assert_eq!(near(&[ev("Started", now - 600, now + 3600, false)], now).map(|e| e.title.as_str()), Some("Started"));
        assert_eq!(near(&[ev("Long ago", now - 601, now + 3600, false)], now), None);
        assert_eq!(near(&[ev("Over", now - 300, now, false)], now), None);
        let mut day = ev("Day off", now, now + DAY_SECS, false);
        day.all_day = true;
        assert_eq!(near(&[day], now), None);
        assert_eq!(near(&[], now), None);
    }

    #[test]
    fn near_prefers_a_call_then_the_closest_start() {
        let now = 1_000_000;
        let events = [ev("Coffee", now, now + 1800, false), ev("Design review", now + 300, now + 1800, true), ev("Sync", now - 120, now + 1800, true)];
        assert_eq!(near(&events, now).map(|e| e.title.as_str()), Some("Sync"));
        let events = [ev("Coffee", now + 60, now + 1800, false), ev("Lunch", now - 30, now + 1800, false)];
        assert_eq!(near(&events, now).map(|e| e.title.as_str()), Some("Lunch"));
    }

    // --- the request ---

    /// A one-shot HTTP server on this computer: it answers each request with `answer`.
    fn serve(answer: Vec<u8>) -> Url {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("an address").port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(&answer);
            }
        });
        Url::parse(&format!("http://127.0.0.1:{port}/cal.ics")).expect("a url")
    }

    fn ok(body: &str) -> Vec<u8> {
        format!("HTTP/1.1 200 OK\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn fetches_a_calendar() {
        let body = format!("\u{feff}{}", ics("Work"));
        assert_eq!(fetch(&serve(ok(&body))), Ok(ics("Work")));
        let url = serve(ok(&ics("Work")));
        assert_eq!(connect(url.as_str()).map(|l| l.name), Ok("Work".into()));
    }

    #[test]
    fn an_error_answer_or_a_huge_one_is_unreachable() {
        let missing = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        assert_eq!(fetch(&serve(missing)), Err(LinkError::Unreachable));
        let huge = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_BYTES + 1).into_bytes();
        assert_eq!(fetch(&serve(huge)), Err(LinkError::Unreachable));
        // No length in the header: the read stops after the limit.
        let mut endless = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        endless.extend(vec![b'x'; MAX_BYTES as usize + 10]);
        assert_eq!(fetch(&serve(endless)), Err(LinkError::Unreachable));
        // Nothing listens there.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(9);
        let url = Url::parse(&format!("http://127.0.0.1:{closed}/x")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(fetch(&url), Err(LinkError::Unreachable));
        assert_eq!(fetch(&Url::parse("http://example.com/x").unwrap_or_else(|e| panic!("{e}"))), Err(LinkError::BadUrl));
    }
}
