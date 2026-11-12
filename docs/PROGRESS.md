# Progress

One line per milestone: date, milestone, what works, what's knowingly left.

- 2026-11-12, M0: scaffold (Tauri 2.12 + plain Svelte 5/Vite, SvelteKit removed), workspace with `crates/tqs`, `crates/engine`, `src-tauri`; tokens copied verbatim; theme (family + Dark/Light/System) stored in settings.json by Rust and injected before first paint, System follows the OS live; empty themed Library; CI and tagged-release workflows; README. Checked here: window opens themed under Xvfb (Linux). Left: seeing it on macOS/Windows by hand.
- 2026-11-12, M1: `crates/tqs` reads, writes and legacy-imports `.tqs` per FORMAT.md (all of 1.3 for legacy zips too, running byte count, UTF-8 name check, symphonia probe + first-packet decode, unknown fields preserved). Fixture tests from index.json: 5 valid match expected, 25 invalid rejected leaving nothing behind, legacy zip matches (hashes included), fixture 02 round-trips. Left: no test for non-UTF-8 entry names (the zip crate can't write one; the check is in `zipcheck.rs`).
