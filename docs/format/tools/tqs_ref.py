#!/usr/bin/env python3
"""
Reference reader and writer for ThornCue packages, format_version 1:
.tqs (a show), .tqc (cues) and .tqg (a group of cues).

Standard library only, Python 3.10+. FORMAT.md is the spec; if this file and the
spec disagree, the spec wins and this file has a bug.

    python tqs_ref.py check  FILE.tqs|.tqc|.tqg   validate and print a summary
    python tqs_ref.py legacy OLD.zip OUT.tqs       convert an old web "Export ZIP"

As a library:

    manifest, warnings = read_package(path, extract_to=dir)    # raises TqsError
    manifest, warnings = read_package(path, expect="cues")      # only accept a .tqc
    write_package(path, show, cues, asset_files)                # a .tqs; asset_files: {asset_id: Path}
    write_package(path, None, cues, asset_files, kind="group", groups=[{"id", "name", "color"}])

Apps may use this file as their implementation (ThornCue Web does). Keep it standard
library only, and keep app-specific code out of it: hooks like `audio_check` exist so
an app can add its own checks without editing this file.

The audio check here is a magic-byte sniff, because the standard library has no
decoders. Real implementations MUST also confirm each file decodes (FORMAT.md 4.5).
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import shutil
import stat
import sys
import tempfile
import zipfile
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath

FORMAT = "thorncue.show"   # kept for code that imported it; see FORMATS
FORMAT_VERSION = 1

# format name -> kind. The kind decides which top-level sections a manifest has.
FORMATS = {"thorncue.show": "show", "thorncue.cues": "cues", "thorncue.group": "group"}
FORMAT_FOR_KIND = {v: k for k, v in FORMATS.items()}
EXTENSIONS = {"show": ".tqs", "cues": ".tqc", "group": ".tqg"}
MIME_TYPES = {"show": "application/vnd.thorncue.show+zip", "cues": "application/vnd.thorncue.cues+zip",
              "group": "application/vnd.thorncue.group+zip"}
KIND_NOUN = {"show": "a show (.tqs)", "cues": "a cue file (.tqc)", "group": "a group file (.tqg)"}

MAX_GROUPS = 200
MAX_GROUP_NAME = 60
DEFAULT_GROUP_COLOR = "#3e7bd6"
COLOR_RE = re.compile(r"^#[0-9a-fA-F]{6}$")

MAX_ENTRIES = 2000
MAX_MANIFEST_BYTES = 4 * 1024 * 1024
DEFAULT_MAX_TOTAL = 4 * 1024 ** 3
MAX_RATIO = 200
RATIO_MIN_SIZE = 1024 * 1024
MAX_CUES = 2000
MAX_CUTS = 50

AUDIO_EXTS = ("mp3", "wav", "ogg", "flac", "m4a", "aiff", "aif")
ID_RE = re.compile(r"^[a-z0-9_-]{1,64}$")
PATH_RE = re.compile(r"^audio/([a-z0-9_-]{1,64})\.(" + "|".join(AUDIO_EXTS) + r")$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
JUNK_RE = re.compile(r"(^|/)(__MACOSX/|\.DS_Store$|Thumbs\.db$)")

CUE_TYPES = ("audio", "silence", "fadeout")
CONTINUE_MODES = ("none", "after_end")


class TqsError(Exception):
    """A package that must be rejected. The message is shown to the user."""


# ---------------------------------------------------------------------------
# Manifest normalisation (FORMAT.md 2.4 to 2.6)
# ---------------------------------------------------------------------------

def _num(cue_name: str, field: str, value, default: float, lo: float, hi: float | None) -> float:
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TqsError(f'Cue "{cue_name}": {field} must be a number.')
    v = float(value)
    if not math.isfinite(v):
        raise TqsError(f'Cue "{cue_name}": {field} must be a finite number.')
    v = max(lo, v)
    if hi is not None:
        v = min(hi, v)
    return round(v, 4)


def _str(value, default: str, max_len: int, what: str) -> str:
    if value is None:
        return default
    if not isinstance(value, str):
        raise TqsError(f"{what} must be text.")
    value = value.strip()[:max_len]
    return value or default


def normalise_cuts(raw, cue_name: str = "?") -> list[list[float]]:
    """Drop invalid pairs, sort, merge overlaps. Wrong container type is a rejection."""
    if raw is None:
        return []
    if not isinstance(raw, list):
        raise TqsError(f'Cue "{cue_name}": cuts must be a list.')
    pairs = []
    for p in raw[:MAX_CUTS]:
        if not isinstance(p, list) or len(p) != 2:
            continue
        a, b = p
        if isinstance(a, bool) or isinstance(b, bool) or not isinstance(a, (int, float)) or not isinstance(b, (int, float)):
            continue
        if not (math.isfinite(a) and math.isfinite(b)) or a < 0 or b <= a:
            continue
        pairs.append([round(float(a), 4), round(float(b), 4)])
    pairs.sort()
    merged: list[list[float]] = []
    for a, b in pairs:
        if merged and a <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], b)
        else:
            merged.append([a, b])
    return merged


def normalise_cue(raw, assets: dict, seen_ids: set) -> dict:
    if not isinstance(raw, dict):
        raise TqsError("Every cue must be an object.")
    cid = raw.get("id")
    if not isinstance(cid, str) or not ID_RE.match(cid):
        raise TqsError(f"Cue id {cid!r} is not valid.")
    if cid in seen_ids:
        raise TqsError(f"Two cues share the id {cid!r}.")
    seen_ids.add(cid)

    ctype = raw.get("type")
    if ctype not in CUE_TYPES:
        raise TqsError(f"Cue {cid} has type {ctype!r}, which this version of ThornCue doesn't know. "
                       "Update ThornCue to open this show.")
    name = _str(raw.get("name"), "Untitled cue", 100, f"Cue {cid}: name")

    cont = raw.get("continue", "none")
    if cont not in CONTINUE_MODES:
        raise TqsError(f'Cue "{name}" uses continue mode {cont!r}, which this version of ThornCue '
                       "doesn't know. Update ThornCue to open this show.")

    asset = raw.get("asset")
    if asset is not None:
        if not isinstance(asset, str):
            raise TqsError(f'Cue "{name}": asset must be text or null.')
        if ctype != "audio":
            raise TqsError(f'Cue "{name}" is a {ctype} cue but has an audio file.')
        if asset not in assets:
            raise TqsError(f'Cue "{name}" points to audio "{asset}", which isn\'t in the package.')

    loop = raw.get("loop", False)
    if not isinstance(loop, bool):
        raise TqsError(f'Cue "{name}": loop must be true or false.')

    trim_start = _num(name, "trim_start", raw.get("trim_start"), 0.0, 0.0, None)
    trim_end_raw = raw.get("trim_end")
    trim_end = None if trim_end_raw is None else _num(name, "trim_end", trim_end_raw, 0.0, 0.0, None)
    if trim_end is not None and trim_end <= trim_start:
        trim_end = None

    return {
        "id": cid,
        "type": ctype,
        "name": name,
        "asset": asset,
        "volume": _num(name, "volume", raw.get("volume"), 1.0, 0.0, 2.0),
        "fade_in": _num(name, "fade_in", raw.get("fade_in"), 0.0, 0.0, 600.0),
        "fade_out": _num(name, "fade_out", raw.get("fade_out"), 0.0, 0.0, 600.0),
        "pre_wait": _num(name, "pre_wait", raw.get("pre_wait"), 0.0, 0.0, 3600.0),
        "duration": _num(name, "duration", raw.get("duration"), 0.0, 0.0, 3600.0) if ctype != "audio" else 0.0,
        "loop": loop if ctype == "audio" else False,
        "continue": cont,
        "trim_start": trim_start,
        "trim_end": trim_end,
        "cuts": normalise_cuts(raw.get("cuts"), name),
    }


def normalise_groups(raw, cues: list[dict], cue_groups: list, kind: str, warnings: list[str]) -> list[dict]:
    """FORMAT.md section 2.7. Sets cue["group"] on `cues` in place (from `cue_groups`, the raw
    group refs in cue order), may reorder nothing itself, and returns the normalised group list.
    The caller pulls each group's cues together afterwards."""
    if raw is None:
        raw = []
    if not isinstance(raw, list):
        raise TqsError("groups must be a list.")
    if kind == "cues":
        if raw or any(g is not None for g in cue_groups):
            warnings.append("Ignoring groups in a cue file (.tqc files don't carry groups).")
        return []
    if len(raw) > MAX_GROUPS:
        raise TqsError(f"This file has {len(raw)} groups; the limit is {MAX_GROUPS}.")
    groups: list[dict] = []
    seen: set = set()
    for g in raw:
        if not isinstance(g, dict):
            raise TqsError("Every group must be an object.")
        gid = g.get("id")
        if not isinstance(gid, str) or not ID_RE.match(gid):
            raise TqsError(f"Group id {gid!r} is not valid.")
        if gid in seen:
            raise TqsError(f"Two groups share the id {gid!r}.")
        seen.add(gid)
        name = _str(g.get("name"), "Group", MAX_GROUP_NAME, f"Group {gid}: name")
        color = g.get("color")
        if color is not None and not isinstance(color, str):
            raise TqsError(f'Group "{name}": color must be text.')
        if not (isinstance(color, str) and COLOR_RE.match(color)):
            if color is not None:
                warnings.append(f'Group "{name}" has an invalid colour {color!r}; using the default.')
            color = DEFAULT_GROUP_COLOR
        groups.append({"id": gid, "name": name, "color": color.lower()})
    for c, gid in zip(cues, cue_groups):
        if gid is None:
            continue
        if not isinstance(gid, str):
            raise TqsError(f'Cue "{c["name"]}": group must be text or null.')
        if gid not in seen:
            raise TqsError(f'Cue "{c["name"]}" is in group "{gid}", which isn\'t in the file.')
        c["group"] = gid
    if kind == "group":
        if len(groups) != 1:
            raise TqsError(f"A group file (.tqg) must hold exactly one group; this one has {len(groups)}.")
        if not cues:
            raise TqsError("This group file (.tqg) has no cues.")
        gid = groups[0]["id"]
        if any(c.get("group") != gid for c in cues):
            raise TqsError(f'Every cue in a group file must be in its group ("{groups[0]["name"]}").')
    used = {c.get("group") for c in cues}
    for g in list(groups):
        if g["id"] not in used:
            warnings.append(f'Group "{g["name"]}" has no cues; dropping it.')
            groups.remove(g)
    return groups


