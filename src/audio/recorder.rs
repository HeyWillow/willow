//! Inactive I2S capture and ESP-SR worker for the Rust audio cut-over.

#![allow(
    dead_code,
    reason = "the recorder worker remains inactive until Rust owns runtime audio"
)]

use core::{fmt, mem::size_of};
use std::{
    collections::TryReserveError,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvError, SyncSender, TryRecvError, sync_channel},
    },
    thread::{self, JoinHandle},
};

use esp_idf_sys::ESP_ERR_TIMEOUT;
use log::{error, info, warn};

use crate::sr::{FrameSpec, SpeechError, SpeechFrontend, VadState, WakeState};

use super::{
    capture::{self, CaptureFramingError},
    i2s::{I2sError, ReceiveChannel},
    record_buffer::{RecordBuffer, RecordBufferError},
    recorder_credit,
};

const CAPTURE_EVENT_CAPACITY: usize = 8;
const CAPTURE_STACK_SIZE: usize = 16 * 1024;
const I2S_READ_TIMEOUT_MS: u32 = 100;
const LOG_TARGET: &str = "WILLOW/AUDIO";
const MAX_CONSECUTIVE_TIMEOUTS: usize = 10;

/// Low-rate detection changes emitted by the continuous capture worker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum CaptureEvent {
    VadChanged { state: VadState },
    WakeDetected { volume_db: f32 },
}

#[derive(Debug)]
pub(super) enum CaptureStartError {
    Spawn { source: io::Error },
    StartupReport { source: RecvError },
    WorkerInitialization { detail: String },
    WorkerPanicked,
}

impl fmt::Display for CaptureStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { source } => {
                write!(formatter, "failed to start recorder capture: {source}")
            }
            Self::StartupReport { source } => {
                write!(
                    formatter,
                    "recorder capture did not report startup: {source}"
                )
            }
            Self::WorkerInitialization { detail } => {
                write!(
                    formatter,
                    "recorder capture initialization failed: {detail}"
                )
            }
            Self::WorkerPanicked => formatter.write_str("recorder capture worker panicked"),
        }
    }
}

impl std::error::Error for CaptureStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source } => Some(source),
            Self::StartupReport { source } => Some(source),
            Self::WorkerInitialization { .. } | Self::WorkerPanicked => None,
        }
    }
}

#[derive(Debug)]
pub(super) enum CaptureError {
    Allocate {
        buffer: &'static str,
        elements: usize,
        element_bytes: usize,
        source: TryReserveError,
    },
    Buffer {
        source: RecordBufferError,
    },
    EventReceiverStopped,
    Framing {
        source: CaptureFramingError,
    },
    I2s {
        source: I2sError,
    },
    InputSizeOverflow {
        frames: usize,
        bytes_per_frame: usize,
    },
    OutputCreditOverflow {
        current: usize,
        added: usize,
    },
    Speech {
        source: SpeechError,
    },
    Stalled {
        consecutive_timeouts: usize,
        timeout_ms: u32,
    },
    UnexpectedFrontend {
        frame: FrameSpec,
    },
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocate {
                buffer,
                elements,
                element_bytes,
                source,
            } => write!(
                formatter,
                "failed to allocate recorder {buffer} with {elements} elements of {element_bytes} bytes: {source}"
            ),
            Self::Buffer { source } => write!(formatter, "recorder PCM buffering failed: {source}"),
            Self::EventReceiverStopped => {
                formatter.write_str("recorder capture event receiver stopped")
            }
            Self::Framing { source } => {
                write!(formatter, "recorder capture framing failed: {source}")
            }
            Self::I2s { source } => write!(formatter, "recorder I2S capture failed: {source}"),
            Self::InputSizeOverflow {
                frames,
                bytes_per_frame,
            } => write!(
                formatter,
                "recorder input size overflows for {frames} frames of {bytes_per_frame} bytes"
            ),
            Self::OutputCreditOverflow { current, added } => write!(
                formatter,
                "ESP-SR output credit {current} overflows after adding {added} samples"
            ),
            Self::Speech { source } => {
                write!(formatter, "recorder ESP-SR processing failed: {source}")
            }
            Self::Stalled {
                consecutive_timeouts,
                timeout_ms,
            } => write!(
                formatter,
                "recorder I2S capture stalled for {consecutive_timeouts} consecutive {timeout_ms} ms waits"
            ),
            Self::UnexpectedFrontend { frame } => write!(
                formatter,
                "ESP-SR frontend dimensions do not match Willow capture: {frame:?}"
            ),
        }
    }
}

impl std::error::Error for CaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocate { source, .. } => Some(source),
            Self::Buffer { source } => Some(source),
            Self::Framing { source } => Some(source),
            Self::I2s { source } => Some(source),
            Self::Speech { source } => Some(source),
            Self::EventReceiverStopped
            | Self::InputSizeOverflow { .. }
            | Self::OutputCreditOverflow { .. }
            | Self::Stalled { .. }
            | Self::UnexpectedFrontend { .. } => None,
        }
    }
}

