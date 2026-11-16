# Fixtures

`index.json` lists every fixture and what a reader must do with it. Run every implementation against all of them in CI.

- **`valid/*`**: must import (`.tqs` shows, `.tqc` cue files, `.tqg` group files). After import, the normalised manifest must equal `expected/<name>.json` (ignore `generator` and `exported_at`; compare numbers with a tolerance of 1e-9; key order doesn't matter). `03-needs-normalising.tqs` deliberately breaks writer rules (missing fields, out-of-range values, junk entries, an unused audio file) and does **not** pass `manifest.schema.json`. That is the point: readers must clamp and default it into exactly `expected/03-needs-normalising.json`. The same goes for `09-groups-need-normalising.tqs` (group cues not next to each other, an empty group, a bad colour) and `10-cues-with-groups.tqc` (a cue file carrying a show section and groups, which readers ignore).
- **`invalid/*`**: must be rejected, with nothing left behind on disk. `rule` says which section of FORMAT.md is being tested. Error wording is up to each implementation, but it must be a sentence a sound operator understands.
- **`legacy/web-export-STAGE-4821.zip`**: ThornCue Web's original Export ZIP. Importers that support legacy zips must produce `expected/legacy-web-export-STAGE-4821.json` (asset hashes included). `web-export-STAGE-4821.converted.tqs` is what the reference converter wrote, for comparison.
- **`audio/`**: the raw generated audio, for engine and decoder tests.

## Groups, cue files and group files (spec 1.1)

| Fixture | What it checks |
|---|---|
| `06-groups.tqs` | Two groups ("Act 1", "Storm") with ungrouped cues before, between and after; Act 1's colour is written uppercase and must come out lowercase |
| `07-cues.tqc` | Three cues, two sharing one FLAC; no `show`, no `groups` |
| `08-group.tqg` | One group ("Storm") with three cues, including a fade-out cue |
| `09-groups-need-normalising.tqs` | Reader pulls group `a`'s cues together (n1, n3, then n2), drops the empty group, names the unnamed one "Group", replaces the colour `blue` with `#3e7bd6`, keeps `x_icon` out of the expected output |
| `10-cues-with-groups.tqc` | Readers ignore a `.tqc`'s `show` and `groups` and output a plain cue |
| `invalid/group-*`, `invalid/groups-not-a-list.tqs` | Broken group references and types are rejections (FORMAT.md 2.7) |
| `invalid/tqg-*` | A `.tqg` must hold exactly one group, every cue in it, at least one cue (6.3) |

Groups never affect playback, so engine tests don't need these; they're for readers, writers and importers.

## The audio

All generated with ffmpeg (see `../tools/make_fixtures.py`). No real recordings.

| File | Contents | Use it for |
|---|---|---|
| `tone-a440.wav` | 2 s, 440 Hz, 22.05 kHz mono, 16-bit | Basic playback |
| `tone-e330.mp3` | 2 s, 330 Hz, MP3 32 kbps | MP3 decode |
| `tone-c262.ogg` | 2 s, 262 Hz, Ogg Vorbis | OGG decode, seamless loop |
| `tone-g392.flac` | 2 s, 392 Hz, FLAC | FLAC decode |
| `tone-d294.m4a` | 2 s, 294 Hz, AAC in MP4 | M4A decode (the one most likely to need an extra decoder feature) |
| `tone-b494.aiff` | 2 s, 494 Hz, AIFF 16-bit big-endian | AIFF decode (check your decoder supports it) |
| `stereo-48k.wav` | 2 s, 48 kHz stereo, left 440 Hz, right 660 Hz | Channel order, resampling to the device rate |
| `ladder-10s.mp3` | 10 s, pitch steps up every second | Hearing trims and cuts |
| `ladder-dc.wav` | 10 s, 8 kHz mono 16-bit, every sample in second *n* equals *n* × 1000 | **Sample-exact engine tests.** Trim 2–9 with a cut at 4–6 must give 2 s of 2000/3000, then 3 s of 6000/7000/8000, with 4 ms ramps only at the join |
| `not-audio.txt` | Text | Must fail the "is this audio" check |

## Fixture 02, cue by cue

| Cue | What it exercises |
|---|---|
| q01 | Trim 1.0–8.0, cuts 3–4 and 5.5–6, fade in 0.5, fade out 1.0, pre-wait 0.25, volume 0.8. Edited length 5.5 s |
| q02 | Same asset as q01, no edits (assets are shared, not copied) |
| q03 | Loop with fade in; fade out is ignored while looping |
| q04 | Fade-out cue, 1.5 s, continues after |
| q05 | Silence cue, 0.5 s, continues after |
| q06 | FLAC at volume 2.0 (+6 dB) |
| q07 | M4A at volume 0 (silent but running) |
| q08 | AIFF |
| q09 | 48 kHz stereo |
| q10 | DC ladder, trim 2–9, cut 4–6: edited length 5 s |
| q11 | `asset: null` on an audio cue: imports, shows as missing, GO reports it can't play |
| q12 | Cuts remove the whole file: length 0, ends at once, continue fires into q13 |
| q13 | Silence of 0 s: the end of the list |
