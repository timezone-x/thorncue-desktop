//! The prep pool: decodes audio off the engine thread (docs/ARCHITECTURE.md "Preparing audio").

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};
use kira::sound::static_sound::StaticSoundData;

use crate::edit::{EditKey, apply};

/// Decodes one file. The default uses kira (symphonia); tests swap in a gated one.
pub type Loader = Arc<dyn Fn(&Path) -> Result<StaticSoundData, String> + Send + Sync>;

pub fn kira_loader() -> Loader {
    Arc::new(|path: &Path| StaticSoundData::from_file(path).map_err(|e| e.to_string()))
}

pub(crate) struct Job {
    pub generation: u64,
    pub asset: usize,
    pub path: PathBuf,
    /// Every edit of this file the show's cues play.
    pub edits: Vec<EditKey>,
    /// Already decoded (an edit changed): skip decoding and only render `edits`.
    pub raw: Option<StaticSoundData>,
}

/// A decoded file and each edit of it. An edit maps to `None` when it leaves nothing.
pub(crate) struct Decoded {
    pub raw: StaticSoundData,
    pub edits: Vec<(EditKey, Option<StaticSoundData>)>,
}

pub(crate) enum PrepMsg {
    Started {
        generation: u64,
        asset: usize,
    },
    Finished {
        generation: u64,
        asset: usize,
        result: Box<Result<Decoded, String>>,
    },
}

pub(crate) struct PrepPool {
    jobs: Option<Sender<Job>>,
    pub results: Receiver<PrepMsg>,
    current: Arc<AtomicU64>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl PrepPool {
    pub fn new(threads: usize, loader: Loader) -> Self {
        let (job_tx, job_rx) = unbounded::<Job>();
        let (res_tx, res_rx) = unbounded::<PrepMsg>();
        let current = Arc::new(AtomicU64::new(0));
        let workers = (0..threads)
            .map(|i| {
                let job_rx = job_rx.clone();
                let res_tx = res_tx.clone();
                let loader = loader.clone();
                let current = current.clone();
                std::thread::Builder::new()
                    .name(format!("thorncue-prep-{i}"))
                    .spawn(move || {
                        for job in job_rx {
                            // A newer show was loaded since this was queued: skip it.
                            if job.generation != current.load(Ordering::Acquire) {
                                continue;
                            }
                            let _ = res_tx.send(PrepMsg::Started {
                                generation: job.generation,
                                asset: job.asset,
                            });
                            let t = std::time::Instant::now();
                            let decoded = match job.raw {
                                Some(raw) => Ok(raw),
                                None if job.path.is_file() => loader(&job.path),
                                None => Err(MISSING.into()),
                            };
                            let result = {
                                decoded.map(|raw| {
                                    let edits = job
                                        .edits
                                        .iter()
                                        .map(|k| (k.clone(), apply(&raw, k)))
                                        .collect();
                                    Decoded { raw, edits }
                                })
                            };
                            tracing::debug!("decoded {} in {:?}", job.path.display(), t.elapsed());
                            let _ = res_tx.send(PrepMsg::Finished {
                                generation: job.generation,
                                asset: job.asset,
                                result: Box::new(result),
                            });
                        }
                    })
                    .expect("spawning a prep thread")
            })
            .collect();
        Self {
            jobs: Some(job_tx),
            results: res_rx,
            current,
            workers,
        }
    }

    /// Start a new generation (a new show): queued jobs from the old one are skipped.
    pub fn set_generation(&self, generation: u64) {
        self.current.store(generation, Ordering::Release);
    }

    pub fn submit(&self, job: Job) {
        if let Some(tx) = &self.jobs {
            let _ = tx.send(job);
        }
    }
}

impl Drop for PrepPool {
    fn drop(&mut self) {
        // Close the queue and let the workers finish on their own. Not joined: a worker can
        // be in the middle of decoding a long file, and closing a show or quitting must not
        // wait for it. Its result goes nowhere (the receiver is gone, or the generation is old).
        self.jobs = None;
        self.current.store(u64::MAX, Ordering::Release);
        self.workers.clear();
    }
}

/// The error a worker reports for a file that isn't on disk.
pub(crate) const MISSING: &str = "\u{0}missing";