def gather_groups(cues: list[dict], warnings: list[str] | None = None) -> list[dict]:
    """Pull each group's cues together at the position of its first cue, keeping their order
    (FORMAT.md 2.7). Ungrouped cues keep their positions relative to everything else."""
    out: list[dict] = []
    placed: set = set()
    for c in cues:
        gid = c.get("group")
        if gid is None:
            out.append(c)
        elif gid not in placed:
            placed.add(gid)
            out.extend(x for x in cues if x.get("group") == gid)
    if warnings is not None and [c["id"] for c in out] != [c["id"] for c in cues]:
        warnings.append("Some groups' cues weren't next to each other; they've been moved together.")
    return out


def normalise_manifest(raw, expect: str | None = None) -> tuple[dict, list[str]]:
    """Validate and normalise a parsed manifest. Returns (manifest, warnings).
    `expect` ("show", "cues" or "group") rejects any other kind with a clear message."""
    warnings: list[str] = []
    if not isinstance(raw, dict):
        raise TqsError("manifest.json must be a JSON object.")
    kind = FORMATS.get(raw.get("format"))
    if kind is None:
        raise TqsError("This isn't a ThornCue file (manifest.json has an unknown format name).")
    if expect is not None and kind != expect:
        raise TqsError(f"This is {KIND_NOUN[kind]}, not {KIND_NOUN[expect]}.")
    ver = raw.get("format_version")
    if isinstance(ver, bool) or not isinstance(ver, int):
        raise TqsError("manifest.json has no valid format_version.")
    if ver > FORMAT_VERSION:
        raise TqsError("This show was made by a newer version of ThornCue. Update ThornCue to open it.")
    if ver < 1:
        raise TqsError(f"Unknown format_version {ver}.")

    show_raw = raw.get("show")
    if kind != "show":
        show_raw = {"name": "", "source": None}   # .tqc/.tqg: a stray show section is ignored
    if not isinstance(show_raw, dict):
        raise TqsError("manifest.json has no show section.")
    source = show_raw.get("source")
    if source is not None:
        if not isinstance(source, dict):
            raise TqsError("show.source must be an object or null.")
        if source.get("kind") != "thorncue-web":
            warnings.append(f"Ignoring unknown show source kind {source.get('kind')!r}.")
            source = None
        else:
            for k in ("server", "code", "revision"):
                if not isinstance(source.get(k), str) or not source.get(k):
                    raise TqsError(f"show.source.{k} is missing.")
            source = {"kind": "thorncue-web", "server": source["server"].rstrip("/"),
                      "code": source["code"], "revision": source["revision"]}
    show = {
        "name": _str(show_raw.get("name"), "Untitled show", 100, "Show name"),
        "author": _str(show_raw.get("author"), "", 100, "Author"),
        "notes": _str(show_raw.get("notes"), "", 2000, "Notes"),
        "source": source,
    }

    assets_raw = raw.get("assets", {})
    if not isinstance(assets_raw, dict):
        raise TqsError("assets must be an object.")
    assets = {}
    for aid, a in assets_raw.items():
        if not ID_RE.match(aid):
            raise TqsError(f"Audio id {aid!r} is not valid.")
        if not isinstance(a, dict):
            raise TqsError(f"Audio {aid} must be an object.")
        path = a.get("path")
        m = PATH_RE.match(path) if isinstance(path, str) else None
        if not m or m.group(1) != aid:
            raise TqsError(f"Audio {aid} has an invalid path {path!r}.")
        sha = a.get("sha256")
        if not isinstance(sha, str) or not SHA_RE.match(sha):
            raise TqsError(f"Audio {aid} has an invalid sha256.")
        size = a.get("bytes")
        if isinstance(size, bool) or not isinstance(size, int) or size < 1:
            raise TqsError(f"Audio {aid} has an invalid size.")
        assets[aid] = {"path": path, "sha256": sha, "bytes": size}

    cues_raw = raw.get("cues", [])
    if not isinstance(cues_raw, list):
        raise TqsError("cues must be a list.")
    if len(cues_raw) > MAX_CUES:
        raise TqsError(f"This show has {len(cues_raw)} cues; the limit is {MAX_CUES}.")
    seen: set = set()
    cues = [normalise_cue(c, assets, seen) for c in cues_raw]
    groups = normalise_groups(raw.get("groups"), cues, [c.get("group") if isinstance(c, dict) else None
                                                        for c in cues_raw], kind, warnings)
    cues = gather_groups(cues, warnings)

    used = {c["asset"] for c in cues if c["asset"]}
    for aid in list(assets):
        if aid not in used:
            warnings.append(f"Audio {aid} isn't used by any cue; dropping it.")
            del assets[aid]

    gen = raw.get("generator") if isinstance(raw.get("generator"), dict) else {}
    out = {
        "format": FORMAT_FOR_KIND[kind],
        "format_version": FORMAT_VERSION,
        "generator": {"name": str(gen.get("name", ""))[:64], "version": str(gen.get("version", ""))[:64]},
        "exported_at": raw.get("exported_at") if isinstance(raw.get("exported_at"), str) else "",
    }
    if kind == "show":
        out["show"] = show
    if groups:   # written only when there are groups, so group-free files are unchanged from 1.0
        out["groups"] = groups
    out["assets"] = assets
    out["cues"] = cues
    return out, warnings