/// Owns the continuous capture task and its bounded event receiver.
pub(super) struct CaptureWorker {
    shutdown: Arc<AtomicBool>,
    events: Option<Receiver<CaptureEvent>>,
    worker: Option<JoinHandle<()>>,
}

impl CaptureWorker {
    pub(super) fn start(
        receive: ReceiveChannel,
        record_buffer: RecordBuffer,
    ) -> Result<Self, CaptureStartError> {
        let (event_sender, event_receiver) = sync_channel(CAPTURE_EVENT_CAPACITY);
        let (startup_sender, startup_receiver) = sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("audio_capture".into())
            .stack_size(CAPTURE_STACK_SIZE)
            .spawn(move || {
                run_worker(
                    receive,
                    record_buffer,
                    &event_sender,
                    &startup_sender,
                    &worker_shutdown,
                );
            })
            .map_err(|source| CaptureStartError::Spawn { source })?;

        match startup_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                shutdown,
                events: Some(event_receiver),
                worker: Some(worker),
            }),
            Ok(Err(detail)) => {
                join_failed_start(worker)?;
                Err(CaptureStartError::WorkerInitialization { detail })
            }
            Err(source) => {
                join_failed_start(worker)?;
                Err(CaptureStartError::StartupReport { source })
            }
        }
    }

    pub(super) fn try_event(&self) -> Result<Option<CaptureEvent>, CaptureError> {
        let Some(events) = self.events.as_ref() else {
            return Err(CaptureError::EventReceiverStopped);
        };
        match events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(CaptureError::EventReceiverStopped),
        }
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.events.take();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            error!(target: LOG_TARGET, "recorder capture worker panicked during shutdown");
        }
    }
}

fn join_failed_start(worker: JoinHandle<()>) -> Result<(), CaptureStartError> {
    worker.join().map_err(|_| CaptureStartError::WorkerPanicked)
}

fn run_worker(
    receive: ReceiveChannel,
    record_buffer: RecordBuffer,
    events: &SyncSender<CaptureEvent>,
    startup: &SyncSender<Result<(), String>>,
    shutdown: &AtomicBool,
) {
    let mut runtime = match CaptureRuntime::new(receive, record_buffer) {
        Ok(runtime) => runtime,
        Err(source) => {
            error!(target: LOG_TARGET, "failed to initialize recorder capture: {source:#?}");
            let _ = startup.send(Err(format!("{source:#?}")));
            return;
        }
    };
    if startup.send(Ok(())).is_err() {
        return;
    }

    while !shutdown.load(Ordering::Acquire) {
        if let Err(source) = runtime.capture_cycle(events, shutdown) {
            if !shutdown.load(Ordering::Acquire) {
                error!(target: LOG_TARGET, "recorder capture stopped: {source:#?}");
            }
            return;
        }
    }
}

struct CaptureRuntime {
    // RX must stop before the AFE/model owners are destroyed.
    receive: ReceiveChannel,
    frontend: SpeechFrontend,
    record_buffer: RecordBuffer,
    frame: FrameSpec,
    raw: Vec<u8>,
    afe_input: Vec<i16>,
    output_credit: usize,
    last_vad: Option<VadState>,
    first_feed: bool,
}

impl CaptureRuntime {
    fn new(mut receive: ReceiveChannel, record_buffer: RecordBuffer) -> Result<Self, CaptureError> {
        // Open ESP-SR before enabling I2S. Field order then disables and drops
        // RX before AFE/model state during all cleanup paths.
        let frontend = SpeechFrontend::open().map_err(|source| CaptureError::Speech { source })?;
        let frame = frontend.frame_spec();
        validate_frontend(frame)?;
        let raw_bytes = frame
            .feed_samples_per_channel
            .checked_mul(capture::RAW_FRAME_BYTES)
            .ok_or(CaptureError::InputSizeOverflow {
                frames: frame.feed_samples_per_channel,
                bytes_per_frame: capture::RAW_FRAME_BYTES,
            })?;
        let afe_samples = frame
            .feed_samples_per_channel
            .checked_mul(frame.input_channels)
            .ok_or(CaptureError::InputSizeOverflow {
                frames: frame.feed_samples_per_channel,
                bytes_per_frame: frame.input_channels * size_of::<i16>(),
            })?;
        let raw = allocate_zeroed("raw I2S input", raw_bytes, 0_u8)?;
        let afe_input = allocate_zeroed("AFE input", afe_samples, 0_i16)?;
        receive
            .enable()
            .map_err(|source| CaptureError::I2s { source })?;

        info!(
            target: LOG_TARGET,
            "recorder capture ready: model={} frame={frame:?} DMA_frames={}",
            frontend.model_index(),
            super::i2s::DuplexChannels::dma_buffered_frames()
        );
        Ok(Self {
            receive,
            frontend,
            record_buffer,
            frame,
            raw,
            afe_input,
            output_credit: 0,
            last_vad: None,
            first_feed: true,
        })
    }

