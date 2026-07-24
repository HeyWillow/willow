//! Pure timing state for the legacy ADF recorder lifecycle.

use core::time::Duration;

const NO_SPEECH_TIMEOUT: Duration = Duration::from_secs(10);
const SPEECH_CONFIRMATION: Duration = Duration::from_millis(160);

// Willow explicitly overrides ADF's 900 ms default with one millisecond.
const WAKE_END_TIMEOUT: Duration = Duration::from_millis(1);

/// Recorder lifecycle changes produced after debouncing VAD transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TimingEvent {
    VadEnded,
    VadStarted,
    WakeEnded,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum TimingState {
    #[default]
    Idle,
    Speeching,
    WaitForSilence,
    WaitForSleep,
    WaitForSpeech,
    Wakeup,
}

/// Reproduces ADF's recorder timers without owning any hardware or threads.
pub(super) struct RecorderTiming {
    silence_confirmation: Duration,
    state: TimingState,
    vad_deadline: Option<Duration>,
    wake_deadline: Option<Duration>,
}

impl RecorderTiming {
    pub(super) const fn new(silence_confirmation: Duration) -> Self {
        Self {
            silence_confirmation,
            state: TimingState::Idle,
            vad_deadline: None,
            wake_deadline: None,
        }
    }

    /// Starts or restarts the wake lifecycle at `now`.
    ///
    /// ADF resets its internal state when another wake word arrives before the
    /// preceding lifecycle ends, without emitting terminal events for the old
    /// lifecycle. Retaining that behavior lets an active upload continue while
    /// the new wake detection restarts the no-speech window.
    pub(super) fn wake(&mut self, now: Duration) {
        self.state = TimingState::Wakeup;
        self.vad_deadline = None;
        self.wake_deadline = Some(now.saturating_add(NO_SPEECH_TIMEOUT));
    }

    /// Applies one raw ESP-SR VAD state change.
    pub(super) fn vad_changed(&mut self, speech: bool, now: Duration) {
        self.state = match (self.state, speech) {
            (TimingState::Wakeup | TimingState::WaitForSleep, true) => {
                self.vad_deadline = Some(now.saturating_add(SPEECH_CONFIRMATION));
                TimingState::WaitForSpeech
            }
            (TimingState::WaitForSpeech, false) => TimingState::Wakeup,
            (TimingState::Speeching, false) => {
                self.vad_deadline = Some(now.saturating_add(self.silence_confirmation));
                TimingState::WaitForSilence
            }
            (TimingState::WaitForSilence, true) => {
                self.vad_deadline = None;
                TimingState::Speeching
            }
            (state, _) => state,
        };
    }

    /// Emits a lifecycle event when the next ADF-compatible deadline expires.
    pub(super) fn tick(&mut self, now: Duration) -> Option<TimingEvent> {
        let wake_due = self
            .wake_deadline
            .filter(|deadline| *deadline <= now && self.wake_timer_is_relevant());
        let vad_due = self
            .vad_deadline
            .filter(|deadline| *deadline <= now && self.vad_timer_is_relevant());

        if wake_due.is_some_and(|wake| vad_due.is_none_or(|vad| wake <= vad)) {
            self.reset();
            return Some(TimingEvent::WakeEnded);
        }

        vad_due?;
        self.vad_deadline = None;
        match self.state {
            TimingState::WaitForSpeech => {
                self.state = TimingState::Speeching;
                self.wake_deadline = None;
                Some(TimingEvent::VadStarted)
            }
            TimingState::WaitForSilence => {
                self.state = TimingState::WaitForSleep;
                self.wake_deadline = Some(now.saturating_add(WAKE_END_TIMEOUT));
                Some(TimingEvent::VadEnded)
            }
            _ => None,
        }
    }

    /// Ends an active lifecycle after an external stop request.
    pub(super) fn force_end(&mut self) -> Option<TimingEvent> {
        if self.state == TimingState::Idle {
            return None;
        }
        self.reset();
        Some(TimingEvent::WakeEnded)
    }

    fn reset(&mut self) {
        self.state = TimingState::Idle;
        self.vad_deadline = None;
        self.wake_deadline = None;
    }

    const fn vad_timer_is_relevant(&self) -> bool {
        matches!(
            self.state,
            TimingState::WaitForSilence | TimingState::WaitForSpeech
        )
    }

