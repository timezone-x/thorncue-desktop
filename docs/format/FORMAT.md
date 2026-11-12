# ThornCue show package format (`.tqs`), version 1

Status: **v1, frozen for the first release.** This file is the contract between ThornCue Web and the ThornCue desktop client. Both repos carry an identical copy. Change it in both, in the same week, or not at all.

| Extension | What it is | Status |
|---|---|---|
| `.tqs` | A whole show: cue list and audio | Defined here |
| `.tqc` | A single cue, or a set of cues, with their audio (for copying cues between shows) | Reserved. Same container rules, `format: "thorncue.cues"`. Not specified yet; nobody writes it in v1 |

Files in this folder:

- `FORMAT.md`: this spec (normative).
- `manifest.schema.json`: JSON Schema for `manifest.json`. Covers shape and ranges; the rules below that a schema can't express (cross-references, zip safety, hashes) are normative too.
- `fixtures/`: packages every implementation must pass. `valid/` must import, `invalid/` must be rejected with a clear error, `legacy/` is the old web "Export ZIP" format.
- `tools/tqs_ref.py`: reference reader and writer, Python standard library only. When this spec and the reference tool disagree, the spec wins and the tool has a bug.

The words MUST, MUST NOT, SHOULD and MAY mean what RFC 2119 says.

---

## 1. Container

A `.tqs` file is a ZIP archive (PKZIP, as written by Python's `zipfile`, Rust's `zip` crate and every OS archiver). ZIP64 MUST be supported by readers, since a show can exceed 4 GiB.

### 1.1 Entries

| Entry | Required | Notes |
|---|---|---|
| `manifest.json` | Yes | UTF-8 JSON, no BOM, at most 4 MiB. Writers MUST write it as the **first** entry so a reader can show the show's name and cue count before reading the audio. |
| `audio/<asset-id>.<ext>` | One per asset | Audio files exactly as uploaded. Never transcoded. |

Nothing else is allowed in v1. A reader MUST reject a package containing any other entry, except that it MUST silently skip directory entries (names ending in `/`) for `audio/` itself, and SHOULD skip OS junk (`__MACOSX/…`, `.DS_Store`, `Thumbs.db`) with a warning rather than failing.

### 1.2 Compression

- `manifest.json`: DEFLATE (writers SHOULD) or STORE.
- Audio entries: writers SHOULD use STORE. MP3, OGG, M4A and FLAC are already compressed, so DEFLATE wastes CPU on both ends for almost nothing. WAV and AIFF MAY be DEFLATEd.
- Readers MUST accept STORE and DEFLATE for any entry and MUST reject any other method.

### 1.3 Safety rules (readers MUST enforce all of these)

A `.tqs` file can come from a stranger. Reject the whole package, before writing anything to its final location, if any entry:

1. has a name that is absolute (`/x`, `C:\x`, `C:/x`), contains `..` as a path segment, contains a backslash, contains a NUL, or is not valid UTF-8;
2. is a symlink (Unix mode bits in the external attributes say `S_IFLNK`);
3. has a name that matches another entry case-insensitively (two entries that would collide on Windows or macOS);
4. is encrypted.

And reject the package if:

5. it has more than **2,000** entries;
6. the declared uncompressed sizes add up to more than the reader's limit (desktop client: 4 GiB by default; web: its `IMPORT_MAX_MB`). Check the declared sizes first, then **also** count bytes while extracting and stop the moment the real count passes the limit or passes that entry's declared size (zip bombs lie about sizes);
7. any single entry's compression ratio exceeds 200:1 and the entry is larger than 1 MiB uncompressed.

Extract to a temporary directory on the same filesystem as the destination, verify everything (section 4), then move into place. A failed import MUST leave nothing behind.

---

## 2. `manifest.json`

