//! Bounded audio playback worker and cooperative cancellation.

#![allow(
    dead_code,
    reason = "the player remains inactive until the atomic Rust audio cut-over"
)]

use core::{fmt, mem::size_of};
use std::{
    collections::TryReserveError,
    io,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvError, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
};

use log::{error, info};

use super::{
    codecs::{CodecError, CodecInterface},
    http_playback::{self, HttpPlaybackError},
    i2s::{I2sError, TransmitChannel},
    lock,
    playback::{PlaybackError, PlaybackWorkspace},
    spiffs_playback::{self, SpiffsPlaybackError},
    stream_codec::{CodecLibrary, StreamCodecError},
};

const COMMAND_CAPACITY: usize = 3;
const DECODED_BYTES: usize = 32 * 1024;
const DECODED_SAMPLES: usize = DECODED_BYTES / size_of::<i16>();
const ENCODED_BYTES: usize = 4 * 1024;
const I2S_SAMPLES: usize = 4 * 1024;
const LOG_TARGET: &str = "WILLOW/AUDIO";
const PLAYER_STACK_SIZE: usize = 20 * 1024;

#[derive(Debug)]
pub(super) enum PlayerStartError {
    Spawn { source: io::Error },
    StartupReport { source: RecvError },
    WorkerPanicked,
    WorkerInitialization { detail: String },
}

impl fmt::Display for PlayerStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { source } => write!(formatter, "failed to start audio player: {source}"),
            Self::StartupReport { source } => {
                write!(formatter, "audio player did not report startup: {source}")
            }
            Self::WorkerPanicked => formatter.write_str("audio player panicked during startup"),
            Self::WorkerInitialization { detail } => {
                write!(formatter, "audio player initialization failed: {detail}")
            }
        }
    }
}

impl std::error::Error for PlayerStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source } => Some(source),
            Self::StartupReport { source } => Some(source),
            Self::WorkerPanicked | Self::WorkerInitialization { .. } => None,
        }
    }
}

#[derive(Debug)]
pub(super) enum PlayerError {
    CommandQueueFull { uri: String },
    WorkerStopped { uri: String },
    CompletionLost { uri: String, source: RecvError },
    Cancelled { uri: String },
    Playback { uri: String, detail: String },
    VolumeCompletionLost { source: RecvError },
    VolumeUpdate { detail: String },
    WorkerUnavailable,
}

impl fmt::Display for PlayerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandQueueFull { uri } => {
                write!(
                    formatter,
                    "audio command queue is full; cannot play {uri:?}"
                )
            }
            Self::WorkerStopped { uri } => {
                write!(formatter, "audio player stopped before accepting {uri:?}")
            }
            Self::CompletionLost { uri, source } => write!(
                formatter,
                "audio player stopped without completing {uri:?}: {source}"
            ),
            Self::Cancelled { uri } => write!(formatter, "playback of {uri:?} was cancelled"),
            Self::Playback { uri, detail } => {
                write!(formatter, "playback of {uri:?} failed: {detail}")
            }
            Self::VolumeCompletionLost { source } => {
                write!(
                    formatter,
                    "audio player stopped while setting volume: {source}"
                )
            }
            Self::VolumeUpdate { detail } => {
                write!(formatter, "failed to set playback volume: {detail}")
            }
            Self::WorkerUnavailable => formatter.write_str("audio player is not available"),
        }
    }
}

impl std::error::Error for PlayerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CompletionLost { source, .. } | Self::VolumeCompletionLost { source } => {
                Some(source)
            }
            Self::CommandQueueFull { .. }
            | Self::WorkerStopped { .. }
            | Self::Cancelled { .. }
            | Self::Playback { .. }
            | Self::VolumeUpdate { .. }
            | Self::WorkerUnavailable => None,
        }
    }
}

