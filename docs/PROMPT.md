# ThornCue desktop: build it

You're building ThornCue desktop, a native, cross-platform (Windows, macOS, Linux) theatre sound-cue player, from an empty repo. It replaces the web version in the booth. It has to be **reliable during a live show** above everything else, then easy to pick up, then fast and light.

Everything you need is in the repo already:

| File | What it is |
|---|---|
| `CLAUDE.md` | Standing rules. Read first. |
| `docs/ARCHITECTURE.md` | The design: stack, threads, engine, files, web import, UI, CI. Decisions are made; follow them. |
| `docs/AUDIT.md` | Audit of the web version: what to port, rebuild or drop, and the web bugs (W1–W9) this app must not repeat. |
| `docs/format/FORMAT.md` | The `.tqs` show format **and the playback rules** (section 2.5). Shared with the web repo. Read-only. |
| `docs/format/fixtures/` | Valid, invalid and legacy packages, expected results, and test audio. Drive tests from `index.json`. |
| `docs/format/tools/tqs_ref.py` | Reference reader/writer in Python. Port its behaviour to `crates/tqs`. |
| `reference/web/` | The web version's `DESIGN.md`, `style.css`, `show.html`, `app.js`. Port the look and the behaviour from these. Read-only, not built. |

## How to work

1. **Start in plan mode.** Read everything above, then show me your plan for M0–M3 with the crate list (newest stable versions, checked on crates.io / npm) and anything in ARCHITECTURE.md you think won't work as written. Wait for my go-ahead.
2. After that, **work through the milestones in order without stopping to ask**, unless you're blocked or a decision contradicts the docs. Commit at the end of each milestone (CI green), push, and write one line in `docs/PROGRESS.md`: date, milestone, what works, what's knowingly left.
3. Each milestone ends with its acceptance checks passing. If one can't pass (no sound device in your environment, for example), say exactly what you couldn't verify and how I should check it by hand.
4. Don't build ahead. A later milestone's feature doesn't go in early "while you're there".
5. If time runs short, a smaller milestone that fully works beats a bigger one that half works.

The goal for today is M0 to M5: I can import a show from a file or from ThornCue Web and run it. M6 onward can follow.

---

## M0: scaffold and CI

- Tauri 2 (latest 2.x stable, not 3.0 alpha) + Svelte 5 + TypeScript + Vite, created with `npm create tauri-app` (`svelte-ts`). Cargo workspace with `crates/tqs`, `crates/engine`, `src-tauri` as in ARCHITECTURE.md.
- Product name `ThornCue`, identifier `xyz.thorngate.thorncue`. `LICENSE`: GPL-3.0.
- `src/styles/tokens.css`: the eight colour-theme blocks and the layout tokens copied **verbatim** from `reference/web/style.css`. Copy `reference/web/DESIGN.md` to `DESIGN.md` at the root and remove the web-only parts (accounts, sharing).
- Theme switching (family + Dark/Light/System) stored in `settings.json` via a Rust command, applied before first paint, following System live. Same model as the web.
- An empty Library screen in the web's style.
- CI workflow per ARCHITECTURE.md (push: fmt, clippy, test, svelte-check, build on all three OSes; tags: draft release with installers).
- README: what it is, how to build, how to open unsigned builds on macOS and Windows, how to run tests.

**Accept:** `npm run tauri dev` opens a themed, empty Library window. CI is green on all three OSes. `cargo test --workspace` runs (even if it has few tests).

## M1: `crates/tqs`

Port `docs/format/tools/tqs_ref.py` to Rust, following FORMAT.md (the spec wins over the Python if they disagree; tell me if you find a disagreement).

- `read_package(path, dest_dir, limits) -> Result<(Manifest, Vec<Warning>), TqsError>`: every rule in sections 1.3 and 4, extracting into a temp dir next to `dest_dir` and renaming into place only on success. Audio check: probe and decode the first packets with symphonia (not just magic bytes).
- `write_package(show_dir, out_path)`: section 3. Manifest first, audio STORE, atomic.
- `import_legacy_zip(path, dest_dir, limits)`: section 5.
- `Manifest`, `Cue`, `Asset` etc. as serde types with the defaults and clamping from 2.4. Unknown fields are preserved on round trip (`#[serde(flatten)] extra: Map`).
- `TqsError` messages are for users and match the meaning of the Python ones.

Tests generated from `docs/format/fixtures/index.json`: every `valid` fixture imports and its manifest equals `expected/<name>.json` on every spec field (numbers within 1e-9; ignore `generator`, `exported_at`, and the unknown `x_…` fields in fixture 03, which you preserve but the expected files leave out); every `invalid` fixture is rejected and leaves **nothing** in the destination's parent directory; the legacy zip converts to its expected manifest (hashes included); write → read round-trips fixture 02 exactly.

**Accept:** all fixture tests pass. `cargo clippy -D warnings` clean.

## M2: library and file import

