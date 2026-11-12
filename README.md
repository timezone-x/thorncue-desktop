# ThornCue

Free, open-source theatre sound-cue playback for Windows, macOS and Linux. Load a show, put the first cue on standby, press **Space**, and ThornCue fires each cue in order: fades, pre-waits, loops and auto-continue included.

ThornCue desktop is what runs in the booth. [ThornCue Web](https://github.com/timezone-x/thorncue-web) stays as the online, multi-user version for building shows together; you can import a show from it, or from a `.tqs` file, and run it here with no network.

Licence: GPL-3.0 (see `LICENSE`).

## Opening an unsigned build

Release builds aren't signed yet, so your computer will warn you the first time.

- **macOS:** open the `.dmg`, drag ThornCue to Applications, then in Finder **right-click ThornCue → Open → Open**. You only need to do this once. If macOS says the app "is damaged", run `xattr -dr com.apple.quarantine /Applications/ThornCue.app` in Terminal and try again.
- **Windows:** when SmartScreen says "Windows protected your PC", click **More info → Run anyway**.
- **Linux:** make the AppImage executable (`chmod +x ThornCue_*.AppImage`) and run it, or install the `.deb`.

## Building from source

You need [Rust](https://rustup.rs) (stable), [Node.js](https://nodejs.org) 22 or newer, and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS. On Debian or Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev librsvg2-dev libasound2-dev libdbus-1-dev pkg-config build-essential
```

Then:

```bash
npm install
npm run tauri dev       # run the app with live reload
npm run tauri build     # build an installer for this OS (in target/release/bundle/)
```

## Tests and checks

```bash
cargo test --workspace                        # all Rust tests; engine tests need no sound card
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
npm run check                                 # svelte-check / TypeScript
```

CI runs all of these on Linux, Windows and macOS for every push. Pushing a tag `v*` builds unsigned installers for all three and attaches them to a draft GitHub release.

## Layout

```
crates/tqs/        .tqs show format: read, write, legacy web zip import
crates/engine/     the cue engine, on kira
src-tauri/         the Tauri app: commands, library, settings, web import
src/               Svelte 5 + TypeScript UI
docs/              architecture, audit, progress, and the shared .tqs format spec
```

Read `docs/ARCHITECTURE.md` before changing anything structural. The show format and the playback rules are in `docs/format/FORMAT.md`.
