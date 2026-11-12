//! AI providers in the cloud (or your own server): they can write the meeting notes and answer
//! questions (chat), and transcribe (speech) in place of the models on this computer.
//!
//! Nothing goes to a provider unless you choose it in Settings: Speech → Speech model, or
//! Meetings → Notes. The keys stay in config.toml (0600) and never go to the page.
//!
//! Four kinds of API cover them: OpenAI's (chat completions and audio transcriptions; also
//! OpenRouter, Vercel AI Gateway, Groq, Gemini, Mistral and servers such as Ollama), Anthropic's
//! Messages API, Deepgram's and ElevenLabs'.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Api {
    /// `/chat/completions`, `/audio/transcriptions`, `/models`, with a bearer key.
    OpenAi,
    /// `/messages`, `/models`, with `x-api-key`.
    Anthropic,
    /// `/listen` (the audio as the body), with `Token <key>`.
    Deepgram,
    /// `/speech-to-text` (multipart), with `xi-api-key`.
    ElevenLabs,
}

/// A model that Settings offers, with what to know about it in a few words.
#[derive(Debug, PartialEq, Serialize)]
pub struct Model {
    pub id: &'static str,
    pub note: &'static str,
}

const fn m(id: &'static str, note: &'static str) -> Model {
    Model { id, note }
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    /// The API's base address; "" for your own server (Settings has its address).
    pub url: &'static str,
    /// It writes notes and answers questions, with this API.
    pub chat: Option<Api>,
    /// The model used when none is chosen ("" = one must be typed).
    pub chat_model: &'static str,
    /// The notes models Settings offers, the default first. Any other name can be typed ("Other
    /// model…"). The notes say what was measured (bench/summary/results.md, "Cloud models").
    pub chat_models: &'static [Model],
    /// It transcribes, with this API.
    pub speech: Option<Api>,
    pub speech_model: &'static str,
    /// The speech models Settings offers, the default first (some APIs have no list of their
    /// own, or one of everything).
    pub speech_models: &'static [Model],
    /// Where to get a key.
    pub keys_url: &'static str,
    /// Its list of models answers without a key: listing them does not check the key.
    pub public_models: bool,
}

/// Your own server: any OpenAI-compatible API (Ollama, LM Studio, LocalAI, Speaches…).
pub const CUSTOM: &str = "custom";

/// The prefix of a speech "model" that a provider runs: `cloud:<provider>/<model>`.
pub const CLOUD_PREFIX: &str = "cloud:";