    const fn wake_timer_is_relevant(&self) -> bool {
        matches!(
            self.state,
            TimingState::WaitForSleep | TimingState::WaitForSpeech | TimingState::Wakeup
        )
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the firmware binary disables Cargo's test harness"
)]
mod tests {
    use core::time::Duration;

    const SILENCE: Duration = Duration::from_millis(300);

    #[test]
    fn nominal_speech_uses_all_three_debounce_intervals() {
        let mut timing = super::RecorderTiming::new(SILENCE);
        timing.wake(Duration::ZERO);
        timing.vad_changed(true, Duration::from_millis(100));

        assert_eq!(timing.tick(Duration::from_millis(259)), None);
        assert_eq!(
            timing.tick(Duration::from_millis(260)),
            Some(super::TimingEvent::VadStarted)
        );

        timing.vad_changed(false, Duration::from_millis(300));
        assert_eq!(timing.tick(Duration::from_millis(599)), None);
        assert_eq!(
            timing.tick(Duration::from_millis(600)),
            Some(super::TimingEvent::VadEnded)
        );
        assert_eq!(timing.tick(Duration::from_millis(600)), None);
        assert_eq!(
            timing.tick(Duration::from_millis(601)),
            Some(super::TimingEvent::WakeEnded)
        );
    }

    #[test]
    fn short_speech_burst_does_not_extend_the_no_speech_timeout() {
        let mut timing = super::RecorderTiming::new(SILENCE);
        timing.wake(Duration::ZERO);
        timing.vad_changed(true, Duration::from_secs(1));
        timing.vad_changed(false, Duration::from_millis(1_100));

        assert_eq!(timing.tick(Duration::from_millis(1_160)), None);
        assert_eq!(
            timing.tick(Duration::from_secs(10)),
            Some(super::TimingEvent::WakeEnded)
        );
    }

    #[test]
    fn resumed_speech_cancels_the_pending_silence_event() {
        let mut timing = super::RecorderTiming::new(SILENCE);
        timing.wake(Duration::ZERO);
        timing.vad_changed(true, Duration::ZERO);
        assert_eq!(
            timing.tick(Duration::from_millis(160)),
            Some(super::TimingEvent::VadStarted)
        );
        timing.vad_changed(false, Duration::from_millis(200));
        timing.vad_changed(true, Duration::from_millis(400));

        assert_eq!(timing.tick(Duration::from_millis(500)), None);
    }

    #[test]
    fn pending_wake_end_wins_when_speech_is_not_confirmed_in_time() {
        let mut timing = super::RecorderTiming::new(SILENCE);
        timing.wake(Duration::ZERO);
        timing.vad_changed(true, Duration::ZERO);
        assert_eq!(
            timing.tick(Duration::from_millis(160)),
            Some(super::TimingEvent::VadStarted)
        );
        timing.vad_changed(false, Duration::from_millis(200));
        assert_eq!(
            timing.tick(Duration::from_millis(500)),
            Some(super::TimingEvent::VadEnded)
        );
        timing.vad_changed(true, Duration::from_millis(500));

        assert_eq!(
            timing.tick(Duration::from_millis(501)),
            Some(super::TimingEvent::WakeEnded)
        );
    }

    #[test]
    fn another_wake_restarts_the_adf_lifecycle_without_terminal_events() {
        let mut timing = super::RecorderTiming::new(SILENCE);
        timing.wake(Duration::ZERO);
        timing.vad_changed(true, Duration::ZERO);
        assert_eq!(
            timing.tick(Duration::from_millis(160)),
            Some(super::TimingEvent::VadStarted)
        );

        timing.wake(Duration::from_secs(2));
        assert_eq!(timing.tick(Duration::from_secs(10)), None);
        assert_eq!(
            timing.tick(Duration::from_secs(12)),
            Some(super::TimingEvent::WakeEnded)
        );
    }

    #[test]
    fn forced_end_is_idempotent() {
        let mut timing = super::RecorderTiming::new(SILENCE);
        assert_eq!(timing.force_end(), None);
        timing.wake(Duration::ZERO);
        assert_eq!(timing.force_end(), Some(super::TimingEvent::WakeEnded));
        assert_eq!(timing.force_end(), None);
    }
}