# ---------------------------------------------------------------------------
# Zip safety (FORMAT.md 1.3)
# ---------------------------------------------------------------------------

def _check_name(name: str) -> None:
    bad = (
        not name
        or "\\" in name
        or "\x00" in name
        or name.startswith("/")
        or re.match(r"^[A-Za-z]:", name)
        or ".." in PurePosixPath(name).parts
    )
    if bad:
        raise TqsError(f"The package contains an unsafe file name: {name!r}.")


def _is_symlink(info: zipfile.ZipInfo) -> bool:
    return stat.S_ISLNK(info.external_attr >> 16)


def check_zip(zf: zipfile.ZipFile, max_total: int) -> tuple[list[zipfile.ZipInfo], list[str]]:
    """Apply the central-directory rules. Returns (entries to use, warnings)."""
    infos = zf.infolist()
    warnings: list[str] = []
    if len(infos) > MAX_ENTRIES:
        raise TqsError(f"The package has {len(infos)} files; the limit is {MAX_ENTRIES}.")
    seen_lower: set = set()
    total = 0
    keep = []
    for info in infos:
        name = info.filename
        _check_name(name)
        if info.flag_bits & 0x1:
            raise TqsError(f"{name} is encrypted. ThornCue packages can't be encrypted.")
        if _is_symlink(info):
            raise TqsError(f"{name} is a symbolic link, which isn't allowed.")
        if info.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
            raise TqsError(f"{name} uses an unsupported compression method.")
        low = name.lower()
        if low in seen_lower:
            raise TqsError(f"The package has two files called {name!r} (ignoring case).")
        seen_lower.add(low)
        if JUNK_RE.search(name):
            warnings.append(f"Skipping {name}.")
            continue
        if name.endswith("/"):
            if name not in ("audio/",):
                raise TqsError(f"Unexpected folder {name!r} in the package.")
            continue
        if name != "manifest.json" and not name.startswith("audio/"):
            raise TqsError(f"Unexpected file {name!r} in the package.")
        if info.file_size > RATIO_MIN_SIZE and info.file_size > MAX_RATIO * max(1, info.compress_size):
            raise TqsError(f"{name} is compressed suspiciously well; refusing it as a possible zip bomb.")
        total += info.file_size
        keep.append(info)
    if total > max_total:
        raise TqsError(f"The package unpacks to {total / 1024 ** 3:.1f} GiB, over the "
                       f"{max_total / 1024 ** 3:.1f} GiB limit.")
    return keep, warnings


