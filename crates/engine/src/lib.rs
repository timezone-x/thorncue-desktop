//! The ThornCue cue engine (docs/ARCHITECTURE.md "Engine design"). kira, no Tauri.
//!
//! The engine owns the cue list, standby and everything that plays. The app sends intents
//! (`Command`) and draws what comes back (`Event`).

mod backend;
mod core;
mod edit;
mod gain;
mod outputs;
mod peaks;
mod plan;
mod prep;
mod state;
mod thread;

pub use backend::{
    AuditionBackend, AuditionBackendSettings, EngineBackend, TestBackend, TestBackendSettings,
};
pub use core::{AuditionOpener, Command, Core, ManagerFactory, Options, OutputSetup};
pub use edit::kept_segments;
pub use kira;
pub use outputs::{
    AuditionBlock, AuditionOutput, AuditionRoute, DeviceList, OutputDevice, ShowOutput, ShowRoute,
    audition_route, list_devices, show_route,
};
pub use peaks::{PEAK_COLUMNS, Peaks, PeaksReply};
pub use plan::{CueKind, PlanCue, ShowPlan};
pub use prep::{Loader, kira_loader};
pub use state::{
    AuditionState, CuePrep, EngineStatus, Event, Notice, OutputsState, Position, PrepState,
    RunPhase, RunningCue, Snapshot,
};
pub use thread::{Engine, EngineError, EngineOutputs};

/// The show output: kira's cpal backend on the chosen device, or the system default. If the
/// device goes away, kira moves the stream to the system default, so the show keeps playing
/// (thorncue-spec#9 "Show output behaviour"); the engine reports it.
pub fn cpal_factory() -> impl FnMut(
    Option<&OutputDevice>,
) -> Result<
    kira::AudioManager<kira::backend::cpal::CpalBackend>,
    String,
> + Send
+ 'static {
    |device| {
        let device = device.and_then(|d| outputs::cpal_device(&d.id));
        kira::AudioManager::<kira::backend::cpal::CpalBackend>::new(kira::AudioManagerSettings {
            backend_settings: kira::backend::cpal::CpalBackendSettings {
                device,
                config: None,
            },
            ..Default::default()
        })
        .map_err(|e| e.to_string())
    }
}

/// The audition output, opened on its own thread so nothing on the engine thread (and so
/// nothing on the GO path) waits for a device (CLAUDE.md rule 2).
pub fn audition_opener() -> impl FnMut(
    Option<OutputDevice>,
) -> crossbeam_channel::Receiver<
    Result<kira::AudioManager<AuditionBackend>, String>,
> + Send
+ 'static {
    |device| {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("thorncue-audition-open".into())
            .spawn(move || {
                let r = kira::AudioManager::<AuditionBackend>::new(kira::AudioManagerSettings {
                    backend_settings: AuditionBackendSettings {
                        device_id: device.map(|d| d.id),
                    },
                    ..Default::default()
                });
                let _ = tx.send(r);
            });
        if let Err(e) = spawned {
            tracing::error!("couldn't start the audition opener: {e}");
        }
        rx
    }
}
