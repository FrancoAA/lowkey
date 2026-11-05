"""Speaker labels for meetings: measure quality and speed on AMI meetings with known speakers.

Conditions:  room4      the AMI mix (4 people, one room)
             call3      the most talkative person removed (as "Me"): the others' stream, 3 people
             call3_opus call3 through Opus 24 kbit/s (as a call app sends it)
Methods:     offline    sherpa-onnx full diarization after Stop (pyannote segmentation 3.0 + embedding)
             live_piece one label per app piece (the app's own cut at pauses, 4-20 s)
             live_vad   one label per speech part inside a piece (silero VAD), as for one line
Score: of the time where exactly one person speaks, the share given to the right person (after
the best 1-to-1 match of found speakers to real ones). Found: speakers with 10+ s of time.
Results and the steps to set it up: docs/voices.md.

    python measure.py <dir> offline titanet_s 0.9,1.0,1.1   # the pass after Stop
    python measure.py <dir> live titanet_s 0.3,0.4          # live labels (not in the app)

<dir> has data/<meeting>.wav + .rttm and models/ (see docs/voices.md).
"""
import json, subprocess, sys, time
import numpy as np, soundfile as sf, sherpa_onnx as so
from scipy.optimize import linear_sum_assignment

D = sys.argv[1] if len(sys.argv) > 1 else "."
R = 16000
FR = 0.01  # score frame, s
THREADS = 8


def load(path):
    a, r = sf.read(path, dtype="float32", always_2d=True)
    a = a.mean(axis=1)
    assert r == R, r
    return a


def rttm(path):
    out = []
    for l in open(path):
        p = l.split()
        out.append((float(p[3]), float(p[3]) + float(p[4]), p[7]))
    return out


def opus(a):
    enc = subprocess.run(["ffmpeg", "-v", "error", "-f", "f32le", "-ar", str(R), "-ac", "1", "-i", "-", "-c:a", "libopus",
                          "-b:a", "24k", "-application", "voip", "-f", "ogg", "-"], input=a.tobytes(), capture_output=True, check=True).stdout
    dec = subprocess.run(["ffmpeg", "-v", "error", "-i", "-", "-f", "f32le", "-ar", str(R), "-ac", "1", "-"], input=enc,
                         capture_output=True, check=True).stdout
    b = np.frombuffer(dec, dtype=np.float32)
    return np.pad(b, (0, max(0, len(a) - len(b))))[: len(a)].copy()


def conditions(name):
    a = load(f"{D}/data/{name}.wav")
    ref = rttm(f"{D}/data/{name}.rttm")
    talk = {}
    for s, e, w in ref:
        talk[w] = talk.get(w, 0) + e - s
    me = max(talk, key=talk.get)
    # AMI is very quiet (speech near the app's silence limit): raise it to a call level, speech
    # frames at a median RMS of 0.03, with a hard limit at 0.99.
    n = len(a) // 160
    fr = np.sqrt(np.mean(a[: n * 160].reshape(n, 160) ** 2, 1))
    sp = np.zeros(n, bool)
    for s, e, w in ref:
        sp[int(s * 100): int(e * 100)] = True
    a = np.clip(a * (0.03 / float(np.median(fr[sp]))), -0.99, 0.99).astype(np.float32)
    b = a.copy()
    for s, e, w in ref:
        if w == me:
            b[int(max(0, s - 0.1) * R): int((e + 0.1) * R)] = 0
    ref3 = [(s, e, w) for s, e, w in ref if w != me]
    return {"room4": (a, ref), "call3": (b, ref3), "call3_opus": (opus(b), ref3)}


def frames_ref(ref, n):
    m = {}
    for s, e, w in ref:
        m.setdefault(w, np.zeros(n, bool))[int(s / FR): int(e / FR)] = True
    return m


