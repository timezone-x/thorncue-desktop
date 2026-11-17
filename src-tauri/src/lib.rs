//! ThornCue desktop app: Tauri commands and wiring. Rust owns the show; the webview
//! only sends intents and draws state (docs/ARCHITECTURE.md).

mod commands;
mod edit_cmds;
mod editing;
mod fsutil;
mod library;
mod playback;
mod saver;
mod settings;
mod web;
mod web_cmds;
#[cfg(test)]
mod web_tests;

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
    /// Downloads in progress (`<app_data_dir>/tmp`).
    pub tmp_dir: PathBuf,
    pub engine: engine::Engine,
    pub saver: saver::Saver,
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
            let tmp_dir = data_dir.join("tmp");
            // Anything left here is from a download that was interrupted.
            let _ = std::fs::remove_dir_all(&tmp_dir);
            std::fs::create_dir_all(&tmp_dir)?;
            tracing::info!("library at {}", library.root().display());
            let open_dir = playback::OpenShowDir::default();
            let engine = playback::start_engine(app.handle().clone(), open_dir.clone(), &settings);
            if let Err(e) = engine.send(engine::Command::SetOptions(settings.engine_options())) {
                tracing::error!("sending options to the engine: {e}");
            }
            app.manage(AppState {
                settings_path,
                settings: Mutex::new(settings),
                library,
                open_show: Mutex::new(None),
                open_dir,
                tmp_dir,
                engine,
                saver: saver::Saver::start(app.handle().clone()),
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
            commands::set_outputs,
            commands::set_audition_volume,
            commands::refresh_devices,
            web_cmds::web_connection,
            web_cmds::web_connect,
            web_cmds::web_check,
            web_cmds::web_disconnect,
            web_cmds::web_list,
            web_cmds::web_link_info,
            web_cmds::web_import,
            web_cmds::web_import_link,
            edit_cmds::update_cue,
            edit_cmds::move_cue,
            edit_cmds::add_control_cue,
            edit_cmds::duplicate_cue,
            edit_cmds::delete_cue,
            edit_cmds::set_show_name,
            edit_cmds::create_group,
            edit_cmds::edit_group,
            edit_cmds::ungroup,
            edit_cmds::move_cues,
            edit_cmds::set_cue_group,
            edit_cmds::set_group_collapsed,
            edit_cmds::add_audio_cues,
            edit_cmds::replace_cue_file,
            edit_cmds::new_show,
            edit_cmds::duplicate_show,
            edit_cmds::export_file_name,
            edit_cmds::export_show,
            edit_cmds::get_peaks,
            edit_cmds::audition,
            edit_cmds::stop_audition,
            edit_cmds::fade_curve,
        ])
        .build(tauri::generate_context!())
        .expect("error while starting ThornCue")
        .run(|app, event| match event {
            // Never lose an edit: save before the window closes and before the app exits.
            tauri::RunEvent::WindowEvent {
                event: tauri::WindowEvent::CloseRequested { .. },
                ..
            }
            | tauri::RunEvent::ExitRequested { .. }
            | tauri::RunEvent::Exit => {
                if let Err(e) = saver::save_now(app) {
                    tracing::error!("saving on exit: {e}");
                }
            }
            _ => {}
        });
}
