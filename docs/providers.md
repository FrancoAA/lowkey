# AI providers (optional)

By default everything runs on this computer: the speech model (whisper.cpp or Parakeet) and the
notes model (llama.cpp). You can choose a provider in the cloud, or your own server, for two
jobs, each on its own:

- **Speech** (Settings → Models → Speech to text → **Where it runs**): it transcribes dictations,
  meetings and files.
- **Notes** (Settings → Models → Meeting notes): it writes the meeting notes and
  answers the questions in the chat.

Nothing goes to a provider until you choose it. Settings then says in orange what is sent and
to whom.

## The providers

| Provider | Speech | Notes and chat | API | Default model |
|---|---|---|---|---|
| OpenAI | yes | yes | OpenAI | `gpt-4o-transcribe`, `gpt-5-mini` |
| Anthropic (Claude) | — | yes | Anthropic Messages | `claude-sonnet-5-5` |
| OpenRouter | yes | yes | OpenAI-compatible | `openai/gpt-4o-mini-transcribe`, `moonshotai/kimi-k2-0905` |
| Vercel AI Gateway | — | yes | OpenAI-compatible | `anthropic/claude-sonnet-5.5` |
| Groq | yes | yes | OpenAI-compatible | `whisper-large-v3-turbo`, `openai/gpt-oss-120b` |
| Google Gemini | — | yes | OpenAI-compatible (`/v1beta/openai`) | `gemini-2.5-flash` |
| Mistral | yes | yes | OpenAI-compatible | `voxtral-mini-latest`, `mistral-medium-latest` |
| Deepgram | yes | — | Deepgram `/v1/listen` | `nova-3` |
| ElevenLabs | yes | — | ElevenLabs `/v1/speech-to-text` | `scribe_v1` |
| Your own server | yes | yes | OpenAI-compatible | `whisper-1` (speech) |

The model is a menu of 2–4 models for each provider, the default first, each with a short note
(e.g. "as good, 9 times the price"). The notes say what was measured where there is a
measurement (below, and bench/summary/results.md). **Other model…** takes any name the provider
knows, with its whole list as suggestions; your own server has no menu, its model is typed.
Choosing the default saves no name, so a new default in a later version applies.

When a key is saved, Settings asks the provider for its models. The menu then hides a model the
provider no longer lists; if it is the chosen one, it stays, with a note to choose another. That
request is also the check of the key ("The key works", or why the provider refused it).
OpenRouter and Vercel AI Gateway list their models without a key, so there the key is checked at
first use.

The menus are `chat_models` and `speech_models` in `desktop/src-tauri/src/providers.rs`. Providers
rename and remove models every few months: `cargo test --lib live_providers::the_offered_models_exist
-- --ignored` checks the OpenRouter and Vercel menus against their lists (no key, no cost). For the
other providers, the check against their list happens in Settings, with the user's key.

Your own server: an OpenAI-compatible address, e.g. Ollama (`http://localhost:11434/v1`), LM
Studio (`http://localhost:1234/v1`) for notes, Speaches (`http://localhost:8000/v1`) or LocalAI
for speech. The notes server and the speech server have their own address and key.

To add a provider with an OpenAI-compatible API, add one entry to `providers::ALL` in
`desktop/src-tauri/src/providers.rs`: Settings and the requests follow it.

## Keys

Each provider has one key, shared by Speech and Notes (an OpenAI key typed in Speech also works
for Notes). The keys are in `~/.config/lowkey/config.toml`, written 0600 (readable by you only):

```toml
speech_provider = "openai"      # "" = the model on this computer
speech_model = ""               # "" = the provider's default
ai_engine = "server"
ai_provider = "anthropic"       # custom = your own server (ai_url)

[api_keys]
openai = "sk-…"
anthropic = "sk-ant-…"
custom-speech = ""              # your own speech server, if it asks for a key
```

The page never gets a key back: only which providers have one. The key of your own notes
server from an older version (`ai_key`) moves to `api_keys.custom` at the first start.

## Speech through a provider

`src/cloud_stt.rs`. The live preview in the dictation strip still runs on this computer (Whistle
or Parakeet); only the text that is typed comes from the provider.

- **Silence is never sent.** Silero VAD (the same small model as for Whisper, 0.9 MB) checks
  the audio first: no cost for silence, and no text made up from it ("Thank you.").