def check_legacy_zip(zf: zipfile.ZipFile, max_total: int) -> list[zipfile.ZipInfo]:
    """FORMAT.md 1.3 for a legacy web export (section 5): every rule except the v1 entry list."""
    infos = zf.infolist()
    if len(infos) > MAX_ENTRIES:
        raise TqsError(f"The zip has {len(infos)} files; the limit is {MAX_ENTRIES}.")
    seen_lower: set = set()
    total = 0
    for info in infos:
        name = info.filename
        _check_name(name)
        if info.flag_bits & 0x1:
            raise TqsError(f"{name} is encrypted. ThornCue can't import encrypted zips.")
        if _is_symlink(info):
            raise TqsError(f"{name} is a symbolic link, which isn't allowed.")
        if info.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
            raise TqsError(f"{name} uses an unsupported compression method.")
        low = name.lower()
        if low in seen_lower:
            raise TqsError(f"The zip has two files called {name!r} (ignoring case).")
        seen_lower.add(low)
        if info.file_size > RATIO_MIN_SIZE and info.file_size > MAX_RATIO * max(1, info.compress_size):
            raise TqsError(f"{name} is compressed suspiciously well; refusing it as a possible zip bomb.")
        total += info.file_size
    if total > max_total:
        raise TqsError(f"The zip unpacks to {total / 1024 ** 2:.0f} MB, over the "
                       f"{max_total / 1024 ** 2:.0f} MB import limit.")
    return infos


