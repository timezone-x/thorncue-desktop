# ThornCue desktop: architecture

The decisions below are made. If one turns out to be wrong in practice (an API doesn't exist, a crate can't do what's described), stop and say so rather than quietly working around it.

## The one rule

**Rust owns the show.** Audio, timing, the cue list, standby, auto-continue, files and network all live in Rust. The webview is a remote control and a display. If the webview reloads, crashes or hangs, nothing that is playing stops, nothing scheduled is lost, and the UI rebuilds itself from a snapshot when it comes back.

This is the main thing the web version couldn't do, and every design choice below follows from it.

## Stack

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri 2** (latest 2.x stable; **not** the 3.0 alphas) | Native webview, ~10 MB installers, Rust backend. |
| UI | **Svelte 5 + TypeScript + Vite** (`npm create tauri-app`, template `svelte-ts`) | The web show page spends a lot of code keeping the DOM in sync by hand (`renderRow`, `renderStandby`, `setValue`); runes remove that. |
| Audio | **kira 0.12** with features `default + aac + isomp4 + aiff` | cpal output, symphonia decoding, sample-accurate clocks, tracks with pause/resume, a mock backend for tests. Defaults cover mp3/ogg/flac/wav; M4A needs `aac` + `isomp4`, AIFF needs `aiff`. |
| Format | Our own crate, `crates/tqs` (`zip`, `serde_json`, `sha2`) | Shared with a possible future Rust web backend. No Tauri or kira dependency. |
| HTTP | `reqwest` with `rustls` (no OpenSSL) | Requests come from Rust, so there is no CORS to deal with. |
| Secrets | `keyring` | API token in the OS keychain. |
| Misc | `keepawake` (stop the OS sleeping during a show), `trash` (delete to the bin, not forever), `uuid`, `thiserror`, `tracing` | |

Versions: use the newest stable release of each when you add it, and check docs.rs for that version's API. This document was written against kira 0.12.6.

## Repository layout

```
Cargo.toml              workspace: crates/tqs, crates/engine, src-tauri
crates/tqs/             .tqs read/write/legacy import. Pure Rust, no Tauri, no kira.
crates/engine/          the cue engine. kira, no Tauri. Testable with kira's MockBackend.
src-tauri/              the app: Tauri commands, library, settings, web client, wiring.
src/                    Svelte UI.
docs/format/            shared .tqs spec, schema, fixtures, reference tool. DO NOT EDIT.
docs/ARCHITECTURE.md    this file
docs/AUDIT.md           audit of the web version
reference/web/          read-only copies of the web version's show page, CSS and DESIGN.md, for porting
.github/workflows/      CI
```

`crates/engine` and `crates/tqs` must not depend on Tauri. That keeps them testable with plain `cargo test` and is what lets CI test audio without a sound card.

## Threads and data flow

```
 Svelte UI (webview)
   │  invoke("go") / invoke("update_cue", …)            ▲  events: show://changed,
   ▼                                                   │  engine://state, engine://positions,
 src-tauri commands  ──────────────────────────────────┘  import://progress
   │  owns OpenShow (Mutex): manifest, paths, dirty flag
   │  validates edits → autosave thread → engine
   ▼
 Engine thread  (owns kira AudioManager; it is !Send on some platforms, so it never leaves this thread)
   │  crossbeam channel in: Command
   │  loop: select!(command, tick every 10 ms)
   │  out: Event via a callback the app turns into Tauri events
   ▼
 Prep pool (2 threads): decode files, render edited audio, compute waveform peaks
```

- **Commands are intents** (`Go`, `StopAll`, `SetStandby(i)`...). The engine decides what they mean using its own copy of the cue list. The UI never decides which cue plays next.
- **Auto-continue runs in the engine.** When a cue ends by itself and has `continue: after_end`, the engine fires the next one on its own thread. No round trip to the UI.
- **Events:** `engine://state` (full snapshot: standby, running cues, paused, engine status, per-cue prep state) whenever something changes; `engine://positions` (elapsed and remaining for running cues) at 30 Hz while anything is running, nothing when idle.
- `get_engine_state` returns the snapshot on demand, so a freshly loaded UI can draw itself without waiting for an event.

GO latency path: keydown → `invoke("go")` → channel send → engine wakes (a blocked `select!` wakes immediately on a command; the 10 ms tick is only for bookkeeping) → `manager.play`. Expect a few milliseconds plus the device buffer. Log the time from command received to `play` returning at debug level, so a slow GO is visible.

## Engine design

### What the engine knows

`ShowPlan`: the ordered cues with everything needed to play them (type, resolved file path, volume, fades, pre-wait, duration, loop, continue, trim, cuts). The app builds it from the manifest and sends `LoadShow(plan)`; edits send `UpdateCue`, `ReorderCues`, `AddCue`, `RemoveCue`. Updating a cue that's playing changes its live volume (as the web does) and nothing else until its next GO.

### Preparing audio

Each audio cue has a prep state: `Pending → Decoding → Ready | Missing | Failed(reason)`. The UI shows it per row, and the transport shows "Loading 12 of 40" until everything is ready.

- Decode with kira (`StaticSoundData::from_file`) on the prep pool, standby cue first, then outward in show order.
- **Edited audio:** trims only → use `StaticSoundData::slice` (no copy). Cuts → render the kept segments into a new frame buffer with the 4 ms join ramps from FORMAT.md 2.5 (the web does this in `reference/web/app.js` L180–202). Cache by (asset, trim, cuts) so cues sharing an asset and edits share buffers.
- **Never block GO on prep.** GO on a cue that isn't ready yet: if it's still decoding, start it the moment it's ready **unless Stop all is pressed first** (the web bug W1). Missing or failed: report, mark the row, standby still advances (same as the web).
- Memory: decoded audio is ~21 MB per stereo minute. For v1, decode everything and show the total in the status bar. A budget with streaming fallback (`StreamingSoundData` for cues far from standby) is a later milestone; leave a seam for it.

### Playing a cue

One kira **sub-track per running cue**, taken from a pool (32 is plenty), each with a small custom effect, `CueGain`:

- Why not kira's own volume tweens: they interpolate in decibels (kira 0.12 `src/decibels.rs` L45–49), so a "linear" kira fade is linear in dB. FORMAT.md 2.5 says linear in **amplitude**, which is what the web does and what the operator has been hearing. A gain effect we control makes the fade shape exact and testable.
- `CueGain` holds `volume`, the scheduled fade-in/fade-out envelope in samples, and accepts commands (set volume now, fade to 0 over N seconds then report done). Per-sample linear interpolation. No allocation in `process`.
- **Allowed shortcut for M3 only:** use kira tweens to get sound out, with a `TODO(M4): CueGain` comment. M4 replaces them.

Pre-wait, loop and timing:

- Pre-wait: start the sound with a delay on the engine's clock (kira `StartTime::Delayed` or a `ClockTime`), so it's sample-accurate and pauses with everything else. Verify with a mock-backend test that Pause freezes a pre-wait; if `Delayed` doesn't, use a kira clock that is paused alongside the tracks.
- Loop: kira `loop_region` over the whole (edited) buffer.
- Ending: a non-looping cue ends when its sound finishes. Fade-out cues and stop fades end when `CueGain` reports silence and the sound is stopped.
- Silence and fade-out cues have no sound; they're timers on the engine clock (the web uses a silent buffer for the same reason: so they're timed by the audio clock and pause with it).

