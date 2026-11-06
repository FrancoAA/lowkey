//! MCP server: `lowkey --mcp` lets AI agents (Claude Code, Codex, Gemini CLI, Cursor, Zed…) read
//! your meetings and dictations and transcribe files, over the Model Context Protocol: JSON-RPC,
//! one message per line on stdin and stdout. The agent starts it. It reads the app's files, so
//! the app need not run, and it never writes them.
//!
//! It only reads, with one exception: `start_meeting` and `stop_meeting` record. They work only
//! when Settings → AI agents → "Let agents start and stop recordings" is on, and the app shows a
//! notification when an agent starts or stops a recording.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config::Config;
use crate::meeting::{self, Meeting, Parts, Summary};

/// The protocol versions this server speaks, newest first (the features used here are the same
/// in all of them).
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Lowkey records meetings and dictations on this computer and transcribes them there. \
In a meeting transcript, \"Me\" is the user who recorded the meeting; the others are \"Them\", or one label \
for each person told apart by voice (\"Speaker 2\", or a name the user gave). \
AI notes are a draft that a language model wrote from the transcript; tasks in them start with the \
owner's name in bold. All times are UTC. start_meeting and stop_meeting work only when the user \
allows them in Lowkey (Settings → AI agents).";

const NOT_ALLOWED: &str = "Agents may not start or stop recordings. The user can allow it in Lowkey: \
Settings → AI agents → \"Let agents start and stop recordings\".";

const URI_PREFIX: &str = "lowkey://meeting/";

pub type TranscribeFn = Box<dyn FnMut(&Path, Option<&str>, Option<&str>) -> anyhow::Result<Value>>;

/// What the server reads and does. `stdio()` gives the real one; the tests give their own.
pub struct Server {
    pub meetings: PathBuf,
    pub history: PathBuf,
    /// The settings, read again for each call (a change in the app applies at once).
    pub config: Box<dyn Fn() -> Config>,
    /// Since when (unix seconds) the app runs; None: it does not run.
    pub app_since: Box<dyn Fn() -> Option<u64>>,
    /// Starts `lowkey <args>`: the running app gets them (or the app starts with them).
    pub launch: Box<dyn Fn(&[String]) -> anyhow::Result<()>>,
    /// A file → the JSON of `lowkey --transcribe` (path, model, language).
    pub transcribe: TranscribeFn,
    pub now: Box<dyn Fn() -> u64>,
    /// How long start_meeting waits for the recording, and stop_meeting for the saved meeting
    /// (the last seconds are transcribed first). Under the tool timeout of common agents.
    pub start_wait: Duration,
    pub stop_wait: Duration,
    /// The app saves a meeting before it opens the microphone, and deletes it when that fails:
    /// a recording is one that is still there this long after it appeared.
    pub settle: Duration,
}

type ToolResult = anyhow::Result<(String, Value)>;

