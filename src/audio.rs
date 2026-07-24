//! Rust-owned capture, speech recognition, recording, upload, and playback.

mod board;
mod capture;
mod codec_ffi;
mod codecs;
mod es7210;
mod http_audio;
mod http_chunk;
mod http_playback;
mod i2s;
mod ogg_headers;
mod pcm;
mod playback;
mod player;
mod record_buffer;
mod record_upload;
mod recorder;
mod recorder_coordinator;
mod recorder_credit;
mod recorder_state;
mod recorder_timing;
mod response;
mod response_config;
mod spiffs_playback;
mod spiffs_uri;
mod stream_codec;
mod wis_config;
mod wis_encoder;
mod wis_framing;
mod wis_upload;

use core::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use log::{error, info};

use self::{
    codecs::{BoardCodecDevices, MicrophoneDevice},
    i2s::DuplexChannels,
    player::Player,
    record_buffer::RecordBuffer,
    record_upload::UploadWorker,
    recorder::CaptureWorker,
    recorder_coordinator::RecorderCoordinator,
    response::{CommandOutcome, ResponseAudio},
};

const DEFAULT_MIC_GAIN: u8 = 14;
const DEFAULT_RECORD_BUFFER_KIB: usize = 12;
const DEFAULT_SPEAKER_VOLUME: u8 = 60;
const LOG_TARGET: &str = "WILLOW/AUDIO";
/// Live PCM headroom formerly supplied by the ADF raw-stream writer.
const WIS_SESSION_BACKLOG_BYTES: usize = 64 * 1024;

static RUNTIME: Mutex<Option<AudioRuntime>> = Mutex::new(None);

struct AudioRuntime {
    // Drop the coordinator before the player and microphone. The coordinator
    // joins capture and upload and releases its player clone before the final
    // player owner stops TX and releases the playback codec.
    coordinator: RecorderCoordinator,
    player: Arc<Player>,
    microphone: Arc<Mutex<MicrophoneDevice>>,
}

impl AudioRuntime {
    fn shutdown(self) {
        let Self {
            coordinator,
            player,
            microphone,
        } = self;
        drop(coordinator);
        player.shutdown();
        drop(player);
        drop(microphone);
    }
}

#[derive(Debug)]
pub(crate) struct AudioStartError {
    detail: String,
}

