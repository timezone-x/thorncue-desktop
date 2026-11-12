//! ThornCue desktop app: Tauri commands and wiring. Rust owns the show; the webview
//! only sends intents and draws state (docs/ARCHITECTURE.md).

mod commands;
mod fsutil;
mod library;
mod playback;
mod settings;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use library::{Library, LoadedShow};
use settings::Settings;

pub struct AppState {
    pub settings_path: PathBuf,
    pub settings: Mutex<Settings>,
    pub library: Library,
    /// The show on the Show screen, if any.
    pub open_show: Mutex<Option<LoadedShow>>,
    /// Its folder, shared with the thread that saves standby.
    pub open_dir: playback::OpenShowDir,
    pub engine: engine::Engine,
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
            let data_dir = app.path().app_data_dir()?;
            let settings_path = settings::settings_path(&config_dir);
            let settings = Settings::load(&settings_path);
            let init = settings::theme_init_script(&settings);
            let settings = settings.sanitised();
            let library = Library::new(&data_dir)?;
            tracing::info!("library at {}", library.root().display());
            let open_dir = playback::OpenShowDir::default();
            let engine = playback::start_engine(app.handle().clone(), open_dir.clone());
            if let Err(e) = engine.send(engine::Command::SetOptions(settings.engine_options())) {
                tracing::error!("sending options to the engine: {e}");
            }
            app.manage(AppState {
                settings_path,
                settings: Mutex::new(settings),
                library,
                open_show: Mutex::new(None),
                open_dir,
                engine,
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
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::set_theme,
            commands::app_ready,
            commands::list_shows,
            commands::import_file,
            commands::delete_show,
            commands::open_show,
            commands::close_show,
            commands::get_open_show,
            commands::get_engine_state,
            commands::go,
            commands::stop_all,
            commands::toggle_pause,
            commands::set_standby,
            commands::move_standby,
            commands::jump,
            commands::restart_engine,
            commands::set_master,
            commands::set_audio_options,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ThornCue");
}
