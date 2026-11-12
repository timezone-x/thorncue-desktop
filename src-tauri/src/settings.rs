//! `settings.json` in the app config folder. Rust is the source of truth; the webview
//! keeps a cached copy of the theme only so a reload paints in the right colours.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsutil::atomic_write;

pub const FAMILIES: [&str; 4] = ["graphite", "ghost-light", "booth", "thorn"];
pub const MODES: [&str; 3] = ["dark", "light", "system"];
pub const MAX_GO_LOCKOUT_MS: u64 = 2000;
pub const MAX_STOP_FADE: f64 = 10.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub theme_family: String,
    pub theme_mode: String,
    /// Ignore a GO within this many milliseconds of the last (0 = off).
    pub go_lockout_ms: u64,
    /// Stop all fades over this many seconds (0 = instant).
    pub stop_fade: f64,
    /// Fields written by a newer ThornCue are kept on save.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme_family: "graphite".into(),
            theme_mode: "dark".into(),
            go_lockout_ms: 0,
            stop_fade: 0.0,
            extra: Default::default(),
        }
    }
}

impl Settings {
    pub fn engine_options(&self) -> engine::Options {
        engine::Options {
            go_lockout_ms: self.go_lockout_ms,
            stop_fade: self.stop_fade,
        }
    }

    /// Never fails: a missing or damaged file gives the defaults (and is logged).
    pub fn load(path: &Path) -> Self {
        let mut s: Settings = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("settings.json is damaged, using defaults: {e}");
                Settings::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => {
                tracing::warn!("couldn't read settings.json, using defaults: {e}");
                Settings::default()
            }
        };
        s.sanitise();
        s
    }

    pub fn sanitised(mut self) -> Self {
        self.sanitise();
        self
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        atomic_write(path, &bytes)
    }

    fn sanitise(&mut self) {
        if !FAMILIES.contains(&self.theme_family.as_str()) {
            self.theme_family = "graphite".into();
        }
        if !MODES.contains(&self.theme_mode.as_str()) {
            self.theme_mode = "dark".into();
        }
        self.go_lockout_ms = self.go_lockout_ms.min(MAX_GO_LOCKOUT_MS);
        if !self.stop_fade.is_finite() {
            self.stop_fade = 0.0;
        }
        self.stop_fade = self.stop_fade.clamp(0.0, MAX_STOP_FADE);
    }
}

/// Runs in the webview before any page script, so the first paint is already themed.
/// A reload re-runs this with the values from window creation, so the page's own cache
/// (`localStorage["thorncue.theme"]`, updated on every change) wins when present.
pub fn theme_init_script(s: &Settings) -> String {
    let initial = serde_json::json!({ "family": s.theme_family, "mode": s.theme_mode });
    format!(
        r#"(function () {{
  var t = {initial};
  try {{
    var c = JSON.parse(localStorage.getItem("thorncue.theme") || "null");
    if (c && typeof c.family === "string" && typeof c.mode === "string") t = c;
  }} catch (e) {{}}
  window.__THORNCUE_THEME__ = t;
  function apply() {{
    var root = document.documentElement;
    if (!root) return false;
    var mode = t.mode === "system"
      ? (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")
      : t.mode;
    root.setAttribute("data-theme", t.family + "-" + mode);
    return true;
  }}
  if (!apply()) document.addEventListener("readystatechange", apply, {{ once: true }});
}})();"#
    )
}

pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_values_fall_back_and_unknown_fields_survive() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(
            &p,
            r#"{"theme_family":"neon","theme_mode":"light","x_future":1}"#,
        )
        .unwrap();
        let s = Settings::load(&p);
        assert_eq!(s.theme_family, "graphite");
        assert_eq!(s.theme_mode, "light");
        s.save(&p).unwrap();
        let again: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(again["x_future"], 1);
    }

    #[test]
    fn damaged_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(Settings::load(&p), Settings::default());
    }
}