pub const ALL: &[Provider] = &[
    Provider {
        id: "openai",
        label: "OpenAI",
        url: "https://api.openai.com/v1",
        chat: Some(Api::OpenAi),
        chat_model: "gpt-5-mini",
        chat_models: &[
            m("gpt-5-mini", "thinks first (about 45 s); missed some facts in the test"),
            m("gpt-5.5", "larger, higher price"),
            m("gpt-5-nano", "smallest, lowest price"),
        ],
        speech: Some(Api::OpenAi),
        speech_model: "gpt-4o-transcribe",
        speech_models: &[
            m("gpt-4o-transcribe", "the most accurate"),
            m("gpt-4o-mini-transcribe", "lower price"),
            m("whisper-1", "a time for each sentence"),
        ],
        keys_url: "https://platform.openai.com/api-keys",
        public_models: false,
    },
    Provider {
        id: "anthropic",
        label: "Anthropic (Claude)",
        url: "https://api.anthropic.com/v1",
        chat: Some(Api::Anthropic),
        chat_model: "claude-sonnet-5-5",
        chat_models: &[
            m("claude-sonnet-5-5", "recommended: every fact in the test"),
            m("claude-opus-5-5", "the most capable, highest price"),
            m("claude-haiku-5-5", "fastest, lowest price; can confuse who does what"),
        ],
        speech: None,
        speech_model: "",
        speech_models: &[],
        keys_url: "https://console.anthropic.com/settings/keys",
        public_models: false,
    },
    Provider {
        id: "openrouter",
        label: "OpenRouter",
        url: "https://openrouter.ai/api/v1",
        chat: Some(Api::OpenAi),
        // bench/summary/results.md, "Cloud models": every fact in 3 of 3 runs, 18 s, $0.004.
        chat_model: "moonshotai/kimi-k2-0905",
        chat_models: &[
            m("moonshotai/kimi-k2-0905", "recommended: every fact in the test, fast, low price"),
            m("anthropic/claude-sonnet-5.5", "as good, 9 times the price"),
            m("google/gemini-3.8-flash", "as good, slow (it thinks first)"),
            m("mistralai/mistral-medium-3.1", "the fastest; can drop facts"),
        ],
        // Its /audio/transcriptions takes OpenAI's speech models (not in its /models list).
        speech: Some(Api::OpenAi),
        speech_model: "openai/gpt-4o-mini-transcribe",
        speech_models: &[
            m("openai/gpt-4o-mini-transcribe", "low price"),
            m("openai/gpt-4o-transcribe", "the most accurate"),
            m("openai/whisper-1", "a time for each sentence"),
        ],
        keys_url: "https://openrouter.ai/keys",
        public_models: true,
    },
    Provider {
        id: "vercel",
        label: "Vercel AI Gateway",
        url: "https://ai-gateway.vercel.sh/v1",
        chat: Some(Api::OpenAi),
        // The same models as measured through OpenRouter (Vercel has no Kimi K2 0905).
        chat_model: "anthropic/claude-sonnet-5.5",
        chat_models: &[
            m("anthropic/claude-sonnet-5.5", "recommended: every fact in the test"),
            m("google/gemini-3.8-flash", "as good, slow (it thinks first)"),
            m("moonshotai/kimi-k2", "low price (the older Kimi K2, not tested)"),
            m("openai/gpt-5-mini", "thinks first (about 45 s); missed some facts in the test"),
        ],
        speech: None,
        speech_model: "",
        speech_models: &[],
        keys_url: "https://vercel.com/docs/ai-gateway",
        public_models: true,
    },
    Provider {
        id: "groq",
        label: "Groq",
        url: "https://api.groq.com/openai/v1",
        chat: Some(Api::OpenAi),
        chat_model: "openai/gpt-oss-120b",
        chat_models: &[m("openai/gpt-oss-120b", "fast"), m("openai/gpt-oss-20b", "smaller, faster, lower price")],
        speech: Some(Api::OpenAi),
        speech_model: "whisper-large-v3-turbo",
        speech_models: &[m("whisper-large-v3-turbo", "fast, low price"), m("whisper-large-v3", "the most accurate")],
        keys_url: "https://console.groq.com/keys",
        public_models: false,
    },
    Provider {
        id: "gemini",
        label: "Google Gemini",
        url: "https://generativelanguage.googleapis.com/v1beta/openai",
        chat: Some(Api::OpenAi),
        chat_model: "gemini-2.5-flash",
        chat_models: &[
            m("gemini-2.5-flash", "fast, low price"),
            m("gemini-3.8-flash", "every fact in the test, slow (it thinks first)"),
            m("gemini-2.5-pro", "larger, higher price"),
        ],
        speech: None,
        speech_model: "",
        speech_models: &[],
        keys_url: "https://aistudio.google.com/apikey",
        public_models: false,
    },
    Provider {
        id: "mistral",
        label: "Mistral",
        url: "https://api.mistral.ai/v1",
        chat: Some(Api::OpenAi),
        chat_model: "mistral-medium-latest",
        chat_models: &[
            m("mistral-medium-latest", "fast, low price; can drop facts"),
            m("mistral-large-latest", "larger, higher price"),
            m("mistral-small-latest", "smallest, lowest price"),
        ],
        speech: Some(Api::OpenAi),
        speech_model: "voxtral-mini-latest",
        speech_models: &[m("voxtral-mini-latest", "")],
        keys_url: "https://console.mistral.ai/api-keys",
        public_models: false,
    },
    Provider {
        id: "deepgram",
        label: "Deepgram",
        url: "https://api.deepgram.com/v1",
        chat: None,
        chat_model: "",
        chat_models: &[],
        speech: Some(Api::Deepgram),
        speech_model: "nova-3",
        speech_models: &[m("nova-3", "the newest"), m("nova-2", "the one before")],
        keys_url: "https://console.deepgram.com",
        public_models: false,
    },
    Provider {
        id: "elevenlabs",
        label: "ElevenLabs",
        url: "https://api.elevenlabs.io/v1",
        chat: None,
        chat_model: "",
        chat_models: &[],
        speech: Some(Api::ElevenLabs),
        speech_model: "scribe_v1",
        speech_models: &[m("scribe_v1", "")],
        keys_url: "https://elevenlabs.io/app/settings/api-keys",
        public_models: false,
    },
    Provider {
        id: CUSTOM,
        label: "Your own server",
        url: "",
        chat: Some(Api::OpenAi),
        chat_model: "",
        chat_models: &[],
        speech: Some(Api::OpenAi),
        // Most OpenAI-compatible speech servers take (or ignore) OpenAI's name.
        speech_model: "whisper-1",
        speech_models: &[],
        keys_url: "",
        public_models: false,
    },
];