impl Server {
    /// One message in, its answer out (None: a notification or a response, nothing to answer).
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        let Some(method) = msg.get("method").and_then(Value::as_str).map(String::from) else {
            // A response to a request of ours (this server sends none): nothing to do. Anything
            // else without a method is not a request.
            let response = msg.get("result").is_some() || msg.get("error").is_some();
            return (!response).then(|| error(msg.get("id").cloned().unwrap_or(Value::Null), -32600, "Invalid request: no method"));
        };
        let id = msg.get("id").cloned()?; // a notification
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let r = match method.as_str() {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call(&params),
            "resources/list" => self.resources(&params),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [{
                "uriTemplate": format!("{URI_PREFIX}{{id}}"), "name": "meeting", "title": "A Lowkey meeting",
                "description": "A meeting as Markdown: title, time (UTC), AI notes, the user's notes and the transcript.",
                "mimeType": "text/markdown",
            }] })),
            "resources/read" => self.read_resource(&params),
            _ => Err((-32601, format!("Unknown method {method:?}"))),
        };
        Some(match r {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn call(&mut self, p: &Value) -> Result<Value, (i64, String)> {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let a = p.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let out = match name {
            "list_meetings" => self.list_meetings(&a),
            "get_meeting" => self.get_meeting(&a),
            "search" => self.search(&a),
            "get_action_items" => self.action_items(&a),
            "last_dictation" => self.last_dictation(),
            "transcribe_file" => self.transcribe_file(&a),
            "recording_status" => self.recording_status(),
            "start_meeting" => self.start_meeting(&a),
            "stop_meeting" => self.stop_meeting(),
            _ => return Err((-32602, format!("Unknown tool {name:?}"))),
        };
        Ok(match out {
            Ok((text, data)) => json!({ "content": [{ "type": "text", "text": text }], "structuredContent": data, "isError": false }),
            Err(e) => json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true }),
        })
    }

    // ------------------------------------------------------------------ reading
    fn list_meetings(&self, a: &Value) -> ToolResult {
        let limit = limit(a, 20);
        let all = meeting::list_in(&self.meetings);
        let now_id = self.recording().map(|s| s.id);
        let shown: Vec<&Summary> = all.iter().take(limit).collect();
        let text = if shown.is_empty() {
            "No meetings yet.".to_string()
        } else {
            let more = all.len().saturating_sub(shown.len());
            let mut t = shown.iter().map(|s| meeting_line(s, now_id.as_deref())).collect::<Vec<_>>().join("\n");
            if more > 0 {
                t += &format!("\n…and {more} older (a larger limit lists them).");
            }
            t
        };
        Ok((text, json!({ "meetings": shown.iter().map(|s| summary_json(s, now_id.as_deref())).collect::<Vec<_>>(), "total": all.len() })))
    }

    fn get_meeting(&self, a: &Value) -> ToolResult {
        let m = self.load(a)?;
        let parts = match a.get("parts").and_then(Value::as_array) {
            None => Parts::ALL,
            Some(list) => {
                let has = |p: &str| list.iter().any(|v| v.as_str() == Some(p));
                Parts { ai_notes: has("ai_notes"), notes: has("notes"), transcript: has("transcript") }
            }
        };
        let now_id = self.recording().map(|s| s.id);
        let data = json!({ "id": m.id, "title": m.title, "started_at": m.started_at, "started_utc": utc(m.started_at),
                           "duration_s": m.duration.round(), "state": shown_state(&m.state, &m.id, now_id.as_deref()), "has_ai_notes": !m.ai_notes.trim().is_empty(),
                           "video_path": meeting::video_path_in(&self.meetings, &m.id).map(|p| p.to_string_lossy().into_owned()) });
        Ok((meeting::to_markdown_utc(&m, parts), data))
    }

    fn search(&self, a: &Value) -> ToolResult {
        let query = a.get("query").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let scope = a.get("in").and_then(Value::as_str).unwrap_or("all");
        anyhow::ensure!(["all", "meetings", "dictations"].contains(&scope), "\"in\" is all, meetings or dictations, not {scope:?}");
        let limit = limit(a, 20);
        let mut text = Vec::new();
        let mut data = json!({});
        if scope != "dictations" {
            let found = meeting::search_in(&self.meetings, &query);
            let now_id = self.recording().map(|s| s.id);
            let shown: Vec<&Summary> = found.iter().take(limit).collect();
            text.push(format!("Meetings ({} found):", found.len()));
            text.extend(shown.iter().map(|s| {
                let mut l = meeting_line(s, now_id.as_deref());
                if !s.found_in.is_empty() {
                    l += &format!("\n  in the {}: {}", s.found_in.replace('_', " "), s.excerpt);
                }
                l
            }));
            data["meetings"] = json!(shown.iter().map(|s| summary_json(s, now_id.as_deref())).collect::<Vec<_>>());
        }
        if scope != "meetings" {
            let keep = (self.config)().history_keep;
            let found = crate::history::read(&self.history, &keep, &query, (self.now)());
            if !text.is_empty() {
                text.push(String::new());
            }
            text.push(format!("Dictations ({} found):", found.len()));
            if keep == "off" {
                text.push("(The dictation history is off: Lowkey → Settings → General.)".into());
            }
            text.extend(found.iter().take(limit).map(|e| format!("- {} UTC · {}", utc(e.at), e.text)));
            data["dictations"] = json!(found.iter().take(limit).map(|e| json!({ "id": e.id, "at": e.at, "at_utc": utc(e.at), "text": e.text })).collect::<Vec<_>>());
        }
        Ok((text.join("\n"), data))
    }

    fn action_items(&self, a: &Value) -> ToolResult {
        let owner = a.get("owner").and_then(Value::as_str).map(|o| o.trim().to_lowercase()).filter(|o| !o.is_empty());
        let docs: Vec<Meeting> = if a.get("id").is_some() {
            vec![self.load(a)?]
        } else {
            // The newest meetings that have AI notes.
            meeting::list_in(&self.meetings)
                .iter()
                .filter_map(|s| meeting::load_in(&self.meetings, &s.id).ok())
                .filter(|m| !m.ai_notes.trim().is_empty())
                .take(limit(a, 5))
                .collect()
        };
        if docs.is_empty() {
            return Ok(("No meeting has AI notes yet.".into(), json!({ "meetings": [] })));
        }
        let mut text = Vec::new();
        let mut out = Vec::new();
        for m in &docs {
            text.push(format!("## {} ({}, {} UTC)", m.title, m.id, meeting::local_time(m.started_at, 0)));
            if m.ai_notes.trim().is_empty() {
                text.push("No AI notes yet (Lowkey writes them after the meeting, or with Write notes).".into());
                out.push(json!({ "id": m.id, "title": m.title, "tasks": [] }));
                continue;
            }
            let tasks: Vec<Task> = tasks(&m.ai_notes).into_iter().filter(|t| owner.as_deref().map_or(true, |o| t.owner.to_lowercase() == o)).collect();
            if tasks.is_empty() {
                text.push("No tasks.".into());
            }
            for t in &tasks {
                let topic = if t.topic.is_empty() { String::new() } else { format!(" ({})", t.topic) };
                text.push(format!("- **{}** {}{topic}", t.owner, t.task));
                text.extend(t.details.iter().map(|d| format!("  - {d}")));
            }
            out.push(json!({ "id": m.id, "title": m.title, "started_utc": utc(m.started_at),
                             "tasks": tasks.iter().map(|t| json!({ "owner": t.owner, "task": t.task, "topic": t.topic, "details": t.details })).collect::<Vec<_>>() }));
        }
        Ok((text.join("\n"), json!({ "meetings": out })))
    }

    fn last_dictation(&self) -> ToolResult {
        let keep = (self.config)().history_keep;
        match crate::history::read(&self.history, &keep, "", (self.now)()).into_iter().next() {
            Some(e) => Ok((e.text.clone(), json!({ "id": e.id, "at": e.at, "at_utc": utc(e.at), "text": e.text, "seconds": e.seconds }))),
            None if keep == "off" => anyhow::bail!("The dictation history is off (Lowkey → Settings → General), so there is no dictation to read."),
            None => anyhow::bail!("No dictation in the history yet."),
        }
    }

    fn transcribe_file(&mut self, a: &Value) -> ToolResult {
        let path = PathBuf::from(a.get("path").and_then(Value::as_str).unwrap_or("").trim());
        anyhow::ensure!(path.is_absolute(), "\"path\" must be the absolute path of an audio or video file");
        anyhow::ensure!(path.is_file(), "No file at {}", path.display());
        let model = a.get("model").and_then(Value::as_str).filter(|m| !m.is_empty());
        let language = a.get("language").and_then(Value::as_str).filter(|l| !l.is_empty() && *l != "auto");
        let out = (self.transcribe)(&path, model, language)?;
        let text = out.get("text").and_then(Value::as_str).unwrap_or("").to_string();
        Ok((if text.is_empty() { "No speech was found.".into() } else { text }, out))
    }

    // ------------------------------------------------------------------ recording
    /// The meeting the running app records now. A file left "recording" by an app that stopped
    /// is not one: it started before the app that runs now.
    fn recording(&self) -> Option<Summary> {
        let since = (self.app_since)()?;
        meeting::list_in(&self.meetings).into_iter().find(|s| s.state == "recording" && s.started_at + 1 >= since)
    }

    fn recording_status(&self) -> ToolResult {
        let allowed = (self.config)().mcp_recording;
        let may = if allowed { "Agents may start and stop recordings." } else { "Agents may not start or stop recordings (Lowkey → Settings → AI agents)." };
        let running = (self.app_since)().is_some();
        let now = self.recording();
        let text = match (&now, running) {
            (Some(s), _) => format!("Recording now: {} ({}), started {} UTC, {} so far.", s.title, s.id, utc(s.started_at), meeting::duration_text((self.now)().saturating_sub(s.started_at) as f64)),
            (None, true) => "Lowkey runs; nothing is being recorded.".into(),
            (None, false) => "Lowkey does not run; nothing is being recorded.".into(),
        };
        Ok((format!("{text} {may}"), json!({ "app_running": running, "recording": now.is_some(), "meeting": now.as_ref().map(|s| summary_json(s, Some(&s.id))), "agents_may_record": allowed })))
    }

    fn start_meeting(&self, a: &Value) -> ToolResult {
        anyhow::ensure!((self.config)().mcp_recording, NOT_ALLOWED);
        if let Some(s) = self.recording() {
            anyhow::bail!("A meeting is being recorded already: {} ({}). stop_meeting stops it.", s.title, s.id);
        }
        let title: String = a.get("title").and_then(Value::as_str).unwrap_or("").trim().chars().take(meeting::MAX_TITLE).collect();
        let t0 = (self.now)();
        let mut args: Vec<String> = vec!["--meeting".into(), "start".into(), "--agent".into()];
        if !title.is_empty() {
            args.extend(["--title".into(), title]);
        }
        (self.launch)(&args)?;
        let first = wait_for(self.start_wait, || self.recording().filter(|s| s.started_at + 1 >= t0)).ok_or_else(|| {
            anyhow::anyhow!("The recording did not start in {} s. Lowkey shows why (no microphone, for example).", self.start_wait.as_secs())
        })?;
        std::thread::sleep(self.settle);
        let s = self
            .recording()
            .filter(|s| s.id == first.id)
            .ok_or_else(|| anyhow::anyhow!("The recording did not start. Lowkey shows why (no microphone, for example)."))?;
        Ok((
            format!("Recording: {} ({}). The user sees it on Lowkey's strip and can stop it there. stop_meeting stops it and saves the transcript.", s.title, s.id),
            summary_json(&s, Some(&s.id)),
        ))
    }

    fn stop_meeting(&self) -> ToolResult {
        anyhow::ensure!((self.config)().mcp_recording, NOT_ALLOWED);
        let s = self.recording().ok_or_else(|| anyhow::anyhow!("No meeting is being recorded."))?;
        (self.launch)(&["--meeting".into(), "stop".into(), "--agent".into()])?;
        let Some(m) = wait_for(self.stop_wait, || meeting::load_in(&self.meetings, &s.id).ok().filter(|m| m.state == "done")) else {
            return Ok((
                format!("Stopped: {} ({}). Lowkey is still saving it (it transcribes the last seconds); recording_status and get_meeting show it when it is done.", s.title, s.id),
                json!({ "id": s.id, "title": s.title, "saved": false }),
            ));
        };
        let notes = if (self.config)().ai_auto && !m.lines.is_empty() { " Lowkey writes the AI notes next; get_meeting shows them when they are ready." } else { "" };
        Ok((
            format!("Stopped and saved: {} ({}), {}, {} transcript lines.{notes}", m.title, m.id, meeting::duration_text(m.duration), m.lines.len()),
            json!({ "id": m.id, "title": m.title, "saved": true, "duration_s": m.duration.round(), "lines": m.lines.len() }),
        ))
    }

    // ------------------------------------------------------------------ resources
    /// The meetings, newest first, 200 a page (the cursor is where the next page starts).
    fn resources(&self, p: &Value) -> Result<Value, (i64, String)> {
        const PAGE: usize = 200;
        let start = match p.get("cursor").and_then(Value::as_str) {
            None => 0,
            Some(c) => c.parse::<usize>().map_err(|_| (-32602, format!("Unknown cursor {c:?}")))?,
        };
        let all = meeting::list_in(&self.meetings);
        let list: Vec<Value> = all
            .iter()
            .skip(start)
            .take(PAGE)
            .map(|s| json!({ "uri": format!("{URI_PREFIX}{}", s.id), "name": s.id, "title": s.title,
                             "description": format!("{} UTC · {}", utc(s.started_at), meeting::duration_text(s.duration)), "mimeType": "text/markdown" }))
            .collect();
        let mut out = json!({ "resources": list });
        if start + PAGE < all.len() {
            out["nextCursor"] = json!((start + PAGE).to_string());
        }
        Ok(out)
    }

    fn read_resource(&self, p: &Value) -> Result<Value, (i64, String)> {
        let uri = p.get("uri").and_then(Value::as_str).unwrap_or("");
        let m = uri
            .strip_prefix(URI_PREFIX)
            .filter(|id| meeting::valid_id(id))
            .and_then(|id| meeting::load_in(&self.meetings, id).ok())
            .ok_or((-32002, format!("Resource not found: {uri}")))?;
        Ok(json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": meeting::to_markdown_utc(&m, Parts::ALL) }] }))
    }

    fn load(&self, a: &Value) -> anyhow::Result<Meeting> {
        let id = a.get("id").and_then(Value::as_str).unwrap_or("").trim();
        anyhow::ensure!(meeting::valid_id(id), "Unknown meeting id {id:?} (list_meetings gives the ids: \"m-\" and digits)");
        meeting::load_in(&self.meetings, id).map_err(|_| anyhow::anyhow!("No meeting {id}"))
    }
}