def score(ref, hyp, dur):
    """hyp: list of (start, end, label)."""
    n = int(dur / FR) + 1
    rm = frames_ref(ref, n)
    names = sorted(rm)
    stack = np.stack([rm[k] for k in names])
    one = stack.sum(0) == 1
    who = np.where(one, stack.argmax(0), -1)
    h = np.full(n, -1)
    labels = sorted({l for _, _, l in hyp})
    li = {l: i for i, l in enumerate(labels)}
    for s, e, l in hyp:
        h[int(s / FR): int(e / FR)] = li[l]
    # Time with no label (between offline segments) takes the nearest label, as a line would.
    if (h >= 0).any():
        idx = np.where(h >= 0)[0]
        near = idx[np.clip(np.searchsorted(idx, np.arange(n)), 0, len(idx) - 1)]
        near2 = idx[np.clip(np.searchsorted(idx, np.arange(n)) - 1, 0, len(idx) - 1)]
        pick = np.where(np.abs(near - np.arange(n)) <= np.abs(near2 - np.arange(n)), near, near2)
        h = np.where(h >= 0, h, h[pick])
    mask = one
    c = np.zeros((max(1, len(labels)), len(names)))
    for i in range(len(labels)):
        for j in range(len(names)):
            c[i, j] = np.sum(mask & (h == i) & (who == j))
    r, k = linear_sum_assignment(-c)
    acc = c[r, k].sum() / max(1, mask.sum())
    found = sum(1 for l in labels if sum(e - s for s, e, x in hyp if x == l) >= 10)
    return acc, found, len(names)


# ---------- embeddings and models ----------

EMB = {
    "campplus": "3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx",
    "resnet34": "wespeaker_en_voxceleb_resnet34_LM.onnx",
    "titanet_s": "nemo_en_titanet_small.onnx",
    "eres2net_zh": "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
}
SEG = f"{D}/models/sherpa-onnx-pyannote-segmentation-3-0/model.onnx"


def extractor(e):
    return so.SpeakerEmbeddingExtractor(so.SpeakerEmbeddingExtractorConfig(model=f"{D}/models/{EMB[e]}", num_threads=THREADS))


def embed(x, pcm):
    st = x.create_stream()
    st.accept_waveform(sample_rate=R, waveform=pcm)
    st.input_finished()
    if not x.is_ready(st):
        return None
    v = np.array(x.compute(st))
    return v / (np.linalg.norm(v) + 1e-9)


def offline(a, e, threshold=0.5, num=-1):
    cfg = so.OfflineSpeakerDiarizationConfig(
        segmentation=so.OfflineSpeakerSegmentationModelConfig(
            pyannote=so.OfflineSpeakerSegmentationPyannoteModelConfig(model=SEG), num_threads=THREADS),
        embedding=so.SpeakerEmbeddingExtractorConfig(model=f"{D}/models/{EMB[e]}", num_threads=THREADS),
        clustering=so.FastClusteringConfig(num_clusters=num, threshold=threshold),
        min_duration_on=0.3, min_duration_off=0.5)
    sd = so.OfflineSpeakerDiarization(cfg)
    res = sd.process(a).sort_by_start_time()
    return [(r.start, r.end, r.speaker) for r in res]


# ---------- the app's piece cut (port of meeting.rs Chunker) ----------
FRAME = R // 10
SILENCE = 0.003
PAUSE_FRAMES = 6
LEAD = 3


def rms(f):
    return float(np.sqrt(np.mean(f * f)))