pub fn get(id: &str) -> Option<&'static Provider> {
    ALL.iter().find(|p| p.id == id)
}

/// A provider that writes notes.
pub fn chat(id: &str) -> Option<&'static Provider> {
    get(id).filter(|p| p.chat.is_some())
}

/// A provider that transcribes.
pub fn speech(id: &str) -> Option<&'static Provider> {
    get(id).filter(|p| p.speech.is_some())
}

/// Where a provider's key is kept in `api_keys`: its id. Your own speech server has a key of
/// its own ("custom-speech"): it is rarely the notes server.
pub fn key_slot(id: &str, for_speech: bool) -> String {
    if id == CUSTOM && for_speech {
        "custom-speech".into()
    } else {
        id.into()
    }
}

/// The key slots that may be saved.
pub fn valid_slot(slot: &str) -> bool {
    slot == "custom-speech" || get(slot).is_some()
}

/// What a request needs: the provider, its address, the key and the model.
#[derive(Debug, Clone, PartialEq)]
pub struct Access {
    pub provider: &'static Provider,
    pub url: String,
    pub key: String,
    pub model: String,
}

impl Access {
    /// The provider's address, or your own server's.
    fn resolve(p: &'static Provider, own_url: &str, keys: &BTreeMap<String, String>, for_speech: bool, model: &str) -> anyhow::Result<Self> {
        let url = if p.id == CUSTOM { own_url.trim() } else { p.url }.trim_end_matches('/').to_string();
        anyhow::ensure!(!url.is_empty(), "Your own server has no address yet: add it in Settings.");
        let key = keys.get(&key_slot(p.id, for_speech)).cloned().unwrap_or_default();
        let place = if for_speech { "Settings → Speech" } else { "Settings → Meetings → Notes" };
        anyhow::ensure!(p.id == CUSTOM || !key.is_empty(), "There is no {} API key yet: add it in {place}.", p.label);
        let default = if for_speech { p.speech_model } else { p.chat_model };
        let model = if model.trim().is_empty() { default } else { model.trim() }.to_string();
        Ok(Self { provider: p, url, key, model })
    }
}

/// The notes provider chosen in Settings → Meetings → Notes (`ai_engine = "server"`).
pub fn chat_access(cfg: &Config) -> anyhow::Result<Access> {
    let p = chat(&cfg.ai_provider).unwrap_or_else(|| get(CUSTOM).expect("custom"));
    Access::resolve(p, &cfg.ai_url, &cfg.api_keys, false, &cfg.ai_model)
}

