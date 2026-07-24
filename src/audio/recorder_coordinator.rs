//! Recorder behavior coordinator for the Rust-owned audio runtime.

use core::{fmt, str, time::Duration};
use std::{
    io,
    sync::{
        Arc,
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use esp_idf_sys::{ESP_ERR_HTTP_EAGAIN, ESP_ERR_TIMEOUT};
use log::{debug, error, info, warn};
use serde_json::Value;

use crate::{backlight, input, sr::VadState, ui, was};

use super::{
    player::Player,
    record_upload::{
        UploadCommandError, UploadCompletion, UploadError, UploadSession, UploadWorker,
    },
    recorder::{CaptureError, CaptureEvent, CaptureWorker},
    recorder_state::{
        CaptureEffect, ChimeEffect, DisplayEffect, RecorderEffects, RecorderEvent, RecorderMachine,
        RecorderPolicy, RecorderStopCause, TimeoutEffect, UploadCompletionEffect, UploadEffect,
        WasEffect,
    },
    recorder_timing::{RecorderTiming, TimingEvent},
    response::{CommandOutcome, ResponseAudio},
    response_config,
    wis_upload::{WisUploadError, WisUploadResponse},
};

const COMMAND_CAPACITY: usize = 8;
const DEFAULT_STREAM_TIMEOUT_SECS: u32 = 5;
const DEFAULT_VAD_TIMEOUT_MS: u32 = 300;
const LOG_TARGET: &str = "WILLOW/AUDIO";
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const WORKER_STACK_SIZE: usize = 16 * 1024;

/// Preserves the startup gate which keeps audio inactive while mute is held.
pub(super) fn wait_for_initial_unmute() {
    match input::is_muted() {
        Ok(false) => return,
        Ok(true) => {
            warn!(target: LOG_TARGET, "mute is activated; waiting for release before audio startup");
            ui::show_error("Mute Activated", Some("Unmute to continue"));
        }
        Err(source) => {
            error!(target: LOG_TARGET, "cannot read startup mute input; continuing audio startup: {source:#?}");
            return;
        }
    }
    if let Err(source) = input::wait_until_unmuted() {
        error!(target: LOG_TARGET, "cannot wait for startup mute release; continuing audio startup: {source:#?}");
    }
}

#[derive(Debug)]
pub(super) enum CoordinatorStartError {
    Spawn { source: io::Error },
}

impl fmt::Display for CoordinatorStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { source } => {
                write!(formatter, "failed to start recorder coordinator: {source}")
            }
        }
    }
}

impl std::error::Error for CoordinatorStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source } => Some(source),
        }
    }
}

#[derive(Debug)]
pub(super) enum CoordinatorCommandError {
    QueueFull,
    WorkerStopped,
}

impl fmt::Display for CoordinatorCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => formatter.write_str("recorder coordinator command queue is full"),
            Self::WorkerStopped => formatter.write_str("recorder coordinator has stopped"),
        }
    }
}

impl std::error::Error for CoordinatorCommandError {}

enum CoordinatorCommand {
    MultiwakeResult { won: bool },
    Shutdown,
    Stop { cause: RecorderStopCause },
    Unmuted,
}

/// Nonblocking control handle for the recorder behavior worker.
pub(super) struct RecorderCoordinator {
    commands: Option<SyncSender<CoordinatorCommand>>,
    worker: Option<JoinHandle<()>>,
}