- Library on disk exactly as ARCHITECTURE.md "Files and the library" (unpacked `.tqs` folders, `local.json`, atomic writes, `.bak` recovery).
- Library screen: table of shows; **Import file…** (dialog, `.tqs` and `.zip`), drag-and-drop onto the window, **Open**, **Delete** (to trash, with confirm). Import shows progress and any warnings, and a clear error on rejection.
- Opening a show shows the Show screen with the cue table filled in (read-only for now; no audio yet). Port the table and its columns, row notes and the standby ▶ from `reference/web/app.js` (`buildRow`, `rowNotes`).

**Accept:** importing every `valid` fixture works and opens; every `invalid` one shows an error and leaves nothing behind; the legacy zip imports; deleting moves the folder to the trash.

## M3: engine core

`crates/engine` per ARCHITECTURE.md "Engine design": engine thread owning the kira `AudioManager`, command channel in, events out, prep pool, prep state per cue, `LoadShow`.

- Transport: GO, Stop all, Pause/Resume, standby (click a row, ↑ ↓, Back to top), J jump-and-play, Restart audio engine. Keyboard exactly as the web (`reference/web/app.js` L1842–1895) plus double-Esc panic.
- Transport bar, standby name, status (Ready / Running n / Paused / Loading n of m / Device lost), now-playing strip with elapsed/remaining. Positions come from `engine://positions`.
- Plain audio cues at their volume. kira tweens are allowed for fades in this milestone only (ARCHITECTURE.md says why they're replaced in M4).
- Per-row prep state (Loading / Ready / Missing / Can't decode).
- `get_engine_state` so a webview reload redraws the current state.

Engine tests with the mock backend: GO plays standby and advances; GO at end of list does nothing; Stop all stops everything; Pause/Resume; **GO on a still-decoding cue followed by Stop all never starts it (W1)**; Restart refuses commands until rebuilt.

**Accept:** with a real sound device, import fixture 02 and play through it with the keyboard. Reload the webview while a cue plays (dev build: right-click → Reload, or `location.reload()` in devtools): audio continues, UI comes back showing it. Tests pass in CI.

## M4: full playback semantics

Everything in FORMAT.md 2.5, matching the web:

- `CueGain` effect replacing kira tweens: linear-amplitude fade in and fade out, fades overlap rule, live volume change while playing, stop-with-fade.
- Pre-wait (sample-accurate, paused by Pause, cancelled by Stop all), loop (seamless, fade-out ignored while looping), restart on re-GO.
- Edited audio: trims via `slice`, cuts rendered with 4 ms join ramps, cached by (asset, trim, cuts).
- Silence and fade-out cues on the engine clock; `continue: after_end` fired by the engine; stopped cues never continue; length-0 cues end at once and continue.
- Master volume (header slider, as the web).
- Settings: GO lockout (default off), Stop-all fade (default 0).

Mock-backend tests against `ladder-dc.wav` for every item in ARCHITECTURE.md "Testing the engine". Compare against the numbers in `docs/format/fixtures/README.md`.

**Accept:** all engine tests pass. Fixture 02 played through by ear behaves as its table in `fixtures/README.md` says.

## M5: import from ThornCue Web

ARCHITECTURE.md "Importing from ThornCue Web": Settings connection (URL + token, tested, token in the keychain), Web shows screen with local status per show, import with progress, Replace / Keep both, import from a pasted join link.

The web endpoints are being built in parallel. Write a tiny mock server for tests (an `axum` or `tiny_http` dev-dependency serving the fixtures under the same paths and JSON shapes) so this milestone doesn't depend on the real server being up. Test: list, import, 304 when unchanged, 401 shown as "token not accepted", 404 for a revoked link, a truncated download rejected and cleaned up.

**Accept:** against the mock server, everything above works from the UI. If the real server is reachable at the URL I give you, import a real show from it.

---

## M6: editing and export

Port the inspector and editing from `reference/web/app.js`: name, volume (slider + number, 0–200%), fades, pre-wait, duration (silence/fade-out), loop, auto-continue; drag to reorder; Add cue (file dialog / drop; copies audio into the show folder), Add silence, Add fade-out, Duplicate (shares the asset), Replace file, Delete. New show, Duplicate show, Export .tqs. Autosave per ARCHITECTURE.md with the header save state shown only when it isn't "Saved". Inspector hide/resize remembered.

**Accept:** build a show from scratch, quit mid-edit, reopen: nothing lost. Export it, import the export: identical cues. Corrupt `manifest.json` by hand: the app recovers from `.bak` and says so.

## M7: trim and cut editor

The web's waveform editor (`reference/web/app.js` L1149–1464): peaks computed in Rust on the prep pool (from the louder channel, fixing W9), in/out markers, select and cut, put back, undo, restore, audition from marker, I/O/X/Ctrl+Z keys.

## M8: show readiness

Output device selection and buffer size (ARCHITECTURE.md "Output devices"), device-lost handling, keep-awake while a show is open, crash recovery (reopen the last show at its standby cue after an unclean exit), memory budget with streaming fallback, audition to a second device, `.tqs` file association, optional master limiter (W8).

---

## When you finish a session

Tell me: which milestones are done and their acceptance results, what you couldn't verify and how I should check it, anything in the docs you disagreed with or worked around, and the next step. File and line references, not whole files.