/// The speech provider chosen in Settings → Speech, as the id the engines get
/// (`cloud:openai/gpt-4o-transcribe`); None: the model on this computer.
pub fn speech_model_id(cfg: &Config) -> Option<String> {
    let p = speech(&cfg.speech_provider)?;
    let model = if cfg.speech_model.trim().is_empty() { p.speech_model } else { cfg.speech_model.trim() };
    Some(format!("{CLOUD_PREFIX}{}/{model}", p.id))
}

/// `cloud:openai/gpt-4o-transcribe` → ("openai", "gpt-4o-transcribe").
pub fn parse_speech_id(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(CLOUD_PREFIX)?.split_once('/')
}

/// The speech access for an id from `speech_model_id`, with the keys and your own server's address.
pub fn speech_access(id: &str, keys: &BTreeMap<String, String>, own_url: &str) -> anyhow::Result<Access> {
    let (pid, model) = parse_speech_id(id).ok_or_else(|| anyhow::anyhow!("Unknown speech service {id:?}"))?;
    let p = speech(pid).ok_or_else(|| anyhow::anyhow!("Unknown speech service {pid:?}"))?;
    Access::resolve(p, own_url, keys, true, model)
}

/// "gpt-4o-transcribe · OpenAI" (history, meetings, the status).
pub fn speech_label(id: &str) -> Option<String> {
    let (pid, model) = parse_speech_id(id)?;
    let p = get(pid)?;
    Some(if p.id == CUSTOM { format!("{model} · your own server") } else { format!("{model} · {}", p.label) })
}

/// True when the address is on this computer (what is sent does not leave it). Parsed as the
/// HTTP client parses it (WHATWG URL), so the host checked is the host reached.
pub fn is_local(url: &str) -> bool {
    use reqwest::Url;
    match Url::parse(url.trim()).ok().and_then(|u| u.host().map(|h| h.to_owned())) {
        Some(url::Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// A request with the provider's way of sending the key.
pub fn authorize(req: reqwest::blocking::RequestBuilder, api: Api, key: &str) -> reqwest::blocking::RequestBuilder {
    if key.is_empty() {
        return req;
    }
    match api {
        Api::OpenAi => req.bearer_auth(key),
        Api::Anthropic => req.header("x-api-key", key).header("anthropic-version", ANTHROPIC_VERSION),
        Api::Deepgram => req.header("Authorization", format!("Token {key}")),
        Api::ElevenLabs => req.header("xi-api-key", key),
    }
}

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A refused request, said for people: the key, the model, the limit, or the server's words.
pub fn explain(label: &str, status: reqwest::StatusCode, body: &str) -> String {
    let said: String = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let e = v.get("error").or_else(|| v.get("detail")).unwrap_or(&v);
            e.get("message").and_then(Value::as_str).or_else(|| e.as_str()).or_else(|| v.get("err_msg").and_then(Value::as_str)).map(String::from)
        })
        .unwrap_or_else(|| body.trim().to_string())
        .chars()
        .take(300)
        .collect();
    let why = match status.as_u16() {
        401 | 403 => format!("{label} refused the API key (check it in Settings)"),
        404 => format!("{label} does not know this model or address"),
        429 => format!("{label} says: too many requests, or no credit left"),
        _ => format!("{label} answered {status}"),
    };
    if said.is_empty() {
        why
    } else {
        format!("{why}: {said}")
    }
}

/// Settings: the provider's models (suggestions for the model field). It is also the check of
/// the key: a refused key is an error. Deepgram and ElevenLabs have no list of speech models:
/// their key is checked, then the known models are offered.
pub fn list_models(a: &Access, for_speech: bool) -> anyhow::Result<Vec<String>> {
    let api = if for_speech { a.provider.speech } else { a.provider.chat }.ok_or_else(|| anyhow::anyhow!("{} does not do this", a.provider.label))?;
    let client = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(30)).build()?;
    let path = match api {
        Api::OpenAi => "/models",
        Api::Anthropic => "/models?limit=1000",
        Api::Deepgram => "/projects",
        Api::ElevenLabs => "/models",
    };
    let resp = authorize(client.get(format!("{}{path}", a.url)), api, &a.key).send().map_err(|e| anyhow::anyhow!("could not reach {}: {e}", a.url))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{}", explain(a.provider.label, status, &body));
    }
    let known: Vec<String> = a.provider.speech_models.iter().map(|m| m.id.to_string()).collect();
    if matches!(api, Api::Deepgram | Api::ElevenLabs) {
        return Ok(known);
    }
    let v: Value = serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("{} sent no list of models", a.provider.label))?;
    let ids = v["data"].as_array().or_else(|| v["models"].as_array()).cloned().unwrap_or_default();
    let mut ids: Vec<String> = ids
        .iter()
        .filter_map(|m| m["id"].as_str().or_else(|| m["name"].as_str()).or_else(|| m.as_str()))
        .map(|id| id.strip_prefix("models/").unwrap_or(id).to_string()) // Gemini
        .filter(|id| if for_speech { is_speech_model(id) } else { is_chat_model(id) })
        .collect();
    ids.sort();
    ids.dedup();
    if for_speech {
        // The known ones first, then the others the server has.
        let mut out = known.clone();
        out.extend(ids.into_iter().filter(|id| !known.contains(id)));
        return Ok(out);
    }
    Ok(ids)
}