def copy_counted(src, dst, limit: int, name: str) -> tuple[int, str]:
    """Copy bytes, refusing to go past `limit`. Returns (bytes, sha256 hex)."""
    return _copy_counted(src, dst, limit, name)


def _copy_counted(src, dst, limit: int, name: str) -> tuple[int, str]:
    """Copy bytes, refusing to go past `limit`. Returns (bytes, sha256 hex)."""
    h = hashlib.sha256()
    n = 0
    while True:
        chunk = src.read(1024 * 1024)
        if not chunk:
            break
        n += len(chunk)
        if n > limit:
            raise TqsError(f"{name} is larger than the package says it is.")
        h.update(chunk)
        dst.write(chunk)
    return n, h.hexdigest()


# ---------------------------------------------------------------------------
# Audio sniffing (stand-in for a real decode check)
# ---------------------------------------------------------------------------

def sniff_audio(path: Path) -> str | None:
    """Return a format guess from magic bytes, or None."""
    with open(path, "rb") as f:
        head = f.read(64)
    if head[:4] == b"RIFF" and head[8:12] == b"WAVE":
        return "wav"
    if head[:4] == b"FORM" and head[8:12] in (b"AIFF", b"AIFC"):
        return "aiff"
    if head[:4] == b"OggS":
        return "ogg"
    if head[:4] == b"fLaC":
        return "flac"
    if head[4:8] == b"ftyp":
        return "m4a"
    if head[:3] == b"ID3" or (len(head) > 1 and head[0] == 0xFF and (head[1] & 0xE0) == 0xE0):
        return "mp3"
    return None


