#!/usr/bin/env python3
"""
Rebuild every fixture in ../fixtures from scratch.

    python make_fixtures.py            # needs ffmpeg (with libmp3lame, libvorbis, aac) on PATH

You normally never run this: the fixtures are committed. It exists so anyone can see
exactly how each one was made, and regenerate them if FORMAT.md changes.

All audio is generated (sine tones, DC steps). Never put real show audio in fixtures.
"""

from __future__ import annotations

import json
import shutil
import struct
import subprocess
import sys
import wave
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import tqs_ref as tqs  # noqa: E402

FIX = HERE.parent / "fixtures"
AUDIO = FIX / "audio"
VALID = FIX / "valid"
INVALID = FIX / "invalid"
LEGACY = FIX / "legacy"
EXPECTED = FIX / "expected"
GEN = ("make_fixtures", "1")
FIXED_TIME = "2026-11-12T00:00:00Z"


def ff(args: list[str], out: Path) -> Path:
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args, str(out)], check=True)
    return out


def make_audio() -> dict[str, Path]:
    AUDIO.mkdir(parents=True, exist_ok=True)
    tone = lambda hz, secs: ["-f", "lavfi", "-i", f"sine=frequency={hz}:duration={secs}:sample_rate=22050"]
    a = {
        "tone-a440.wav": ff(tone(440, 2) + ["-ac", "1", "-c:a", "pcm_s16le"], AUDIO / "tone-a440.wav"),
        "tone-e330.mp3": ff(tone(330, 2) + ["-ac", "1", "-c:a", "libmp3lame", "-b:a", "32k"], AUDIO / "tone-e330.mp3"),
        "tone-c262.ogg": ff(tone(262, 2) + ["-ac", "1", "-c:a", "libvorbis", "-q:a", "0"], AUDIO / "tone-c262.ogg"),
        "tone-g392.flac": ff(tone(392, 2) + ["-ac", "1", "-c:a", "flac"], AUDIO / "tone-g392.flac"),
        "tone-d294.m4a": ff(tone(294, 2) + ["-ac", "1", "-c:a", "aac", "-b:a", "32k"], AUDIO / "tone-d294.m4a"),
        "tone-b494.aiff": ff(tone(494, 2) + ["-ac", "1", "-c:a", "pcm_s16be"], AUDIO / "tone-b494.aiff"),
        # 48 kHz stereo, left and right different, to catch channel and resampling bugs.
        "stereo-48k.wav": ff(["-f", "lavfi", "-i", "sine=frequency=440:duration=2:sample_rate=48000",
                              "-f", "lavfi", "-i", "sine=frequency=660:duration=2:sample_rate=48000",
                              "-filter_complex", "[0:a][1:a]join=inputs=2:channel_layout=stereo",
                              "-c:a", "pcm_s16le"], AUDIO / "stereo-48k.wav"),
        # 10 s, the pitch steps up every second: trims and cuts are audible.
        "ladder-10s.mp3": ff(["-f", "lavfi", "-i",
                              "aevalsrc=0.3*sin(2*PI*(220+55*floor(t))*t):s=22050:d=10",
                              "-ac", "1", "-c:a", "libmp3lame", "-b:a", "32k"], AUDIO / "ladder-10s.mp3"),
    }
    # 10 s of DC steps for engine unit tests: 8 kHz mono 16-bit, second n holds the value n * 1000.
    # Trims and cuts can be checked sample-exactly against this file.
    p = AUDIO / "ladder-dc.wav"
    with wave.open(str(p), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(8000)
        w.writeframes(b"".join(struct.pack("<h", (i // 8000) * 1000) for i in range(80000)))
    a["ladder-dc.wav"] = p
    (AUDIO / "not-audio.txt").write_text("This is not audio. It is here to be renamed .wav and rejected.\n")
    a["not-audio.txt"] = AUDIO / "not-audio.txt"
    return a


def cue(cid, ctype="audio", name=None, asset=None, **kw):
    c = {"id": cid, "type": ctype, "name": name or cid, "asset": asset, "volume": 1.0, "fade_in": 0.0,
         "fade_out": 0.0, "pre_wait": 0.0, "duration": 0.0, "loop": False, "continue": "none",
         "trim_start": 0.0, "trim_end": None, "cuts": []}
    c.update(kw)
    return c


def expected(name: str, manifest: dict) -> None:
    """Write the normalised manifest a reader must produce (generator/exported_at removed)."""
    EXPECTED.mkdir(parents=True, exist_ok=True)
    m = {k: v for k, v in manifest.items() if k not in ("generator", "exported_at")}
    (EXPECTED / f"{name}.json").write_text(json.dumps(m, indent=2, ensure_ascii=False) + "\n", "utf-8")


def raw_zip(path: Path, entries: list[tuple], comment: str = "") -> None:
    """entries: (name, bytes, compress_type, extra_kwargs). For hand-made broken packages."""
    with zipfile.ZipFile(path, "w", allowZip64=True) as zf:
        for e in entries:
            name, data = e[0], e[1]
            ctype = e[2] if len(e) > 2 else zipfile.ZIP_DEFLATED
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = ctype
            if len(e) > 3 and e[3].get("symlink"):
                info.external_attr = (0o120777 << 16)
            zf.writestr(info, data)


def manifest_bytes(m: dict) -> bytes:
    return json.dumps(m, indent=2).encode("utf-8")


def build(name: str, show: dict, cues: list, files: dict, normalise_expectation: bool = True) -> dict:
    VALID.mkdir(parents=True, exist_ok=True)
    m = tqs.write_package(VALID / f"{name}.tqs", show, cues, files, generator=GEN)
    # Pin exported_at so the committed fixtures don't change on every rebuild.
    _rewrite_manifest(VALID / f"{name}.tqs", lambda mm: mm.update(exported_at=FIXED_TIME))
    got, _ = tqs.read_package(VALID / f"{name}.tqs")
    if normalise_expectation:
        expected(name, got)
    return got


def _rewrite_manifest(path: Path, fn) -> None:
    """Rewrite manifest.json in place, keeping it the first entry. Used to pin or break fields."""
    tmp = path.with_suffix(".rewrite")
    with zipfile.ZipFile(path) as src, zipfile.ZipFile(tmp, "w", allowZip64=True) as dst:
        for info in src.infolist():
            data = src.read(info)
            if info.filename == "manifest.json":
                m = json.loads(data)
                fn(m)
                data = manifest_bytes(m)
            dst.writestr(info, data)
    tmp.replace(path)


def main() -> None:
    if shutil.which("ffmpeg") is None:
        sys.exit("ffmpeg is needed to rebuild the fixtures.")
    for d in (VALID, INVALID, LEGACY, EXPECTED):
        shutil.rmtree(d, ignore_errors=True)
        d.mkdir(parents=True)
    a = make_audio()
    index = []

    def ok(name, about):
        index.append({"file": f"valid/{name}.tqs", "expect": "accept", "about": about,
                      "expected": f"expected/{name}.json"})

    def bad(name, rule, about):
        index.append({"file": f"invalid/{name}", "expect": "reject", "rule": rule, "about": about})

    # ---------------- valid ----------------
    build("01-minimal", {"name": "Minimal", "author": "", "notes": "", "source": None},
          [cue("q1", name="Tone", asset="tone1")], {"tone1": a["tone-a440.wav"]})
    ok("01-minimal", "One WAV cue. The smallest real show.")

    files = {
        "ladder": a["ladder-10s.mp3"], "oggloop": a["tone-c262.ogg"], "flac1": a["tone-g392.flac"],
        "m4a1": a["tone-d294.m4a"], "aiff1": a["tone-b494.aiff"], "stereo": a["stereo-48k.wav"],
        "dcladder": a["ladder-dc.wav"], "gone": AUDIO / "this-file-does-not-exist.wav",
    }
    cues = [
        cue("q01", name="Edited ladder: trims 1.0 to 8.0, cuts 3-4 and 5.5-6, fades", asset="ladder",
            volume=0.8, fade_in=0.5, fade_out=1.0, pre_wait=0.25, trim_start=1.0, trim_end=8.0,
            cuts=[[3.0, 4.0], [5.5, 6.0]]),
        cue("q02", name="Same file, untouched (shares the asset)", asset="ladder"),
        cue("q03", name="Looping OGG", asset="oggloop", loop=True, fade_in=0.2, fade_out=2.0),
        cue("q04", "fadeout", name="Fade everything over 1.5 s", duration=1.5, **{"continue": "after_end"}),
        cue("q05", "silence", name="Stop all, wait 0.5 s, then continue", duration=0.5, **{"continue": "after_end"}),
        cue("q06", name="FLAC", asset="flac1", volume=2.0),
        cue("q07", name="M4A (AAC)", asset="m4a1", volume=0.0),
        cue("q08", name="AIFF", asset="aiff1"),
        cue("q09", name="48 kHz stereo WAV, L=440 R=660", asset="stereo"),
        cue("q10", name="DC ladder for engine tests: trim 2-9, cut 4-6", asset="dcladder",
            trim_start=2.0, trim_end=9.0, cuts=[[4.0, 6.0]]),
        cue("q11", name="Audio cue whose file was missing at export", asset="gone"),
        cue("q12", name="Cuts remove everything: length 0", asset="flac1", cuts=[[0.0, 5.0]],
            **{"continue": "after_end"}),
        cue("q13", "silence", name="End marker", duration=0.0),
    ]
    build("02-all-features", {"name": "All features", "author": "Fixture", "notes": "Every cue type and option.",
                              "source": None}, cues, files)
    ok("02-all-features", "Every cue type, every format, shared asset, missing asset (asset null), "
                          "trims, cuts, loop, fades, continue.")

    # 03: a messy but acceptable manifest. Readers must normalise it to expected/03-needs-normalising.json.
    p = VALID / "03-needs-normalising.tqs"
    data = a["tone-a440.wav"].read_bytes()
    sha = tqs.hashlib.sha256(data).hexdigest()
    messy = {
        "format": "thorncue.show", "format_version": 1,
        "generator": {"name": "hand-written", "version": "0"}, "exported_at": FIXED_TIME,
        "x_future_field": {"readers": "must ignore this"},
        "show": {"name": "   ", "author": "Someone", "notes": "", "source": None, "x_colour": "plum"},
        "assets": {"t": {"path": "audio/t.wav", "sha256": sha, "bytes": len(data), "x_note": "ignored"}},
        "cues": [
            {"id": "a", "type": "audio", "name": "", "asset": "t", "volume": 3.5, "fade_in": -2,
             "fade_out": 900, "pre_wait": 5000, "trim_start": 1.5, "trim_end": 0.5,
             "cuts": [[1.2, 1.4], [0.2, 0.6], [0.5, 0.8], [0.9, 0.9], [1.0], "x", [-1, 0.1]],
             "x_colour": "red"},
            {"id": "b", "type": "silence"},
            {"id": "c", "type": "fadeout", "duration": 2, "loop": True, "trim_start": 3},
        ],
    }
    raw_zip(p, [("manifest.json", manifest_bytes(messy)), ("audio/", b""),
                ("audio/t.wav", data, zipfile.ZIP_STORED),
                ("audio/unused.wav", data, zipfile.ZIP_STORED),
                ("__MACOSX/audio/._t.wav", b"junk"),
                (".DS_Store", b"junk")])
    got, _ = tqs.read_package(p)
    expected("03-needs-normalising", got)
    ok("03-needs-normalising", "Defaults, clamping, cut merging, unknown fields, junk entries, unused audio, "
                               "manifest not dropping fields it must ignore. Compare with expected/.")

    build("04-empty", {"name": "Empty show", "author": "", "notes": "", "source": None}, [], {})
    ok("04-empty", "No cues, no audio.")

    build("05-from-web", {"name": "From the web", "author": "Connor", "notes": "",
                          "source": {"kind": "thorncue-web", "server": "https://cue.example.com",
                                     "code": "STAGE-4821", "revision": "2026-11-12T03:10:41Z"}},
          [cue("abcdef123456", name="Walk-in", asset="abcdef123456", loop=True)],
          {"abcdef123456": a["tone-e330.mp3"]})
    ok("05-from-web", "Has show.source, as the web's package endpoint writes it.")

    # ---------------- invalid ----------------
    good = json.loads((EXPECTED / "01-minimal.json").read_text())
    good = {"format": "thorncue.show", "format_version": 1, "generator": {"name": "x", "version": "1"},
            "exported_at": FIXED_TIME, **{k: good[k] for k in ("show", "assets", "cues")}}
    wav = a["tone-a440.wav"].read_bytes()

    def variant(name, rule, about, mutate=None, extra=(), drop_audio=False, audio_bytes=wav,
                manifest_raw=None, audio_name="audio/tone1.wav"):
        m = json.loads(json.dumps(good))
        if mutate:
            mutate(m)
        entries = [("manifest.json", manifest_raw if manifest_raw is not None else manifest_bytes(m))]
        if not drop_audio:
            entries.append((audio_name, audio_bytes, zipfile.ZIP_STORED))
        entries += list(extra)
        raw_zip(INVALID / name, entries)
        bad(name, rule, about)

    variant("bad-format-name.tqs", "2.1 format", "format is not thorncue.show",
            lambda m: m.update(format="thorncue.cues"))
    variant("future-version.tqs", "2.1 format_version", "format_version 2: must say 'update ThornCue'",
            lambda m: m.update(format_version=2))
    variant("unknown-cue-type.tqs", "2.6 unknown enum", "cue type 'midi'",
            lambda m: m["cues"][0].update(type="midi"))
    variant("unknown-continue.tqs", "2.6 unknown enum", "continue 'with_start'",
            lambda m: m["cues"][0].update(**{"continue": "with_start"}))
    variant("wrong-type.tqs", "2.4 types", "volume is a string",
            lambda m: m["cues"][0].update(volume="loud"))
    variant("duplicate-cue-id.tqs", "2.4 id", "two cues with id q1",
            lambda m: m["cues"].append(dict(m["cues"][0])))
    variant("silence-with-asset.tqs", "2.4 asset", "a silence cue that points at audio",
            lambda m: m["cues"][0].update(type="silence"))
    variant("unknown-asset.tqs", "4.3 cross-reference", "cue points at an asset id not in assets",
            lambda m: m["cues"][0].update(asset="nope"))
    variant("asset-entry-missing.tqs", "2.3 / 4.3", "asset listed, audio entry missing from zip",
            drop_audio=True)
    variant("sha-mismatch.tqs", "4.4 hash", "audio bytes don't match sha256",
            audio_bytes=wav[:-2] + b"\x00\x01")
    variant("size-mismatch.tqs", "4.4 size", "bytes field doesn't match the entry",
            lambda m: m["assets"]["tone1"].update(bytes=len(wav) + 10))
    notaudio = (AUDIO / "not-audio.txt").read_bytes()
    variant("not-audio.tqs", "4.5 decode", "a text file named .wav with a correct hash",
            lambda m: m["assets"]["tone1"].update(sha256=tqs.hashlib.sha256(notaudio).hexdigest(),
                                                   bytes=len(notaudio)),
            audio_bytes=notaudio)
    variant("bad-asset-path.tqs", "2.3 path", "asset path doesn't match its id",
            lambda m: m["assets"]["tone1"].update(path="audio/other.wav"), audio_name="audio/other.wav")
    variant("path-traversal.tqs", "1.3 rule 1", "an entry named ../evil.wav",
            extra=[("../evil.wav", wav, zipfile.ZIP_STORED)])
    variant("absolute-path.tqs", "1.3 rule 1", "an entry named /tmp/evil.wav",
            extra=[("/tmp/evil.wav", wav, zipfile.ZIP_STORED)])
    variant("backslash-path.tqs", "1.3 rule 1", "an entry named audio\\..\\evil.wav",
            extra=[("audio\\..\\evil.wav", wav, zipfile.ZIP_STORED)])
    variant("symlink.tqs", "1.3 rule 2", "a symlink entry",
            extra=[("audio/link.wav", b"/etc/passwd", zipfile.ZIP_STORED, {"symlink": True})])
    variant("case-collision.tqs", "1.3 rule 3", "audio/tone1.wav and audio/TONE1.wav",
            extra=[("audio/TONE1.wav", wav, zipfile.ZIP_STORED)])
    variant("extra-file.tqs", "1.1", "an unexpected notes.txt at the root",
            extra=[("notes.txt", b"hello")])
    variant("zip-bomb.tqs", "1.3 rule 7", "8 MiB of zeros deflated to a few KB under audio/",
            extra=[("audio/bomb.wav", b"\x00" * (8 * 1024 * 1024), zipfile.ZIP_DEFLATED)])
    variant("manifest-infinity.tqs", "2.4 finite", "manifest contains the token Infinity",
            manifest_raw=manifest_bytes(good).replace(b'"volume": 1.0', b'"volume": Infinity'))
    variant("manifest-bom.tqs", "2 manifest", "manifest.json starts with a UTF-8 BOM",
            manifest_raw=b"\xef\xbb\xbf" + manifest_bytes(good))
    variant("manifest-not-json.tqs", "2 manifest", "manifest.json is not JSON",
            manifest_raw=b"{ this is not json")
    raw_zip(INVALID / "no-manifest.tqs", [("audio/tone1.wav", wav, zipfile.ZIP_STORED)])
    bad("no-manifest.tqs", "1.1", "no manifest.json at all")
    (INVALID / "not-a-zip.tqs").write_bytes(b"RIFF....WAVE this is a wav renamed to .tqs")
    bad("not-a-zip.tqs", "1", "not a zip archive")

    # ---------------- legacy ----------------
    LEGACY.mkdir(exist_ok=True)
    legacy_show = {
        "code": "STAGE-4821", "name": "Old web export", "author": "Connor", "notes": "Made by Export ZIP",
        "cues": [
            {"id": "0a1b2c3d4e5f", "name": "Overture", "filename": "0a1b2c3d4e5f.mp3", "volume": 0.9,
             "loop": False, "fade_in": 0.0, "fade_out": 2.0, "auto_continue": False, "pre_wait": 0.0,
             "type": "audio", "trim_start": 0.5, "trim_end": None, "cuts": [[1.0, 1.25]]},
            {"id": "1b2c3d4e5f6a", "name": "Old cue without newer fields", "filename": "1b2c3d4e5f6a.wav",
             "volume": 1.0, "loop": True, "fade_in": 0.0, "fade_out": 0.0, "auto_continue": False,
             "pre_wait": 0.0},
            {"id": "2c3d4e5f6a7b", "name": "Silence", "filename": "", "volume": 1.0, "loop": False,
             "fade_in": 0.0, "fade_out": 0.0, "auto_continue": True, "pre_wait": 0.0, "type": "silence",
             "duration": 2.0, "trim_start": 0.0, "trim_end": None, "cuts": []},
            {"id": "3d4e5f6a7b8c", "name": "Fade out all", "filename": "", "volume": 1.0, "loop": False,
             "fade_in": 0.0, "fade_out": 2.0, "auto_continue": False, "pre_wait": 0.0, "type": "fadeout",
             "duration": 3.0, "trim_start": 0.0, "trim_end": None, "cuts": []},
            {"id": "4e5f6a7b8c9d", "name": "File missing on the server", "filename": "4e5f6a7b8c9d.mp3",
             "volume": 1.0, "loop": False, "fade_in": 0.0, "fade_out": 0.0, "auto_continue": False,
             "pre_wait": 0.0, "type": "audio", "trim_start": 0.0, "trim_end": None, "cuts": []},
        ],
    }
    lz = LEGACY / "web-export-STAGE-4821.zip"
    raw_zip(lz, [("STAGE-4821/show.json", json.dumps(legacy_show, indent=2).encode()),
                 ("STAGE-4821/audio/0a1b2c3d4e5f.mp3", a["tone-e330.mp3"].read_bytes(), zipfile.ZIP_DEFLATED),
                 ("STAGE-4821/audio/1b2c3d4e5f6a.wav", wav, zipfile.ZIP_DEFLATED),
                 ("STAGE-4821/audio/orphan-from-deleted-cue.mp3", a["tone-e330.mp3"].read_bytes(),
                  zipfile.ZIP_DEFLATED)])
    out = LEGACY / "web-export-STAGE-4821.converted.tqs"
    m, _ = tqs.convert_legacy(lz, out)
    _rewrite_manifest(out, lambda mm: mm.update(exported_at=FIXED_TIME))
    expected("legacy-web-export-STAGE-4821", tqs.read_package(out)[0])
    index.append({"file": "legacy/web-export-STAGE-4821.zip", "expect": "accept-legacy",
                  "about": "The web's original Export ZIP layout. Convert per FORMAT.md section 5.",
                  "expected": "expected/legacy-web-export-STAGE-4821.json"})

    (FIX / "index.json").write_text(json.dumps(index, indent=2) + "\n", "utf-8")
    print(f"Wrote {len(index)} fixtures to {FIX}")


if __name__ == "__main__":
    main()
