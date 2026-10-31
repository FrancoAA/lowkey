"""Transcript export formats: plain text, SubRip (.srt) and WebVTT (.vtt)."""

from __future__ import annotations

from collections.abc import Iterable
from typing import TypedDict


class Segment(TypedDict):
    start: float
    end: float
    text: str


def _stamp(seconds: float, sep: str) -> str:
    ms = max(0, round(seconds * 1000))
    h, ms = divmod(ms, 3_600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d}{sep}{ms:03d}"


def to_txt(segments: Iterable[Segment]) -> str:
    return " ".join(s["text"].strip() for s in segments if s["text"].strip()).strip() + "\n"


def to_srt(segments: Iterable[Segment]) -> str:
    blocks = []
    for i, s in enumerate((s for s in segments if s["text"].strip()), start=1):
        blocks.append(f"{i}\n{_stamp(s['start'], ',')} --> {_stamp(s['end'], ',')}\n{s['text'].strip()}\n")
    return "\n".join(blocks)


def to_vtt(segments: Iterable[Segment]) -> str:
    cues = [
        f"{_stamp(s['start'], '.')} --> {_stamp(s['end'], '.')}\n{s['text'].strip()}\n"
        for s in segments
        if s["text"].strip()
    ]
    return "WEBVTT\n\n" + "\n".join(cues)
