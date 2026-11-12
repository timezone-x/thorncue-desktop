# Progress

One line per milestone: date, milestone, what works, what's knowingly left.

- 2026-11-12, M0: scaffold (Tauri 2.12 + plain Svelte 5/Vite, SvelteKit removed), workspace with `crates/tqs`, `crates/engine`, `src-tauri`; tokens copied verbatim; theme (family + Dark/Light/System) stored in settings.json by Rust and injected before first paint, System follows the OS live; empty themed Library; CI and tagged-release workflows; README. Checked here: window opens themed under Xvfb (Linux). Left: seeing it on macOS/Windows by hand.
