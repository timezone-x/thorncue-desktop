#!/usr/bin/env python3
"""
Reference renderer for FORMAT.md section 7.1-7.2 (audio export): one cue to a 16-bit WAV.

    python render_ref.py render <source.wav> <cue.json> <out.wav>
    python render_ref.py compare <expected.wav> <actual.wav> [--tolerance N]
    python render_ref.py check                 # every case in ../fixtures/render/index.json

Needs numpy; reads and writes WAV with the standard library's `wave`. It reads 16-bit PCM WAV
sources only: decoding other formats is each app's business (7.1 step 1), and the fixtures
use PCM so decoder differences (MP3 priming, resampling) can't creep in. MP3 encoding isn't
here either: it needs LAME, and its exact bytes depend on the encoder version.

When FORMAT.md and this file disagree, FORMAT.md wins and this file has a bug.
"""

from __future__ import annotations

import hashlib
import io
import json
import math
import sys
import wave
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
RENDER_FIXTURES = HERE.parent / "fixtures" / "render"


def rnd(x: float) -> int:
    """Round half away from zero (FORMAT.md 7.1), unlike Python's round()."""
    return int(math.floor(abs(x) + 0.5)) * (1 if x >= 0 else -1)


def kept_segments(trim_start: float, trim_end, cuts, duration: float) -> list[tuple[float, float]]:
    """The parts of the source that play, in source seconds (7.1 step 2)."""
    start = min(max(float(trim_start or 0.0), 0.0), duration)
    end = duration if trim_end is None else min(max(float(trim_end), 0.0), duration)
    if end <= start:
        end = duration
    out = [(start, end)]
    for a, b in cuts or []:
        a, b = float(a), float(b)
        if not b > a:
            continue
        nxt = []
        for s, e in out:
            if b <= s or a >= e:
                nxt.append((s, e))
                continue
            if a > s:
                nxt.append((s, a))
            if b < e:
                nxt.append((b, e))
        out = nxt
    return [(s, e) for s, e in out if e - s > 0.001]