    fn capture_cycle(
        &mut self,
        events: &SyncSender<CaptureEvent>,
        shutdown: &AtomicBool,
    ) -> Result<(), CaptureError> {
        if !self.read_exact(shutdown)? {
            return Ok(());
        }
        let frames = capture::extract_legacy_afe_inputs(&self.raw, &mut self.afe_input)
            .map_err(|source| CaptureError::Framing { source })?;
        if frames != self.frame.feed_samples_per_channel {
            return Err(CaptureError::UnexpectedFrontend { frame: self.frame });
        }

        let status = self
            .frontend
            .feed(&self.afe_input)
            .map_err(|source| CaptureError::Speech { source })?;
        if self.first_feed {
            info!(
                target: LOG_TARGET,
                "first ESP-SR feed accepted with runtime return {}",
                status.runtime_return
            );
            self.first_feed = false;
        }
        self.output_credit = self
            .output_credit
            .checked_add(self.frame.feed_samples_per_channel)
            .ok_or(CaptureError::OutputCreditOverflow {
                current: self.output_credit,
                added: self.frame.feed_samples_per_channel,
            })?;

        // AEC/BSS emits smaller internal blocks than a feed frame. Keep one
        // feed frame of input credit so sequential fetch never waits for input
        // that only this task can provide.
        while recorder_credit::fetch_ready(
            self.output_credit,
            self.frame.feed_samples_per_channel,
            self.frame.fetch_samples,
        ) {
            self.fetch_one(events)?;
            self.output_credit -= self.frame.fetch_samples;
        }
        Ok(())
    }

    fn fetch_one(&mut self, events: &SyncSender<CaptureEvent>) -> Result<(), CaptureError> {
        let (vad_state, wake_state, volume_db) = {
            let frame = self
                .frontend
                .fetch()
                .map_err(|source| CaptureError::Speech { source })?;
            self.record_buffer
                .write(frame.samples)
                .map_err(|source| CaptureError::Buffer { source })?;
            (frame.vad_state, frame.wake_state, frame.data_volume_db)
        };

        match wake_state {
            WakeState::Detected(_) => {
                send_event(events, CaptureEvent::WakeDetected { volume_db })?;
                // ADF reports only the wake result for this fetch. Force the
                // next ordinary fetch to publish its current VAD state.
                self.last_vad = None;
            }
            WakeState::None if self.last_vad != Some(vad_state) => {
                send_event(events, CaptureEvent::VadChanged { state: vad_state })?;
                self.last_vad = Some(vad_state);
            }
            WakeState::ChannelVerified { .. } | WakeState::None => {}
        }
        Ok(())
    }

    fn read_exact(&mut self, shutdown: &AtomicBool) -> Result<bool, CaptureError> {
        let mut filled = 0;
        let mut consecutive_timeouts = 0;
        while filled < self.raw.len() {
            if shutdown.load(Ordering::Acquire) {
                return Ok(false);
            }
            let requested = self.raw.len() - filled;
            match self
                .receive
                .read(&mut self.raw[filled..], I2S_READ_TIMEOUT_MS)
            {
                Ok(0) => consecutive_timeouts += 1,
                Ok(read) => {
                    if read < requested {
                        warn!(
                            target: LOG_TARGET,
                            "short recorder I2S read: requested {requested} bytes, received {read}"
                        );
                    }
                    filled += read;
                    consecutive_timeouts = 0;
                }
                Err(source) if i2s_timeout(&source) => consecutive_timeouts += 1,
                Err(source) => return Err(CaptureError::I2s { source }),
            }
            if consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
                return Err(CaptureError::Stalled {
                    consecutive_timeouts,
                    timeout_ms: I2S_READ_TIMEOUT_MS,
                });
            }
        }
        Ok(true)
    }
}

fn validate_frontend(frame: FrameSpec) -> Result<(), CaptureError> {
    let valid = frame.sample_rate == capture::SAMPLE_RATE_HZ
        && frame.input_channels == capture::LEGACY_AFE_CHANNELS
        && frame.microphone_channels == capture::MICROPHONE_CHANNELS
        && frame.reference_channels == 1
        && frame.feed_samples_per_channel > 0
        && frame.fetch_samples > 0;
    if valid {
        Ok(())
    } else {
        Err(CaptureError::UnexpectedFrontend { frame })
    }
}

fn send_event(events: &SyncSender<CaptureEvent>, event: CaptureEvent) -> Result<(), CaptureError> {
    events
        .send(event)
        .map_err(|_| CaptureError::EventReceiverStopped)
}

fn i2s_timeout(error: &I2sError) -> bool {
    matches!(
        error,
        I2sError::Hal { source, .. } if source.code() == ESP_ERR_TIMEOUT
    )
}

fn allocate_zeroed<T: Copy>(
    buffer: &'static str,
    elements: usize,
    zero: T,
) -> Result<Vec<T>, CaptureError> {
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(elements)
        .map_err(|source| CaptureError::Allocate {
            buffer,
            elements,
            element_bytes: size_of::<T>(),
            source,
        })?;
    samples.resize(elements, zero);
    Ok(samples)
}
