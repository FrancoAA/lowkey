//! Speech to text through a provider (Settings → Speech): OpenAI, Groq, Mistral, Deepgram,
//! ElevenLabs, or your own OpenAI-compatible server. The audio of a dictation, a meeting piece
//! or a file goes there; the live preview still runs on this computer.
//!
//! - Silence is never sent: Silero VAD (the same model as for Whisper) checks the audio first,
//!   so no cost and no text made up from silence ("Thank you.").
//! - Long audio goes in pieces of up to 10 minutes, cut at a pause (as 16-bit WAV, 19 MB: under
//!   the 25 MB that OpenAI and Groq take). Each piece's text comes as soon as it is back.
//! - Times: OpenAI-style servers are asked for segments (`verbose_json`); a model that refuses it
//!   (gpt-4o-transcribe) is asked again for plain text, and that is kept for the next time.
//!   Text without times is spread over the piece by sentence length.
//! - The network, a 429 and a 5xx are tried again (3 times); a refused key is not.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::Value;

use crate::audio;
use crate::engine::{self, EngineEvent, Output, Sink};
use crate::formats::Segment;
use crate::providers::{self, Access, Api};

/// The longest piece in one request.
const MAX_PIECE_S: f64 = 600.0;
const TRIES: u32 = 3;

/// What helps the provider: the language (None = it finds it), the vocabulary as a prompt
/// (OpenAI-style) and as key terms (Deepgram).
#[derive(Default)]
pub struct Hints<'a> {
    pub language: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub terms: &'a [String],
}

/// One answer: its text, the language, and segments with times from the start of the piece.
#[derive(Debug, Default, PartialEq)]
struct Piece {
    text: String,
    language: Option<String>,
    segments: Vec<Segment>,
}

pub fn transcribe(a: &Access, pcm: &[f32], hints: &Hints, vad: Option<&Path>, sink: &Sink) -> anyhow::Result<Output> {
    let rate = audio::RATE as f64;
    let duration = pcm.len() as f64 / rate;
    sink(EngineEvent::Transcribing);
    let speech = |p: &[f32]| vad.map_or(true, |v| engine::has_speech(v, p));
    if pcm.is_empty() || !speech(pcm) {
        return Ok(Output { language: None, duration, segments: vec![] });
    }
    let pieces = engine::join_and_split(&[(0, pcm.len())], pcm, (MAX_PIECE_S * rate) as usize);
    let client = Client::builder().connect_timeout(Duration::from_secs(15)).timeout(Duration::from_secs(300)).build()?;
    let (mut segments, mut language) = (Vec::new(), None);
    for (n, &(from, to)) in pieces.iter().enumerate() {
        let part = &pcm[from..to];
        if pieces.len() == 1 || speech(part) {
            let wav = wav(part)?;
            let got = with_retries(|| request(&client, a, &wav, part.len() as f64 / rate, hints))?;
            let (start, end) = (from as f64 / rate, to as f64 / rate);
            if language.is_none() {
                if let Some(l) = got.language {
                    sink(EngineEvent::Language(l.clone(), None));
                    language = Some(l);
                }
            }
            let segs = if got.segments.is_empty() {
                spread(&got.text, start, end)
            } else {
                got.segments
                    .into_iter()
                    .map(|s| {
                        let a = (start + s.start.max(0.0)).min(end);
                        Segment { start: a, end: (start + s.end).clamp(a, end), text: s.text.trim().to_string() }
                    })
                    .filter(|s| !s.text.is_empty())
                    .collect()
            };
            for s in segs {
                sink(EngineEvent::Segment(s.clone()));
                segments.push(s);
            }
        }
        sink(EngineEvent::Progress(((n + 1) as f64 / pieces.len() as f64).min(0.99)));
    }
    Ok(Output { language, duration, segments })
}