# ---------------------------------------------------------------------------
# Reading
# ---------------------------------------------------------------------------

def read_package(path, extract_to=None, max_total: int = DEFAULT_MAX_TOTAL, audio_check=None,
                 expect: str | None = None) -> tuple[dict, list[str]]:
    """
    Validate a .tqs file. If extract_to is given, audio ends up in extract_to/audio/
    and manifest.json (normalised) in extract_to/. Nothing is written there on failure.
    `audio_check(path) -> bool` is an optional extra content check run on each audio file.
    `expect` ("show", "cues", "group") refuses other kinds; None accepts all three.
    """
    path = Path(path)
    if not zipfile.is_zipfile(path):
        raise TqsError("This file isn't a ThornCue file (it isn't a zip archive).")
    with zipfile.ZipFile(path) as zf:
        entries, warnings = check_zip(zf, max_total)
        by_name = {i.filename: i for i in entries}
        if "manifest.json" not in by_name:
            raise TqsError("This file isn't a ThornCue file (no manifest.json).")
        if entries[0].filename != "manifest.json":
            warnings.append("manifest.json isn't the first entry (writers should put it first).")
        minfo = by_name["manifest.json"]
        if minfo.file_size > MAX_MANIFEST_BYTES:
            raise TqsError("manifest.json is too large.")
        with zf.open(minfo) as f:
            data = f.read(MAX_MANIFEST_BYTES + 1)
        if len(data) > MAX_MANIFEST_BYTES:
            raise TqsError("manifest.json is too large.")
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            raise TqsError("manifest.json isn't valid UTF-8.")
        if text.startswith("﻿"):
            raise TqsError("manifest.json starts with a byte-order mark; write it without one.")
        try:
            raw = json.loads(text, parse_constant=lambda c: (_ for _ in ()).throw(TqsError(f"manifest.json contains {c}.")))
        except json.JSONDecodeError as e:
            raise TqsError(f"manifest.json isn't valid JSON ({e.msg}, line {e.lineno}).")
        manifest, w2 = normalise_manifest(raw, expect)
        warnings += w2

        wanted = {a["path"]: aid for aid, a in manifest["assets"].items()}
        for p, aid in wanted.items():
            if p not in by_name:
                raise TqsError(f"Audio file {p} is listed but missing from the package.")
        for name in by_name:
            if name.startswith("audio/") and name not in wanted:
                warnings.append(f"Skipping {name}: no cue uses it.")

        tmp_parent = Path(extract_to).parent if extract_to else None
        if tmp_parent:
            tmp_parent.mkdir(parents=True, exist_ok=True)
        tmp = Path(tempfile.mkdtemp(prefix=".tqs-import-", dir=tmp_parent))
        try:
            (tmp / "audio").mkdir()
            for p, aid in wanted.items():
                info = by_name[p]
                asset = manifest["assets"][aid]
                out_path = tmp / p
                with zf.open(info) as src, open(out_path, "wb") as dst:
                    n, digest = _copy_counted(src, dst, min(info.file_size, asset["bytes"]), p)
                if n != asset["bytes"] or n != info.file_size:
                    raise TqsError(f"{p} is damaged (wrong size).")
                if digest != asset["sha256"]:
                    raise TqsError(f"{p} is damaged (checksum doesn't match).")
                kind = sniff_audio(out_path)
                if kind is None or (audio_check is not None and not audio_check(out_path)):
                    raise TqsError(f"{p} doesn't look like an audio file.")
            if extract_to:
                (tmp / "manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False), "utf-8")
                dest = Path(extract_to)
                if dest.exists():
                    raise TqsError(f"{dest} already exists.")
                os.replace(tmp, dest)
                tmp = None
        finally:
            if tmp is not None:
                shutil.rmtree(tmp, ignore_errors=True)
    return manifest, warnings


# ---------------------------------------------------------------------------
# Writing (FORMAT.md section 3)
# ---------------------------------------------------------------------------

def _hash_file(path: Path) -> tuple[str, int]:
    h = hashlib.sha256()
    n = 0
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
            n += len(chunk)
    return h.hexdigest(), n


def now_rfc3339() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def build_manifest(show: dict | None, cues: list[dict], asset_files: dict, generator=("tqs_ref", "1"), *,
                   kind: str = "show", groups: list[dict] | None = None) -> dict:
    """asset_files: {asset_id: Path}. Cues whose asset file is missing get asset null.
    kind: "show" (.tqs, needs `show`), "cues" (.tqc) or "group" (.tqg, `groups` has exactly one
    group and every cue's "group" is its id). Cues carry "group" for group membership."""
    assets = {}
    for aid, p in asset_files.items():
        p = Path(p)
        if not p.is_file():
            continue
        ext = p.suffix.lower().lstrip(".")
        if ext not in AUDIO_EXTS:
            raise ValueError(f"{p} has an unsupported extension")
        digest, size = _hash_file(p)
        assets[aid] = {"path": f"audio/{aid}.{ext}", "sha256": digest, "bytes": size}
    out_cues = []
    for c in cues:
        c = dict(c)
        if c.get("asset") and c["asset"] not in assets:
            c["asset"] = None
        out_cues.append(c)
    raw = {
        "format": FORMAT_FOR_KIND[kind],
        "format_version": FORMAT_VERSION,
        "generator": {"name": generator[0], "version": generator[1]},
        "exported_at": now_rfc3339(),
        "assets": assets,
        "cues": out_cues,
    }
    if kind == "show":
        raw["show"] = show
    if groups:
        raw["groups"] = groups
    manifest, _ = normalise_manifest(raw, kind)  # writers must emit a valid manifest
    manifest["exported_at"] = raw["exported_at"]
    return manifest


def write_package(path, show: dict | None, cues: list[dict], asset_files: dict, generator=("tqs_ref", "1"),
                  manifest: dict | None = None, *, kind: str = "show", groups: list[dict] | None = None) -> dict:
    """Write a .tqs/.tqc/.tqg atomically. Pass `manifest` to write one verbatim (fixtures only)."""
    path = Path(path)
    if manifest is None:
        manifest = build_manifest(show, cues, asset_files, generator, kind=kind, groups=groups)
    tmp = path.with_name(path.name + ".part")
    with zipfile.ZipFile(tmp, "w", allowZip64=True) as zf:
        zf.writestr(zipfile.ZipInfo("manifest.json", date_time=(2026, 1, 1, 0, 0, 0)),
                    json.dumps(manifest, indent=2, ensure_ascii=False).encode("utf-8"),
                    compress_type=zipfile.ZIP_DEFLATED)
        for aid, a in manifest["assets"].items():
            zf.write(asset_files[aid], a["path"], compress_type=zipfile.ZIP_STORED)
    os.replace(tmp, path)
    return manifest


# ---------------------------------------------------------------------------
# Legacy web "Export ZIP" (FORMAT.md section 5)
# ---------------------------------------------------------------------------

def convert_legacy(zip_path, out_path, max_total: int = DEFAULT_MAX_TOTAL) -> tuple[dict, list[str]]:
    zip_path = Path(zip_path)
    warnings: list[str] = []
    with zipfile.ZipFile(zip_path) as zf:
        infos = zf.infolist()
        if len(infos) > MAX_ENTRIES:
            raise TqsError("Too many files in the zip.")
        total = 0
        for i in infos:
            _check_name(i.filename)
            if i.flag_bits & 0x1 or _is_symlink(i):
                raise TqsError(f"{i.filename} is encrypted or a link.")
            total += i.file_size
        if total > max_total:
            raise TqsError("The zip is too large.")
        names = [i.filename for i in infos]
        entry = next((n for n in names if n == "show.json" or (n.endswith("/show.json") and n.count("/") == 1)), None)
        if not entry:
            raise TqsError("This zip has no show.json, so it isn't a ThornCue export.")
        try:
            legacy = json.loads(zf.read(entry).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise TqsError("show.json isn't valid JSON.")
        if not isinstance(legacy, dict) or not isinstance(legacy.get("cues"), list):
            raise TqsError("show.json has no cue list.")
        prefix = entry[: -len("show.json")]
        with tempfile.TemporaryDirectory() as tmp:
            asset_files = {}
            cues = []
            for i, c in enumerate(legacy["cues"]):
                if not isinstance(c, dict):
                    continue
                ctype = c.get("type") or "audio"
                cid = str(c.get("id") or f"cue{i}").lower()
                cid = re.sub(r"[^a-z0-9_-]", "", cid)[:64] or f"cue{i}"
                asset = None
                fn = c.get("filename") or ""
                if ctype == "audio" and fn:
                    stem, _, ext = fn.rpartition(".")
                    stem = re.sub(r"[^a-z0-9_-]", "", stem.lower())[:64]
                    ext = ext.lower()
                    member = prefix + "audio/" + fn
                    if stem and ext in AUDIO_EXTS and member in names:
                        dest = Path(tmp) / f"{stem}.{ext}"
                        if stem not in asset_files:
                            with zf.open(member) as s, open(dest, "wb") as d:
                                _copy_counted(s, d, zf.getinfo(member).file_size, member)
                            if sniff_audio(dest) is None:
                                raise TqsError(f"{fn} doesn't look like an audio file.")
                            asset_files[stem] = dest
                        asset = stem
                    else:
                        warnings.append(f'Cue "{c.get("name")}": audio file {fn} is missing; importing it as missing.')
                cues.append({
                    "id": cid, "type": ctype, "name": c.get("name"), "asset": asset,
                    "volume": c.get("volume"), "fade_in": c.get("fade_in"), "fade_out": c.get("fade_out"),
                    "pre_wait": c.get("pre_wait"), "duration": c.get("duration"),
                    "loop": bool(c.get("loop", False)),
                    "continue": "after_end" if c.get("auto_continue") else "none",
                    "trim_start": c.get("trim_start"), "trim_end": c.get("trim_end"), "cuts": c.get("cuts"),
                })
            show = {"name": legacy.get("name"), "author": legacy.get("author", ""),
                    "notes": legacy.get("notes", ""), "source": None}
            manifest = write_package(out_path, show, cues, asset_files, generator=("tqs_ref legacy import", "1"))
    return manifest, warnings


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _summary(m: dict) -> str:
    total = sum(a["bytes"] for a in m["assets"].values())
    kinds: dict = {}
    for c in m["cues"]:
        kinds[c["type"]] = kinds.get(c["type"], 0) + 1
    kind = FORMATS[m["format"]]
    title = m["show"]["name"] if kind == "show" else (
        f'group "{m["groups"][0]["name"]}"' if kind == "group" else "cue file")
    groups = f', {len(m.get("groups", []))} groups' if kind == "show" and m.get("groups") else ""
    return (f'{title}: {len(m["cues"])} cues '
            f'({", ".join(f"{v} {k}" for k, v in kinds.items()) or "none"}){groups}, '
            f'{len(m["assets"])} audio files, {total / 1024 / 1024:.1f} MiB')


def main(argv: list[str]) -> int:
    if len(argv) >= 2 and argv[0] == "check":
        rc = 0
        for p in argv[1:]:
            try:
                m, w = read_package(p)
                print(f"OK    {p}: {_summary(m)}")
                for x in w:
                    print(f"      warning: {x}")
            except TqsError as e:
                print(f"FAIL  {p}: {e}")
                rc = 1
        return rc
    if len(argv) == 3 and argv[0] == "legacy":
        try:
            m, w = convert_legacy(argv[1], argv[2])
        except TqsError as e:
            print(f"FAIL: {e}")
            return 1
        print(f"Wrote {argv[2]}: {_summary(m)}")
        for x in w:
            print(f"warning: {x}")
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