impl RecorderCoordinator {
    pub(super) fn start(
        capture: CaptureWorker,
        upload: UploadWorker,
        player: Arc<Player>,
        unmuted: impl FnMut() + Send + 'static,
    ) -> Result<Self, CoordinatorStartError> {
        let configuration = ActiveConfiguration::load();
        let (commands, command_receiver) = sync_channel(COMMAND_CAPACITY);
        let monitor_commands = commands.clone();
        let unmute_monitor = input::start_unmute_monitor(move || {
            match monitor_commands.try_send(CoordinatorCommand::Unmuted) {
                Err(TrySendError::Full(_)) => {
                    warn!(target: LOG_TARGET, "dropping unmute event because recorder commands are full");
                }
                Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            }
        });
        let unmute_monitor = match unmute_monitor {
            Ok(monitor) => Some(monitor),
            Err(source) => {
                error!(target: LOG_TARGET, "failed to start mute input monitor: {source:#?}");
                None
            }
        };
        let worker = thread::Builder::new()
            .name("recorder_coord".into())
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                RecorderRuntime::new(
                    configuration,
                    capture,
                    upload,
                    player,
                    unmute_monitor,
                    unmuted,
                )
                .run(&command_receiver);
            })
            .map_err(|source| CoordinatorStartError::Spawn { source })?;
        Ok(Self {
            commands: Some(commands),
            worker: Some(worker),
        })
    }

    /// Requests user cancellation without blocking the touch task.
    pub(super) fn stop(&self) -> Result<(), CoordinatorCommandError> {
        self.send(CoordinatorCommand::Stop {
            cause: RecorderStopCause::User,
        })
    }

    /// Publishes Willow One Wake arbitration from the WAS event handler.
    pub(super) fn multiwake_result(&self, won: bool) -> Result<(), CoordinatorCommandError> {
        self.send(CoordinatorCommand::MultiwakeResult { won })
    }

    fn send(&self, command: CoordinatorCommand) -> Result<(), CoordinatorCommandError> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(CoordinatorCommandError::WorkerStopped);
        };
        match commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(CoordinatorCommandError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(CoordinatorCommandError::WorkerStopped),
        }
    }
}

