//! Blocking WIS upload worker backed by the bounded PCM record buffer.

#![allow(
    dead_code,
    reason = "the upload worker remains inactive until Rust owns runtime audio"
)]

use core::fmt;
use std::{
    collections::TryReserveError,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvError, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
};

use log::{error, warn};

use super::{
    record_buffer::{RecordBuffer, RecordBufferError, RecordSessionId},
    wis_config,
    wis_framing::WisFormat,
    wis_upload::{WisUpload, WisUploadError, WisUploadResponse},
};

const COMMAND_CAPACITY: usize = 1;
const COMPLETION_CAPACITY: usize = 1;
const LOG_TARGET: &str = "WILLOW/AUDIO";
const UPLOAD_READ_SAMPLES: usize = 1_024;
const UPLOAD_STACK_SIZE: usize = 16 * 1024;

#[derive(Debug)]
pub(super) enum UploadStartError {
    Spawn { source: io::Error },
    StartupReport { source: RecvError },
    WorkerInitialization { detail: String },
    WorkerPanicked,
}

impl fmt::Display for UploadStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { source } => {
                write!(formatter, "failed to start WIS upload worker: {source}")
            }
            Self::StartupReport { source } => {
                write!(
                    formatter,
                    "WIS upload worker did not report startup: {source}"
                )
            }
            Self::WorkerInitialization { detail } => {
                write!(
                    formatter,
                    "WIS upload worker initialization failed: {detail}"
                )
            }
            Self::WorkerPanicked => formatter.write_str("WIS upload worker panicked"),
        }
    }
}

impl std::error::Error for UploadStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source } => Some(source),
            Self::StartupReport { source } => Some(source),
            Self::WorkerInitialization { .. } | Self::WorkerPanicked => None,
        }
    }
}

#[derive(Debug)]
pub(super) enum UploadCommandError {
    Buffer { source: RecordBufferError },
    CommandQueueFull,
    WorkerStopped,
    CompletionChannelStopped,
}

impl fmt::Display for UploadCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buffer { source } => write!(formatter, "cannot start WIS recording: {source}"),
            Self::CommandQueueFull => formatter.write_str("WIS upload command queue is full"),
            Self::WorkerStopped => formatter.write_str("WIS upload worker has stopped"),
            Self::CompletionChannelStopped => {
                formatter.write_str("WIS upload completion channel has stopped")
            }
        }
    }
}

impl std::error::Error for UploadCommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Buffer { source } => Some(source),
            Self::CommandQueueFull | Self::WorkerStopped | Self::CompletionChannelStopped => None,
        }
    }
}

#[derive(Debug)]
pub(super) enum UploadError {
    AllocateScratch {
        samples: usize,
        source: TryReserveError,
    },
    Buffer {
        source: RecordBufferError,
    },
    Wis {
        source: WisUploadError,
    },
}

impl fmt::Display for UploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateScratch { samples, source } => write!(
                formatter,
                "failed to allocate {samples}-sample WIS upload buffer: {source}"
            ),
            Self::Buffer { source } => write!(formatter, "WIS PCM handoff failed: {source}"),
            Self::Wis { source } => write!(formatter, "WIS streaming failed: {source}"),
        }
    }
}

impl std::error::Error for UploadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateScratch { source, .. } => Some(source),
            Self::Buffer { source } => Some(source),
            Self::Wis { source } => Some(source),
        }
    }
}

/// Result returned after one upload ends or fails.
pub(super) struct UploadCompletion {
    pub(super) session: RecordSessionId,
    pub(super) result: Result<WisUploadResponse, UploadError>,
    pub(super) dropped_samples: u64,
}

/// Producer-side control for one active WIS request.
pub(super) struct UploadSession {
    id: RecordSessionId,
    record_buffer: RecordBuffer,
    cancelled: Arc<AtomicBool>,
    finished: bool,
}

impl UploadSession {
    pub(super) const fn id(&self) -> RecordSessionId {
        self.id
    }

    pub(super) fn finish(&mut self) -> Result<(), RecordBufferError> {
        self.record_buffer.finish_session(self.id)?;
        self.finished = true;
        Ok(())
    }

    pub(super) fn cancel(&mut self) -> Result<(), RecordBufferError> {
        self.cancelled.store(true, Ordering::Release);
        self.finish()
    }
}

impl Drop for UploadSession {
    fn drop(&mut self) {
        if !self.finished {
            self.cancelled.store(true, Ordering::Release);
            let _ = self.record_buffer.finish_session(self.id);
        }
    }
}

/// Owns the single blocking HTTP worker and completion channel.
pub(super) struct UploadWorker {
    record_buffer: RecordBuffer,
    commands: Option<SyncSender<UploadCommand>>,
    completions: Option<Receiver<UploadCompletion>>,
    worker: Option<JoinHandle<()>>,
}