```json
{
  "format": "thorncue.show",
  "format_version": 1,
  "generator": { "name": "thorncue-web", "version": "2026.11.12" },
  "exported_at": "2026-11-12T03:15:00Z",
  "show": {
    "name": "Cinderella",
    "author": "Connor",
    "notes": "Panto, Act 1 and 2",
    "source": {
      "kind": "thorncue-web",
      "server": "https://cue.example.com",
      "code": "STAGE-4821",
      "revision": "2026-11-12T03:10:41Z"
    }
  },
  "assets": {
    "a1b2c3d4e5f6": {
      "path": "audio/a1b2c3d4e5f6.mp3",
      "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
      "bytes": 4823011
    }
  },
  "cues": [
    {
      "id": "a1b2c3d4e5f6",
      "type": "audio",
      "name": "Overture",
      "asset": "a1b2c3d4e5f6",
      "volume": 1.0,
      "fade_in": 0.0,
      "fade_out": 3.0,
      "pre_wait": 0.0,
      "duration": 0.0,
      "loop": false,
      "continue": "none",
      "trim_start": 1.25,
      "trim_end": null,
      "cuts": [[42.5, 48.0]]
    }
  ]
}
```

### 2.1 Top level

| Field | Type | Rule |
|---|---|---|
| `format` | string | MUST be `"thorncue.show"`. Anything else: reject ("not a ThornCue show"). |
| `format_version` | integer | `1`. A reader MUST reject a higher version with a message telling the user to update ThornCue ("This show was made by a newer version of ThornCue"). There is no minor version: additive changes don't bump it (see 2.6). |
| `generator` | object | `name` (string, ≤ 64) and `version` (string, ≤ 64). Informational. Readers MUST NOT change behaviour based on it. |
| `exported_at` | string | RFC 3339 UTC timestamp, `Z` suffix. Informational. |
| `show` | object | Section 2.2. |
| `assets` | object | Map of asset id → asset. MAY be empty. Section 2.3. |
| `cues` | array | Ordered cue list, at most 2,000 cues. Order is play order. MAY be empty. Section 2.4. |

### 2.2 `show`

| Field | Type | Rule |
|---|---|---|
| `name` | string | 1–100 characters after trimming. Empty after trimming: reader uses `"Untitled show"`. |
| `author` | string | ≤ 100. MAY be empty. |
| `notes` | string | ≤ 2,000. MAY be empty. |
| `source` | object or null | Where the show came from, so a client can recognise a re-import of the same show. Writers that aren't exporting from a server write `null`. |

`source` (when not null):

| Field | Type | Rule |
|---|---|---|
| `kind` | string | `"thorncue-web"` in v1. Readers MUST ignore `source` entirely if `kind` is unknown. |
| `server` | string | The server's public base URL, scheme + host (+ port), no trailing slash. |
| `code` | string | The show code on that server. |
| `revision` | string | Opaque string that changes whenever the show changes. Clients compare it for equality only and MUST NOT parse it. (The web uses the show file's modification time in RFC 3339, but don't rely on that.) |

There is deliberately no show id. A show's identity inside a client is the client's own business; `source.server` + `source.code` is how a client recognises "I already imported this one".

### 2.3 `assets`

Keys are asset ids: 1–64 characters from `[a-z0-9_-]`. Values:

| Field | Type | Rule |
|---|---|---|
| `path` | string | MUST be exactly `audio/<asset-id>.<ext>`, where `<ext>` is lowercase and one of `mp3 wav ogg flac m4a aiff aif`. |
| `sha256` | string | 64 lowercase hex characters: SHA-256 of the entry's uncompressed bytes. |
| `bytes` | integer | Uncompressed size in bytes, ≥ 1. |

Every asset MUST have exactly one matching entry in the zip, and every `audio/` entry SHOULD be referenced by an asset. Unreferenced audio entries: reader skips them with a warning. An asset nobody's cue uses: reader MAY drop it.

