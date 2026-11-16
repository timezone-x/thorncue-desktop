# ThornCue desktop

Free, open-source theatre sound-cue playback for Windows, macOS and Linux. A native rebuild of ThornCue Web (`timezone-x/thorncue-web`), which stays as the online, multi-user version. This app is what runs in the booth during a show.

Read `docs/ARCHITECTURE.md` before changing anything structural. Playback rules are in `docs/format/FORMAT.md` section 2.5.

## Rules that don't bend

1. **Rust owns the show.** Audio, timing, standby, auto-continue, files and network live in Rust. The webview only sends intents and draws state. A webview reload must never stop or change playback.
2. **Never block GO.** Nothing on the GO path waits on disk, network, decoding or the UI. Nothing in an audio callback (`CueGain::process`, anything kira calls) allocates, locks or logs.
3. **Stop all always wins.** It stops every running cue and cancels every pending start, including cues still waiting on decode. (A running audition on the separate audition output is deliberately left alone: `docs/ARCHITECTURE.md` "Stop all and the audition".)
4. **Never lose a show.** Writes are atomic (temp file, fsync, rename) with a `.bak`. Imports go to a temp folder and only move into the library once fully validated. Deletes go to the OS trash.
5. **`docs/format/` is shared with the web repo. Don't edit it.** If the spec looks wrong, stop and say so.
6. `crates/tqs` and `crates/engine` never depend on Tauri.

## Commands

```bash
npm install
npm run tauri dev                     # run the app
cargo test --workspace                # all Rust tests (engine tests use kira's mock backend, no sound card needed)
cargo clippy --all-targets -- -D warnings
cargo fmt --all
npm run check                         # svelte-check / TypeScript
npm run tauri build                   # local installer for this OS
```

CI (`.github/workflows/`) runs fmt, clippy, tests, svelte-check and a build on Linux, Windows and macOS. Keep it green.

## Layout

```
crates/tqs/        .tqs format: read, write, legacy web zip import
crates/engine/     cue engine on kira
src-tauri/         Tauri app: commands, library, settings, web import
src/               Svelte 5 + TypeScript UI
docs/format/       shared .tqs spec + fixtures (read-only here)
reference/web/     the web version's show page, CSS and design doc, for porting (read-only, not built)
```

## Conventions

- Rust: `thiserror` for library errors, `anyhow` only in `src-tauri`. Errors that reach the user are full sentences a sound operator understands ("Q12's audio file is damaged. Re-import the show."), never debug output.
- Every Tauri command returns `Result<T, String>` with that user-facing message; log the detail with `tracing`.
- UI copy follows `DESIGN.md`: sentence case, plain words, buttons say what they do, status is never colour alone.
- Styles use the tokens in `src/styles/tokens.css`. No hex values in components.
- Tests: format code gets fixture tests (`docs/format/fixtures/index.json` drives them); engine behaviour gets mock-backend tests against `ladder-dc.wav`. A bug fix comes with a test that fails without it.
- Commits: one milestone or one fix per commit, imperative subject, body says what and why.

## Working with Connor

- When something's broken, explain why it's broken and how to reason about it, then make the fix. Don't just hand back a corrected file.
- Prefer targeted changes with file and line references over rewrites.
- Push back if a requested feature is a bad idea for a live-show tool, briefly, with the reason.