/// 16 kHz mono, 16-bit WAV: every provider takes it.
fn wav(pcm: &[f32]) -> anyhow::Result<Vec<u8>> {
    let spec = hound::WavSpec { channels: 1, sample_rate: audio::RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut out = std::io::Cursor::new(Vec::with_capacity(pcm.len() * 2 + 44));
    {
        let mut w = hound::WavWriter::new(&mut out, spec)?;
        for &x in pcm {
            w.write_sample((x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        w.finalize()?;
    }
    Ok(out.into_inner())
}

enum Fail {
    /// Try again (after this long, when the server says so).
    Retry(anyhow::Error, Option<Duration>),
    Stop(anyhow::Error),
}

fn with_retries(mut f: impl FnMut() -> Result<Piece, Fail>) -> anyhow::Result<Piece> {
    let mut wait = Duration::from_secs(1);
    for attempt in 1..=TRIES {
        match f() {
            Ok(p) => return Ok(p),
            Err(Fail::Stop(e)) => return Err(e),
            Err(Fail::Retry(e, _)) if attempt == TRIES => return Err(e),
            Err(Fail::Retry(e, after)) => {
                eprintln!("  speech service: {e:#}; trying again ({attempt}/{TRIES})");
                std::thread::sleep(after.unwrap_or(wait).min(Duration::from_secs(20)));
                wait *= 3;
            }
        }
    }
    unreachable!("the last try returns")
}

/// Send; a success gives the body. The network, 408, 429 and 5xx can be tried again.
fn send(req: RequestBuilder, label: &str) -> Result<(reqwest::StatusCode, String), Fail> {
    let resp = req.send().map_err(|e| Fail::Retry(anyhow::anyhow!("could not reach {label}: {e}"), None))?;
    let status = resp.status();
    let after = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
    let body = resp.text().map_err(|e| Fail::Retry(anyhow::anyhow!("{label}: the answer was cut ({e})"), None))?;
    if status.is_success() {
        return Ok((status, body));
    }
    let e = anyhow::anyhow!("{}", providers::explain(label, status, &body));
    if status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error() {
        return Err(Fail::Retry(e, after));
    }
    Ok((status, body))
}

fn request(client: &Client, a: &Access, wav: &[u8], seconds: f64, h: &Hints) -> Result<Piece, Fail> {
    match a.provider.speech {
        Some(Api::OpenAi) => openai(client, a, wav, h),
        Some(Api::Deepgram) => deepgram(client, a, wav, h),
        Some(Api::ElevenLabs) => elevenlabs(client, a, wav, seconds, h),
        _ => Err(Fail::Stop(anyhow::anyhow!("{} does not transcribe", a.provider.label))),
    }
}

fn refused(a: &Access, status: reqwest::StatusCode, body: &str) -> Fail {
    Fail::Stop(anyhow::anyhow!("{}", providers::explain(a.provider.label, status, body)))
}

fn audio_part(wav: &[u8]) -> Part {
    Part::bytes(wav.to_vec()).file_name("audio.wav").mime_str("audio/wav").expect("a valid type")
}

/// What an OpenAI-style server takes, by address and model: 0 = segments and the prompt,
/// 1 = no response format (plain JSON), 2 = no prompt either.
fn levels() -> &'static Mutex<HashMap<String, u8>> {
    static L: OnceLock<Mutex<HashMap<String, u8>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

fn openai(client: &Client, a: &Access, wav: &[u8], h: &Hints) -> Result<Piece, Fail> {
    let remember = format!("{} {}", a.url, a.model);
    let mut level = levels().lock().get(&remember).copied().unwrap_or(0);
    loop {
        let mut form = Form::new().text("model", a.model.clone()).part("file", audio_part(wav));
        if level == 0 {
            form = form.text("response_format", "verbose_json").text("timestamp_granularities[]", "segment");
        }
        if let Some(l) = h.language {
            form = form.text("language", l.to_string());
        }
        if let (true, Some(p)) = (level < 2, h.prompt) {
            form = form.text("prompt", p.to_string());
        }
        let req = providers::authorize(client.post(format!("{}/audio/transcriptions", a.url)).multipart(form), Api::OpenAi, &a.key);
        let (status, body) = send(req, a.provider.label)?;
        if status.is_success() {
            if level > 0 {
                levels().lock().insert(remember, level);
            }
            return Ok(parse_openai(&body));
        }
        // A model or a server that does not take segments or a prompt says 400 (or 422).
        if matches!(status.as_u16(), 400 | 422) && level < 2 {
            level += 1;
            continue;
        }
        return Err(refused(a, status, &body));
    }
}

fn parse_openai(body: &str) -> Piece {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Piece { text: body.trim().to_string(), ..Default::default() }; // response_format=text
    };
    Piece {
        text: v["text"].as_str().unwrap_or_default().trim().to_string(),
        language: v["language"].as_str().filter(|l| !l.is_empty()).map(lang_code),
        segments: v["segments"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| Some(Segment { start: s["start"].as_f64()?, end: s["end"].as_f64()?, text: s["text"].as_str()?.trim().to_string() }))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn deepgram(client: &Client, a: &Access, wav: &[u8], h: &Hints) -> Result<Piece, Fail> {
    let mut url = reqwest::Url::parse(&format!("{}/listen", a.url)).map_err(|e| Fail::Stop(anyhow::anyhow!("{}: {e}", a.url)))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("model", &a.model).append_pair("smart_format", "true").append_pair("punctuate", "true").append_pair("utterances", "true");
        match h.language {
            Some(l) => q.append_pair("language", l),
            None => q.append_pair("detect_language", "true"),
        };
        // The vocabulary as key terms (Nova-3).
        if a.model.starts_with("nova-3") {
            for t in h.terms.iter().take(50) {
                q.append_pair("keyterm", t);
            }
        }
    }
    let req = client.post(url).header(reqwest::header::CONTENT_TYPE, "audio/wav").body(wav.to_vec());
    let (status, body) = send(providers::authorize(req, Api::Deepgram, &a.key), a.provider.label)?;
    if !status.is_success() {
        return Err(refused(a, status, &body));
    }
    Ok(parse_deepgram(&body))
}

fn parse_deepgram(body: &str) -> Piece {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    let channel = &v["results"]["channels"][0];
    Piece {
        text: channel["alternatives"][0]["transcript"].as_str().unwrap_or_default().trim().to_string(),
        language: channel["detected_language"].as_str().map(lang_code),
        segments: v["results"]["utterances"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|u| Some(Segment { start: u["start"].as_f64()?, end: u["end"].as_f64()?, text: u["transcript"].as_str()?.trim().to_string() }))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn elevenlabs(client: &Client, a: &Access, wav: &[u8], seconds: f64, h: &Hints) -> Result<Piece, Fail> {
    let mut form = Form::new()
        .text("model_id", a.model.clone())
        .part("file", audio_part(wav))
        .text("tag_audio_events", "false")
        .text("timestamps_granularity", "word");
    if let Some(l) = h.language {
        form = form.text("language_code", l.to_string());
    }
    let req = client.post(format!("{}/speech-to-text", a.url)).multipart(form);
    let (status, body) = send(providers::authorize(req, Api::ElevenLabs, &a.key), a.provider.label)?;
    if !status.is_success() {
        return Err(refused(a, status, &body));
    }
    Ok(parse_elevenlabs(&body, seconds))
}

/// Its words, made into one segment per sentence. A space goes in front of the next word, so a
/// sentence starts when its first word does.
fn parse_elevenlabs(body: &str, seconds: f64) -> Piece {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    let (mut tokens, mut times, mut durations) = (Vec::new(), Vec::new(), Vec::new());
    let mut space = String::new();
    for w in v["words"].as_array().into_iter().flatten().filter(|w| w["type"] != "audio_event") {
        let (Some(text), Some(start)) = (w["text"].as_str(), w["start"].as_f64()) else { continue };
        if w["type"] == "spacing" {
            space.push_str(text);
            continue;
        }
        tokens.push(format!("{}{text}", std::mem::take(&mut space)));
        times.push(start as f32);
        durations.push((w["end"].as_f64().unwrap_or(start) - start).max(0.0) as f32);
    }
    Piece {
        text: v["text"].as_str().unwrap_or_default().trim().to_string(),
        language: v["language_code"].as_str().map(lang_code),
        segments: engine::sentences(&tokens, Some(&times), Some(&durations), 0.0, seconds),
    }
}

/// Text without times: one segment per sentence, each with a share of the piece's time as long
/// as its share of the text.
fn spread(text: &str, start: f64, end: f64) -> Vec<Segment> {
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = text.trim().chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        if matches!(c, '.' | '?' | '!' | '…' | '。' | '？' | '！') && chars.peek().map_or(true, |n| n.is_whitespace()) {
            sentences.push(std::mem::take(&mut cur));
        }
    }
    sentences.push(cur);
    let sentences: Vec<String> = sentences.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let total: usize = sentences.iter().map(|s| s.chars().count()).sum();
    let mut at = start;
    sentences
        .into_iter()
        .map(|s| {
            let len = (end - start) * s.chars().count() as f64 / total.max(1) as f64;
            let seg = Segment { start: at, end: (at + len).min(end), text: s };
            at += len;
            seg
        })
        .collect()
}

/// "english" (OpenAI's verbose_json) → "en"; codes stay as they are.
fn lang_code(name: &str) -> String {
    let l = name.trim().to_ascii_lowercase();
    if l.len() <= 3 {
        return l;
    }
    (0..=whisper_rs::get_lang_max_id())
        .find(|&i| whisper_rs::get_lang_str_full(i) == Some(l.as_str()))
        .and_then(whisper_rs::get_lang_str)
        .map(String::from)
        .unwrap_or(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;

    /// A server on a local port that answers each request in turn with (status, body), and
    /// returns what it got (head and body, as text).
    fn fake_server(answers: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
        let got = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in answers {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 65536];
                loop {
                    let n = s.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let len: usize = text[..i].lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                        if buf.len() >= i + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                seen.push(String::from_utf8_lossy(&buf).to_string());
            }
            seen
        });
        (url, got)
    }

    fn access(provider: &str, url: &str, model: &str) -> Access {
        Access { provider: providers::get(provider).unwrap(), url: url.into(), key: "k-1".into(), model: model.into() }
    }

    fn collect() -> (Sink, Arc<Mutex<Vec<Segment>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        (Arc::new(move |e| {
            if let EngineEvent::Segment(s) = e {
                s2.lock().push(s)
            }
        }), seen)
    }

    #[test]
    fn openai_segments_come_with_their_times_and_the_language_as_a_code() {
        let body = r#"{"text":"Hola. ¿Qué tal?","language":"spanish","segments":[{"start":0.0,"end":1.2,"text":" Hola."},{"start":1.4,"end":2.5,"text":" ¿Qué tal?"}]}"#;
        let (url, got) = fake_server(vec![(200, body.into())]);
        let a = access("groq", &url, "whisper-large-v3-turbo");
        let (sink, seen) = collect();
        let terms = vec!["Lisandro".to_string()];
        let out = transcribe(&a, &vec![0.1; 16_000 * 3], &Hints { language: Some("es"), prompt: Some("Glossary: Lisandro."), terms: &terms }, None, &sink).unwrap();
        assert_eq!(out.language.as_deref(), Some("es"));
        assert_eq!(out.segments.iter().map(|s| (s.start, s.text.as_str())).collect::<Vec<_>>(), vec![(0.0, "Hola."), (1.4, "¿Qué tal?")]);
        assert_eq!(seen.lock().len(), 2, "each segment goes to the sink as it comes");
        let req = &got.join().unwrap()[0];
        assert!(req.starts_with("POST /v1/audio/transcriptions "), "{req}");
        assert!(req.to_ascii_lowercase().contains("authorization: bearer k-1"));
        for part in ["whisper-large-v3-turbo", "verbose_json", "name=\"language\"\r\n\r\nes", "Glossary: Lisandro.", "filename=\"audio.wav\"", "RIFF"] {
            assert!(req.contains(part), "{part} in the request");
        }
    }

    #[test]
    fn a_model_without_segments_is_asked_again_and_that_is_kept() {
        let refusal = r#"{"error":{"message":"response_format 'verbose_json' is not compatible with model 'gpt-4o-transcribe'"}}"#;
        let (url, got) = fake_server(vec![(400, refusal.into()), (200, r#"{"text":"One. Two three."}"#.into()), (200, r#"{"text":"Again."}"#.into())]);
        let a = access("openai", &url, "gpt-4o-transcribe");
        let (sink, _) = collect();
        let out = transcribe(&a, &vec![0.1; 16_000 * 4], &Hints::default(), None, &sink).unwrap();
        // No times from the server: the 4 s are shared by sentence length.
        assert_eq!(out.segments.len(), 2);
        assert_eq!((out.segments[0].text.as_str(), out.segments[0].start, out.segments[1].end), ("One.", 0.0, 4.0));
        // "One." is 4 of the 14 characters: 4/14 of the 4 s.
        assert!((out.segments[1].start - 16.0 / 14.0).abs() < 1e-9, "{:?}", out.segments);
        transcribe(&a, &vec![0.1; 16_000], &Hints::default(), None, &sink).unwrap();
        let reqs = got.join().unwrap();
        assert!(reqs[0].contains("verbose_json") && !reqs[1].contains("verbose_json"));
        assert!(!reqs[2].contains("verbose_json"), "the next request remembers it");
    }

    #[test]
    fn a_refused_key_is_not_tried_again_and_says_why() {
        let (url, got) = fake_server(vec![(401, r#"{"error":{"message":"Incorrect API key provided"}}"#.into())]);
        let (sink, _) = collect();
        let e = transcribe(&access("openai", &url, "whisper-1"), &vec![0.1; 16_000], &Hints::default(), None, &sink).unwrap_err().to_string();
        assert_eq!(e, "OpenAI refused the API key (check it in Settings): Incorrect API key provided");
        assert_eq!(got.join().unwrap().len(), 1);
    }

    #[test]
    fn a_busy_server_is_tried_again() {
        let (url, got) = fake_server(vec![(503, "busy".into()), (200, r#"{"text":"ok"}"#.into())]);
        let (sink, _) = collect();
        let out = transcribe(&access("custom", &url, "whisper-1"), &vec![0.1; 16_000], &Hints::default(), None, &sink).unwrap();
        assert_eq!(out.segments[0].text, "ok");
        assert_eq!(got.join().unwrap().len(), 2);
    }

    #[test]
    fn deepgram_gets_the_audio_as_the_body_and_its_utterances_are_segments() {
        let body = r#"{"results":{"channels":[{"detected_language":"en","alternatives":[{"transcript":"Hello there. Bye."}]}],"utterances":[{"start":0.1,"end":0.9,"transcript":"Hello there."},{"start":1.5,"end":1.9,"transcript":"Bye."}]}}"#;
        let (url, got) = fake_server(vec![(200, body.into())]);
        let (sink, _) = collect();
        let terms = vec!["Lisandro".to_string()];
        let out = transcribe(&access("deepgram", &url, "nova-3"), &vec![0.1; 32_000], &Hints { terms: &terms, ..Default::default() }, None, &sink).unwrap();
        assert_eq!((out.language.as_deref(), out.segments.len(), out.segments[1].start), (Some("en"), 2, 1.5));
        let req = &got.join().unwrap()[0];
        assert!(req.starts_with("POST /v1/listen?model=nova-3&"), "{req}");
        assert!(req.contains("detect_language=true") && req.contains("keyterm=Lisandro") && req.contains("utterances=true"));
        assert!(req.contains("Token k-1") && req.to_ascii_lowercase().contains("content-type: audio/wav"));
    }

    #[test]
    fn elevenlabs_words_become_sentences() {
        let body = r#"{"language_code":"en","text":"Hi there. Bye.","words":[{"text":"Hi","start":0.0,"end":0.3,"type":"word"},{"text":" ","start":0.3,"end":0.35,"type":"spacing"},{"text":"there.","start":0.35,"end":0.8,"type":"word"},{"text":"(laughs)","start":0.8,"end":1.0,"type":"audio_event"},{"text":" ","start":1.0,"end":1.1,"type":"spacing"},{"text":"Bye.","start":1.2,"end":1.5,"type":"word"}]}"#;
        let p = parse_elevenlabs(body, 2.0);
        assert_eq!(p.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Hi there.", "Bye."]);
        assert_eq!((p.segments[0].start, p.segments[1].start), (0.0, 1.2f32 as f64));
        let (url, got) = fake_server(vec![(200, body.into())]);
        let (sink, _) = collect();
        transcribe(&access("elevenlabs", &url, "scribe_v1"), &vec![0.1; 32_000], &Hints::default(), None, &sink).unwrap();
        let req = &got.join().unwrap()[0];
        assert!(req.starts_with("POST /v1/speech-to-text ") && req.contains("xi-api-key: k-1") && req.contains("scribe_v1"), "{req}");
    }

    #[test]
    fn long_audio_goes_in_pieces_with_their_offsets() {
        let piece = |t: &str| (200, format!(r#"{{"text":"{t}","segments":[{{"start":1.0,"end":2.0,"text":"{t}"}}]}}"#));
        let (url, got) = fake_server(vec![piece("first"), piece("second")]);
        let (sink, _) = collect();
        let pcm = vec![0.1; (audio::RATE as f64 * (MAX_PIECE_S + 60.0)) as usize];
        let out = transcribe(&access("groq", &url, "whisper-large-v3"), &pcm, &Hints::default(), None, &sink).unwrap();
        assert_eq!(got.join().unwrap().len(), 2);
        assert_eq!(out.segments[0].start, 1.0);
        assert!(out.segments[1].start > MAX_PIECE_S * 0.75, "the second piece's time counts from its start: {:?}", out.segments[1]);
    }

    #[test]
    fn spread_and_language_names() {
        assert!(spread("", 0.0, 3.0).is_empty());
        let s = spread("Uno dos. Tres…  Cuatro? v1.2 sale", 10.0, 20.0);
        assert_eq!(s.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["Uno dos.", "Tres…", "Cuatro?", "v1.2 sale"]);
        assert_eq!((s[0].start, s.last().unwrap().end.round()), (10.0, 20.0));
        assert_eq!(lang_code("english"), "en");
        assert_eq!(lang_code("Spanish"), "es");
        assert_eq!(lang_code("eng"), "eng");
        assert_eq!(lang_code("klingon"), "klingon");
        let w = wav(&[0.0, 1.0, -1.0]).unwrap();
        assert_eq!((&w[..4], w.len()), (&b"RIFF"[..], 44 + 6));
    }
}