fn is_speech_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ["whisper", "transcribe", "voxtral", "stt", "scribe", "parakeet", "canary"].iter().any(|k| id.contains(k)) && !id.contains("tts")
}

/// Leaves out what cannot write text (embeddings, images, speech): only suggestions, any name
/// can be typed.
fn is_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !["embed", "whisper", "tts", "transcribe", "dall-e", "image", "moderation", "realtime", "audio", "rerank", "guard", "imagen", "veo", "speech", "voxtral", "omni-moderation"]
        .iter()
        .any(|k| id.contains(k))
}

/// The providers for the page (no keys: only which slots have one).
pub fn status(keys: &BTreeMap<String, String>) -> Value {
    serde_json::json!({
        "list": ALL,
        "keys": keys.iter().filter(|(_, k)| !k.is_empty()).map(|(s, _)| s.clone()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses() {
        for u in ["http://localhost:11434/v1", "http://127.0.0.1:1234/v1", "http://[::1]:8080/v1", "http://LOCALHOST/v1", "http://user@localhost:1/v1"] {
            assert!(is_local(u), "{u}");
        }
        for u in ["https://api.openai.com/v1", "http://192.168.1.5:11434/v1", "http://localhost.evil.com/v1", "http://127.0.0.1.evil.com/v1x", "https://evil.com/?localhost", "http://evil.com\\@localhost/v1", "not a url"] {
            assert!(!is_local(u), "{u}");
        }
    }

    #[test]
    fn the_registry_is_consistent() {
        let mut ids: Vec<_> = ALL.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ALL.len(), "unique ids");
        for p in ALL {
            assert!(p.chat.is_some() || p.speech.is_some(), "{}: does something", p.id);
            assert!(!p.id.contains('/'), "{}: the speech id splits at the first /", p.id);
            if p.id != CUSTOM {
                assert!(p.url.starts_with("https://") && !p.url.ends_with('/'), "{}", p.id);
                assert!(p.chat.is_none() || !p.chat_model.is_empty(), "{}: a default notes model", p.id);
            }
            assert!(p.speech.is_none() || !p.speech_model.is_empty(), "{}: a default speech model", p.id);
            for (does, default, list) in [(p.chat.is_some(), p.chat_model, p.chat_models), (p.speech.is_some(), p.speech_model, p.speech_models)] {
                assert!(does || list.is_empty(), "{}: offers models for what it does not do", p.id);
                assert!(list.is_empty() || list[0].id == default, "{}: the default ({default}) is the first offered", p.id);
                let mut ids: Vec<_> = list.iter().map(|m| m.id).collect();
                ids.sort();
                ids.dedup();
                assert_eq!(ids.len(), list.len(), "{}: each model once", p.id);
            }
            assert!(p.id == CUSTOM || p.chat.is_none() || !p.chat_models.is_empty(), "{}: notes models to offer", p.id);
        }
        assert!(chat("deepgram").is_none() && speech("anthropic").is_none() && speech("deepgram").is_some());
    }

    #[test]
    fn the_chosen_provider_becomes_an_access() {
        let mut cfg = Config::default();
        assert_eq!(speech_model_id(&cfg), None, "the model on this computer by default");
        cfg.speech_provider = "openai".into();
        assert_eq!(speech_model_id(&cfg).as_deref(), Some("cloud:openai/gpt-4o-transcribe"));
        cfg.speech_model = "Systran/faster-whisper-small".into();
        cfg.speech_provider = CUSTOM.into();
        let id = speech_model_id(&cfg).unwrap();
        assert_eq!(parse_speech_id(&id), Some((CUSTOM, "Systran/faster-whisper-small")), "a model name with a slash");
        cfg.speech_provider = "anthropic".into();
        assert_eq!(speech_model_id(&cfg), None, "Anthropic does not transcribe");

        let mut keys = BTreeMap::new();
        let e = speech_access("cloud:groq/whisper-large-v3", &keys, "").unwrap_err().to_string();
        assert!(e.contains("Groq API key") && e.contains("Settings → Speech"), "{e}");
        keys.insert("groq".to_string(), "gsk-1".to_string());
        let a = speech_access("cloud:groq/whisper-large-v3", &keys, "").unwrap();
        assert_eq!((a.url.as_str(), a.key.as_str(), a.model.as_str()), ("https://api.groq.com/openai/v1", "gsk-1", "whisper-large-v3"));
        // Your own speech server: its own address and key slot, no key needed.
        keys.insert("custom".to_string(), "notes-key".to_string());
        let a = speech_access("cloud:custom/", &keys, "http://localhost:8000/v1/").unwrap();
        assert_eq!((a.url.as_str(), a.key.as_str(), a.model.as_str()), ("http://localhost:8000/v1", "", "whisper-1"));

        // Notes: the provider's default model, the key of that provider.
        cfg.ai_provider = "anthropic".into();
        cfg.api_keys.insert("anthropic".into(), "sk-ant".into());
        let a = chat_access(&cfg).unwrap();
        assert_eq!((a.provider.chat, a.model.as_str(), a.key.as_str()), (Some(Api::Anthropic), "claude-sonnet-5-5", "sk-ant"));
        cfg.ai_provider = "nope".into();
        assert_eq!(chat_access(&cfg).unwrap().provider.id, CUSTOM, "an unknown provider: your own server");
    }

    #[test]
    fn refusals_are_explained() {
        let s = |n| reqwest::StatusCode::from_u16(n).unwrap();
        let e = explain("OpenAI", s(401), r#"{"error":{"message":"Incorrect API key provided"}}"#);
        assert_eq!(e, "OpenAI refused the API key (check it in Settings): Incorrect API key provided");
        assert_eq!(explain("Groq", s(500), ""), "Groq answered 500 Internal Server Error");
        assert!(explain("Deepgram", s(400), r#"{"err_msg":"Bad audio"}"#).ends_with("Bad audio"));
        assert!(explain("x", s(422), r#"{"detail":"bad model"}"#).ends_with("bad model"));
    }

    #[test]
    fn model_lists_keep_what_fits() {
        assert!(is_chat_model("gpt-5-mini") && is_chat_model("anthropic/claude-sonnet-5.5"));
        assert!(!is_chat_model("text-embedding-3-large") && !is_chat_model("whisper-1") && !is_chat_model("gpt-4o-mini-tts"));
        assert!(is_speech_model("whisper-large-v3-turbo") && is_speech_model("gpt-4o-transcribe") && is_speech_model("voxtral-mini-latest"));
        assert!(!is_speech_model("gpt-4o-mini-tts") && !is_speech_model("gpt-5-mini"));
    }
}
