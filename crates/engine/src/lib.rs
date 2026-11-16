//! The ThornCue cue engine (docs/ARCHITECTURE.md "Engine design"). kira, no Tauri.
//!
//! The engine owns the cue list, standby and everything that plays. The app sends intents
//! (`Command`) and draws what comes back (`Event`).

mod backend;
mod core;
mod edit;
mod gain;
mod peaks;
mod plan;
mod prep;
mod state;
mod thread;

pub use backend::{EngineBackend, TestBackend, TestBackendSettings};
pub use core::{Command, Core, ManagerFactory, Options};
pub use edit::kept_segments;
pub use kira;
pub use peaks::{PEAK_COLUMNS, Peaks, PeaksReply};
pub use plan::{CueKind, PlanCue, ShowPlan};
pub use prep::{Loader, kira_loader};
pub use state::{
    CuePrep, EngineStatus, Event, Notice, Position, PrepState, RunPhase, RunningCue, Snapshot,
};
pub use thread::{Engine, EngineError};

/// The real output: kira's cpal backend on the system default device.
pub fn cpal_factory()
-> impl FnMut() -> Result<kira::AudioManager<kira::backend::cpal::CpalBackend>, String> + Send + 'static
{
    || {
        kira::AudioManager::<kira::backend::cpal::CpalBackend>::new(
            kira::AudioManagerSettings::default(),
        )
        .map_err(|e| e.to_string())
    }
}
