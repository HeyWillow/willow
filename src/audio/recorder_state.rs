//! Pure recorder-session transitions and side-effect decisions.

#![allow(
    dead_code,
    reason = "the state machine remains inactive until Rust owns runtime audio"
)]

/// Recorder behavior fixed from the active startup configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RecorderPolicy {
    pub(super) multiwake_enabled: bool,
    pub(super) wake_confirmation: bool,
}

/// Reason a capture or upload session is being stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecorderStopCause {
    MultiwakeLost,
    Shutdown,
    Timeout,
    UploadFailed,
    User,
    WakeEnded,
}

/// Input event observed by the recorder coordinator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RecorderEvent {
    MultiwakeResult { won: bool },
    StopRequested { cause: RecorderStopCause },
    UploadCompleted { has_response: bool },
    VadEnded,
    VadStarted,
    WakeDetected { volume_db: f32 },
    WakeEnded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum CaptureEffect {
    #[default]
    None,
    StopSession {
        cause: RecorderStopCause,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TimeoutEffect {
    #[default]
    None,
    Arm,
    Cancel,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UploadEffect {
    #[default]
    None,
    Start,
    Finish {
        cause: RecorderStopCause,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum WasEffect {
    #[default]
    None,
    WakeStart {
        volume_db: f32,
    },
    WakeEnd,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DisplayEffect {
    #[default]
    None,
    Listening,
    Thinking {
        multiwake_won: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ChimeEffect {
    #[default]
    None,
    WakeConfirmation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UploadCompletionEffect {
    #[default]
    None,
    Discard,
    Process,
    ReportFailure,
}

/// Bounded side effects selected for one transition.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct RecorderEffects {
    pub(super) capture: CaptureEffect,
    pub(super) timeout: TimeoutEffect,
    pub(super) upload: UploadEffect,
    pub(super) was: WasEffect,
    pub(super) display: DisplayEffect,
    pub(super) chime: ChimeEffect,
    pub(super) upload_completion: UploadCompletionEffect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SessionState {
    multiwake: MultiwakeState,
}

impl SessionState {
    const fn multiwake_won(self) -> bool {
        !matches!(self.multiwake, MultiwakeState::Lost)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MultiwakeState {
    Disabled,
    Pending,
    Won,
    Lost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FinishingState {
    pub(super) session: SessionState,
    pub(super) wake_ended: bool,
    pub(super) upload_finished: bool,
}

/// Observable phase of the current recorder session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum RecorderState {
    #[default]
    Idle,
    Listening(SessionState),
    Recording(SessionState),
    Finishing(FinishingState),
}

/// Owns the current phase and applies one event atomically.
pub(super) struct RecorderMachine {
    policy: RecorderPolicy,
    state: RecorderState,
}

impl RecorderMachine {
    pub(super) const fn new(policy: RecorderPolicy) -> Self {
        Self {
            policy,
            state: RecorderState::Idle,
        }
    }

    pub(super) const fn state(&self) -> RecorderState {
        self.state
    }

    pub(super) fn apply(&mut self, event: RecorderEvent) -> RecorderEffects {
        let (state, effects) = match (self.state, event) {
            (RecorderState::Idle, RecorderEvent::WakeDetected { volume_db }) => {
                self.start_wake(volume_db)
            }
            (RecorderState::Listening(session), RecorderEvent::VadStarted) => (
                RecorderState::Recording(session),
                RecorderEffects {
                    upload: UploadEffect::Start,
                    ..RecorderEffects::default()
                },
            ),
            (
                state @ (RecorderState::Listening(_) | RecorderState::Recording(_)),
                RecorderEvent::VadEnded,
            ) => (
                state,
                RecorderEffects {
                    timeout: TimeoutEffect::Cancel,
                    ..RecorderEffects::default()
                },
            ),
            (RecorderState::Listening(session), RecorderEvent::WakeEnded) => (
                RecorderState::Idle,
                wake_end_effects(session, UploadEffect::None),
            ),
            (RecorderState::Recording(session), RecorderEvent::WakeEnded) => (
                RecorderState::Finishing(FinishingState {
                    session,
                    wake_ended: true,
                    upload_finished: false,
                }),
                wake_end_effects(
                    session,
                    UploadEffect::Finish {
                        cause: RecorderStopCause::WakeEnded,
                    },
                ),
            ),
            (RecorderState::Finishing(finishing), RecorderEvent::WakeEnded) => {
                finish_wake(finishing)
            }
            (
                state @ (RecorderState::Listening(_)
                | RecorderState::Recording(_)
                | RecorderState::Finishing(_)),
                RecorderEvent::MultiwakeResult { won },
            ) => self.multiwake_result(state, won),
            (RecorderState::Listening(session), RecorderEvent::StopRequested { cause }) => {
                stop_listening(session, cause)
            }
            (RecorderState::Recording(session), RecorderEvent::StopRequested { cause }) => {
                stop_recording(session, cause)
            }
            (
                RecorderState::Recording(session),
                RecorderEvent::UploadCompleted { has_response },
            ) => upload_stopped_early(session, has_response),
            (
                RecorderState::Finishing(finishing),
                RecorderEvent::UploadCompleted { has_response },
            ) => finish_upload(finishing, has_response),
            (state, _) => (state, RecorderEffects::default()),
        };
        self.state = state;
        effects
    }

    fn start_wake(&self, volume_db: f32) -> (RecorderState, RecorderEffects) {
        let session = SessionState {
            multiwake: if self.policy.multiwake_enabled {
                MultiwakeState::Pending
            } else {
                MultiwakeState::Disabled
            },
        };
        let chime = if !self.policy.multiwake_enabled && self.policy.wake_confirmation {
            ChimeEffect::WakeConfirmation
        } else {
            ChimeEffect::None
        };
        (
            RecorderState::Listening(session),
            RecorderEffects {
                timeout: TimeoutEffect::Arm,
                was: WasEffect::WakeStart { volume_db },
                display: DisplayEffect::Listening,
                chime,
                ..RecorderEffects::default()
            },
        )
    }

    fn multiwake_result(
        &self,
        state: RecorderState,
        won: bool,
    ) -> (RecorderState, RecorderEffects) {
        if !self.policy.multiwake_enabled {
            return (state, RecorderEffects::default());
        }

        let chime = if won && self.policy.wake_confirmation {
            ChimeEffect::WakeConfirmation
        } else {
            ChimeEffect::None
        };
        match state {
            RecorderState::Listening(session) => {
                update_multiwake(session, won, chime, RecorderState::Listening)
            }
            RecorderState::Recording(session) => {
                update_multiwake(session, won, chime, RecorderState::Recording)
            }
            RecorderState::Finishing(mut finishing) => {
                let (session, effects) = update_multiwake_session(finishing.session, won, chime);
                finishing.session = session;
                let effects = if matches!(
                    effects.capture,
                    CaptureEffect::StopSession {
                        cause: RecorderStopCause::MultiwakeLost
                    }
                ) {
                    RecorderEffects {
                        display: DisplayEffect::Thinking {
                            multiwake_won: false,
                        },
                        ..effects
                    }
                } else {
                    effects
                };
                (RecorderState::Finishing(finishing), effects)
            }
            RecorderState::Idle => (state, RecorderEffects::default()),
        }
    }
}

fn wake_end_effects(session: SessionState, upload: UploadEffect) -> RecorderEffects {
    RecorderEffects {
        timeout: TimeoutEffect::Cancel,
        upload,
        was: WasEffect::WakeEnd,
        display: DisplayEffect::Thinking {
            multiwake_won: session.multiwake_won(),
        },
        ..RecorderEffects::default()
    }
}

fn finish_wake(mut finishing: FinishingState) -> (RecorderState, RecorderEffects) {
    if finishing.wake_ended {
        return (
            RecorderState::Finishing(finishing),
            RecorderEffects::default(),
        );
    }
    finishing.wake_ended = true;
    let effects = RecorderEffects {
        was: WasEffect::WakeEnd,
        ..RecorderEffects::default()
    };
    (settle(finishing), effects)
}

fn update_multiwake(
    session: SessionState,
    won: bool,
    chime: ChimeEffect,
    wrap: impl FnOnce(SessionState) -> RecorderState,
) -> (RecorderState, RecorderEffects) {
    let (session, effects) = update_multiwake_session(session, won, chime);
    (wrap(session), effects)
}

fn update_multiwake_session(
    mut session: SessionState,
    won: bool,
    chime: ChimeEffect,
) -> (SessionState, RecorderEffects) {
    match (session.multiwake, won) {
        (MultiwakeState::Pending, true) => {
            session.multiwake = MultiwakeState::Won;
            (
                session,
                RecorderEffects {
                    chime,
                    ..RecorderEffects::default()
                },
            )
        }
        (MultiwakeState::Pending | MultiwakeState::Won, false) => {
            session.multiwake = MultiwakeState::Lost;
            (
                session,
                RecorderEffects {
                    capture: CaptureEffect::StopSession {
                        cause: RecorderStopCause::MultiwakeLost,
                    },
                    ..RecorderEffects::default()
                },
            )
        }
        _ => (session, RecorderEffects::default()),
    }
}

fn stop_listening(
    session: SessionState,
    cause: RecorderStopCause,
) -> (RecorderState, RecorderEffects) {
    (
        RecorderState::Finishing(FinishingState {
            session,
            wake_ended: false,
            upload_finished: true,
        }),
        stop_effects(session, cause, UploadEffect::None),
    )
}

fn stop_recording(
    session: SessionState,
    cause: RecorderStopCause,
) -> (RecorderState, RecorderEffects) {
    (
        RecorderState::Finishing(FinishingState {
            session,
            wake_ended: false,
            upload_finished: false,
        }),
        stop_effects(session, cause, UploadEffect::Finish { cause }),
    )
}

fn stop_effects(
    session: SessionState,
    cause: RecorderStopCause,
    upload: UploadEffect,
) -> RecorderEffects {
    RecorderEffects {
        capture: CaptureEffect::StopSession { cause },
        timeout: TimeoutEffect::Cancel,
        upload,
        display: DisplayEffect::Thinking {
            multiwake_won: session.multiwake_won(),
        },
        ..RecorderEffects::default()
    }
}

fn upload_stopped_early(
    session: SessionState,
    has_response: bool,
) -> (RecorderState, RecorderEffects) {
    (
        RecorderState::Finishing(FinishingState {
            session,
            wake_ended: false,
            upload_finished: true,
        }),
        RecorderEffects {
            capture: CaptureEffect::StopSession {
                cause: RecorderStopCause::UploadFailed,
            },
            timeout: TimeoutEffect::Cancel,
            display: DisplayEffect::Thinking {
                multiwake_won: session.multiwake_won(),
            },
            upload_completion: upload_completion(session, has_response),
            ..RecorderEffects::default()
        },
    )
}

fn finish_upload(
    mut finishing: FinishingState,
    has_response: bool,
) -> (RecorderState, RecorderEffects) {
    if finishing.upload_finished {
        return (
            RecorderState::Finishing(finishing),
            RecorderEffects::default(),
        );
    }
    finishing.upload_finished = true;
    let effects = RecorderEffects {
        upload_completion: upload_completion(finishing.session, has_response),
        ..RecorderEffects::default()
    };
    (settle(finishing), effects)
}

const fn upload_completion(session: SessionState, has_response: bool) -> UploadCompletionEffect {
    if !session.multiwake_won() {
        UploadCompletionEffect::Discard
    } else if has_response {
        UploadCompletionEffect::Process
    } else {
        UploadCompletionEffect::ReportFailure
    }
}

const fn settle(finishing: FinishingState) -> RecorderState {
    if finishing.wake_ended && finishing.upload_finished {
        RecorderState::Idle
    } else {
        RecorderState::Finishing(finishing)
    }
}

#[cfg(test)]
mod tests {
    const CONFIRMING_POLICY: super::RecorderPolicy = super::RecorderPolicy {
        multiwake_enabled: false,
        wake_confirmation: true,
    };
    const MULTIWAKE_POLICY: super::RecorderPolicy = super::RecorderPolicy {
        multiwake_enabled: true,
        wake_confirmation: true,
    };

    #[test]
    fn nominal_wake_records_and_processes_the_upload() {
        let mut recorder = super::RecorderMachine::new(CONFIRMING_POLICY);

        let wake = recorder.apply(super::RecorderEvent::WakeDetected { volume_db: -12.5 });
        assert_eq!(wake.timeout, super::TimeoutEffect::Arm);
        assert_eq!(wake.was, super::WasEffect::WakeStart { volume_db: -12.5 });
        assert_eq!(wake.display, super::DisplayEffect::Listening);
        assert_eq!(wake.chime, super::ChimeEffect::WakeConfirmation);
        assert!(matches!(
            recorder.state(),
            super::RecorderState::Listening(_)
        ));
        assert_eq!(
            recorder.apply(super::RecorderEvent::MultiwakeResult { won: false }),
            super::RecorderEffects::default()
        );

        let start = recorder.apply(super::RecorderEvent::VadStarted);
        assert_eq!(start.upload, super::UploadEffect::Start);
        assert!(matches!(
            recorder.state(),
            super::RecorderState::Recording(_)
        ));

        let vad_end = recorder.apply(super::RecorderEvent::VadEnded);
        assert_eq!(vad_end.timeout, super::TimeoutEffect::Cancel);

        let wake_end = recorder.apply(super::RecorderEvent::WakeEnded);
        assert_eq!(wake_end.was, super::WasEffect::WakeEnd);
        assert_eq!(
            wake_end.upload,
            super::UploadEffect::Finish {
                cause: super::RecorderStopCause::WakeEnded
            }
        );
        assert_eq!(
            wake_end.display,
            super::DisplayEffect::Thinking {
                multiwake_won: true
            }
        );

        let completed =
            recorder.apply(super::RecorderEvent::UploadCompleted { has_response: true });
        assert_eq!(
            completed.upload_completion,
            super::UploadCompletionEffect::Process
        );
        assert_eq!(recorder.state(), super::RecorderState::Idle);
    }

    #[test]
    fn multiwake_loss_stops_capture_and_discards_the_response() {
        let mut recorder = super::RecorderMachine::new(MULTIWAKE_POLICY);
        let wake = recorder.apply(super::RecorderEvent::WakeDetected { volume_db: -4.0 });
        assert_eq!(wake.chime, super::ChimeEffect::None);
        let _ = recorder.apply(super::RecorderEvent::VadStarted);

        let won = recorder.apply(super::RecorderEvent::MultiwakeResult { won: true });
        assert_eq!(won.chime, super::ChimeEffect::WakeConfirmation);
        assert_eq!(
            recorder.apply(super::RecorderEvent::MultiwakeResult { won: true }),
            super::RecorderEffects::default()
        );

        let lost = recorder.apply(super::RecorderEvent::MultiwakeResult { won: false });
        assert_eq!(
            lost.capture,
            super::CaptureEffect::StopSession {
                cause: super::RecorderStopCause::MultiwakeLost
            }
        );
        assert_eq!(lost.upload, super::UploadEffect::None);
        assert_eq!(
            recorder.apply(super::RecorderEvent::MultiwakeResult { won: false }),
            super::RecorderEffects::default()
        );

        let wake_end = recorder.apply(super::RecorderEvent::WakeEnded);
        assert_eq!(
            wake_end.display,
            super::DisplayEffect::Thinking {
                multiwake_won: false
            }
        );
        let completed =
            recorder.apply(super::RecorderEvent::UploadCompleted { has_response: true });
        assert_eq!(
            completed.upload_completion,
            super::UploadCompletionEffect::Discard
        );
        assert_eq!(recorder.state(), super::RecorderState::Idle);
    }

    #[test]
    fn multiwake_loss_also_discards_upload_failures() {
        let mut recorder = super::RecorderMachine::new(MULTIWAKE_POLICY);
        let _ = recorder.apply(super::RecorderEvent::WakeDetected { volume_db: -4.0 });
        let _ = recorder.apply(super::RecorderEvent::VadStarted);
        let _ = recorder.apply(super::RecorderEvent::MultiwakeResult { won: false });
        let _ = recorder.apply(super::RecorderEvent::WakeEnded);

        let completed = recorder.apply(super::RecorderEvent::UploadCompleted {
            has_response: false,
        });
        assert_eq!(
            completed.upload_completion,
            super::UploadCompletionEffect::Discard
        );
        assert_eq!(recorder.state(), super::RecorderState::Idle);
    }

    #[test]
    fn upload_can_complete_before_capture_reports_wake_end() {
        let mut recorder = super::RecorderMachine::new(CONFIRMING_POLICY);
        let _ = recorder.apply(super::RecorderEvent::WakeDetected { volume_db: -1.0 });
        let _ = recorder.apply(super::RecorderEvent::VadStarted);

        let stop = recorder.apply(super::RecorderEvent::StopRequested {
            cause: super::RecorderStopCause::User,
        });
        assert_eq!(
            stop.capture,
            super::CaptureEffect::StopSession {
                cause: super::RecorderStopCause::User
            }
        );
        assert_eq!(
            stop.upload,
            super::UploadEffect::Finish {
                cause: super::RecorderStopCause::User
            }
        );

        let completed =
            recorder.apply(super::RecorderEvent::UploadCompleted { has_response: true });
        assert_eq!(
            completed.upload_completion,
            super::UploadCompletionEffect::Process
        );
        assert!(matches!(
            recorder.state(),
            super::RecorderState::Finishing(_)
        ));

        let wake_end = recorder.apply(super::RecorderEvent::WakeEnded);
        assert_eq!(wake_end.was, super::WasEffect::WakeEnd);
        assert_eq!(wake_end.upload, super::UploadEffect::None);
        assert_eq!(recorder.state(), super::RecorderState::Idle);
    }

    #[test]
    fn stopping_before_vad_does_not_finish_a_missing_upload() {
        let mut recorder = super::RecorderMachine::new(CONFIRMING_POLICY);
        let _ = recorder.apply(super::RecorderEvent::WakeDetected { volume_db: -8.0 });

        let stop = recorder.apply(super::RecorderEvent::StopRequested {
            cause: super::RecorderStopCause::Timeout,
        });
        assert_eq!(stop.upload, super::UploadEffect::None);
        assert!(matches!(
            recorder.state(),
            super::RecorderState::Finishing(_)
        ));

        let wake_end = recorder.apply(super::RecorderEvent::WakeEnded);
        assert_eq!(wake_end.was, super::WasEffect::WakeEnd);
        assert_eq!(recorder.state(), super::RecorderState::Idle);
    }

    #[test]
    fn early_upload_failure_stops_capture_without_processing_a_response() {
        let mut recorder = super::RecorderMachine::new(CONFIRMING_POLICY);
        let _ = recorder.apply(super::RecorderEvent::WakeDetected { volume_db: -3.0 });
        let _ = recorder.apply(super::RecorderEvent::VadStarted);

        let failed = recorder.apply(super::RecorderEvent::UploadCompleted {
            has_response: false,
        });
        assert_eq!(
            failed.capture,
            super::CaptureEffect::StopSession {
                cause: super::RecorderStopCause::UploadFailed
            }
        );
        assert_eq!(
            failed.upload_completion,
            super::UploadCompletionEffect::ReportFailure
        );
        assert!(matches!(
            recorder.state(),
            super::RecorderState::Finishing(_)
        ));

        let wake_end = recorder.apply(super::RecorderEvent::WakeEnded);
        assert_eq!(wake_end.was, super::WasEffect::WakeEnd);
        assert_eq!(recorder.state(), super::RecorderState::Idle);
    }

    #[test]
    fn ambient_vad_and_duplicate_terminal_events_are_ignored() {
        let mut recorder = super::RecorderMachine::new(CONFIRMING_POLICY);

        assert_eq!(
            recorder.apply(super::RecorderEvent::VadStarted),
            super::RecorderEffects::default()
        );
        assert_eq!(
            recorder.apply(super::RecorderEvent::WakeEnded),
            super::RecorderEffects::default()
        );
        assert_eq!(
            recorder.apply(super::RecorderEvent::UploadCompleted {
                has_response: false
            }),
            super::RecorderEffects::default()
        );
        assert_eq!(
            recorder.apply(super::RecorderEvent::MultiwakeResult { won: false }),
            super::RecorderEffects::default()
        );
        assert_eq!(recorder.state(), super::RecorderState::Idle);
    }
}
