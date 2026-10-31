from openwhisper.formats import to_srt, to_txt, to_vtt

SEGS = [
    {"start": 0.0, "end": 1.234, "text": " Hi. "},
    {"start": 3661.5, "end": 3662.0, "text": "Later"},
    {"start": 4000.0, "end": 4001.0, "text": "   "},
]


def test_txt_joins_and_skips_blank():
    assert to_txt(SEGS) == "Hi. Later\n"


def test_srt_numbering_and_hours():
    assert to_srt(SEGS) == "1\n00:00:00,000 --> 00:00:01,234\nHi.\n\n2\n01:01:01,500 --> 01:01:02,000\nLater\n"


def test_vtt_header_and_dot_millis():
    assert to_vtt(SEGS) == "WEBVTT\n\n00:00:00.000 --> 00:00:01.234\nHi.\n\n01:01:01.500 --> 01:01:02.000\nLater\n"
