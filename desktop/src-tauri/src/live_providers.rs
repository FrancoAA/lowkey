//! Tests against a real provider (OpenRouter), with a real key and real cost (a fraction of a
//! cent each). Ignored by default; run them with the key in a file:
//!
//! ```sh
//! LOWKEY_TEST_OPENROUTER_KEY_FILE=~/or.key cargo test --lib live_providers -- --ignored --nocapture
//! ```
//!
//! LOWKEY_TEST_CHAT_MODEL and LOWKEY_TEST_SPEECH_MODEL choose other models.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use crate::engine::{EngineEvent, Engines, Input};
use crate::llm;
use crate::providers::{self, Access};

fn key() -> String {
    let file = std::env::var("LOWKEY_TEST_OPENROUTER_KEY_FILE").expect("LOWKEY_TEST_OPENROUTER_KEY_FILE: a file with the OpenRouter key");
    std::fs::read_to_string(file).expect("the key file").trim().to_string()
}

fn chat_model() -> String {
    std::env::var("LOWKEY_TEST_CHAT_MODEL").unwrap_or_else(|_| "moonshotai/kimi-k2-0905".into())
}

fn speech_model() -> String {
    std::env::var("LOWKEY_TEST_SPEECH_MODEL").unwrap_or_else(|_| "openai/gpt-4o-mini-transcribe".into())
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn openrouter(model: &str) -> Access {
    let keys = BTreeMap::from([("openrouter".to_string(), key())]);
    let cfg = crate::config::Config { ai_provider: "openrouter".into(), ai_model: model.into(), api_keys: keys, ..Default::default() };
    providers::chat_access(&cfg).unwrap()
}

#[test]
#[ignore]
fn openrouter_lists_its_models_and_checks_the_key() {
    let a = openrouter("");
    let chat = providers::list_models(&a, false).unwrap();
    eprintln!("{} chat models", chat.len());
    assert!(chat.contains(&chat_model()), "{} is in the list", chat_model());
    let speech = providers::list_models(&a, true).unwrap();
    eprintln!("speech: {speech:?}");
    assert_eq!(speech[0], "openai/gpt-4o-mini-transcribe");
}

/// The menus offer only models that exist. OpenRouter and Vercel list theirs without a key (no
/// cost); OpenRouter's speech models are not in its list.
#[test]
#[ignore]
fn the_offered_models_exist() {
    for id in ["openrouter", "vercel"] {
        let p = providers::get(id).unwrap();
        let a = Access { provider: p, url: p.url.into(), key: String::new(), model: String::new() };
        let list = providers::list_models(&a, false).unwrap();
        let missing: Vec<_> = p.chat_models.iter().map(|m| m.id).filter(|m| !list.iter().any(|l| l == m)).collect();
        eprintln!("{id}: {} chat models", list.len());
        assert!(missing.is_empty(), "{id} does not list {missing:?}");
    }
}

/// Through the engines, as a dictation does: the `cloud:` id (its model name has a slash), the
/// keys, the VAD, the vocabulary.
#[test]
#[ignore]
fn openrouter_transcribes_through_the_engines() {
    let engines = Engines::new(0);
    engines.set_cloud(&BTreeMap::from([("openrouter".to_string(), key())]), "");
    engines.set_vocabulary(&[crate::vocabulary::Term { term: "compatriots".into(), sounds_like: vec!["fellow Americans".into()] }]);
    for model in [speech_model(), "openai/whisper-1".to_string()] {
        let id = format!("cloud:openrouter/{model}");
        engines.prepare(&id).unwrap();
        let live = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let l2 = live.clone();
        let t = Instant::now();
        let out = engines
            .transcribe(&id, Input { path: Some(root().join("tests/fixtures/jfk.ogg")), pcm: None, language: None }, Arc::new(move |e| {
                if let EngineEvent::Segment(s) = e {
                    l2.lock().push(s.text)
                }
            }))
            .unwrap();
        let text = crate::formats::to_txt(&out.segments);
        eprintln!("{model}: {:.1} s, language {:?}\n{:#?}", t.elapsed().as_secs_f64(), out.language, out.segments);
        assert!(text.to_lowercase().contains("ask not what your country"), "{text}");
        assert!(text.contains("compatriots"), "the vocabulary applies: {text}");
        assert_eq!(live.lock().len(), out.segments.len(), "every segment went to the page as it came");
        assert!(out.segments.windows(2).all(|w| w[0].start <= w[1].start) && out.segments.last().unwrap().end <= out.duration + 0.01);
    }
    // Silence is never sent: no request, no text.
    let out = engines.transcribe(&format!("cloud:openrouter/{}", speech_model()), Input { path: None, pcm: Some(vec![0.0; 16_000 * 3]), language: None }, Arc::new(|_| {})).unwrap();
    assert!(out.segments.is_empty());
}

#[test]
#[ignore]
fn openrouter_writes_the_notes_and_answers() {
    let engine = llm::Engine::Server { access: openrouter(&chat_model()) };
    let input = std::fs::read_to_string(root().join("bench/summary/meeting-es.md")).unwrap();
    let t = Instant::now();
    let (mut first, mut updates, mut steps) = (None, 0, Vec::new());
    let notes = llm::write_notes(&engine, &input, "Spanish", &["Martín".into()], "", &mut |w, _, _| steps.push(w.to_string()), &mut |_| {
        first.get_or_insert_with(|| t.elapsed());
        updates += 1;
    }, &AtomicBool::new(false))
    .unwrap();
    eprintln!("notes: {:.1} s (first words after {:.1} s, {updates} updates; steps {steps:?})\n{notes}\n", t.elapsed().as_secs_f64(), first.unwrap_or_default().as_secs_f64());
    assert!(notes.contains("## ") && notes.contains("Martín"), "sections, names");
    assert!(notes.lines().any(|l| l.trim_start().starts_with("- ") && l.trim_end().ends_with(')')), "bullets with their times");
    assert!(updates > 1, "it streamed");

    let t = Instant::now();
    let meeting = format!("{input}\n\n## AI notes\n\n{notes}");
    let answer = llm::chat(&engine, &meeting, &[], "¿Cuándo sale la versión 2.4 y quién avisa a Acme?", &["Martín".into()], &mut |_, _, _| {}, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    eprintln!("answer: {:.1} s\n{answer}", t.elapsed().as_secs_f64());
    assert!(!answer.is_empty() && !answer.contains("length limit"));
}

/// A wrong key says so, and names the provider.
#[test]
#[ignore]
fn openrouter_refuses_a_wrong_key_clearly() {
    let mut a = openrouter(&chat_model());
    a.key = "sk-or-v1-wrong".into();
    let e = llm::write_notes(&llm::Engine::Server { access: a.clone() }, "**Me** (00:01): Hola.", "Spanish", &[], "", &mut |_, _, _| {}, &mut |_| {}, &AtomicBool::new(false)).unwrap_err();
    eprintln!("notes: {e}");
    assert!(e.to_string().starts_with("OpenRouter refused the API key"), "{e}");
    let speech = providers::speech_access(&format!("cloud:openrouter/{}", speech_model()), &BTreeMap::from([("openrouter".to_string(), "sk-or-v1-wrong".to_string())]), "").unwrap();
    let pcm = crate::audio::decode_file(&root().join("tests/fixtures/jfk.ogg")).unwrap();
    let e = crate::cloud_stt::transcribe(&speech, &pcm, &Default::default(), None, &(Arc::new(|_| {}) as crate::engine::Sink)).unwrap_err();
    eprintln!("speech: {e}");
    assert!(e.to_string().starts_with("OpenRouter refused the API key"), "{e}");
}