- **Audio:** 16 kHz mono 16-bit WAV. Audio longer than 10 minutes goes in pieces, cut at a
  pause (10 minutes is 19 MB, under the 25 MB limit of OpenAI and Groq). Each piece's text shows
  as soon as it is back. A meeting sends its 4–20 s pieces one by one, as with a local model.
- **Times:** OpenAI-compatible servers are asked for segments (`verbose_json`). A model that
  refuses it (`gpt-4o-transcribe`) is asked again for plain text, and a server that refuses the
  prompt is asked again without it; the app keeps that for the next request. Text without times
  is shared over the piece by the length of its sentences. Deepgram gives utterances, ElevenLabs
  words: both give real times.
- **Language:** the one set in Settings → General, else the provider finds it.
- **Vocabulary:** the terms go as the prompt (OpenAI-compatible) or as key terms (Deepgram
  Nova-3). The "sounds like" replacements apply to every provider's text.
- **Errors:** the network, 408, 429 and 5xx are tried 3 times (1 s, then 3 s, or what
  `Retry-After` says, up to 20 s). A refused key is not tried again; the message says which
  provider refused it and why. In a meeting, a piece that fails shows a notice in the meeting.
- The tray menu shows the models in use ("Speech: gpt-4o-transcribe · OpenAI"); they are chosen
  in Settings → Models only.
- A provider is used only after **Test and use** passes in Settings → Models (the key works, or
  your server answers and has the model). Until then the model in use stays. The status of the
  provider in use is tested again when Settings draws it, at most every 30 s.
- AI agents (`lowkey --mcp`, `transcribe_file`) and `lowkey --transcribe` always use the model on
  this computer.

## Notes and chat through a provider

`src/llm.rs`, the same instructions as for the built-in model.

- **OpenAI-compatible:** `POST /chat/completions`, streamed (server-sent events). OpenAI itself
  gets `max_completion_tokens` (its reasoning models refuse `max_tokens`).
- **Anthropic:** `POST /v1/messages`, streamed, with the instructions and the meeting as the
  `system` block, marked for Anthropic's prompt cache: the next question about the same meeting
  reads them from the cache (cheaper and faster).
- **Limits:** up to 32,000 tokens per answer from a provider in the cloud, because a reasoning
  model's thinking counts too. A model with a lower maximum says so in its error, and is asked
  again with that number. Your own server keeps the limits measured for small local models
  (3,000 for notes, 1,500 for an answer).
- **Temperature:** a provider in the cloud gets none (its own default). The built-in model and
  your own server get 0 for the notes and 0.2 for answers; a server that refuses the value is
  asked again without it.
- **Reasoning models:** while the model thinks, the meeting shows "The model thinks before it
  writes…" (or "Thinking…" in the chat). A model that thinks until the limit and writes nothing
  gives an error that says so, not empty notes.

## Measured with OpenRouter (2026-11-11)

The live tests are in `src/live_providers.rs` (ignored by default; they need a key):
`LOWKEY_TEST_OPENROUTER_KEY_FILE=… cargo test --lib live_providers -- --ignored --nocapture`.

- **Speech**, the JFK clip (11 s): `openai/gpt-4o-mini-transcribe` 2.4 s, `openai/whisper-1` 2.5 s
  (with segments and the language), both right word for word. OpenRouter's
  `/audio/transcriptions` takes only OpenAI's speech models; they are not in its `/models` list.
- **Notes:** 9 models compared on the 16-minute Spanish meeting (bench/summary/results.md,
  "Cloud models"). The default, `moonshotai/kimi-k2-0905`, had every fact in 3 of 3 runs, in
  18 s for $0.004; through the app, the first words came after 2 s and the notes after 11 s.
  `anthropic/claude-sonnet-5.5` was as good, at 9 times the price. Reasoning models were slow
  for this job: DeepSeek V4.1 Flash thought 53–94 s (and in a loop at temperature 0), Gemini
  3.8 Flash 33–70 s, Qwen3.8 Flash 8 minutes.
- **Chat** (Kimi K2): an answer in 2–3 s.
- **Cancel** stops the stream at once. A meeting over about four hours is refused as before.
- The meeting's notes say which model wrote them, e.g. "gpt-5-mini (OpenAI)".
- **Vercel AI Gateway** has no Kimi K2 0905 (only the older Kimi K2, not tested), so its default
  is `anthropic/claude-sonnet-5.5`, the other model with every fact in 3 of 3 runs. The old
  default, `openai/gpt-5-mini`, had 12 of 15 facts and thought for 46 s.
