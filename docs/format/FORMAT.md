# ThornCue package format (`.tqs`, `.tqc`, `.tqg`), format_version 1

Spec revision **1.2** (2026-11-19). Status: **format_version 1, stable.** This file is the contract between ThornCue Web and the ThornCue desktop client. The master copy lives in `timezone-x/thorncue-spec`; each app repo carries an unedited copy in `docs/format/`. Changes are made in the spec repo only (see its `SESSIONS.md`), and `CHANGELOG.md` there lists what changed in each revision.

| Extension | `format` | What it is |
|---|---|---|
| `.tqs` | `thorncue.show` | A whole show: cue list, groups and audio |
| `.tqc` | `thorncue.cues` | One or more cues with their audio, for copying cues between shows (section 6) |
| `.tqg` | `thorncue.group` | One group with its cues and audio (section 6) |

All three are the same container with the same manifest; they differ only in which top-level sections they have. One reader handles all three.

Section 7 covers something that is **not** a package: the audio export (`<show> audio.zip`), so both apps render cues and name the files the same way, and both recognise one when it's imported by mistake.

Files in this folder:

- `FORMAT.md`: this spec (normative).
- `manifest.schema.json`: JSON Schema for `manifest.json` (all three kinds). Covers shape and ranges; the rules below that a schema can't express (cross-references, zip safety, hashes, grouping) are normative too.
- `fixtures/`: packages every implementation must pass. `valid/` must import, `invalid/` must be rejected with a clear error, `legacy/` is the old web "Export ZIP" format.
- `tools/tqs_ref.py`: reference reader and writer, Python standard library only. When this spec and the reference tool disagree, the spec wins and the tool has a bug.
- `tools/render_ref.py`: reference renderer for section 7 (numpy and the standard library's `wave`), with `fixtures/render/` to check an app's renderer against.

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
| `format` | string | `"thorncue.show"`, `"thorncue.cues"` or `"thorncue.group"` (the *kind*: show, cues, group). Anything else: reject ("not a ThornCue file"). An app that only accepts one kind in a given place (opening a show, importing cues) says which kind the file is instead ("This is a cue file (.tqc), not a show"). |
| `format_version` | integer | `1`. A reader MUST reject a higher version with a message telling the user to update ThornCue ("This show was made by a newer version of ThornCue"). There is no minor version: additive changes don't bump it (see 2.6). |
| `generator` | object | `name` (string, ≤ 64) and `version` (string, ≤ 64). Informational. Readers MUST NOT change behaviour based on it. |
| `exported_at` | string | RFC 3339 UTC timestamp, `Z` suffix. Informational. |
| `show` | object | Shows only, required there. Section 2.2. In `.tqc` and `.tqg` it is absent; readers MUST ignore it if present. |
| `groups` | array | Optional, default `[]`. Section 2.7. Writers SHOULD omit it when there are no groups, so a group-free file is byte-for-byte what a 1.0 writer produces. |
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
| `revision` | string | Opaque string that changes whenever the show changes. Clients compare it for equality only and MUST NOT parse it. (The web currently uses the show file's modification time in nanoseconds as hex; don't rely on that.) |

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

Every cue has every field below except `group`. Writers MUST write all of them, and write `group` only for cues that are in a group. Readers MUST accept a missing field by using the default (this is what makes additive changes safe, see 2.6).

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
| `group` | string or absent | absent | Id of the group this cue belongs to (section 2.7). Absent (or `null`) means ungrouped. |

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

### 2.7 Groups

A group is a named, coloured label around a run of consecutive cues, for finding your way around a long show. **Groups never change playback**: GO, standby and `continue` behave exactly as if there were no groups. Whether a group is shown collapsed is a per-user view setting and is never stored in a package.

`groups` is an array of:

| Field | Type | Rule |
|---|---|---|
| `id` | string | 1–64 chars from `[a-z0-9_-]`, unique among the file's groups. Readers MAY assign new ids, keeping cue references in step. |
| `name` | string | 1–60 characters after trimming. Empty: readers use `"Group"`. |
| `color` | string | `#rrggbb`, lowercase. Writers MUST write lowercase; readers lowercase it. Anything that isn't six hex digits: readers use `#3e7bd6` and warn. Colour is the user's label only; apps MUST NOT use it to mean status. |

Rules (readers enforce them in this order):

1. `groups` that isn't an array, an entry that isn't an object, an invalid or duplicate `id`, or a cue whose `group` names an id not in `groups`: **reject** (wrong types and broken references are rejections, as for assets).
2. A group's cues MUST be consecutive in `cues`. If they aren't, readers pull each group's cues together at the position of the group's first cue, keeping their order, and warn. (Ungrouped cues keep their relative order.)
3. A group with no cues is dropped with a warning.
4. One group per cue. Groups don't nest.

Order of `groups` has no meaning; writers SHOULD list them in the order they first appear in `cues`.

---

## 3. Writing a package (what every exporter does)

1. Build the manifest with every field present (and `groups` / cue `group` only when used). Drop `audio` cues' assets that are missing on disk and set those cues' `asset` to `null` (don't fail the whole export because one file is gone; the importer will show it as missing). Include only the assets the exported cues use.
2. Write `manifest.json` first, then each audio file under `audio/<asset-id>.<ext>` with STORE. Compute SHA-256 and byte count while streaming each file; don't load whole files into memory.
3. Because the hashes go in the manifest and the manifest goes first, hash the files in a first pass, then write. (Or write audio to a temp zip and the manifest afterwards; but then it isn't first. Hash first.)
4. Write to a temporary file, then rename into place / stream it to the client. Never build the whole package in RAM.

Suggested download filenames, made filesystem-safe:

| Kind | Filename | MIME type |
|---|---|---|
| show | `<show name>.tqs`, e.g. `Cinderella.tqs` | `application/vnd.thorncue.show+zip` |
| cues | one cue: `<cue name>.tqc`; several: `<show name> - Q4 to Q9.tqc` | `application/vnd.thorncue.cues+zip` |
| group | `<group name>.tqg`, e.g. `Storm.tqg` | `application/vnd.thorncue.group+zip` |

The MIME types are unregistered; `application/zip` is an acceptable fallback.

## 4. Reading a package (what every importer does)

1. Open the zip. Apply every rule in 1.3 to the central directory before extracting anything.
2. Read and parse `manifest.json`. Check `format` (and that it's a kind this place accepts) and `format_version`. Validate types, clamp ranges, apply defaults, normalise cuts.
3. Check cross-references: every non-null cue `asset` exists in `assets`; every asset has its entry; every asset `path` matches its id. Then groups, per 2.7 and, for `.tqc`/`.tqg`, section 6.
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
| `fade_out` on a `fadeout` cue | kept as it is but never used for playback (a fade-out cue fades over `duration`; the web never read `fade_out` for these) |

Legacy zips get the same safety rules as section 1.3. There is no hash to verify, so importers compute one.

## 6. Cue files (`.tqc`) and group files (`.tqg`)

Same container (section 1) and manifest (section 2), for moving cues between shows.

### 6.1 Shared rules

- No `show` section. Readers ignore one if present.
- `cues` has at least 1 cue. `assets` holds only what those cues use; two cues sharing a file share an asset, as in a show.
- Cue ids only need to be unique within the file.

### 6.2 `.tqc`: cues

`format: "thorncue.cues"`. One or more cues, in order. No groups: writers omit `groups` and cue `group`. Readers ignore both if present (with a warning), so exporting cues that were in a group gives plain cues.

### 6.3 `.tqg`: a group

`format: "thorncue.group"`. `groups` holds **exactly one** group, and **every** cue's `group` is that group's id. Anything else (no group, two groups, a cue outside the group, no cues) is rejected.

### 6.4 Importing a `.tqc` or `.tqg` into a show

What both apps do, so the result is the same in either:

1. Validate the whole file first (sections 1.3 and 4). Nothing is added to the show unless the whole file is good.
2. Every imported cue, asset and group gets a **new id**. Nothing in the file is ever merged into something already in the show: a `.tqg` called "Storm" imported into a show that has a "Storm" group becomes a second group with the same name.
3. Imported cues are added **at the end of the show**, in file order. (Apps MAY offer "insert after the selected cue" as well; if a `.tqc` lands inside a group it joins that group, and a `.tqg` is never inserted inside another group.)
4. A `.tqg`'s cues stay together as one group with the file's name and colour.
5. Standby doesn't move. Running cues aren't touched.

Audio is copied into the show the same way the app stores any other audio (the web gives each cue its own file; the desktop app keeps shared assets shared). An app MAY skip copying a file it already holds with the same SHA-256.

## 7. Audio export (not a package)

Both apps can export cues as ordinary audio files, for a sound designer, a rehearsal recording or another playback system. An audio export is **not** a ThornCue file: there is no manifest, and nothing in it is read back as cues. This section fixes how a cue is rendered and how the files are named, so both apps produce the same export from the same show.

### 7.1 Rendering a cue

An audio cue renders to exactly what GO plays (2.5), once, with nothing else running:

1. **Decode** the source file at its own sample rate, to floating point. 16-bit PCM decodes as `sample / 32768`. Never resample (except for MP3, 7.2). Channels: a mono file stays mono and a stereo file stays stereo. A file with more than two channels is mixed down to stereo the way the app's decoder does it for playback. (An app whose decoder doesn't keep the channel count MAY treat a file whose two channels are equal in every frame as mono.)
2. **Edit** as in 2.5, to the sample. With `sr` the sample rate, `N` the decoded length in frames and `D = N / sr`:
   - Kept segments: start with `[clamp(trim_start, 0, D), clamp(trim_end, 0, D)]` (`trim_end` null, or not after the start: `D`), subtract every cut, and drop any segment of 1 ms or less (`end − start ≤ 0.001`). No segments: the cue renders to **nothing** (no file).
   - A segment `[s, e]` is frames `round(s × sr)` up to (not including) `round(e × sr)`, never past `N`. `round` is round-half-away-from-zero everywhere in this section.
   - One segment: those frames, untouched. Several: the segments end to end, and at each join a linear ramp of `r = min(round(0.004 × sr), len // 2)` frames, where `len` is that segment's frame count. A segment after a join has frame `k` (from its start, `0 ≤ k < r`) multiplied by `k / r`; a segment before a join has frame `k` from its **end** (the last frame is `k = 0`) multiplied by `k / r`. No ramp at the very start or end.
3. **Gain.** With `M` edited frames, `L = M / sr` and frame `i` at `t = i / sr`:
   - Fade-out start: if `loop` is false and `fade_out > 0`, `fs = max(L − fade_out, min(fade_in, L))`; otherwise there is no fade-out.
   - Envelope: if there is a fade-out and `t ≥ fs`, `(L − t) / (L − fs)` clamped to 0–1 (0 if `L = fs`); otherwise, if `fade_in > 0` and `t < fade_in`, `t / fade_in`; otherwise 1.
   - Each sample is multiplied by `volume × envelope(t)`. `volume` is the cue's 0–2. **Master volume is ignored**: it's a playback setting (2.5).
4. **Loops** render one pass, with no fade-out (as 2.5 step 5: the fade-out never plays while looping). The fade-in applies.
5. **Clip.** Every sample is clamped to −1…+1 before encoding. Only a cue whose volume is over 100% (or whose source is already at full scale) can reach that. The file is then clipped where playback would have been louder than full scale; it is not turned down to fit.

Silence and fade-out cues have no audio and render to nothing. So does an audio cue whose audio is missing.

### 7.2 File formats

| Format | Encoding |
|---|---|
| WAV | RIFF/WAVE, PCM 16-bit little-endian, the source's sample rate, 1 or 2 channels as 7.1. A sample `v` (after clipping) becomes `round(v × 32768)` if negative and `round(v × 32767)` otherwise. The 44-byte canonical header (`fmt ` chunk of 16 bytes, then `data`), nothing else. |
| MP3 | MPEG audio layer III, constant bit rate, encoded with LAME (or anything equivalent) with the Xing/LAME info frame, so players skip the encoder delay. **320 kbps** at the source's sample rate. MP3 only allows 8, 11.025, 12, 16, 22.05, 24, 32, 44.1 and 48 kHz, and lower rates have lower ceilings, so the highest the rate allows is used: **160 kbps** at 16–24 kHz, **64 kbps** at 8–12 kHz. Any other rate is resampled to 44.1 kHz if it is a multiple of 11,025 Hz and to 48 kHz otherwise. Stereo is joint stereo. The decoded length MUST be the rendered length within one MP3 frame (1,152 samples at 32 kHz and up, 576 below). |

"Original files" (an export with edits turned off, 7.3) are the uploaded files exactly as stored, byte for byte, with their own extension, lowercased.

Apps that render in floating point won't match the reference to the bit. `fixtures/render/` gives a cue, its source and the expected WAV; an app's WAV MUST have the same header and length, and every sample within the tolerance stated there (one 16-bit step) of the expected one.

### 7.3 The audio zip

The options, with defaults in brackets. Apps show them in this order and with these meanings; the wording is each app's own.

| Option | Meaning |
|---|---|
| Cues [all] | All cues, the selected cues, or one group's cues. |
| Apply edits [on] | On: each audio cue rendered (7.1) as WAV [default] or MP3. Off: the original files (7.2), no format choice. |
| Keep cue numbers in file names [on] | `Q03 Storm.wav` instead of `Storm.wav`. |
| Folder per group [off] | Grouped cues go in a folder named after their group; others stay at the top level. |
| Include the cue list [on] | Adds `cue-list.csv` (7.4). |
| Include the .tqs [off] | Adds the show as a package (sections 1–3), so the export can be imported later (7.5). |

**Files.** One file for every audio cue that renders to something (7.1), or, with edits off, every audio cue whose file exists. Cues sharing a source each get their own file: the point is files named after cues. A cue that renders to nothing gets no file and keeps its row in the cue list.

**Cue numbers.** In ThornCue v1 a cue's number is its position in the show, from 1, shown as `Q3`. It is always the cue's number in the whole show, even when only some cues are exported. In file names it is zero-padded to the number of digits in the show's cue count, so files sort in cue order: `Q03` in a show of 24 cues, `Q003` in a show of 120, `Q3` in a show of 9. Each app builds a number in exactly one place (custom cue numbers will replace it there).

**Names.** A file's name is `<number> <cue name>.<ext>` (or `<cue name>.<ext>` with numbers off), made safe by this rule, which every name in this section uses:

1. Remove `\ / : * ? " < > |` and control characters other than whitespace (U+0000–U+0008, U+000E–U+001F, U+007F).
2. Turn every run of whitespace (spaces, tabs, line breaks, no-break spaces) into one space; trim spaces and dots from both ends.
3. Cut to 100 characters (60 for a folder name), then trim again.
4. If what's left is empty, use the fallback (a file: the cue's number; a folder: `Group`).
5. If it is a name Windows reserves (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, any case), add `_`.

The extension is added after this: `wav`, `mp3`, or the original's.

**Folders.** With "Folder per group", each group's files go under `<group name>/`. Ungrouped cues stay at the top level. Groups are folders one level deep; there is no other nesting.

**Collisions.** Names are compared case-insensitively, within one folder (the top level is one folder, and each group folder another). The top level reserves `cue-list.csv` and the `.tqs`'s name first, and folder names and file names share the top level. The first claimant (in cue order; folders in the order their groups first appear) keeps the name; later ones get ` (2)`, ` (3)` and so on before the extension: `Storm.wav`, `Storm (2).wav`. Two groups both called "Storm" are `Storm/` and `Storm (2)/`.

**The zip.** Named `<show name> audio.zip`. A partial export MAY add which part, as the `.tqc` name does (3): `<show name> - Q4 to Q9 audio.zip`, `<show name> - Storm audio.zip`. Entries in this order: `cue-list.csv`, the audio files in cue order, the `.tqs` last. Audio and the `.tqs` are **stored** (they're compressed already, or WAV that DEFLATE barely helps); `cue-list.csv` is DEFLATEd. The `.tqs` is `<show name>.tqs`, holding the whole show whatever the cue choice. The zip comment is exactly `thorncue.audio-export` (it's how readers recognise one, 7.5). Paths use `/` and are UTF-8, with the zip's language-encoding flag set on any name that isn't plain ASCII. The zip is written to a temporary file first, as in section 3.

### 7.4 `cue-list.csv`

One row per exported cue, in show order, **including silence and fade-out cues** (their timing matters to whoever rebuilds the show). UTF-8 with a byte-order mark (so spreadsheet apps read the names right), CRLF line ends, RFC 4180 quoting. The header row is exactly:

```
number,name,type,file,group,volume %,fade in,fade out,pre-wait,loop,auto-continue,length
```

| Column | Value |
|---|---|
| `number` | The cue number, unpadded: `Q3`. |
| `name` | The cue's name. |
| `type` | `Audio`, `Silence` or `Fade-out`. |
| `file` | Its path in the zip (`Act 1/Q03 Storm.wav`), or empty: silence and fade-out cues, missing audio, an edit that leaves nothing. |
| `group` | The group's name, or empty. |
| `volume %` | Audio cues: `volume × 100`, rounded to a whole number. Otherwise empty. |
| `fade in`, `fade out` | Audio cues: seconds. Otherwise empty. |
| `pre-wait` | Seconds, every cue. |
| `loop` | Audio cues: `no`, or `yes (file plays once)`, so nobody expects a loop in the file. Otherwise empty. |
| `auto-continue` | `yes` or `no`. |
| `length` | Seconds the cue lasts: an audio cue's edited length (2.5) when its audio is there (empty if not), a silence or fade-out cue's `duration`. |

Seconds are written with up to 3 decimals and no trailing zeros (`1.5`, `0`, `12.345`). The values describe the cue, not the file: with edits off, `volume %` and the fades are what the operator would have to apply to the original. A field that starts with `=`, `+`, `-`, `@`, a tab or a carriage return gets a leading `'`, so a spreadsheet never runs a cue name as a formula.

### 7.5 Importing an audio export

An audio export isn't a show, but people will try to open one. An importer recognises it **before** the legacy test (5), so one is never mistaken for an old Export ZIP: a zip whose comment is `thorncue.audio-export`, or (if the comment was lost when someone re-zipped it) a zip with no `manifest.json`, no `show.json` at the top level or one folder down, and a `cue-list.csv` at the top level.

- **With a `.tqs` at the top level** (exactly one): import that `.tqs` as a show, with every rule in sections 1–4 (its size counts against the same limit), and say so: "This is an audio export. Imported the show file inside it." Places that take cues (`.tqc`/`.tqg`) say instead that the show inside can be imported as a show.
- **Without one:** refuse it with: "This is an audio export, not a show. Export it again with "Include the .tqs" ticked."

The section 1.3 safety rules apply to the zip itself before anything is read from it.
