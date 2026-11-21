# ThornCue

Free, open-source theatre sound-cue playback for Windows, macOS and Linux. Load a show, put the first cue on standby, press **Space**, and ThornCue fires each cue in order: fades, pre-waits, loops and auto-continue included.

ThornCue desktop is what runs in the booth. [ThornCue Web](https://github.com/timezone-x/thorncue-web) stays as the online, multi-user version for building shows together; you can import a show from it, or from a `.tqs` file, and run it here with no network.

Licence: GPL-3.0 (see `LICENSE`).

## Sample show

The first time you open ThornCue, the Library offers **Open the sample show** or **Skip**. Either answer is remembered, so the card shows once. **Settings → Sample show** has **Open the sample show** and **Reset the sample show** (which asks first, then replaces it with a fresh copy).

The sample show is a short cue list that teaches by its cue names: what to press and what each cue does (fades, a trim and cut, a loop, an auto-continue chain, silence, a fade-out, two groups). It's an ordinary show in your library named "Sample show": rename it, edit it or delete it. Open goes back to it, even renamed; if you've deleted it, Open imports a new one. It comes from `src-tauri/sample/sample-show.tqs`, a copy of thorncue-spec's `samples/sample-show.tqs` (CC0 audio; credits there).

## Export audio

**Export audio…** (in the **Show ▾** menu at the top of a show, or a show's **More ▾** menu in the Library) saves a zip of audio files, for a sound designer, a rehearsal recording or another playback system. It isn't a show file: use Export .tqs… to move a show. The options (defaults in brackets):

- **Cues:** [All], Selected (when cues are shift-selected), or one group.
- **Apply edits** [on]: each cue as it plays (trims, cuts, fades, volume; loops once), as [WAV] or MP3 (320 kbps; lower for low sample rates). Off: the original files, byte for byte.
- **Keep cue numbers in file names** [on]: `Q03 Storm.wav`, padded so they sort. Off: `Storm.wav`.
- **Folder per group** [off]: `Act 1/Q03 Storm.wav`; cues not in a group stay at the top.
- **The cue list** [on]: `cue-list.csv` with number, name, type, file, group, volume %, fades, pre-wait, loop, auto-continue and length.
- **The .tqs** [off]: the whole show, so the zip can be imported again later (Import file… opens the show inside it).

The line under the options says how many files and roughly how big. Exporting shows "Rendering 7 of 24" with **Stop**; **Hide** closes the dialog and keeps exporting, so GO and the keyboard work. Playback is never affected: the export decodes and encodes on its own thread.

In the inspector, **Export ▾** saves one cue as a cue file (.tqc), WAV or MP3; with a shift-selected range, WAV and MP3 save one zip with the defaults above.

Spec: `docs/format/FORMAT.md` section 7 (shared with ThornCue Web, which makes the same zip). MP3 encoding uses [LAME](https://lame.sourceforge.io) (LGPL), built in.

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
