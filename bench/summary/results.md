# Meeting summaries with a small local model — measured

Question: can open-whisper install a model that writes the meeting notes (summary, decisions,
tasks) on the computer, and which one?

Setup: the development machine (Ryzen 7 H 255, Radeon 780M, 30 GB RAM), llama.cpp built with
Vulkan (`bench/.work/llama.cpp`), `llama-server` and its OpenAI-compatible API, Q4_K_M files.
The meeting (`meeting-es.md`) is an 8-minute Spanish planning meeting as open-whisper exports it
(notes, then the Me/Them transcript, 2,770 tokens) with known facts (`expected.json`): 4
decisions, 5 tasks with owner and date, 2 open questions, numbers, and traps ("Them" is three
people; one idea was dropped). `run.py` gives every model the same instructions.

## Candidates (all Apache-2.0)

| Model | File | Hugging Face |
|---|---|---|
| Qwen3.5 2B | 1.3 GB | unsloth/Qwen3.5-2B-GGUF |
| **Qwen3.5 4B** | **2.7 GB** | unsloth/Qwen3.5-4B-GGUF (base: Qwen/Qwen3.5-4B, 262K context) |
| Gemma 4 E4B | 5.0 GB | unsloth/gemma-4-E4B-it-GGUF (base: google/gemma-4-E4B-it) |

## Speed

| Model | Device | Load | Prompt | Writing | 8-min meeting | 1-hour meeting (estimate) |
|---|---|---|---|---|---|---|
| Qwen3.5 2B | GPU (Vulkan) | 1.5 s | 1,232 tok/s | 48 tok/s | 15 s | ~30 s |
| Qwen3.5 4B | GPU (Vulkan) | 2.4 s | 485 tok/s (440 at 16K) | 23 tok/s | 22–27 s | ~75 s |
| Qwen3.5 4B | CPU only | 2.1 s | 327 tok/s | 11.5 tok/s | 61 s | ~2.5 min |
| Gemma 4 E4B | GPU (Vulkan) | 3.9 s | 502 tok/s (445 at 16K) | 22 tok/s | 20–30 s | ~75 s |

1 hour ≈ 20,000 tokens of transcript; `llama-bench` pp16384 gives the long-prompt speed.
Memory: the file size plus a small cache (Qwen3.5 uses mostly linear attention): about 3 GB
for Qwen3.5 4B, 5.5 GB for Gemma 4 E4B, and only while it writes.

## Quality (counted by hand against expected.json)

Run 1 told the model "write in the language of the meeting" in English instructions: Qwen3.5 2B
and Gemma 4 E4B answered in **English**. Run 2 names the language ("Write in Spanish"; the app
knows it from Whisper or from its settings) and asks for a label per number. Run 2:

| Model | Decisions (4) | Tasks (5), right owner | Wrong or invented | Open questions (2) | Numbers |
|---|---|---|---|---|---|
| Qwen3.5 2B | 4 | 3 (Acme call missing) | 4 tasks with a wrong or invented owner (e.g. the security review given to Martín) | 1, plus 2 invented | partly labelled |
| **Qwen3.5 4B** (GPU) | 4 (+ "Team price not decided") | **5** | 1 (the revenue table given to Sofía, not Lucía); "security ticket" in the summary (it was payments) | 2 (pricing in the summary and decisions) | labelled, right |
| Qwen3.5 4B (CPU) | 4 | **5** | 1 (the table given to Me); two odd numbers | 2 | labelled, two wrong labels |
| Gemma 4 E4B | 4 (tasks also listed as decisions) | 4 (the Acme call given to Lucía, in both runs) | 1 | **2** | labelled, right |

The full answers are in `run1/` and `run2/`.

## Result

- **Qwen3.5 4B is the pick**: every task with the right owner (the main thing in meeting
  notes), Spanish without being forced in run 1, a 2.7 GB download, ~25 s for 8 minutes and
  ~75 s for an hour on this GPU, and it still works on a CPU (~1 min for 8 minutes).
- Gemma 4 E4B is as fast and better at open questions and numbers, but it gave the recorder's
  own task to someone else twice, and it is a 5 GB download.
- Qwen3.5 2B is fast but invents owners: not good enough for notes that people act on.
- Small models still make small mistakes (one wrong owner in an extra task). The app must show
  the notes as a draft next to the transcript, and it must never hide the transcript.

## Notes by topic (run3, 2026-11-03)

The app's notes changed from fixed sections (Summary, Decisions, Action items, Open questions,
Numbers) to the meeting's own topics: a `##` section for each topic, a bullet for each
decision, fact, figure or task (the person in bold), one level of sub-bullets. The prompt is
`run3/prompt.txt` (the app's `llm::system_prompt`). Qwen3.5 4B on the GPU, the same meeting,
~30 s for each run. The check: who calls Acme (the recorder: "**Me** (03:50): Yo hablo con Acme").

| Prompt | Temperature | Runs | Acme call for **Me** |
|---|---|---|---|
| Fixed sections (run2's prompt) | 0.2 | 2 (`run3/old-prompt-*.md`) | 2 of 2 |
| By topic, no owner example | 0.2 | 4 (2 in `run3/qwen3.5-4b-*.md`) | 2 of 4 (once given to Sofía only, once to both) |
| By topic, owner rule as its own paragraph | 0.2 | 3 | 2 of 3 |
| **By topic** (`run3/prompt.txt`) | **0** | 1 (`run3/qwen3.5-4b-temp0.md`) | **yes**, and every other owner right |

- Inside a topic, the task sits among the talk about it, and at 0.2 the model sometimes gives
  the recorder's task to the person Me talks to. A separate "Action items" list did not have
  this problem. At temperature 0 the answer is the same each time, and it is right here.
- The app writes the notes at temperature 0; the chat answers at 0.2.
- One meeting only: check the owners in your notes, as before.
