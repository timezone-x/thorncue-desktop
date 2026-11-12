//! Backends the engine can run on: cpal for real output, `TestBackend` for tests.

use std::collections::VecDeque;

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