/// Serve until stdin ends (the agent closes it).
pub fn serve(server: &mut Server, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            // A batch (older protocol versions).
            Ok(Value::Array(batch)) if batch.is_empty() => Some(error(Value::Null, -32600, "Invalid request: an empty batch")),
            Ok(Value::Array(batch)) => Some(Value::Array(batch.into_iter().filter_map(|m| server.handle(m)).collect())).filter(|v| v.as_array().is_some_and(|a| !a.is_empty())),
            Ok(msg) => server.handle(msg),
            Err(e) => Some(error(Value::Null, -32700, &format!("Parse error: {e}"))),
        };
        if let Some(r) = reply {
            writeln!(output, "{r}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// `lowkey --mcp`: the real server on stdin and stdout.
pub fn stdio() {
    // Only protocol messages may reach stdout: what the speech engines print (C libraries
    // too) goes to stderr, which agents keep as the server's log.
    let out = protocol_stdout();
    let mut server = Server {
        meetings: meeting::dir(),
        history: crate::history::path(),
        config: Box::new(Config::load),
        app_since: Box::new(crate::app::running_since),
        launch: Box::new(launch),
        transcribe: real_transcribe(),
        now: Box::new(crate::history::now),
        start_wait: Duration::from_secs(40),
        stop_wait: Duration::from_secs(50),
        settle: Duration::from_secs(3),
    };
    if let Err(e) = serve(&mut server, std::io::stdin().lock(), out) {
        eprintln!("lowkey --mcp: {e}");
    }
}

fn real_transcribe() -> TranscribeFn {
    let mut engines: Option<std::sync::Arc<crate::engine::Engines>> = None;
    Box::new(move |path, model, language| {
        let cfg = Config::load();
        let e = engines.get_or_insert_with(|| crate::engine::Engines::new(cfg.idle_unload_minutes));
        e.set_vocabulary(&cfg.vocabulary);
        crate::app::transcribe_json(e, &cfg, path, model, language.map(String::from), false).1
    })
}

/// `lowkey <args>`, not waited for: the running app takes them over and this one ends, or it
/// becomes the app. In its own process group, so the agent ending this server never ends it.
fn launch(args: &[String]) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(crate::autostart::launcher());
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("Could not start Lowkey: {e}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// The protocol's stream: a copy of stdout that programs started from here never inherit (the
/// app that start_meeting starts would keep the agent's pipe open). fd 1 itself then goes to
/// stderr (or nowhere, without one).
#[cfg(unix)]
fn protocol_stdout() -> Box<dyn Write> {
    use std::os::fd::{AsRawFd, FromRawFd};
    extern "C" {
        fn dup(fd: i32) -> i32;
        fn dup2(fd: i32, to: i32) -> i32;
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    const F_SETFD: i32 = 2;
    const FD_CLOEXEC: i32 = 1;
    // SAFETY: plain descriptor calls; the copy of stdout is owned by the File from here on.
    unsafe {
        let proto = dup(1);
        if proto < 0 {
            return Box::new(std::io::stdout());
        }
        fcntl(proto, F_SETFD, FD_CLOEXEC);
        if dup2(2, 1) < 0 {
            if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
                dup2(null.as_raw_fd(), 1);
            }
        }
        Box::new(std::fs::File::from_raw_fd(proto))
    }
}

#[cfg(not(unix))]
fn protocol_stdout() -> Box<dyn Write> {
    Box::new(std::io::stdout())
}

// ------------------------------------------------------------------ the tools
fn tools() -> Value {
    let read = json!({ "readOnlyHint": true, "openWorldHint": false });
    let record = json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false });
    let id = json!({ "type": "string", "description": "The meeting id, \"m-\" and digits (from list_meetings or search)." });
    json!([
        { "name": "list_meetings", "title": "List meetings", "annotations": read,
          "description": "The meetings Lowkey recorded, newest first: id, title, start (UTC), length and the first words.",
          "inputSchema": { "type": "object", "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "How many (default 20)." } } } },
        { "name": "get_meeting", "title": "Get a meeting", "annotations": read,
          "description": "One meeting as Markdown: the AI notes (a draft), the user's own notes and the transcript, where \"Me\" is the user and the others are \"Them\" or one label per person (\"Speaker 2\", or a name). Long meetings have long transcripts: ask for the parts you need. `video_path`: the meeting's screen video, when it was recorded (a moment t of the transcript is t - offset in it; see the file's meeting JSON).",
          "inputSchema": { "type": "object", "required": ["id"], "properties": { "id": id,
              "parts": { "type": "array", "items": { "type": "string", "enum": ["ai_notes", "notes", "transcript"] }, "description": "Only these parts (default: all)." } } } },
        { "name": "search", "title": "Search meetings and dictations", "annotations": read,
          "description": "Meetings and dictations that have every word of the query (any order, any case), newest first. For meetings: where the words were found, with the words around them. An empty query lists the newest.",
          "inputSchema": { "type": "object", "properties": {
              "query": { "type": "string", "description": "The words to find." },
              "in": { "type": "string", "enum": ["all", "meetings", "dictations"], "description": "Where to search (default all)." },
              "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "How many of each (default 20)." } } } },
        { "name": "get_action_items", "title": "Get action items", "annotations": read,
          "description": "The tasks in the AI notes of a meeting (or of the newest meetings that have AI notes), with the owner, the topic and details. \"Me\" is the user.",
          "inputSchema": { "type": "object", "properties": { "id": id,
              "owner": { "type": "string", "description": "Only this person's tasks (\"Me\" for the user's own)." },
              "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Without an id: how many meetings (default 5)." } } } },
        { "name": "last_dictation", "title": "Last dictation", "annotations": read,
          "description": "The text of the user's last dictation (from Lowkey's dictation history).",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "transcribe_file", "title": "Transcribe a file", "annotations": read,
          "description": "Transcribe an audio or video file on this computer with Lowkey's local speech model (nothing is sent anywhere). Returns the text, and segments with times in the structured result. The first call loads the model (a few seconds); a long file takes minutes. Only models already downloaded in Lowkey.",
          "inputSchema": { "type": "object", "required": ["path"], "properties": {
              "path": { "type": "string", "description": "The file's absolute path." },
              "model": { "type": "string", "description": "A downloaded Lowkey model id, such as \"small\" or \"large-v3-turbo-q5_0\" (default: the one chosen in Lowkey)." },
              "language": { "type": "string", "description": "A language code such as \"en\" or \"es\" (default: detected)." } } } },
        { "name": "recording_status", "title": "Recording status", "annotations": read,
          "description": "Whether Lowkey runs and records a meeting now, and whether agents may start and stop recordings.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "start_meeting", "title": "Start a meeting recording", "annotations": record,
          "description": "Start recording a meeting: the user's microphone and the computer's sound. Only when the user asked for it; the people in the meeting must know they are recorded. Works only when the user allows it in Lowkey (Settings → AI agents); Lowkey shows a notification.",
          "inputSchema": { "type": "object", "properties": { "title": { "type": "string", "description": "The meeting's title (default: Lowkey's)." } } } },
        { "name": "stop_meeting", "title": "Stop the meeting recording", "annotations": record,
          "description": "Stop the meeting recording and save it; returns when the transcript is saved. Works only when the user allows it in Lowkey (Settings → AI agents).",
          "inputSchema": { "type": "object", "properties": {} } },
    ])
}

fn initialize(p: &Value) -> Value {
    let asked = p.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {}, "resources": {} },
        "serverInfo": { "name": "lowkey", "title": "Lowkey", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn limit(a: &Value, default: u64) -> usize {
    a.get("limit").and_then(Value::as_u64).unwrap_or(default).clamp(1, 200) as usize
}

fn utc(unix: u64) -> String {
    meeting::local_time(unix, 0)
}

/// "recording" only for the meeting the app records now; a file left so by an app that stopped
/// is "interrupted".
fn shown_state<'a>(state: &'a str, id: &str, recording_now: Option<&str>) -> &'a str {
    match state {
        "recording" if recording_now == Some(id) => "recording",
        "recording" => "interrupted",
        other => other,
    }
}

fn meeting_line(s: &Summary, recording_now: Option<&str>) -> String {
    let state = match shown_state(&s.state, &s.id, recording_now) {
        "recording" => " (recording now)",
        "interrupted" => " (interrupted)",
        _ => "",
    };
    format!("- {} · {} UTC · {} · {}{state}", s.id, utc(s.started_at), meeting::duration_text(s.duration), s.title)
}

fn summary_json(s: &Summary, recording_now: Option<&str>) -> Value {
    let state = shown_state(&s.state, &s.id, recording_now);
    json!({ "id": s.id, "title": s.title, "started_at": s.started_at, "started_utc": utc(s.started_at),
            "duration_s": s.duration.round(), "state": state, "excerpt": s.excerpt })
}

fn wait_for<T>(timeout: Duration, mut found: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + timeout;
    loop {
        if let Some(v) = found() {
            return Some(v);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A task in the AI notes: a top-level bullet that starts with a name in bold
/// ("- **Ana** sends the contract by Friday"), as the notes' instructions ask (llm.rs).
#[derive(Debug, Clone, PartialEq)]
struct Task {
    owner: String,
    task: String,
    /// The "## " heading the task is under.
    topic: String,
    /// Its sub-bullets.
    details: Vec<String>,
}

fn tasks(notes: &str) -> Vec<Task> {
    let mut out: Vec<Task> = Vec::new();
    let mut topic = String::new();
    let mut in_task = false;
    for line in notes.lines() {
        if let Some(h) = line.strip_prefix("## ").or_else(|| line.strip_prefix("# ")) {
            topic = h.trim().to_string();
            in_task = false;
        } else if let Some(rest) = line.strip_prefix("- **").or_else(|| line.strip_prefix("* **")) {
            in_task = false;
            if let Some((owner, task)) = rest.split_once("**") {
                let task = task.trim_start_matches([':', ' ']).trim();
                if !owner.trim().is_empty() && !task.is_empty() {
                    out.push(Task { owner: owner.trim().to_string(), task: task.to_string(), topic: topic.clone(), details: vec![] });
                    in_task = true;
                }
            }
        } else if let Some(d) = line.strip_prefix("  - ").or_else(|| line.strip_prefix("  * ")) {
            if in_task {
                if let Some(t) = out.last_mut() {
                    t.details.push(d.trim().to_string());
                }
            }
        } else if !line.trim().is_empty() {
            in_task = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::{Line, Speaker};
    use std::cell::RefCell;
    use std::rc::Rc;

    const NOTES: &str = "## Contract\n- The contract goes out this week.\n- **Ana** sends the contract by Friday\n  - legal checked it\n- **Me** calls the customer on Monday\n\n## Hiring\n- **Them**: posts the job ad\n- A question is open: the budget.\n";

    fn doc(id: &str, title: &str, started_at: u64, state: &str, notes: &str) -> Meeting {
        Meeting {
            id: id.into(),
            title: title.into(),
            started_at,
            duration: 1800.0,
            state: state.into(),
            model: "small".into(),
            lines: vec![
                Line { speaker: Speaker::Me, start: 0.0, end: 3.0, text: "I call the customer on Monday.".into(), person: 0 },
                Line { speaker: Speaker::Them, start: 4.0, end: 8.0, text: "Ana sends the contract by Friday.".into(), person: 0 },
            ],
            ai_notes: notes.into(),
            ai_model: "Qwen3.5 4B".into(),
            ..Default::default()
        }
    }

    struct Fixture {
        dir: PathBuf,
        allowed: Rc<RefCell<bool>>,
        launched: Rc<RefCell<Vec<Vec<String>>>>,
    }

    /// Two meetings (one interrupted long ago), three dictations, the app running since 1000.
    fn server(name: &str) -> (Server, Fixture) {
        let dir = std::env::temp_dir().join(format!("ow-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let meetings = dir.join("meetings");
        std::fs::create_dir_all(&meetings).unwrap();
        let old = Meeting { lines: vec![Line { speaker: Speaker::Me, start: 0.0, end: 2.0, text: "Hello everyone.".into(), person: 0 }], ..doc("m-1", "Old sync", 500, "recording", "") };
        meeting::save_in(&meetings, &old).unwrap();
        meeting::save_in(&meetings, &doc("m-2", "Contract review", 2000, "done", NOTES)).unwrap();
        let history = dir.join("history.json");
        let entries = json!([
            { "id": "h-1", "at": 100, "text": "too old for a day", "source": "key", "model": "small", "seconds": 1.0 },
            { "id": "h-2", "at": 590_000, "text": "buy milk", "source": "key", "model": "small", "seconds": 1.0 },
            { "id": "h-3", "at": 599_000, "text": "Reply to Ana about the contract", "source": "key", "model": "small", "seconds": 2.0 },
        ]);
        std::fs::write(&history, entries.to_string()).unwrap();
        let allowed = Rc::new(RefCell::new(false));
        let launched: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(vec![]));
        let (a2, l2, m2) = (allowed.clone(), launched.clone(), meetings.clone());
        let s = Server {
            meetings: meetings.clone(),
            history,
            config: Box::new(move || Config { mcp_recording: *a2.borrow(), history_keep: "day".into(), ..Config::default() }),
            app_since: Box::new(|| Some(1000)),
            // The fake app: start writes a recording, stop marks it done.
            launch: Box::new(move |args| {
                l2.borrow_mut().push(args.to_vec());
                if args[1] == "start" {
                    let title = args.iter().position(|a| a == "--title").map(|i| args[i + 1].clone()).unwrap_or("Meeting".into());
                    meeting::save_in(&m2, &doc("m-600000", &title, 600_000, "recording", "")).unwrap();
                } else {
                    meeting::edit_in(&m2, "m-600000", |m| m.state = "done".into()).unwrap();
                }
                Ok(())
            }),
            transcribe: Box::new(|p, model, lang| Ok(json!({ "text": format!("text of {}", p.display()), "model": model, "language": lang }))),
            now: Box::new(|| 600_000),
            start_wait: Duration::from_millis(300),
            stop_wait: Duration::from_millis(300),
            settle: Duration::ZERO,
        };
        (s, Fixture { dir, allowed, launched })
    }

    fn call(s: &mut Server, name: &str, args: Value) -> (bool, String, Value) {
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": args } })).unwrap();
        let res = &r["result"];
        (res["isError"].as_bool().unwrap(), res["content"][0]["text"].as_str().unwrap().to_string(), res["structuredContent"].clone())
    }

    #[test]
    fn the_handshake_and_the_tool_list() {
        let (mut s, f) = server("init");
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } })).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "lowkey");
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } })).unwrap();
        assert_eq!(r["result"]["protocolVersion"], VERSIONS[0], "an unknown version: the newest this server speaks");
        assert_eq!(s.handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })), None);
        let names: Vec<String> = s.handle(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" })).unwrap()["result"]["tools"]
            .as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["list_meetings", "get_meeting", "search", "get_action_items", "last_dictation", "transcribe_file", "recording_status", "start_meeting", "stop_meeting"]);
        assert_eq!(s.handle(json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" })).unwrap()["error"]["code"], -32601);
        assert_eq!(s.handle(json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "nope" } })).unwrap()["error"]["code"], -32602);
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn meetings_are_listed_read_and_searched() {
        let (mut s, f) = server("read");
        let (err, text, data) = call(&mut s, "list_meetings", json!({}));
        assert!(!err);
        assert_eq!(text, "- m-2 · 1970-01-01 00:33 UTC · 30 min · Contract review\n- m-1 · 1970-01-01 00:08 UTC · 30 min · Old sync (interrupted)");
        assert_eq!(data["meetings"][1]["state"], "interrupted");
        let (_, text, _) = call(&mut s, "list_meetings", json!({ "limit": 1 }));
        assert!(text.ends_with("…and 1 older (a larger limit lists them)."), "{text}");

        let (err, md, data) = call(&mut s, "get_meeting", json!({ "id": "m-2" }));
        assert!(!err && data["has_ai_notes"] == true);
        assert!(md.starts_with("# Contract review\n\n1970-01-01 00:33 UTC · 30 min · id m-2\n\n## AI notes (Qwen3.5 4B, a draft)"), "{md}");
        assert!(md.contains("**Me** (00:00): I call the customer on Monday."), "{md}");
        let (_, md, _) = call(&mut s, "get_meeting", json!({ "id": "m-2", "parts": ["transcript"] }));
        assert!(!md.contains("AI notes") && md.contains("## Transcript"), "{md}");
        let (_, md, _) = call(&mut s, "get_meeting", json!({ "id": "m-2", "parts": ["ai_notes"] }));
        assert!(md.contains("AI notes") && !md.contains("## Transcript"), "{md}");
        assert!(call(&mut s, "get_meeting", json!({ "id": "../../etc/passwd" })).0, "only meeting ids");
        assert!(call(&mut s, "get_meeting", json!({ "id": "m-9" })).0);

        let (err, text, data) = call(&mut s, "search", json!({ "query": "contract friday" }));
        assert!(!err);
        assert!(text.starts_with("Meetings (1 found):\n- m-2"), "{text}");
        assert!(text.contains("in the transcript: ") && text.contains("Dictations (0 found):"), "{text}");
        assert_eq!(data["meetings"][0]["id"], "m-2");
        let (_, text, data) = call(&mut s, "search", json!({ "query": "ana", "in": "dictations" }));
        assert_eq!(text, "Dictations (1 found):\n- 1970-01-07 22:23 UTC · Reply to Ana about the contract");
        assert!(data.get("meetings").is_none());
        let (_, text, _) = call(&mut s, "search", json!({ "in": "dictations" }));
        assert!(text.starts_with("Dictations (2 found):"), "the day's dictations, newest first: {text}");
        assert!(call(&mut s, "search", json!({ "in": "mail" })).0);
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn action_items_come_from_the_ai_notes() {
        let t = tasks(NOTES);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0], Task { owner: "Ana".into(), task: "sends the contract by Friday".into(), topic: "Contract".into(), details: vec!["legal checked it".into()] });
        assert_eq!((t[2].owner.as_str(), t[2].task.as_str(), t[2].topic.as_str()), ("Them", "posts the job ad", "Hiring"));
        let (mut s, f) = server("tasks");
        let (err, text, data) = call(&mut s, "get_action_items", json!({}));
        assert!(!err);
        assert!(text.starts_with("## Contract review (m-2, 1970-01-01 00:33 UTC)\n- **Ana** sends the contract by Friday (Contract)\n  - legal checked it\n"), "{text}");
        assert_eq!(data["meetings"].as_array().unwrap().len(), 1, "only meetings with AI notes");
        let (_, text, _) = call(&mut s, "get_action_items", json!({ "id": "m-2", "owner": "me" }));
        assert_eq!(text, "## Contract review (m-2, 1970-01-01 00:33 UTC)\n- **Me** calls the customer on Monday (Contract)");
        let (_, text, _) = call(&mut s, "get_action_items", json!({ "id": "m-1" }));
        assert!(text.contains("No AI notes yet"), "{text}");
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn dictations_and_files() {
        let (mut s, f) = server("dict");
        let (err, text, _) = call(&mut s, "last_dictation", json!({}));
        assert!(!err);
        assert_eq!(text, "Reply to Ana about the contract");
        let file = f.dir.join("a.ogg");
        std::fs::write(&file, b"x").unwrap();
        let (err, text, data) = call(&mut s, "transcribe_file", json!({ "path": file, "language": "es" }));
        assert!(!err);
        assert_eq!(text, format!("text of {}", file.display()));
        assert_eq!(data["language"], "es");
        assert!(call(&mut s, "transcribe_file", json!({ "path": "a.ogg" })).0, "relative paths are refused");
        assert!(call(&mut s, "transcribe_file", json!({ "path": f.dir.join("none.ogg") })).0);
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn recording_needs_the_setting() {
        let (mut s, f) = server("rec");
        let (err, text, _) = call(&mut s, "start_meeting", json!({ "title": "Weekly" }));
        assert!(err && text.starts_with("Agents may not start or stop recordings."), "{text}");
        assert!(call(&mut s, "stop_meeting", json!({})).0);
        assert!(f.launched.borrow().is_empty(), "nothing was sent to the app");
        let (_, text, data) = call(&mut s, "recording_status", json!({}));
        assert_eq!(text, "Lowkey runs; nothing is being recorded. Agents may not start or stop recordings (Lowkey → Settings → AI agents).");
        assert_eq!(data["recording"], false, "the interrupted m-1 is not a recording");

        *f.allowed.borrow_mut() = true;
        let (err, text, data) = call(&mut s, "start_meeting", json!({ "title": "Weekly" }));
        assert!(!err, "{text}");
        assert_eq!(f.launched.borrow()[0], ["--meeting", "start", "--agent", "--title", "Weekly"]);
        assert_eq!(data["id"], "m-600000");
        let (_, text, _) = call(&mut s, "recording_status", json!({}));
        assert!(text.starts_with("Recording now: Weekly (m-600000)"), "{text}");
        let (err, text, _) = call(&mut s, "start_meeting", json!({}));
        assert!(err && text.starts_with("A meeting is being recorded already"), "{text}");
        let (err, text, _) = call(&mut s, "stop_meeting", json!({}));
        assert!(!err, "{text}");
        assert!(text.starts_with("Stopped and saved: Weekly (m-600000), 30 min, 2 transcript lines."), "{text}");
        assert_eq!(f.launched.borrow()[1], ["--meeting", "stop", "--agent"]);
        assert!(call(&mut s, "stop_meeting", json!({})).0, "nothing to stop");
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn a_start_that_never_comes_says_so() {
        let (mut s, f) = server("nostart");
        *f.allowed.borrow_mut() = true;
        s.launch = Box::new(|_| Ok(())); // the app shows an error (no microphone) and records nothing
        let (err, text, _) = call(&mut s, "start_meeting", json!({}));
        assert!(err && text.starts_with("The recording did not start"), "{text}");
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn a_start_that_fails_a_moment_later_is_not_confirmed() {
        let (mut s, f) = server("vanish");
        *f.allowed.borrow_mut() = true;
        let dir = s.meetings.clone();
        // As the app does when the microphone does not open: saved, then deleted.
        s.launch = Box::new(move |_| {
            meeting::save_in(&dir, &doc("m-600000", "Meeting", 600_000, "recording", "")).unwrap();
            let d = dir.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                let _ = meeting::delete_in(&d, "m-600000");
            });
            Ok(())
        });
        s.settle = Duration::from_millis(250);
        let (err, text, _) = call(&mut s, "start_meeting", json!({}));
        assert!(err && text.starts_with("The recording did not start."), "{text}");
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn a_stop_that_is_still_saving_is_not_an_error() {
        let (mut s, f) = server("saving");
        *f.allowed.borrow_mut() = true;
        assert!(!call(&mut s, "start_meeting", json!({})).0);
        s.launch = Box::new(|_| Ok(())); // the app still transcribes the last seconds
        let (err, text, data) = call(&mut s, "stop_meeting", json!({}));
        assert!(!err && text.starts_with("Stopped: Meeting (m-600000). Lowkey is still saving it"), "{text}");
        assert_eq!(data["saved"], false);
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn what_is_not_a_request_gets_an_error() {
        let (mut s, f) = server("invalid");
        assert_eq!(s.handle(json!({ "jsonrpc": "2.0", "id": 1 })).unwrap()["error"]["code"], -32600);
        assert_eq!(s.handle(json!(5)).unwrap()["error"]["code"], -32600);
        assert_eq!(s.handle(json!({ "jsonrpc": "2.0", "id": 1, "result": {} })), None, "a response: nothing to answer");
        let mut out = Vec::new();
        serve(&mut s, "[]\n".as_bytes(), &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("-32600"));
        let (_, _, data) = call(&mut s, "get_meeting", json!({ "id": "m-1" }));
        assert_eq!(data["state"], "interrupted", "as list_meetings says");
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn meetings_are_resources() {
        let (mut s, f) = server("res");
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" })).unwrap();
        assert_eq!(r["result"]["resources"][0]["uri"], "lowkey://meeting/m-2");
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": { "uri": "lowkey://meeting/m-2" } })).unwrap();
        assert!(r["result"]["contents"][0]["text"].as_str().unwrap().starts_with("# Contract review"));
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": { "uri": "lowkey://meeting/../x" } })).unwrap();
        assert_eq!(r["error"]["code"], -32002);
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" })).unwrap();
        assert!(r["result"].get("nextCursor").is_none(), "one page");
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 5, "method": "resources/list", "params": { "cursor": "1" } })).unwrap();
        assert_eq!(r["result"]["resources"][0]["uri"], "lowkey://meeting/m-1");
        let r = s.handle(json!({ "jsonrpc": "2.0", "id": 6, "method": "resources/list", "params": { "cursor": "x" } })).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        let _ = std::fs::remove_dir_all(f.dir);
    }

    #[test]
    fn serve_answers_one_line_per_request() {
        let (mut s, f) = server("serve");
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\nnot json\n";
        let mut out = Vec::new();
        serve(&mut s, input.as_bytes(), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], json!({ "jsonrpc": "2.0", "id": 1, "result": {} }));
        assert_eq!(lines[1]["error"]["code"], -32700);
        let _ = std::fs::remove_dir_all(f.dir);
    }
}
