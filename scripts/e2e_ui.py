"""Headless-browser check of the real UI against the real backend and model.

- Starts the app (or attaches to one with --url), waits for the model to be ready.
- File flow: drops a speech file, waits for the transcript, checks Copy and the .srt download.
- Error flow: drops a non-audio file and checks the error message.
- Dictation flow: Chromium's fake microphone plays a speech WAV; holds Space (push-to-talk)
  and checks that the waveform reacts and the transcript comes back; then tap-to-toggle.
- Saves screenshots under docs/screenshots/.

Usage: uv run python scripts/e2e_ui.py [--url http://127.0.0.1:8000] [--browser /usr/bin/chromium]
"""

from __future__ import annotations

import argparse
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

from playwright.sync_api import Page, expect, sync_playwright

ROOT = Path(__file__).resolve().parent.parent
SHOTS = ROOT / "docs" / "screenshots"
SPEECH = ROOT / "tests" / "fixtures" / "hello.ogg"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_http(url: str, timeout: float = 120) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=2)
            return
        except Exception:
            time.sleep(0.3)
    raise SystemExit(f"server at {url} did not come up")


def to_wav(src: Path, dest: Path, pad_seconds: float = 1.0) -> None:
    """Chromium's fake mic wants a WAV; add silence so the loop has a clear gap."""
    import av
    import numpy as np
    import wave

    with av.open(str(src)) as c:
        res = av.AudioResampler(format="s16", layout="mono", rate=48000)
        pcm = []
        for frame in c.decode(audio=0):
            for f in res.resample(frame):
                pcm.append(f.to_ndarray().reshape(-1))
    audio = np.concatenate(pcm + [np.zeros(int(48000 * pad_seconds), dtype=np.int16)])
    with wave.open(str(dest), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(48000)
        w.writeframes(audio.tobytes())


def shot(page: Page, name: str) -> None:
    SHOTS.mkdir(parents=True, exist_ok=True)
    page.screenshot(path=str(SHOTS / name), full_page=True)
    print(f"  screenshot docs/screenshots/{name}")


def check(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionError(msg)
    print(f"  ok  {msg}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", help="attach to a running app instead of starting one")
    ap.add_argument("--browser", default=shutil.which("chromium") or shutil.which("google-chrome"))
    ap.add_argument("--first-run-shot", action="store_true", help="screenshot the model download state if seen")
    args = ap.parse_args()

    server = None
    url = args.url
    if not url:
        port = free_port()
        url = f"http://127.0.0.1:{port}"
        server = subprocess.Popen(
            [sys.executable, "-m", "openwhisper", "--no-browser", "--no-hotkey", "--port", str(port)], cwd=ROOT
        )
    try:
        wait_http(url + "/api/status")
        tmp = Path(tempfile.mkdtemp(prefix="ow-e2e-"))
        mic_wav = tmp / "mic.wav"
        to_wav(SPEECH, mic_wav)
        bad = tmp / "notes.pdf"
        bad.write_bytes(b"%PDF-1.4\n" + b"not audio\n" * 100)

        with sync_playwright() as p:
            browser = p.chromium.launch(
                executable_path=args.browser or None,  # None: Playwright's own Chromium
                headless=True,
                args=[
                    "--use-fake-ui-for-media-stream",
                    "--use-fake-device-for-media-stream",
                    f"--use-file-for-fake-audio-capture={mic_wav}",
                    "--autoplay-policy=no-user-gesture-required",
                ],
            )
            ctx = browser.new_context(viewport={"width": 900, "height": 820}, color_scheme="light", accept_downloads=True)
            ctx.grant_permissions(["microphone", "clipboard-read", "clipboard-write"], origin=url)
            page = ctx.new_page()
            page.on("console", lambda m: m.type == "error" and print("  console error:", m.text))
            page.goto(url)

            print("first load")
            status = page.locator("#model-status")
            if args.first_run_shot:
                try:
                    expect(status).to_have_attribute("data-state", "downloading", timeout=15_000)
                    # Capture mid-download so the screenshot shows real progress.
                    page.wait_for_function(
                        "parseFloat(document.getElementById('model-bar-fill').style.width) >= 30", timeout=10 * 60_000
                    )
                    page.wait_for_timeout(500)  # let the bar's width transition finish
                    shot(page, "00-first-run-downloading.png")
                except AssertionError:
                    print("  (model already present; no download state to capture)")
            expect(status).to_have_attribute("data-state", "ready", timeout=30 * 60_000)
            check(page.locator("#language-select").input_value() == "auto", "language defaults to Auto-detect")
            check(not page.locator("#options").evaluate("e => e.open"), "Options are collapsed by default")
            shot(page, "01-home.png")

            print("file flow (drop a file)")
            page.locator("#file-input").set_input_files(str(SPEECH))
            expect(page.locator("#copy-button")).to_be_enabled(timeout=180_000)
            text = page.locator("#transcript").inner_text()
            check("hello" in text.lower() and "test" in text.lower(), f"transcript came back: {text!r}")
            check("English" in page.locator("#result-meta").inner_text(), "detected language shown")
            page.locator("#copy-button").click()
            clip = page.evaluate("navigator.clipboard.readText()")
            check(clip == text, "Copy puts the transcript on the clipboard")
            with page.expect_download() as dl:
                page.locator("button[data-format=srt]").click()
            srt_path = tmp / "out.srt"
            dl.value.save_as(srt_path)
            check(dl.value.suggested_filename == "hello.srt" and "-->" in srt_path.read_text(), ".srt download works")
            shot(page, "02-transcript.png")

            print("error flow (unsupported file)")
            page.locator("#file-input").set_input_files(str(bad))
            err = page.locator("#error")
            expect(err).to_be_visible()
            check("not an audio or video file" in err.inner_text(), f"clear error: {err.inner_text()!r}")
            shot(page, "04-unsupported-file.png")

            print("dictation flow (hold Space, fake microphone)")
            page.locator("body").click(position={"x": 5, "y": 5})
            page.keyboard.down("Space")
            expect(page.locator("#dictation")).to_have_attribute("data-state", "listening", timeout=10_000)
            page.wait_for_timeout(1800)
            level = page.evaluate("window.__ow.waveform.level")
            check(level > 0.2, f"waveform reacts to mic level (level={level:.2f})")
            check(page.locator("#waveform").is_visible(), "waveform visible while listening")
            check(not page.locator("#error").is_visible(), "no error shown while listening")
            shot(page, "03-dictation-listening.png")
            page.wait_for_timeout(2200)  # let the whole sentence play
            page.keyboard.up("Space")
            expect(page.locator("#dictation")).to_have_attribute("data-state", "idle", timeout=5_000)
            expect(page.locator("#result-title")).to_have_text("Dictation")
            expect(page.locator("#copy-button")).to_be_enabled(timeout=120_000)
            dict_text = page.locator("#transcript").inner_text()
            check("test" in dict_text.lower() or "hello" in dict_text.lower(), f"dictation transcript: {dict_text!r}")
            shot(page, "05-dictation-result.png")

            print("dictation flow (tap Space to start, tap again to stop)")
            page.keyboard.press("Space")
            expect(page.locator("#dictation")).to_have_attribute("data-state", "listening", timeout=10_000)
            page.wait_for_timeout(4000)
            check(page.locator("#dictation").get_attribute("data-state") == "listening", "tap mode keeps listening after key release")
            page.keyboard.press("Space")
            expect(page.locator("#dictation")).to_have_attribute("data-state", "idle", timeout=5_000)
            expect(page.locator("#copy-button")).to_be_enabled(timeout=120_000)
            check(len(page.locator("#transcript").inner_text()) > 3, "tap-mode dictation transcribed")

            print("dictation flow (mic button; a dropped file does not cut it off; Esc cancels)")
            page.locator("#mic-button").click()
            expect(page.locator("#dictation")).to_have_attribute("data-state", "listening", timeout=10_000)
            page.locator("#file-input").set_input_files(str(SPEECH))
            expect(page.locator("#error")).to_contain_text("Finish the dictation first")
            check(page.locator("#dictation").get_attribute("data-state") == "listening", "file drop during dictation keeps the recording")
            page.keyboard.press("Escape")
            expect(page.locator("#dictation")).to_have_attribute("data-state", "idle", timeout=5_000)

            browser.close()
        print("E2E OK")
    finally:
        if server:
            server.terminate()
            server.wait(10)


if __name__ == "__main__":
    main()