def loud(v):
    if not v:
        return 0.0
    v = sorted(v)
    return v[min(len(v) * 4 // 5, len(v) - 1)]


def pieces(a, mn=4, mx=20):
    mn, mx = max(mn * R, FRAME * (PAUSE_FRAMES + 1)), max(mx * R, mn * R + 4 * FRAME)
    out, start, pos = [], 0, 0
    buf = np.zeros(0, np.float32)
    block = R // 10 * 2
    while pos < len(a) or len(buf):
        if pos < len(a):
            buf = np.concatenate([buf, a[pos: pos + block]])
            pos += block
        elif len(buf):
            if rms(buf) >= SILENCE:
                out.append((start, buf))
            break
        while True:
            n = len(buf)
            if n < mn:
                break
            fr = [rms(buf[i: i + FRAME]) for i in range(0, n - FRAME + 1, FRAME)]
            i0 = next((i for i, r in enumerate(fr) if r >= SILENCE), None)
            if i0 is None:
                start += len(fr) * FRAME; buf = buf[len(fr) * FRAME:]; continue
            if i0 > LEAD:
                d = (i0 - LEAD) * FRAME; start += d; buf = buf[d:]; continue
            lv = loud([r for r in fr if r >= SILENCE])
            q = max(0.2 * lv, SILENCE)
            cut = None
            if len(fr) >= PAUSE_FRAMES and all(r < q for r in fr[-PAUSE_FRAMES:]):
                cut = len(fr) * FRAME
            elif n >= mx:
                lo, hi = mx * 3 // 4 // FRAME, min(mx // FRAME, len(fr))
                i = min(range(lo, hi), key=lambda k: fr[k])
                cut = i * FRAME + FRAME // 2
            if cut is None:
                break
            out.append((start, buf[:cut])); start += cut; buf = buf[cut:]
    return out


class Online:
    """New voice when no known voice is similar enough; a voice is the mean of its parts."""

    def __init__(self, thr):
        self.thr, self.c, self.n = thr, [], []

    def add(self, v):
        if self.c:
            sims = [float(v @ (c / np.linalg.norm(c))) for c in self.c]
            i = int(np.argmax(sims))
            if sims[i] >= self.thr:
                self.c[i] = self.c[i] + v; self.n[i] += 1
                return i
        self.c.append(v.copy()); self.n.append(1)
        return len(self.c) - 1


def vad_parts(pcm):
    cfg = so.VadModelConfig()
    cfg.silero_vad.model = f"{D}/models/silero_vad.onnx"
    cfg.silero_vad.min_silence_duration = 0.25
    cfg.silero_vad.min_speech_duration = 0.25
    cfg.silero_vad.max_speech_duration = 20
    cfg.sample_rate = R
    v = so.VoiceActivityDetector(cfg, buffer_size_in_seconds=60)
    out = []
    w = 512
    p = np.concatenate([pcm, np.zeros(R, np.float32)])
    for i in range(0, len(p) - w + 1, w):
        v.accept_waveform(p[i: i + w])
        while not v.empty():
            out.append((v.front.start, np.array(v.front.samples, np.float32))); v.pop()
    v.flush()
    while not v.empty():
        out.append((v.front.start, np.array(v.front.samples, np.float32))); v.pop()
    return out


def live(a, x, thr, split):
    cl, hyp, last = Online(thr), [], None
    for st, pcm in pieces(a):
        parts = vad_parts(pcm) if split else [(0, pcm)]
        for s0, p in parts:
            v = embed(x, p) if len(p) >= R // 2 else None
            if v is None:
                lab = last if last is not None else 0
            else:
                lab = cl.add(v)
            last = lab
            s = (st + s0) / R
            hyp.append((s, s + len(p) / R, lab))
    return hyp


if __name__ == "__main__":
    what = sys.argv[2]
    meets = ["ES2004a", "IS1009a"]
    rows = []
    cache = {m: conditions(m) for m in meets}
    if what == "offline":
        models = sys.argv[3].split(",")
        thrs = [float(t) for t in sys.argv[4].split(",")]
        for e in models:
            for m in meets:
                for cond, (a, ref) in cache[m].items():
                    dur = len(a) / R
                    for t in thrs:
                        t0 = time.time(); hyp = offline(a, e, t); dt = time.time() - t0
                        acc, found, real = score(ref, hyp, dur)
                        rows.append(dict(method="offline", model=e, meeting=m, cond=cond, thr=t, acc=round(acc, 3), found=found, real=real, rtf=round(dt / dur, 4)))
                        print(json.dumps(rows[-1]), flush=True)
                    t0 = time.time(); hyp = offline(a, e, num=len({w for *_, w in ref})); dt = time.time() - t0
                    acc, found, real = score(ref, hyp, dur)
                    rows.append(dict(method="offline_known_n", model=e, meeting=m, cond=cond, thr=None, acc=round(acc, 3), found=found, real=real, rtf=round(dt / dur, 4)))
                    print(json.dumps(rows[-1]), flush=True)
    else:
        e = sys.argv[3]
        thrs = [float(t) for t in sys.argv[4].split(",")]
        x = extractor(e)
        for m in meets:
            for cond, (a, ref) in cache[m].items():
                dur = len(a) / R
                for split in (False, True):
                    for t in thrs:
                        t0 = time.time(); hyp = live(a, x, t, split); dt = time.time() - t0
                        acc, found, real = score(ref, hyp, dur)
                        rows.append(dict(method="live_vad" if split else "live_piece", model=e, meeting=m, cond=cond, thr=t, acc=round(acc, 3), found=found, real=real, rtf=round(dt / dur, 4)))
                        print(json.dumps(rows[-1]), flush=True)