impl AudioStartError {
    fn message(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    fn stage(stage: &str, source: &impl fmt::Debug) -> Self {
        Self::message(format!("{stage}: {source:#?}"))
    }
}

impl fmt::Display for AudioStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for AudioStartError {}

#[derive(Debug)]
pub(crate) enum AudioError {
    Command { detail: String },
    NotInitialized,
}

impl AudioError {
    fn command(operation: &str, source: &impl fmt::Debug) -> Self {
        Self::Command {
            detail: format!("{operation}: {source:#?}"),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command { detail } => formatter.write_str(detail),
            Self::NotInitialized => formatter.write_str("audio is not initialized"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Starts all audio hardware and workers under one Rust owner.
pub(crate) fn initialize() -> Result<(), AudioStartError> {
    if lock_runtime().is_some() {
        return Err(AudioStartError::message("audio is already initialized"));
    }
    recorder_coordinator::wait_for_initial_unmute();

    let configuration = crate::config::config();
    let mic_gain = configuration
        .and_then(|config| config.mic_gain)
        .unwrap_or(DEFAULT_MIC_GAIN);
    let speaker_volume = configuration
        .and_then(|config| config.speaker_volume)
        .unwrap_or(DEFAULT_SPEAKER_VOLUME)
        .min(100);
    let record_buffer_kib = configuration
        .and_then(|config| config.record_buffer)
        .map_or(DEFAULT_RECORD_BUFFER_KIB, usize::from);
    let record_buffer_bytes = record_buffer_kib.checked_mul(1024).ok_or_else(|| {
        AudioStartError::message(format!(
            "configured {record_buffer_kib} KiB recorder buffer capacity overflows"
        ))
    })?;

    let BoardCodecDevices {
        mut microphone,
        playback,
    } = BoardCodecDevices::new()
        .map_err(|source| AudioStartError::stage("codec startup failed", &source))?;
    microphone
        .apply_gain(mic_gain)
        .map_err(|source| AudioStartError::stage("microphone gain setup failed", &source))?;
    let microphone = Arc::new(Mutex::new(microphone));
    let DuplexChannels { receive, transmit } = DuplexChannels::new()
        .map_err(|source| AudioStartError::stage("I2S startup failed", &source))?;
    let record_buffer = RecordBuffer::new(record_buffer_bytes, WIS_SESSION_BACKLOG_BYTES)
        .map_err(|source| AudioStartError::stage("recorder buffer startup failed", &source))?;
    let upload = UploadWorker::start(record_buffer.clone())
        .map_err(|source| AudioStartError::stage("WIS upload startup failed", &source))?;
    let capture = CaptureWorker::start(receive, record_buffer)
        .map_err(|source| AudioStartError::stage("capture startup failed", &source))?;
    let player = Arc::new(
        Player::start(transmit, playback, speaker_volume)
            .map_err(|source| AudioStartError::stage("player startup failed", &source))?,
    );

    let reset_microphone = Arc::clone(&microphone);
    let coordinator = RecorderCoordinator::start(capture, upload, Arc::clone(&player), move || {
        let result = lock(&reset_microphone).reinitialize(mic_gain);
        if let Err(source) = result {
            error!(target: LOG_TARGET, "failed to reinitialize microphone after unmute: {source:#?}");
        } else {
            info!(target: LOG_TARGET, "reinitialized microphone after unmute");
        }
    })
    .map_err(|source| AudioStartError::stage("coordinator startup failed", &source))?;

    let mut runtime = lock_runtime();
    if runtime.is_some() {
        return Err(AudioStartError::message("audio is already initialized"));
    }
    *runtime = Some(AudioRuntime {
        coordinator,
        player,
        microphone,
    });
    drop(runtime);

    crate::ui::show_ready(wake_help());
    info!(target: LOG_TARGET, "Rust audio runtime started");
    Ok(())
}

/// Stops and joins every audio worker before releasing the hardware.
pub(crate) fn deinitialize() {
    let runtime = lock_runtime().take();
    if let Some(runtime) = runtime {
        runtime.shutdown();
    }
}

/// Requests recorder cancellation without blocking the caller.
pub(crate) fn stop_recording() -> Result<(), AudioError> {
    lock_runtime()
        .as_ref()
        .ok_or(AudioError::NotInitialized)?
        .coordinator
        .stop()
        .map_err(|source| AudioError::command("failed to stop recorder", &source))
}

/// Applies the Willow One Wake arbitration result.
pub(crate) fn multiwake_result(won: bool) -> Result<(), AudioError> {
    lock_runtime()
        .as_ref()
        .ok_or(AudioError::NotInitialized)?
        .coordinator
        .multiwake_result(won)
        .map_err(|source| AudioError::command("failed to apply multiwake result", &source))
}

/// Queues configured command-result audio.
pub(crate) fn play_response(ok: bool, text: Option<&str>) -> Result<(), AudioError> {
    let outcome = if ok {
        CommandOutcome::Success
    } else {
        CommandOutcome::Error
    };
    match response_config::active_policy().select(outcome, text) {
        Ok(ResponseAudio::None) => Ok(()),
        Ok(ResponseAudio::Play(uri)) => {
            response_config::prepare_playback();
            player()?
                .play(&uri)
                .map_err(|source| AudioError::command("failed to queue response audio", &source))
        }
        Err(source) => {
            error!(target: LOG_TARGET, "failed to select command response audio: {source:#?}");
            Ok(())
        }
    }
}

/// Cooperatively cancels active playback.
pub(crate) fn cancel_playback() -> Result<(), AudioError> {
    player()?.cancel();
    Ok(())
}

/// Plays one URI synchronously on the player worker.
pub(crate) fn play_sync(uri: &str) -> Result<(), AudioError> {
    player()?
        .play_sync(uri)
        .map_err(|source| AudioError::command("synchronous playback failed", &source))
}

/// Applies a temporary or configured-default playback volume.
pub(crate) fn set_volume(volume: Option<i32>) -> Result<(), AudioError> {
    let volume = volume.unwrap_or_else(|| {
        crate::config::config()
            .and_then(|config| config.speaker_volume)
            .map_or(i32::from(DEFAULT_SPEAKER_VOLUME), i32::from)
    });
    let volume = u8::try_from(volume.clamp(0, 100)).unwrap_or(100);
    player()?
        .set_volume(volume)
        .map_err(|source| AudioError::command("volume update failed", &source))
}

fn player() -> Result<Arc<Player>, AudioError> {
    lock_runtime()
        .as_ref()
        .map(|runtime| Arc::clone(&runtime.player))
        .ok_or(AudioError::NotInitialized)
}

fn lock_runtime() -> MutexGuard<'static, Option<AudioRuntime>> {
    match RUNTIME.lock() {
        Ok(runtime) => runtime,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(value) => value,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wake_help() -> &'static str {
    match crate::config::config().and_then(|config| config.wake_word.as_deref()) {
        Some("alexa") => "Say 'Alexa' to start!",
        Some("hilexin") => "Say 'Hi Lexin' to start!",
        Some("hiesp") | None => "Say 'Hi ESP' to start!",
        Some(wake_word) => {
            error!(target: LOG_TARGET, "selected wake word {wake_word:?} is not supported");
            "Ready!"
        }
    }
}