/// Owns one playback worker and its bounded command channel.
pub(super) struct Player {
    commands: Mutex<Option<SyncSender<PlayerCommand>>>,
    cancellation: CancellationController,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Player {
    pub(super) fn start(
        transmit: TransmitChannel,
        playback: CodecInterface,
        volume: u8,
    ) -> Result<Self, PlayerStartError> {
        let (command_sender, command_receiver) = sync_channel(COMMAND_CAPACITY);
        let (startup_sender, startup_receiver) = sync_channel(1);
        let cancellation = CancellationController::default();
        let worker = thread::Builder::new()
            .name("audio_player".into())
            .stack_size(PLAYER_STACK_SIZE)
            .spawn(move || {
                run_worker(
                    transmit,
                    playback,
                    volume,
                    &command_receiver,
                    &startup_sender,
                );
            })
            .map_err(|source| PlayerStartError::Spawn { source })?;

        match startup_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                commands: Mutex::new(Some(command_sender)),
                cancellation,
                worker: Mutex::new(Some(worker)),
            }),
            Ok(Err(detail)) => {
                join_failed_start(worker)?;
                Err(PlayerStartError::WorkerInitialization { detail })
            }
            Err(source) => {
                join_failed_start(worker)?;
                Err(PlayerStartError::StartupReport { source })
            }
        }
    }

    /// Queues playback and returns without waiting for it to finish.
    pub(super) fn play(&self, uri: &str) -> Result<(), PlayerError> {
        self.enqueue(uri.to_owned(), None)
    }

    /// Queues playback and waits for completion or cooperative cancellation.
    pub(super) fn play_sync(&self, uri: &str) -> Result<(), PlayerError> {
        let (completion_sender, completion_receiver) = sync_channel(1);
        self.enqueue(uri.to_owned(), Some(completion_sender))?;
        match completion_receiver.recv() {
            Ok(PlaybackOutcome::Finished) => Ok(()),
            Ok(PlaybackOutcome::Cancelled) => Err(PlayerError::Cancelled {
                uri: uri.to_owned(),
            }),
            Ok(PlaybackOutcome::Failed(detail)) => Err(PlayerError::Playback {
                uri: uri.to_owned(),
                detail,
            }),
            Err(source) => Err(PlayerError::CompletionLost {
                uri: uri.to_owned(),
                source,
            }),
        }
    }

    /// Cooperatively cancels active and queued playback requests.
    pub(super) fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Applies a playback volume on the codec-owning worker.
    pub(super) fn set_volume(&self, volume: u8) -> Result<(), PlayerError> {
        let (completion, result) = sync_channel(1);
        let commands = lock(&self.commands);
        let commands = commands.as_ref().ok_or(PlayerError::WorkerUnavailable)?;
        commands
            .send(PlayerCommand::SetVolume { volume, completion })
            .map_err(|_| PlayerError::WorkerUnavailable)?;
        match result.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(detail)) => Err(PlayerError::VolumeUpdate { detail }),
            Err(source) => Err(PlayerError::VolumeCompletionLost { source }),
        }
    }

    fn enqueue(
        &self,
        uri: String,
        completion: Option<SyncSender<PlaybackOutcome>>,
    ) -> Result<(), PlayerError> {
        let mut current = self.cancellation.lock_current();
        let cancellation = CancellationToken::new();
        let command = PlayCommand {
            uri,
            cancellation: cancellation.clone(),
            completion,
        };

        let commands = lock(&self.commands);
        let commands = commands
            .as_ref()
            .ok_or_else(|| PlayerError::WorkerStopped {
                uri: command.uri.clone(),
            })?;
        match commands.try_send(PlayerCommand::Play(command)) {
            Ok(()) => {
                if let Some(previous) = current.replace(cancellation) {
                    previous.cancel();
                }
                Ok(())
            }
            Err(TrySendError::Full(PlayerCommand::Play(command))) => {
                Err(PlayerError::CommandQueueFull { uri: command.uri })
            }
            Err(TrySendError::Disconnected(PlayerCommand::Play(command))) => {
                Err(PlayerError::WorkerStopped { uri: command.uri })
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                Err(PlayerError::WorkerStopped {
                    uri: "internal player command".to_owned(),
                })
            }
        }
    }

    /// Cancels pending work and joins the codec-owning worker.
    pub(super) fn shutdown(&self) {
        self.cancellation.cancel();
        if let Some(commands) = lock(&self.commands).take() {
            let _ = commands.send(PlayerCommand::Shutdown);
        }
        if let Some(worker) = lock(&self.worker).take()
            && worker.join().is_err()
        {
            error!(target: LOG_TARGET, "audio player panicked during shutdown");
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Default)]
struct CancellationController {
    current: Mutex<Option<CancellationToken>>,
}

