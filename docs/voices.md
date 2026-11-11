# Who is speaking: measurements

Meetings record two sources, the microphone ("Me") and the computer's sound ("Them"). All the
others in a call were one "Them". With Settings → Meetings → Who is speaking, the app tells them
apart by voice after Stop (`desktop/src-tauri/src/voices.rs`). This page records why it works
the way it does. Script: [bench/voices/measure.py](../bench/voices/measure.py).

## Test data

Two meetings of the [AMI corpus](https://groups.inf.ed.ac.uk/ami/corpus/) (CC BY 4.0) with
known speaker times ([pyannote's AMI setup](https://github.com/pyannote/AMI-diarization-setup),
`only_words` RTTM, test set): ES2004a (17.5 min) and IS1009a (14 min), four people each, the
Mix-Headset audio. AMI is quiet (speech near the app's silence limit), so it is raised to a
call level first. Three cases for each meeting:

- **room4**: all four people.
- **call3**: the person who talks most is removed (as "Me"), so three people are left, as on
  the other side of a call.
- **call3_opus**: call3 through Opus at 24 kbit/s, as a call app sends it.

Score: of the time when exactly one person speaks, the share that gets the right person, after
the best one-to-one match of the found speakers to the real ones. Time with no label takes
the nearest label, as a transcript line does. One label for everyone scores 46–66% (54% mean).

## Results (mean of the six cases)

| Method | Fingerprint model | Setting | Score |
|---|---|---|---|
| One label for all | – | – | 0.54 |
| Live, one label per app piece (4–20 s) | TitaNet-S | 0.4–0.6 | 0.76 |
| Live, one label per speech part (silero VAD) | TitaNet-S | 0.3 | 0.82 |
| **After Stop** (pyannote segmentation 3.0 + clustering) | **TitaNet-S** | **1.0** | **0.86** |
| After Stop, the real number of people given | TitaNet-S | – | 0.81 |
| After Stop | WeSpeaker ResNet34 | 0.8 | 0.67 |
| After Stop | 3D-Speaker ERes2Net (zh) | 0.9 | 0.63 |
| After Stop | 3D-Speaker CAM++ (en) | 0.9 | 0.44 (at the AMI level) |

- CAM++ did not separate these voices (0.59 same person against 0.49 different people). The
  other three did (0.65–0.71 against 0.15–0.25).
- After Stop at 1.0 found too many people: up to 11 for 3. Most had a few seconds of speech.
  Dropping a "person" with under 5% of the speech time (their lines go to the nearest kept
  voice) gave the right number, or one more, in all six cases, and a mean of **0.89**:
  ES2004a 0.96 / 0.88 / 0.93, IS1009a 0.92 / 0.78 / 0.85 (room4 / call3 / call3_opus). The app
  also keeps a person with 60 s or more, for long meetings (not measured: these meetings are
  short).
- The settings were chosen on the same two meetings that they were scored on, so the numbers
  are a little high.
- The app's own Rust code (16-bit temporary file) on ES2004a call3_opus: 0.88, against 0.90 in
  Python with float audio (before the 5% rule).

Why after Stop and not live: live labels were almost as good on average, but they merged two
or three people into one in IS1009a, and a merge cannot be undone by the user. After Stop errs
the other way (one person found twice), which a rename to the same name fixes.

## Speed

On the development machine (Ryzen 7 H 255, CPU, 8 threads): 30–35 s for 17.5 min of meeting,
about 2 min for one hour. Live labels would cost 0.5% of real time.

## Models

| File | Model | Size | Licence | Source (pinned) |
|---|---|---|---|---|
| segmentation-3.0.onnx | pyannote segmentation 3.0 | 6.0 MB | MIT | csukuangfj/sherpa-onnx-pyannote-segmentation-3-0 @ 9403a69 |
| titanet-small.onnx | NVIDIA TitaNet-S | 40.3 MB | NeMo toolkit (Apache-2.0) | csukuangfj/speaker-embedding-models @ 0743f30 |

Both are checked with SHA-256 after the download (`voices.rs`).

## Limits

- Voices that are alike, people who talk at the same time, and poor call audio give errors.
- A line that has two voices goes to the voice that covers most of it (lines are not split).
- A person who says little (under 5% of the time and under 60 s) gets the nearest voice's label.
- Not tested on a real call recording yet.

## Run it again

```sh
uv venv -p 3.12 venv && VIRTUAL_ENV=venv uv pip install sherpa-onnx==2.0.1 numpy scipy soundfile
# data/: ES2004a.wav and IS1009a.wav (amicorpus/<id>/audio/<id>.Mix-Headset.wav) and their
# .rttm (AMI-diarization-setup/only_words/rttms/test/)
# models/: sherpa-onnx-pyannote-segmentation-3-0/model.onnx and the *.onnx speaker models from
# the sherpa-onnx releases (speaker-segmentation-models, speaker-recongition-models)
venv/bin/python bench/voices/measure.py <dir> offline titanet_s 1.0
```