Assets are separate from cues so that two cues can share one file (a duplicated cue doesn't need two copies). Writers MAY dedupe by hash; readers MUST handle sharing.

### 2.4 Cues

Every cue has every field below. Writers MUST write all of them. Readers MUST accept a missing field by using the default (this is what makes additive changes safe, see 2.6).

| Field | Type | Default | Range / rule |
|---|---|---|---|
| `id` | string | (required) | 1–64 chars from `[a-z0-9_-]`, unique within the show. Readers MAY assign new ids on import, but MUST keep ids unique. |
| `type` | string | (required) | `"audio"`, `"silence"` or `"fadeout"`. Unknown type: reject the package (section 2.6). |
| `name` | string | `"Untitled cue"` | 1–100 characters after trimming. Empty: use the default. |
| `asset` | string or null | `null` | Audio cues: an id in `assets`, or `null` meaning "this cue's audio is missing" (the cue imports and shows as missing; it is not an error). Non-audio cues: MUST be `null`. A non-null id that isn't in `assets`: reject. |
| `volume` | number | `1.0` | 0.0–2.0. Linear amplitude multiplier: 1.0 = unchanged, 0.5 ≈ −6 dB, 2.0 ≈ +6 dB. |
| `fade_in` | number | `0.0` | 0–600 seconds. |
| `fade_out` | number | `0.0` | 0–600 seconds. |
| `pre_wait` | number | `0.0` | 0–3,600 seconds. Delay between GO and the cue starting. |
| `duration` | number | `0.0` | 0–3,600 seconds. Used only by `silence` and `fadeout` (see 2.5). Audio cues: writers write `0`, readers ignore it. |
| `loop` | boolean | `false` | Audio only. |
| `continue` | string | `"none"` | `"none"` or `"after_end"`. Section 2.5. |
| `trim_start` | number | `0.0` | ≥ 0 seconds into the source file. |
| `trim_end` | number or null | `null` | Seconds into the source file, or `null` for "to the end of the file". If not null it MUST be > `trim_start`; otherwise readers treat it as `null`. |
| `cuts` | array | `[]` | Up to 50 `[start, end]` pairs in source-file seconds, `end > start ≥ 0`. Section 2.5. |

All times are seconds as JSON numbers. Writers SHOULD round to 4 decimal places. Every number MUST be finite (no NaN or Infinity, which JSON can't carry anyway, but check after parsing in languages that allow them).

**Out of range values:** readers clamp numbers into range rather than rejecting, since an old show with a fade of 700 s is still a show. Wrong *types* (a string where a number goes) are a rejection.

### 2.5 What the fields mean (playback semantics)

Both implementations MUST play a show the same way. These are the rules ThornCue Web already follows; the desktop client copies them.

**Edited audio.** The audible part of an audio cue is the source file from `trim_start` to `trim_end` (or the end of the file), minus every `cuts` range. Readers normalise cuts first: drop pairs that are invalid, sort by start, merge overlaps. The kept segments are joined end to end with a 4 ms linear fade at each join so cuts don't click. Cuts outside the trim range have no effect. If `trim_start` is past the end of the file, it is clamped to the end. If the cuts remove everything, the cue has length 0: it ends immediately (and `continue` fires). Editors SHOULD refuse any edit that leaves less than 0.05 s. The file itself is never modified.

`length` below means the length of the edited audio.

**Audio cue, on GO:**
1. Wait `pre_wait` seconds. (A `pre_wait` cue shows as "Waiting"; Stop all cancels it.)
2. Start playing the edited audio at gain `volume × master`.
3. If `fade_in` > 0: gain ramps linearly from 0 to `volume` over `fade_in` seconds.
4. If not looping and `fade_out` > 0: gain ramps linearly from `volume` to 0 over the last `fade_out` seconds of `length`. If the fade-in and fade-out overlap, the fade-out starts where the fade-in ends.
5. If `loop` is true, the edited audio repeats seamlessly forever. `fade_out` is ignored while looping (it only applies if the cue is stopped with a fade). A looping cue never "ends" by itself.
6. GO on a cue that is already playing restarts it from the beginning.

**`silence` cue, on GO:** stops every running cue immediately (no fade), then counts `duration` seconds. Its purpose is a timed gap before an auto-continue.

**`fadeout` cue, on GO:** every running audio cue fades from its current level to 0 over `duration` seconds, then stops. The cue itself lasts `duration` seconds. Cues stopped this way do **not** trigger their own `continue`.

**`continue`:**
- `"none"`: nothing happens when the cue ends.
- `"after_end"`: when this cue ends by itself (reached the end of its length, or its `duration` for silence/fadeout), the next cue in the list fires as if GO were pressed. If the cue was stopped (Stop all, a fadeout cue, a restart), the continue does not fire. A looping cue never ends, so it never continues.

The web's checkbox is labelled "Auto-continue" and its JSON field is `auto_continue: true`. In this format that is `"continue": "after_end"`. The field is an enum, not a boolean, so v1 readers will reject an unknown future mode such as `"with_start"` instead of silently playing it wrong (section 2.6).

**Volume and master:** `volume` multiplies the signal. Master volume is a playback setting, not part of the show, and isn't in the package.

### 2.6 Compatibility rules

- **Unknown fields** anywhere in the manifest: readers MUST ignore them, and editors that rewrite a show SHOULD preserve them. Adding an optional field with a default that means "behave as before" is not a version bump.
- **Unknown enum values** (`type`, `continue`): readers MUST reject the package with a message to update ThornCue. Playing a cue with a misunderstood type during a show is worse than refusing to open it.
- Anything that changes the meaning of an existing field, or adds a value a v1 player would misplay, bumps `format_version`.

---

## 3. Writing a package (what every exporter does)

1. Build the manifest with every field present. Drop `audio` cues' assets that are missing on disk and set those cues' `asset` to `null` (don't fail the whole export because one file is gone; the importer will show it as missing).
2. Write `manifest.json` first, then each audio file under `audio/<asset-id>.<ext>` with STORE. Compute SHA-256 and byte count while streaming each file; don't load whole files into memory.
3. Because the hashes go in the manifest and the manifest goes first, hash the files in a first pass, then write. (Or write audio to a temp zip and the manifest afterwards; but then it isn't first. Hash first.)
4. Write to a temporary file, then rename into place / stream it to the client. Never build the whole package in RAM.

Suggested download filename: the show name made filesystem-safe, plus `.tqs`, e.g. `Cinderella.tqs`. MIME type: `application/vnd.thorncue.show+zip` (unregistered; `application/zip` is an acceptable fallback).

## 4. Reading a package (what every importer does)

1. Open the zip. Apply every rule in 1.3 to the central directory before extracting anything.
2. Read and parse `manifest.json`. Check `format` and `format_version`. Validate types, clamp ranges, apply defaults, normalise cuts.
3. Check cross-references: every non-null cue `asset` exists in `assets`; every asset has its entry; every asset `path` matches its id.
4. Extract audio to a temp directory, counting bytes and computing SHA-256 as you go. Reject on hash mismatch or size mismatch ("file X is damaged").
5. Check each audio file actually decodes as audio (web: content sniffing; client: probe with the decoder). A file that doesn't decode is rejected; don't import a show that will fail at GO.
6. Move into place. Clean up the temp directory on every failure path.

Error messages are for a sound operator, not a developer: say which file or cue, and what to do.

## 5. Legacy: web "Export ZIP" (pre-v1)

ThornCue Web's original export, still worth importing:

```
<CODE>/show.json
<CODE>/audio/<cue-id>.<ext>
```

`show.json` is `{ "code", "name", "author", "notes", "cues": [...] }` where each cue has `id, name, filename, volume, loop, fade_in, fade_out, auto_continue, pre_wait, type, duration, trim_start, trim_end, cuts`. Some older cues lack `type`, `duration`, `trim_*` or `cuts`; default them as in 2.4. Map:

| Legacy | v1 |
|---|---|
| top-level folder name / `code` | `show.source = null` (a legacy zip doesn't say which server it came from); keep `code` only for display |
| `filename` `""` | `asset: null` (non-audio cue, or audio cue with no file) |
| `filename` `"<x>.<ext>"` | asset id `<x>` (lowercased), path `audio/<x>.<ext>`; hash computed on import |
| `auto_continue: true/false` | `continue: "after_end"/"none"` |
| `fade_out` on a `fadeout` cue | ignored (the web plays `duration` as the fade time; it only ever used `duration`) |

Legacy zips get the same safety rules as section 1.3. There is no hash to verify, so importers compute one.