impl CancellationController {
    fn lock_current(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        match self.current.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn cancel(&self) {
        if let Some(current) = self.lock_current().as_ref() {
            current.cancel();
        }
    }
}

#[derive(Clone)]
struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct PlayCommand {
    uri: String,
    cancellation: CancellationToken,
    completion: Option<SyncSender<PlaybackOutcome>>,
}

enum PlayerCommand {
    Play(PlayCommand),
    SetVolume {
        volume: u8,
        completion: SyncSender<Result<(), String>>,
    },
    Shutdown,
}

enum PlaybackOutcome {
    Finished,
    Cancelled,
    Failed(String),
}

fn join_failed_start(worker: JoinHandle<()>) -> Result<(), PlayerStartError> {
    worker.join().map_err(|_| PlayerStartError::WorkerPanicked)
}

fn run_worker(
    transmit: TransmitChannel,
    playback: CodecInterface,
    volume: u8,
    commands: &Receiver<PlayerCommand>,
    startup: &SyncSender<Result<(), String>>,
) {
    let mut runtime = match PlayerRuntime::new(transmit, playback, volume) {
        Ok(runtime) => runtime,
        Err(source) => {
            error!(target: LOG_TARGET, "failed to initialize audio player: {source:#?}");
            let _ = startup.send(Err(format!("{source:#?}")));
            return;
        }
    };
    if startup.send(Ok(())).is_err() {
        return;
    }

    while let Ok(command) = commands.recv() {
        match command {
            PlayerCommand::Play(command) => run_command(&mut runtime, command),
            PlayerCommand::SetVolume { volume, completion } => {
                let result = runtime
                    .set_volume(volume)
                    .map_err(|source| format!("{source:#?}"));
                let _ = completion.send(result);
            }
            PlayerCommand::Shutdown => break,
        }
    }
}

fn run_command(runtime: &mut PlayerRuntime, command: PlayCommand) {
    info!(target: LOG_TARGET, "starting playback of {:?}", command.uri);
    let cancelled = || command.cancellation.is_cancelled();
    let result = if command.cancellation.is_cancelled() {
        Err(PlaybackSourceError::Cancelled)
    } else {
        runtime.play(&command.uri, &cancelled)
    };
    let outcome = match result {
        Ok(()) => {
            info!(target: LOG_TARGET, "completed playback of {:?}", command.uri);
            PlaybackOutcome::Finished
        }
        Err(source) if source.is_cancelled() => {
            info!(target: LOG_TARGET, "cancelled playback of {:?}", command.uri);
            PlaybackOutcome::Cancelled
        }
        Err(source) => {
            error!(
                target: LOG_TARGET,
                "failed playback of {:?}: {source:#?}",
                command.uri
            );
            PlaybackOutcome::Failed(format!("{source:#?}"))
        }
    };

    if let Some(completion) = command.completion {
        let _ = completion.send(outcome);
    }
}

struct PlayerRuntime {
    codecs: CodecLibrary,
    playback: CodecInterface,
    transmit: TransmitChannel,
    buffers: WorkspaceBuffers,
    volume: u8,
}

impl PlayerRuntime {
    fn new(
        mut transmit: TransmitChannel,
        mut playback: CodecInterface,
        volume: u8,
    ) -> Result<Self, WorkerStartError> {
        let codecs = CodecLibrary::new().map_err(|source| WorkerStartError::Codec { source })?;
        let mut buffers = WorkspaceBuffers::new()?;
        let _workspace = buffers
            .workspace()
            .map_err(|source| WorkerStartError::Workspace { source })?;
        transmit
            .enable()
            .map_err(|source| WorkerStartError::I2s { source })?;
        playback
            .configure_playback()
            .map_err(|source| WorkerStartError::CodecDevice { source })?;
        playback
            .set_volume(volume)
            .map_err(|source| WorkerStartError::CodecDevice { source })?;
        Ok(Self {
            codecs,
            playback,
            transmit,
            buffers,
            volume,
        })
    }

    fn play(&mut self, uri: &str, cancelled: &dyn Fn() -> bool) -> Result<(), PlaybackSourceError> {
        self.playback
            .enable(true)
            .map_err(|source| PlaybackSourceError::CodecDevice { source })?;
        // ES8311 suspend clears its DAC volume without restoring it on the
        // next start; ES8156 start restores a hardcoded default. Reapply the
        // configured volume after every enable on both playback codecs.
        let result = match self.playback.set_volume(self.volume) {
            Ok(()) => self.play_enabled(uri, cancelled),
            Err(source) => Err(PlaybackSourceError::CodecDevice { source }),
        };
        let disable = self.playback.enable(false);
        match (result, disable) {
            (Err(source), _) => Err(source),
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(source)) => Err(PlaybackSourceError::CodecDevice { source }),
        }
    }

    fn play_enabled(
        &mut self,
        uri: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), PlaybackSourceError> {
        let mut workspace = self
            .buffers
            .workspace()
            .map_err(|source| PlaybackSourceError::Workspace { source })?;

        if uri.starts_with("spiffs://") {
            spiffs_playback::play(
                uri,
                &self.codecs,
                &mut self.transmit,
                &mut workspace,
                cancelled,
            )
            .map_err(|source| PlaybackSourceError::Spiffs { source })
        } else if uri.starts_with("http://") || uri.starts_with("https://") {
            http_playback::play(
                uri,
                &self.codecs,
                &mut self.transmit,
                &mut workspace,
                cancelled,
            )
            .map_err(|source| PlaybackSourceError::Http { source })
        } else {
            Err(PlaybackSourceError::UnsupportedUri {
                uri: uri.to_owned(),
            })
        }
    }