Transport, matching FORMAT.md 2.5 and the web's behaviour:

| Action | Behaviour |
|---|---|
| GO | Fire standby, move standby to the next cue. At the end of the list, do nothing and say so. |
| GO lockout | Optional setting (default 0 ms = off): ignore a GO within N ms of the last. |
| Stop all | Stop every running cue, cancel every pending start (pre-wait **and** still-decoding). Optional fade (setting, default 0 = instant, as the web). Does not trigger `continue`. Leaves a running audition alone: see "Stop all and the audition" below. |
| Pause / Resume | Freeze everything: sounds, fades, pre-waits, silence/fade-out timers. Pause the parent track(s) and the clock. |
| Restart audio engine | Drop the show's AudioManager and build a new one (on the chosen device). Everything on the show output stops; the audition output is separate and isn't touched. Commands during the restart are refused with a clear message. This replaces the web's "Hard stop" and is what you press after an interface is unplugged. |
| Panic (double Esc within 500 ms) | Stop all with no fade, even if a stop fade is set. |

### Output devices

Two outputs, each its own kira `AudioManager` (thorncue-spec#9): the **show output**, which GO plays on, and the **audition output** (headphones), which the trim editor's Audition from marker plays on. The outputs bar between the header and the transport chooses both, plus an audition volume that is separate from the master. Choices are saved per machine in `settings.json`, never in the show, and a saved device is matched by its id first, then by its name (`engine::outputs`).

**The rule: an audition never plays through the show output unless the operator chose that on purpose.** No silent fallback, ever. Auditions are *blocked*, never rerouted, when:

1. no audition output has been chosen (the default on a fresh install);
2. the chosen device isn't connected. A running audition stops at once when its device goes, whether its stream reports it or the device watcher notices;
3. it's the same physical device as the show output, including "System default" resolving to it. If we can't tell, it counts as the same. On Linux, ALSA's `default`/`pulse`/`pipewire` go through the system mixer to a card we can't name, so they count as the same as any card; ALSA aliases of one card (`hw:`, `plughw:`, `front:`, `CARD=`) are one device;
4. its stream won't open, or fails.

A blocked Audition button still answers a click: it shows the reason and unfolds the bar, and does nothing else. It is dimmed and marked ⊘, with the reason as its tooltip. The only way to audition on the show output is **Same as show output (the audience hears it)**. That choice, or picking the show's own device, asks first in a danger-styled confirm, and the bar keeps a standing warning while it's set. When the show output is changed onto the audition device, a running audition stops and auditions are blocked until the operator picks another device.

How it's built:

- **Show output:** kira's `CpalBackend` on the chosen device (`engine::cpal_factory`). If that device disappears, kira moves the stream to the system default, so the show keeps playing (never block GO). The engine reports Device lost and the bar warns. A saved show device that isn't connected at start also opens on the system default, with a warning.
- **Audition output:** our own `AuditionBackend` (`backend.rs`), not `CpalBackend`. kira's backend reopens a lost stream on the system default even when it was given a device, which for an audition would be exactly the silent reroute the rule forbids. Ours plays silence once its device fails, and the engine then stops the audition and blocks auditions. The stream lives on its own thread, and the manager is opened off the engine thread (`engine::audition_opener`), so nothing on the GO path waits on it. Until it has opened, the audition state is Opening.
- **Fades:** auditions go through `CueGain` with an `offset`, so fades and volume sound exactly as they do in playback from that point. The web's auditions ignored fades. Before the in point or after the out point, the file itself plays, at the cue's volume.
- **Device watcher:** a thread that lists devices with `engine::list_devices` and sends `Command::Devices` when the list changes. It never lists on the engine thread, because ALSA probing can be slow. It polls every 2 s on Windows and Linux. On macOS it lists only on request (the bar's lists are opened, via `refresh_devices`), because querying devices while audio plays can glitch there (kira issue #38). A lost audition device is still caught by its stream error. Every change re-checks rules 1 to 4.
- **Changing the show output during a show** is allowed, and running cues are **moved**: each carries on from where it was on the new device, after a short gap, with its fades in place. Pre-waits keep their remaining time, silence and fade-out timers keep theirs, and Pause stays as it was. Cues already stopping with a fade are let go. A chosen show device that comes back after a loss isn't reopened by itself: press Restart audio engine, or choose it again.
- Still to do (rest of M8): buffer size (Auto / 256 / 512 / 1024 frames through `CpalBackendSettings.config`), and device-lost recovery beyond the warning and Restart.

### Stop all and the audition

**Stop all, Esc, double-Esc panic and Restart audio engine leave a running audition alone.** This is deliberate. It is the one exception to "Stop all always wins" (CLAUDE.md rule 3), which is about the show output. The audition is on a different device, and the operator may be previewing the next cue while stopping the show. GO doesn't end it either. It has its own Stop (Stop audition in the trim editor). Loading a different show, or deleting the cue, does end it. Don't "fix" this: `stop_all_panic_go_and_restart_leave_the_audition_alone` in the engine tests holds it in place.

### Testing the engine

kira has `backend::mock::MockBackend`. Make the engine constructible with any backend (generic or a constructor that takes a backend settings enum), and drive the mock renderer manually in tests to advance time. Test against `docs/format/fixtures/audio/ladder-dc.wav` (every sample in second *n* = *n* × 1000), which makes trims, cuts, ramps, fades and pre-waits checkable to the sample. At minimum:

- edited length and sample values for fixture 02's q01 and q10;
- fade-in/out are linear in amplitude and land at the right samples;
- pre-wait delays start by the right number of samples; Stop all during pre-wait cancels it; Pause freezes it;
- `continue: after_end` fires the next cue; stopped cues don't continue; looping cues never do;
- silence stops all then waits; fade-out fades all over `duration`;
- GO on a cue still decoding, then Stop all: it never starts (W1);
- restart engine refuses commands until done;
- the audition output: rules 1 to 4, Stop all and Restart leaving it alone, and fades matching playback sample for sample (a second `TestBackend` stands in for the headphones).

CI has no sound card: every engine test uses the mock backend.

## Files and the library

Locations from Tauri's path API:

```
<app_data_dir>/library/<show-uuid>/manifest.json       the show (a .tqs manifest, format_version 1)
<app_data_dir>/library/<show-uuid>/manifest.json.bak   last known-good copy
<app_data_dir>/library/<show-uuid>/audio/<asset>.<ext>
<app_data_dir>/library/<show-uuid>/local.json          client-only state (see below)
<app_data_dir>/tmp/                                    imports and downloads in progress
<app_data_dir>/session.json                            the open show, while one is open (crash recovery)
<app_config_dir>/settings.json
```

**A library show is an unpacked `.tqs`.** Same manifest schema, same `audio/` layout. Export is "zip this folder" (manifest first, audio STORE); import is "unpack and validate into a new folder". One schema, not two.

`local.json` holds what isn't part of the show: `{ "last_opened", "standby_index", "imported_at", "imported_revision", "modified_since_import" }`.

Writing:

- Every edit marks the show dirty; an autosave thread writes 300 ms after the last edit, and immediately on close, on show switch, and before export.
- Write = serialise → `manifest.json.tmp` → fsync → rename over `manifest.json` (on Windows use a replace that works over an existing file; the `tempfile` crate's `persist` does). Before the first write in a session, copy the current good file to `.bak`.
- On open: if `manifest.json` doesn't parse, open `.bak` and tell the user which version was recovered.
- Audio added to a show is **copied** into its `audio/` folder with a new asset id (`uuid` simple, lowercased). Shows never reference files outside their folder, so a show folder is always self-contained and exportable.
- Deleting a show moves its folder to the OS trash.

Show readiness (M8, `src-tauri/src/session.rs`):

- **Keep-awake:** while a show is open, the display and the machine are kept awake (`keepawake`: SetThreadExecutionState, an IOKit assertion, or a systemd/D-Bus inhibitor). It's released when the show closes or the app quits. If the OS refuses, the operator is told once per run to turn sleep off by hand. It never blocks opening the show.
- **Crash recovery:** `session.json` names the open show. It's written atomically when a show opens and removed when it closes and on a clean exit. If it's there at the next start, the last run ended uncleanly, and that show opens again at the standby its `local.json` holds (kept current by the standby saver), with a banner saying so. Nothing plays until GO. The marker is read and cleared at start, so a crash during startup can't loop.

## Importing from ThornCue Web

The web API is specified in the web repo's prompt and summarised here:

| Call | Use |
|---|---|
| `GET /api/v1` | Check the URL is a ThornCue server (`app == "thorncue-web"`, `formats["thorncue.show"]` contains 1). |
| `GET /api/v1/me` (Bearer) | Check the token; show "Connected as …". |
| `GET /api/v1/shows` (Bearer) | List: `code, name, author, owner, role, cue_count, audio_bytes, revision, updated_at`. |
| `GET /api/v1/shows/<code>/package` (Bearer) | The `.tqs`. Supports `If-None-Match: "<revision>"` → 304. |
| `GET /api/v1/links/<token>` and `/package` (no auth) | A show from a share link (`https://…/join/<token>`). |

Errors are `{"error": {"code", "message"}}`; show `message` to the user.

Flow:

1. **Settings → ThornCue Web:** server URL and an API token (created on the web's Settings page). Test with `/api/v1` and `/api/v1/me`, then save the URL in settings and the token in the keychain (`keyring`, service `thorncue`, user = server URL). If the keychain isn't available (some Linux setups), store it in `settings.json` with 0600 permissions and say so. Plain `http://` URLs are allowed (LAN installs) with a warning that the token is sent unencrypted.
2. **Web shows screen:** the list, with each row's local status, by matching `source.server` + `source.code` against library shows: *Not imported* / *Imported* / *Update available* (revision differs) / *Changed here and on the web* (revision differs and `modified_since_import`).
3. **Import:** stream the package to `<app_data_dir>/tmp/` with progress events (bytes / Content-Length), then `tqs::read_package` into a new library folder. For an already-imported show ask: **Replace** (keeps the local show id and standby, swaps the content; warn if `modified_since_import`) or **Keep both**.
4. **Import from a link:** paste a join URL; derive the server from it; `GET /api/v1/links/<token>` to show what it is; confirm; download.

Never show a token after it's saved, never log it, and never put it in a URL.

## UI

Port the web's look and behaviour; don't redesign. `reference/web/` has the web's `DESIGN.md`, `style.css`, `show.html` and `app.js`. `DESIGN.md` is the contract: copy it to the repo root as `DESIGN.md`, keep its rules, and update only the parts that are web-specific (accounts, share links). Copy the eight colour-token blocks and the layout tokens verbatim into `src/styles/tokens.css`.

Screens:

- **Library** (the home screen): a table of local shows (name, cues, size, source, last opened) with Open, New show, Import file…, Import from the web…, Duplicate, Export .tqs, Delete. Drag-and-drop a `.tqs` or legacy `.zip` onto the window to import.
- **Show:** the web's show page: header, transport (GO, Stop all, Pause, Back to top, standby name, status), now-playing strip, toolbar, cue table, inspector. Plus: an engine status indicator in the transport (Ready / Loading n of m / Device lost / Restarting), and the output device name.
- **Settings:** theme (family + mode, as the web), audio (output device, buffer size, GO lockout, Stop-all fade), ThornCue Web connection.
- **Web shows:** as above.

Keyboard: the web's shortcuts and its "Space is GO except in text fields" rule (`reference/web/app.js` L1842–1895), plus double-Esc panic. Shortcuts are handled in the webview and sent as commands; they must work regardless of which button has focus.

The show screen must never steal focus from the transport when something updates, and theme changes must not touch playback (the web gets this right; keep it).

## CI and builds

- On every push: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (workspace), `npm run check` (svelte-check), `npm run build`. Linux runner needs `libwebkit2gtk-4.1-dev librsvg2-dev libasound2-dev` (ALSA headers for cpal).
- On tags `v*`: `tauri-apps/tauri-action` builds unsigned installers for Windows (MSI/NSIS), macOS (universal DMG) and Linux (AppImage + .deb) and attaches them to a draft release.
- Unsigned builds trigger Gatekeeper and SmartScreen warnings. Signing is a later decision; the README explains how to open an unsigned build.
- App identifier `xyz.thorngate.thorncue`, product name `ThornCue`. Register `.tqs` as a file association in the bundle config (opening one imports it).