def edit(frames: np.ndarray, sr: int, cue: dict) -> np.ndarray | None:
    """Edited audio (7.1 step 2). frames: float64 array (N, channels). None if nothing is left."""
    n = frames.shape[0]
    segs = kept_segments(cue.get("trim_start", 0.0), cue.get("trim_end"), cue.get("cuts") or [], n / sr)
    if not segs:
        return None
    parts = []
    for i, (s, e) in enumerate(segs):
        a = min(rnd(s * sr), n)
        b = min(rnd(e * sr), n)
        seg = frames[a:b].copy()
        if len(segs) > 1:
            length = seg.shape[0]
            r = min(rnd(0.004 * sr), length // 2)
            if r > 0:
                ramp = (np.arange(r, dtype=np.float64) / r)[:, None]
                if i > 0:
                    seg[:r] *= ramp
                if i < len(segs) - 1:
                    seg[length - r:] *= ramp[::-1]   # last frame is k = 0
        parts.append(seg)
    out = np.concatenate(parts, axis=0)
    return out if out.shape[0] > 0 else None


def envelope(t: np.ndarray, fade_in: float, fs, length: float) -> np.ndarray:
    """Gain 0..1 at times t (7.1 step 3)."""
    env = np.ones_like(t)
    if fade_in > 0:
        env = np.where(t < fade_in, t / fade_in, env)
    if fs is not None:
        span = length - fs
        out = np.clip((length - t) / span, 0.0, 1.0) if span > 0 else np.zeros_like(t)
        env = np.where(t >= fs, out, env)
    return env


def render(frames: np.ndarray, sr: int, cue: dict) -> np.ndarray | None:
    """A cue as it plays, once (7.1). Returns float64 (M, channels), clipped to -1..1, or None."""
    if (cue.get("type") or "audio") != "audio":
        return None
    edited = edit(frames, sr, cue)
    if edited is None:
        return None
    m = edited.shape[0]
    length = m / sr
    fade_in = max(float(cue.get("fade_in", 0.0)), 0.0)
    fade_out = max(float(cue.get("fade_out", 0.0)), 0.0)
    looping = bool(cue.get("loop", False))
    fs = max(length - fade_out, min(fade_in, length)) if (not looping and fade_out > 0) else None
    volume = min(max(float(cue.get("volume", 1.0)), 0.0), 2.0)
    t = np.arange(m, dtype=np.float64) / sr
    gain = volume * envelope(t, fade_in, fs, length)
    return np.clip(edited * gain[:, None], -1.0, 1.0)


def to_pcm16(samples: np.ndarray) -> np.ndarray:
    """7.2: negative x 32768, positive x 32767, rounded half away from zero."""
    scaled = np.where(samples < 0, samples * 32768.0, samples * 32767.0)
    return (np.sign(scaled) * np.floor(np.abs(scaled) + 0.5)).astype("<i2")


def wav_bytes(samples: np.ndarray, sr: int) -> bytes:
    """Canonical 44-byte-header PCM WAV (7.2)."""
    channels = samples.shape[1]
    data = to_pcm16(samples).tobytes()
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(data)
    return buf.getvalue()


def read_wav(path: Path) -> tuple[np.ndarray, int]:
    """16-bit PCM WAV to float64 (N, channels), decoded as sample / 32768 (7.1 step 1)."""
    with wave.open(str(path), "rb") as w:
        if w.getsampwidth() != 2 or w.getcomptype() != "NONE":
            raise SystemExit(f"{path}: render_ref reads 16-bit PCM WAV only")
        ch, sr = w.getnchannels(), w.getframerate()
        raw = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2")
    return raw.reshape(-1, ch).astype(np.float64) / 32768.0, sr


def render_file(source: Path, cue: dict) -> bytes | None:
    frames, sr = read_wav(source)
    out = render(frames, sr, cue)
    return None if out is None else wav_bytes(out, sr)


def compare(expected: bytes, actual: bytes, tolerance: int) -> str | None:
    """None if `actual` matches `expected` per 7.2 (same header and length, samples within
    `tolerance` 16-bit steps); otherwise what's wrong."""
    if expected[:44] != actual[:44]:
        return "header differs (format, rate, channels or length)"
    if len(expected) != len(actual):
        return f"length differs: {len(actual)} bytes, expected {len(expected)}"
    a = np.frombuffer(expected[44:], dtype="<i2").astype(np.int32)
    b = np.frombuffer(actual[44:], dtype="<i2").astype(np.int32)
    worst = int(np.max(np.abs(a - b))) if a.size else 0
    if worst > tolerance:
        i = int(np.argmax(np.abs(a - b)))
        return f"sample {i} is off by {worst} (tolerance {tolerance})"
    return None


def check_fixtures() -> int:
    cases = json.loads((RENDER_FIXTURES / "index.json").read_text())
    failed = 0
    for case in cases:
        src = RENDER_FIXTURES / case["source"]
        got = render_file(src, case["cue"])
        exp_path = RENDER_FIXTURES / case["expected"]
        exp = exp_path.read_bytes()
        problems = []
        if got is None:
            problems.append("rendered nothing")
        else:
            if hashlib.sha256(got).hexdigest() != case["sha256"]:
                problems.append("SHA-256 differs from index.json")
            why = compare(exp, got, case["tolerance"])
            if why:
                problems.append(why)
        if hashlib.sha256(exp).hexdigest() != case["sha256"]:
            problems.append(f"{case['expected']} doesn't match its SHA-256 in index.json")
        print(("FAIL " if problems else "ok   ") + case["name"] + ("" if not problems else ": " + "; ".join(problems)))
        failed += bool(problems)
    return 1 if failed else 0


def main(argv: list[str]) -> int:
    if len(argv) >= 2 and argv[1] == "check":
        return check_fixtures()
    if len(argv) == 5 and argv[1] == "render":
        out = render_file(Path(argv[2]), json.loads(Path(argv[3]).read_text()))
        if out is None:
            print("The cue renders to nothing (no audio, or the edit leaves nothing).")
            return 1
        Path(argv[4]).write_bytes(out)
        return 0
    if len(argv) >= 4 and argv[1] == "compare":
        tol = int(argv[argv.index("--tolerance") + 1]) if "--tolerance" in argv else 1
        why = compare(Path(argv[2]).read_bytes(), Path(argv[3]).read_bytes(), tol)
        print(why or "match")
        return 1 if why else 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
