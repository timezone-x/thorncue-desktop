//! Backends the engine can run on: kira's cpal backend for the show, `AuditionBackend` for
//! the audition output, `TestBackend` for tests.

use std::collections::VecDeque;
use std::time::Duration;

use kira::backend::cpal::CpalBackend;
use kira::backend::{Backend, Renderer};

/// What the engine needs from a backend beyond kira's trait.
pub trait EngineBackend: Backend + 'static {
    /// A device problem the operator must know about (interface unplugged), if one happened.
    fn take_device_error(&mut self) -> Option<String>;

    /// Whether a silent output (the audio clock not moving) means the device is gone.
    /// True for real devices; a test backend only advances when the test renders.
    fn watch_for_stalls(&self) -> bool;
}

/// Stream errors can arrive thousands of times a second from a dead device: log a summary.
static ERRORS_SINCE_LOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_ERROR_LOG: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

fn log_stream_error(e: &dyn std::fmt::Display) {
    use std::sync::atomic::Ordering;
    let n = ERRORS_SINCE_LOG.fetch_add(1, Ordering::Relaxed) + 1;
    let Ok(mut last) = LAST_ERROR_LOG.lock() else {
        return;
    };
    if last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(5)) {
        tracing::warn!("audio stream error ({n} since the last report): {e}");
        ERRORS_SINCE_LOG.store(0, Ordering::Relaxed);
        *last = Some(std::time::Instant::now());
    }
}

impl EngineBackend for CpalBackend {
    fn take_device_error(&mut self) -> Option<String> {
        use kira::backend::cpal::cpal::ErrorKind;
        let mut lost = None;
        while let Some(e) = self.pop_error() {
            match e.kind() {
                // kira restarts the stream on the system default device after these, so the
                // show may now be coming out of the wrong speakers. Say so loudly.
                ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated => {
                    lost = Some(e.to_string())
                }
                // Other errors (EIO from a dead ALSA/Pulse stream, underruns) are only logged;
                // a dead output is caught by the stall watch in the engine.
                _ => log_stream_error(&e),
            }
        }
        lost
    }

    fn watch_for_stalls(&self) -> bool {
        true
    }
}

/// Like kira's `MockBackend`, but hands back what it rendered so tests can check samples.
/// Nothing advances unless a test calls [`TestBackend::render`].
pub struct TestBackend {
    renderer: Option<Renderer>,
    sample_rate: u32,
    watch_for_stalls: bool,
    buffer: Vec<f32>,
    injected_errors: VecDeque<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestBackendSettings {
    pub sample_rate: u32,
    /// Behave like a real device for the stall watch (the clock must keep moving).
    pub watch_for_stalls: bool,
}

impl Default for TestBackendSettings {
    fn default() -> Self {
        Self {
            sample_rate: 8000,
            watch_for_stalls: false,
        }
    }
}

impl Backend for TestBackend {
    type Settings = TestBackendSettings;
    type Error = std::convert::Infallible;

    fn setup(
        settings: Self::Settings,
        _internal_buffer_size: usize,
    ) -> Result<(Self, u32), Self::Error> {
        Ok((
            Self {
                renderer: None,
                sample_rate: settings.sample_rate,
                watch_for_stalls: settings.watch_for_stalls,
                buffer: Vec::new(),
                injected_errors: VecDeque::new(),
            },
            settings.sample_rate,
        ))
    }

    fn start(&mut self, renderer: Renderer) -> Result<(), Self::Error> {
        self.renderer = Some(renderer);
        Ok(())
    }
}

impl TestBackend {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Render `frames` stereo frames (interleaved L, R), as one audio callback would.
    pub fn render(&mut self, frames: usize) -> &[f32] {
        let r = self.renderer.as_mut().expect("backend not started");
        self.buffer.clear();
        self.buffer.resize(frames * 2, 0.0);
        r.on_start_processing();
        r.process(&mut self.buffer, 2);
        &self.buffer
    }

    /// Pretend the device went away.
    pub fn inject_device_error(&mut self, message: &str) {
        self.injected_errors.push_back(message.into());
    }
}

impl EngineBackend for TestBackend {
    fn take_device_error(&mut self) -> Option<String> {
        self.injected_errors.pop_front()
    }

