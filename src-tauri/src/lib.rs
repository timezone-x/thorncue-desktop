//! ThornCue desktop app: Tauri commands and wiring. Rust owns the show; the webview
//! only sends intents and draws state (docs/ARCHITECTURE.md).

mod fsutil;
mod settings;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

use settings::{FAMILIES, MODES, Settings};

pub struct AppState {
    settings_path: PathBuf,
    settings: Mutex<Settings>,
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.settings.lock().map_err(|_| busy())?.clone())
}

#[tauri::command]
fn set_theme(family: String, mode: String, state: State<'_, AppState>) -> Result<Settings, String> {
    if !FAMILIES.contains(&family.as_str()) || !MODES.contains(&mode.as_str()) {
        return Err("That theme isn't one ThornCue knows.".into());
    }
    let mut s = state.settings.lock().map_err(|_| busy())?;
    s.theme_family = family;
    s.theme_mode = mode;
    s.save(&state.settings_path).map_err(|e| {
        tracing::error!("saving settings: {e:#}");
        "Couldn't save your settings. Check there is free disk space.".to_string()
    })?;
    Ok(s.clone())
}

/// The page calls this once it has painted, so the window never shows unstyled.
#[tauri::command]
fn app_ready(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
    }
}

fn busy() -> String {
    "ThornCue is busy. Try again.".into()
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,thorncue_lib=debug,engine=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let settings_path = settings::settings_path(&config_dir);
            let settings = Settings::load(&settings_path);
            let init = settings::theme_init_script(&settings);
            app.manage(AppState {
                settings_path,
                settings: Mutex::new(settings),
            });

            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("ThornCue")
                .inner_size(1200.0, 800.0)
                .min_inner_size(640.0, 480.0)
                .visible(false)
                .initialization_script(&init)
                .build()?;

            // If the page never reports ready (a broken build), show the window anyway.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                let _ = window.show();
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_settings, set_theme, app_ready])
        .run(tauri::generate_context!())
        .expect("error while running ThornCue");
}