impl UploadWorker {
    pub(super) fn start(record_buffer: RecordBuffer) -> Result<Self, UploadStartError> {
        let (command_sender, command_receiver) = sync_channel(COMMAND_CAPACITY);
        let (completion_sender, completion_receiver) = sync_channel(COMPLETION_CAPACITY);
        let (startup_sender, startup_receiver) = sync_channel(1);
        let worker_buffer = record_buffer.clone();
        let worker = thread::Builder::new()
            .name("wis_upload".into())
            .stack_size(UPLOAD_STACK_SIZE)
            .spawn(move || {
                run_worker(
                    &worker_buffer,
                    &command_receiver,
                    &completion_sender,
                    &startup_sender,
                );
            })
            .map_err(|source| UploadStartError::Spawn { source })?;

        match startup_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                record_buffer,
                commands: Some(command_sender),
                completions: Some(completion_receiver),
                worker: Some(worker),
            }),
            Ok(Err(detail)) => {
                join_failed_start(worker)?;
                Err(UploadStartError::WorkerInitialization { detail })
            }
            Err(source) => {
                join_failed_start(worker)?;
                Err(UploadStartError::StartupReport { source })
            }
        }
    }

    pub(super) fn begin(&self) -> Result<UploadSession, UploadCommandError> {
        let session = self
            .record_buffer
            .begin_session()
            .map_err(|source| UploadCommandError::Buffer { source })?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let command = UploadCommand {
            session,
            url: wis_config::active_url(),
            format: wis_config::active_format(),
            cancelled: Arc::clone(&cancelled),
        };
        let Some(commands) = self.commands.as_ref() else {
            abort_failed_begin(&self.record_buffer, session);
            return Err(UploadCommandError::WorkerStopped);
        };
        match commands.try_send(command) {
            Ok(()) => Ok(UploadSession {
                id: session,
                record_buffer: self.record_buffer.clone(),
                cancelled,
                finished: false,
            }),
            Err(TrySendError::Full(_)) => {
                abort_failed_begin(&self.record_buffer, session);
                Err(UploadCommandError::CommandQueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                abort_failed_begin(&self.record_buffer, session);
                Err(UploadCommandError::WorkerStopped)
            }
        }
    }

    pub(super) fn try_completion(&self) -> Result<Option<UploadCompletion>, UploadCommandError> {
        let Some(completions) = self.completions.as_ref() else {
            return Err(UploadCommandError::CompletionChannelStopped);
        };
        match completions.try_recv() {
            Ok(completion) => Ok(Some(completion)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(UploadCommandError::CompletionChannelStopped),
        }
    }
}

impl Drop for UploadWorker {
    fn drop(&mut self) {
        self.commands.take();
        self.record_buffer.shutdown();
        self.completions.take();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            error!(target: LOG_TARGET, "WIS upload worker panicked during shutdown");
        }
    }
}

struct UploadCommand {
    session: RecordSessionId,
    url: &'static str,
    format: WisFormat,
    cancelled: Arc<AtomicBool>,
}

struct CompletedUpload {
    response: WisUploadResponse,
    dropped_samples: u64,
}

fn join_failed_start(worker: JoinHandle<()>) -> Result<(), UploadStartError> {
    worker.join().map_err(|_| UploadStartError::WorkerPanicked)
}

fn run_worker(
    record_buffer: &RecordBuffer,
    commands: &Receiver<UploadCommand>,
    completions: &SyncSender<UploadCompletion>,
    startup: &SyncSender<Result<(), String>>,
) {
    let mut scratch = match allocate_scratch() {
        Ok(scratch) => scratch,
        Err(source) => {
            error!(target: LOG_TARGET, "failed to initialize WIS upload worker: {source:#?}");
            let _ = startup.send(Err(format!("{source:#?}")));
            return;
        }
    };
    if startup.send(Ok(())).is_err() {
        return;
    }

    while let Ok(command) = commands.recv() {
        let session = command.session;
        let result = run_upload(record_buffer, &command, &mut scratch);
        let (result, dropped_samples) = match result {
            Ok(completed) => (Ok(completed.response), completed.dropped_samples),
            Err(source) => {
                error!(target: LOG_TARGET, "WIS upload failed: {source:#?}");
                let dropped_samples = match record_buffer.abort_session(session) {
                    Ok(dropped_samples) => dropped_samples,
                    Err(abort_error) => {
                        warn!(
                            target: LOG_TARGET,
                            "failed to abort WIS record-buffer session {session:?}: {abort_error:#?}"
                        );
                        0
                    }
                };
                (Err(source), dropped_samples)
            }
        };
        if completions
            .send(UploadCompletion {
                session,
                result,
                dropped_samples,
            })
            .is_err()
        {
            return;
        }
    }
}

fn run_upload(
    record_buffer: &RecordBuffer,
    command: &UploadCommand,
    scratch: &mut [i16],
) -> Result<CompletedUpload, UploadError> {
    let cancelled = || command.cancelled.load(Ordering::Acquire);
    let mut upload = WisUpload::start(command.url, command.format, &cancelled)
        .map_err(|source| UploadError::Wis { source })?;
    loop {
        let read = record_buffer
            .read_session(command.session, scratch)
            .map_err(|source| UploadError::Buffer { source })?;
        if read.samples > 0 {
            upload
                .write_samples(&scratch[..read.samples], &cancelled)
                .map_err(|source| UploadError::Wis { source })?;
        }
        if read.end_of_session {
            break;
        }
    }

    let response = upload
        .finish(&cancelled)
        .map_err(|source| UploadError::Wis { source })?;
    let encoder_dropped_samples =
        u64::try_from(response.encoding_finish.dropped_samples).unwrap_or(u64::MAX);
    let dropped_samples = record_buffer
        .complete_session(command.session)
        .map_err(|source| UploadError::Buffer { source })?;
    Ok(CompletedUpload {
        response,
        dropped_samples: dropped_samples.saturating_add(encoder_dropped_samples),
    })
}

fn allocate_scratch() -> Result<Vec<i16>, UploadError> {
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(UPLOAD_READ_SAMPLES)
        .map_err(|source| UploadError::AllocateScratch {
            samples: UPLOAD_READ_SAMPLES,
            source,
        })?;
    scratch.resize(UPLOAD_READ_SAMPLES, 0);
    Ok(scratch)
}

fn abort_failed_begin(record_buffer: &RecordBuffer, session: RecordSessionId) {
    if let Err(source) = record_buffer.abort_session(session) {
        error!(
            target: LOG_TARGET,
            "failed to roll back WIS session {session:?} after command rejection: {source:#?}"
        );
    }
}