    fn watch_for_stalls(&self) -> bool {
        self.watch_for_stalls
    }
}

// ---------------------------------------------------------------------------
// The audition output
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use kira::backend::cpal::cpal;

const STREAM_OK: u8 = 0;
const STREAM_DEVICE_GONE: u8 = 1;

/// Which device the audition stream opens. `None` = the system default (only ever used when
/// the operator chose "Same as show output" and the show is on the default).
#[derive(Debug, Clone, Default)]
pub struct AuditionBackendSettings {
    pub device_id: Option<String>,
}

/// The audition output: one cpal stream on exactly the chosen device.
///
/// Not kira's `CpalBackend`, because that one reopens a lost stream on the *system default*
/// device (kira `stream_manager.rs` `check_stream`), even when it was given a device. For the
/// show that's right (keep playing); for an audition it would be a silent reroute, possibly
/// to the show's speakers (thorncue-spec#9). Here a lost device means silence, and the engine
/// stops the audition and blocks auditions until the device is back.
///
/// The stream lives on its own thread (cpal streams aren't `Send` everywhere), so this
/// backend, and the `AudioManager` holding it, can be opened off the engine thread.
pub struct AuditionBackend {
    device_id: Option<String>,
    config: cpal::StreamConfig,
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn open_device(device_id: &Option<String>) -> Result<cpal::Device, String> {
    use cpal::traits::HostTrait;
    match device_id {
        Some(id) => {
            crate::outputs::cpal_device(id).ok_or_else(|| "the device isn't connected.".to_string())
        }
        None => cpal::default_host()
            .default_output_device()
            .ok_or_else(|| "there is no default output.".to_string()),
    }
}

impl Backend for AuditionBackend {
    type Settings = AuditionBackendSettings;
    type Error = String;

    fn setup(settings: Self::Settings, _buffer: usize) -> Result<(Self, u32), Self::Error> {
        use cpal::traits::DeviceTrait;
        let device = open_device(&settings.device_id)?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("it has no usable output format ({e})."))?
            .config();
        let rate = config.sample_rate;
        Ok((
            Self {
                device_id: settings.device_id,
                config,
                state: Arc::new(AtomicU8::new(STREAM_OK)),
                stop: Arc::new(AtomicBool::new(false)),
                thread: None,
            },
            rate,
        ))
    }

    fn start(&mut self, mut renderer: Renderer) -> Result<(), Self::Error> {
        use cpal::traits::{DeviceTrait, StreamTrait};
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
        let device_id = self.device_id.clone();
        let config = self.config;
        let state = self.state.clone();
        let stop = self.stop.clone();
        let thread = std::thread::Builder::new()
            .name("thorncue-audition-output".into())
            .spawn(move || {
                let opened = open_device(&device_id).and_then(|device| {
                    let channels = config.channels;
                    let st = state.clone();
                    let st_err = state.clone();
                    let stream = device
                        .build_output_stream(
                            config,
                            move |data: &mut [f32], _| {
                                // Audio thread: no allocation, locks or logging here.
                                if st.load(Ordering::Relaxed) != STREAM_OK {
                                    data.fill(0.0);
                                    return;
                                }
                                renderer.on_start_processing();
                                renderer.process(data, channels);
                            },
                            move |e| {
                                if matches!(
                                    e.kind(),
                                    cpal::ErrorKind::DeviceNotAvailable
                                        | cpal::ErrorKind::StreamInvalidated
                                ) {
                                    st_err.store(STREAM_DEVICE_GONE, Ordering::Relaxed);
                                }
                            },
                            None,
                        )
                        .map_err(|e| format!("its stream wouldn't open ({e})."))?;
                    stream
                        .play()
                        .map_err(|e| format!("its stream wouldn't start ({e})."))?;
                    Ok(stream)
                });
                match opened {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        while !stop.load(Ordering::Acquire) {
                            std::thread::park_timeout(Duration::from_millis(200));
                        }
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| format!("couldn't start its thread ({e})."))?;
        self.thread = Some(thread);
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| Err("it didn't answer in time.".into()))
    }
}

impl Drop for AuditionBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

impl EngineBackend for AuditionBackend {
    fn take_device_error(&mut self) -> Option<String> {
        (self.state.load(Ordering::Relaxed) == STREAM_DEVICE_GONE)
            .then(|| "the device was disconnected.".to_string())
    }

    fn watch_for_stalls(&self) -> bool {
        false
    }
}
