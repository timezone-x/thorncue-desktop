//! The engine thread and the handle the app holds. The `AudioManager` is created on the
//! engine thread and never leaves it (it isn't `Send` on every platform).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Sender, select, unbounded};

use crate::backend::EngineBackend;
use crate::core::{Command, Core, ManagerFactory};
use crate::prep::Loader;
use crate::state::{EngineStatus, Event, Snapshot};

/// Why a command wasn't accepted. The message is for the operator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("The audio engine is restarting. Try again in a moment.")]
    Restarting,
    #[error("The audio engine has stopped. Restart ThornCue.")]
    Gone,
}

enum Msg {
    Cmd(Command),
    Shutdown,
}

struct Shared {
    restarting: AtomicBool,
    snapshot: Mutex<Snapshot>,
}

/// The app's handle on the engine. Cheap to share behind an `Arc`.
pub struct Engine {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    /// Start the engine thread. `factory` builds the `AudioManager` (on the engine thread);
    /// `on_event` is called on the engine thread and must not block (hand events to another
    /// thread if delivering them can wait).
    pub fn start<B: EngineBackend>(
        factory: impl FnMut() -> Result<kira::AudioManager<B>, String> + Send + 'static,
        loader: Loader,
        mut on_event: impl FnMut(Event) + Send + 'static,
    ) -> Engine {
        let (tx, rx) = unbounded::<Msg>();
        let shared = Arc::new(Shared {
            restarting: AtomicBool::new(false),
            snapshot: Mutex::new(Snapshot::empty()),
        });
        let shared2 = shared.clone();
        let factory: ManagerFactory<B> = Box::new(factory);
        let thread = std::thread::Builder::new()
            .name("thorncue-engine".into())
            .spawn(move || {
                let shared3 = shared2.clone();
                let out = Box::new(move |e: Event| {
                    if let Event::State(s) = &e
                        && let Ok(mut snap) = shared3.snapshot.lock()
                    {
                        *snap = s.clone();
                    }
                    on_event(e);
                });
                let mut core = Core::<B>::new(factory, loader, out);
                let prep_rx = core.prep_results();
                core.tick();
                loop {
                    select! {
                        recv(rx) -> msg => match msg {
                            Ok(Msg::Cmd(cmd)) => {
                                let restart = matches!(cmd, Command::RestartAudio);
                                core.handle(cmd);
                                if restart {
                                    shared2.restarting.store(false, Ordering::Release);
                                }
                            }
                            Ok(Msg::Shutdown) | Err(_) => break,
                        },
                        recv(prep_rx) -> msg => if let Ok(msg) = msg { core.on_prep(msg) },
                        default(Duration::from_millis(10)) => {}
                    }
                    core.tick();
                }
                tracing::info!("engine thread stopped");
            })
            .expect("spawning the engine thread");
        Engine {
            tx,
            shared,
            thread: Some(thread),
        }
    }

    /// Send a command. Playback commands are refused while Restart audio engine is in
    /// progress; Stop all always goes through (it always wins, and has nothing left to stop).
    pub fn send(&self, cmd: Command) -> Result<(), EngineError> {
        // During a restart only playback is refused. Edits, standby moves and settings still
        // go through, so the engine's copy of the show never falls behind the app's.
        let playback = matches!(
            cmd,
            Command::Go
                | Command::JumpAndPlay(_)
                | Command::TogglePause
                | Command::RestartAudio
                | Command::Audition { .. }
        );
        if self.shared.restarting.load(Ordering::Acquire) && playback {
            return Err(EngineError::Restarting);
        }
        if matches!(cmd, Command::RestartAudio) {
            self.shared.restarting.store(true, Ordering::Release);
            if let Ok(mut s) = self.shared.snapshot.lock() {
                s.status = EngineStatus::Restarting;
            }
        }
        self.tx.send(Msg::Cmd(cmd)).map_err(|_| EngineError::Gone)
    }

    pub fn is_restarting(&self) -> bool {
        self.shared.restarting.load(Ordering::Acquire)
    }

    /// The latest state, for a UI that has just (re)loaded.
    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .snapshot
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| Snapshot::empty())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