    fn set_volume(&mut self, volume: u8) -> Result<(), CodecError> {
        self.playback.set_volume(volume)?;
        self.volume = volume;
        Ok(())
    }
}

#[derive(Debug)]
enum WorkerStartError {
    Allocate {
        buffer: &'static str,
        bytes: usize,
        source: TryReserveError,
    },
    Codec {
        source: StreamCodecError,
    },
    CodecDevice {
        source: CodecError,
    },
    I2s {
        source: I2sError,
    },
    Workspace {
        source: PlaybackError,
    },
}

impl fmt::Display for WorkerStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocate {
                buffer,
                bytes,
                source,
            } => write!(
                formatter,
                "failed to allocate {bytes} bytes for player {buffer}: {source}"
            ),
            Self::Codec { source } => {
                write!(formatter, "failed to initialize stream codecs: {source}")
            }
            Self::CodecDevice { source } => {
                write!(formatter, "failed to initialize playback codec: {source}")
            }
            Self::I2s { source } => write!(formatter, "failed to initialize I2S output: {source}"),
            Self::Workspace { source } => {
                write!(formatter, "invalid player workspace: {source}")
            }
        }
    }
}

impl std::error::Error for WorkerStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocate { source, .. } => Some(source),
            Self::Codec { source } => Some(source),
            Self::CodecDevice { source } => Some(source),
            Self::I2s { source } => Some(source),
            Self::Workspace { source } => Some(source),
        }
    }
}

#[derive(Debug)]
enum PlaybackSourceError {
    Cancelled,
    CodecDevice { source: CodecError },
    Workspace { source: PlaybackError },
    Spiffs { source: SpiffsPlaybackError },
    Http { source: HttpPlaybackError },
    UnsupportedUri { uri: String },
}

impl fmt::Display for PlaybackSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("playback was cancelled"),
            Self::CodecDevice { source } => write!(formatter, "playback codec failed: {source}"),
            Self::Workspace { source } => write!(formatter, "playback workspace failed: {source}"),
            Self::Spiffs { source } => write!(formatter, "SPIFFS playback failed: {source}"),
            Self::Http { source } => write!(formatter, "HTTP playback failed: {source}"),
            Self::UnsupportedUri { uri } => {
                write!(formatter, "unsupported playback URI {uri:?}")
            }
        }
    }
}

impl std::error::Error for PlaybackSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CodecDevice { source } => Some(source),
            Self::Workspace { source } => Some(source),
            Self::Spiffs { source } => Some(source),
            Self::Http { source } => Some(source),
            Self::Cancelled | Self::UnsupportedUri { .. } => None,
        }
    }
}

impl PlaybackSourceError {
    const fn is_cancelled(&self) -> bool {
        match self {
            Self::Cancelled => true,
            Self::CodecDevice { .. } | Self::UnsupportedUri { .. } => false,
            Self::Workspace { source } => source.is_cancelled(),
            Self::Spiffs { source } => source.is_cancelled(),
            Self::Http { source } => source.is_cancelled(),
        }
    }
}

struct WorkspaceBuffers {
    encoded: Vec<u8>,
    decoded: Vec<u8>,
    decoded_samples: Vec<i16>,
    i2s_samples: Vec<i32>,
}

impl WorkspaceBuffers {
    fn new() -> Result<Self, WorkerStartError> {
        Ok(Self {
            encoded: allocate_zeroed("encoded input", ENCODED_BYTES, 0_u8)?,
            decoded: allocate_zeroed("decoded PCM", DECODED_BYTES, 0_u8)?,
            decoded_samples: allocate_zeroed("decoded samples", DECODED_SAMPLES, 0_i16)?,
            i2s_samples: allocate_zeroed("I2S samples", I2S_SAMPLES, 0_i32)?,
        })
    }

    fn workspace(&mut self) -> Result<PlaybackWorkspace<'_>, PlaybackError> {
        PlaybackWorkspace::new(
            &mut self.encoded,
            &mut self.decoded,
            &mut self.decoded_samples,
            &mut self.i2s_samples,
        )
    }
}

fn allocate_zeroed<T: Copy>(
    buffer_name: &'static str,
    elements: usize,
    zero: T,
) -> Result<Vec<T>, WorkerStartError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(elements)
        .map_err(|source| WorkerStartError::Allocate {
            buffer: buffer_name,
            bytes: elements.saturating_mul(size_of::<T>()),
            source,
        })?;
    buffer.resize(elements, zero);
    Ok(buffer)
}