impl Drop for RecorderCoordinator {
    fn drop(&mut self) {
        if let Some(commands) = self.commands.take() {
            // The monitor retained by the worker owns another sender, so a
            // full queue would not disconnect when this handle is dropped.
            // A blocking shutdown command guarantees the subsequent join can
            // complete; ordinary UI and WAS commands remain nonblocking.
            let _ = commands.send(CoordinatorCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            error!(target: LOG_TARGET, "recorder coordinator panicked during shutdown");
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveConfiguration {
    policy: RecorderPolicy,
    session_timeout: Option<Duration>,
    vad_timeout: Duration,
}

impl ActiveConfiguration {
    fn load() -> Self {
        let configuration = crate::config::config();
        let stream_timeout_secs = configuration
            .and_then(|config| config.stream_timeout)
            .unwrap_or(DEFAULT_STREAM_TIMEOUT_SECS);
        Self {
            policy: RecorderPolicy {
                multiwake_enabled: configuration
                    .and_then(|config| config.multiwake)
                    .unwrap_or(false),
                wake_confirmation: configuration
                    .and_then(|config| config.wake_confirmation)
                    .unwrap_or(false),
            },
            session_timeout: (stream_timeout_secs != 0)
                .then(|| Duration::from_secs(u64::from(stream_timeout_secs))),
            vad_timeout: Duration::from_millis(u64::from(
                configuration
                    .and_then(|config| config.vad_timeout)
                    .unwrap_or(DEFAULT_VAD_TIMEOUT_MS),
            )),
        }
    }
}

struct RecorderRuntime<Unmuted> {
    capture: CaptureWorker,
    machine: RecorderMachine,
    origin: Instant,
    player: Arc<Player>,
    session_deadline: Option<Duration>,
    session_timeout: Option<Duration>,
    timing: RecorderTiming,
    upload: UploadWorker,
    upload_session: Option<UploadSession>,
    _unmute_monitor: Option<input::UnmuteMonitor>,
    unmuted: Unmuted,
}

impl<Unmuted> RecorderRuntime<Unmuted>
where
    Unmuted: FnMut(),
{
    fn new(
        configuration: ActiveConfiguration,
        capture: CaptureWorker,
        upload: UploadWorker,
        player: Arc<Player>,
        unmute_monitor: Option<input::UnmuteMonitor>,
        unmuted: Unmuted,
    ) -> Self {
        Self {
            capture,
            machine: RecorderMachine::new(configuration.policy),
            origin: Instant::now(),
            player,
            session_deadline: None,
            session_timeout: configuration.session_timeout,
            timing: RecorderTiming::new(configuration.vad_timeout),
            upload,
            upload_session: None,
            _unmute_monitor: unmute_monitor,
            unmuted,
        }
    }

    fn run(mut self, commands: &Receiver<CoordinatorCommand>) {
        loop {
            if let Err(source) = self.drain_capture() {
                error!(target: LOG_TARGET, "recorder capture event stream stopped: {source:#?}");
                break;
            }
            if let Err(source) = self.drain_upload() {
                error!(target: LOG_TARGET, "WIS completion stream stopped: {source:#?}");
                break;
            }

            let now = self.now();
            while let Some(event) = self.timing.tick(now) {
                self.handle_timing_event(event, now);
            }
            if self
                .session_deadline
                .is_some_and(|deadline| deadline <= now)
            {
                info!(target: LOG_TARGET, "session timer expired - forcing end stream");
                self.handle_event(
                    RecorderEvent::StopRequested {
                        cause: RecorderStopCause::Timeout,
                    },
                    now,
                );
            }

            match commands.recv_timeout(POLL_INTERVAL) {
                Ok(CoordinatorCommand::MultiwakeResult { won }) => {
                    self.handle_event(RecorderEvent::MultiwakeResult { won }, self.now());
                }
                Ok(CoordinatorCommand::Stop { cause }) => {
                    self.handle_event(RecorderEvent::StopRequested { cause }, self.now());
                }
                Ok(CoordinatorCommand::Unmuted) => (self.unmuted)(),
                Ok(CoordinatorCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    self.shutdown();
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn drain_capture(&mut self) -> Result<(), CaptureError> {
        while let Some(event) = self.capture.try_event()? {
            let now = self.now();
            match event {
                CaptureEvent::WakeDetected { volume_db } => {
                    self.timing.wake(now);
                    self.handle_event(RecorderEvent::WakeDetected { volume_db }, now);
                }
                CaptureEvent::VadChanged { state } => {
                    self.timing
                        .vad_changed(matches!(state, VadState::Speech), now);
                }
            }
        }
        Ok(())
    }

    fn drain_upload(&mut self) -> Result<(), UploadCommandError> {
        while let Some(completion) = self.upload.try_completion()? {
            self.handle_upload_completion(completion, self.now());
        }
        Ok(())
    }

    fn handle_timing_event(&mut self, event: TimingEvent, now: Duration) {
        let event = match event {
            TimingEvent::VadEnded => RecorderEvent::VadEnded,
            TimingEvent::VadStarted => RecorderEvent::VadStarted,
            TimingEvent::WakeEnded => RecorderEvent::WakeEnded,
        };
        self.handle_event(event, now);
    }

    fn handle_event(&mut self, event: RecorderEvent, now: Duration) {
        let effects = self.machine.apply(event);
        self.apply_effects(effects, now);
    }

    fn apply_effects(&mut self, effects: RecorderEffects, now: Duration) {
        self.apply_timeout(effects.timeout, now);
        Self::apply_was(effects.was);
        Self::apply_display(effects.display);
        self.apply_chime(effects.chime);

        let upload_failed = self.apply_upload(effects.upload);
        if matches!(effects.capture, CaptureEffect::StopSession { .. })
            && let Some(event) = self.timing.force_end()
        {
            self.handle_timing_event(event, now);
        }
        if upload_failed {
            let followup = self.machine.apply(RecorderEvent::UploadCompleted {
                has_response: false,
            });
            self.apply_effects(followup, now);
            self.show_upload_failure("Cannot Reach WIS", "Check Server & Settings", true);
        }
    }

    fn apply_timeout(&mut self, effect: TimeoutEffect, now: Duration) {
        match effect {
            TimeoutEffect::None => {}
            TimeoutEffect::Arm => {
                self.session_deadline = self
                    .session_timeout
                    .map(|timeout| now.saturating_add(timeout));
            }
            TimeoutEffect::Cancel => self.session_deadline = None,
        }
    }

    fn apply_was(effect: WasEffect) {
        let result = match effect {
            WasEffect::None => return,
            WasEffect::WakeEnd => was::send_wake_end(),
            WasEffect::WakeStart { volume_db } => was::send_wake_start(volume_db),
        };
        if let Err(source) = result {
            error!(target: LOG_TARGET, "failed to publish recorder event to WAS: {source:#?}");
        }
    }

    fn apply_display(effect: DisplayEffect) {
        match effect {
            DisplayEffect::None => {}
            DisplayEffect::Listening => {
                if let Err(source) = backlight::reset_display_timer(true) {
                    error!(target: LOG_TARGET, "failed to pause display timeout: {source:#?}");
                }
                ui::show_listening();
                backlight::set(true, false);
            }
            DisplayEffect::Thinking { multiwake_won } => {
                ui::show_thinking(multiwake_won);
                if let Err(source) = backlight::reset_display_timer(false) {
                    error!(target: LOG_TARGET, "failed to schedule display timeout: {source:#?}");
                }
            }
        }
    }

    fn apply_chime(&self, effect: ChimeEffect) {
        if matches!(effect, ChimeEffect::WakeConfirmation) {
            self.play_response(CommandOutcome::Success, None);
        }
    }

    fn apply_upload(&mut self, effect: UploadEffect) -> bool {
        match effect {
            UploadEffect::None => false,
            UploadEffect::Start => match self.upload.begin() {
                Ok(session) => {
                    debug!(target: LOG_TARGET, "started WIS session {:?}", session.id());
                    self.upload_session = Some(session);
                    false
                }
                Err(source) => {
                    error!(target: LOG_TARGET, "failed to start WIS upload: {source:#?}");
                    true
                }
            },
            UploadEffect::Finish { cause } => {
                let Some(session) = self.upload_session.as_mut() else {
                    warn!(target: LOG_TARGET, "cannot finish missing WIS session after {cause:?}");
                    return false;
                };
                if let Err(source) = session.finish() {
                    error!(
                        target: LOG_TARGET,
                        "failed to finish WIS session {:?} after {cause:?}: {source:#?}",
                        session.id()
                    );
                    true
                } else {
                    false
                }
            }
        }
    }

    fn handle_upload_completion(&mut self, completion: UploadCompletion, now: Duration) {
        let Some(active) = self.upload_session.as_ref() else {
            warn!(
                target: LOG_TARGET,
                "ignoring completion for untracked WIS session {:?}",
                completion.session
            );
            return;
        };
        if active.id() != completion.session {
            warn!(
                target: LOG_TARGET,
                "ignoring stale WIS session completion: active={:?}, completed={:?}",
                active.id(),
                completion.session
            );
            return;
        }

        if completion.dropped_samples > 0 {
            warn!(
                target: LOG_TARGET,
                "WIS session {:?} lost {} PCM samples",
                completion.session,
                completion.dropped_samples
            );
        }
        if let Some(mut session) = self.upload_session.take() {
            session.acknowledge_completion();
        }

        let has_response = completion.result.is_ok();
        let effects = self
            .machine
            .apply(RecorderEvent::UploadCompleted { has_response });
        let completion_effect = effects.upload_completion;
        self.apply_effects(effects, now);
        match (completion_effect, completion.result) {
            (UploadCompletionEffect::Process, Ok(response)) => {
                self.process_wis_response(response);
            }
            (UploadCompletionEffect::ReportFailure, Err(source)) => {
                self.report_upload_failure(&source);
            }
            (UploadCompletionEffect::Discard, result) => {
                debug!(
                    target: LOG_TARGET,
                    "discarding WIS session {:?} result after multiwake loss: success={}",
                    completion.session,
                    result.is_ok()
                );
            }
            (UploadCompletionEffect::None, _) => {}
            (effect, result) => {
                error!(
                    target: LOG_TARGET,
                    "recorder selected inconsistent WIS completion {effect:?}: success={}",
                    result.is_ok()
                );
            }
        }
    }

    fn process_wis_response(&self, response: WisUploadResponse) {
        let body = response.into_body();
        let Ok(body) = str::from_utf8(&body) else {
            self.show_upload_failure("Invalid WIS response", "Response is not UTF-8", true);
            return;
        };
        info!(target: LOG_TARGET, "WIS HTTP response: {body}");
        if let Err(source) = was::send_endpoint(body) {
            error!(target: LOG_TARGET, "failed to send WIS response to WAS: {source:#?}");
        }

        let parsed = serde_json::from_str::<Value>(body).ok();
        let speaker_status = parsed
            .as_ref()
            .and_then(|value| value.get("speaker_status"))
            .and_then(Value::as_str)
            .unwrap_or("I heard:");
        let text = parsed
            .as_ref()
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
            .unwrap_or(body);
        ui::show_recognition(speaker_status, text);
    }

    fn report_upload_failure(&self, failure: &UploadError) {
        match failure {
            UploadError::Wis {
                source: WisUploadError::HttpStatus { status: 401, .. },
            } => self.show_upload_failure("WIS auth failed", "Check server & settings", false),
            UploadError::Wis {
                source: WisUploadError::HttpStatus { status: 406, .. },
            } => self.show_upload_failure("Unauthorized Speaker", "", true),
            UploadError::Wis {
                source: WisUploadError::HttpStatus { status, .. },
            } => {
                let message = format!("WIS HTTP {status}");
                self.show_upload_failure(&message, "", true);
            }
            UploadError::Wis {
                source: WisUploadError::Http { source, .. },
            } if matches!(source.code(), ESP_ERR_TIMEOUT | ESP_ERR_HTTP_EAGAIN) => {
                self.show_upload_failure("WIS timeout", "Check server performance", false);
            }
            UploadError::Wis {
                source: WisUploadError::Cancelled { .. },
            } => debug!(target: LOG_TARGET, "WIS upload was cancelled"),
            _ => self.show_upload_failure("Cannot Reach WIS", "Check Server & Settings", true),
        }
    }

    fn show_upload_failure(&self, primary: &str, secondary: &str, audible: bool) {
        ui::show_error(primary, (!secondary.is_empty()).then_some(secondary));
        if audible {
            self.play_response(CommandOutcome::Error, Some(primary));
        }
    }

    fn play_response(&self, outcome: CommandOutcome, text: Option<&str>) {
        match response_config::active_policy().select(outcome, text) {
            Ok(ResponseAudio::None) => {}
            Ok(ResponseAudio::Play(uri)) => {
                response_config::prepare_playback();
                if let Err(source) = self.player.play(&uri) {
                    error!(
                        target: LOG_TARGET,
                        "failed to queue recorder response audio {uri:?}: {source:#?}"
                    );
                }
            }
            Err(source) => {
                warn!(target: LOG_TARGET, "cannot select recorder response audio: {source:#?}");
            }
        }
    }

    fn shutdown(&mut self) {
        let now = self.now();
        self.handle_event(
            RecorderEvent::StopRequested {
                cause: RecorderStopCause::Shutdown,
            },
            now,
        );
        if let Some(session) = self.upload_session.as_mut()
            && let Err(source) = session.cancel()
        {
            warn!(
                target: LOG_TARGET,
                "failed to cancel WIS session {:?} during shutdown: {source:#?}",
                session.id()
            );
        }
    }
}
