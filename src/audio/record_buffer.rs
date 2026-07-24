//! Fixed-capacity PCM history and session handoff for recording.

#![allow(
    dead_code,
    reason = "the record buffer remains inactive until Rust owns runtime audio"
)]

use core::{fmt, mem};
use std::{
    collections::TryReserveError,
    sync::{Arc, Condvar, Mutex, MutexGuard},
};

const BYTES_PER_SAMPLE: usize = mem::size_of::<i16>();

/// Identifies one exclusive reader of the rolling PCM history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RecordSessionId(u64);

#[derive(Debug)]
pub(super) enum RecordBufferError {
    Allocate {
        bytes: usize,
        source: TryReserveError,
    },
    InvalidCapacity {
        history_bytes: usize,
        session_backlog_bytes: usize,
    },
    CapacityOverflow {
        history_bytes: usize,
        session_backlog_bytes: usize,
    },
    EmptyDestination,
    OffsetOverflow {
        current: u64,
        samples: usize,
    },
    SessionIdOverflow,
    SessionAlreadyActive {
        active: RecordSessionId,
    },
    SessionInactive {
        requested: RecordSessionId,
        active: Option<RecordSessionId>,
    },
    SessionNotFinished {
        session: RecordSessionId,
    },
    Shutdown,
    InternalInvariant(&'static str),
}

impl fmt::Display for RecordBufferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocate { bytes, source } => {
                write!(
                    formatter,
                    "failed to allocate {bytes}-byte PCM record buffer: {source}"
                )
            }
            Self::InvalidCapacity {
                history_bytes,
                session_backlog_bytes,
            } => write!(
                formatter,
                "PCM record-buffer history must be positive and both capacities must be multiples of {BYTES_PER_SAMPLE} bytes, got history={history_bytes}, session backlog={session_backlog_bytes}"
            ),
            Self::CapacityOverflow {
                history_bytes,
                session_backlog_bytes,
            } => write!(
                formatter,
                "PCM record-buffer capacity overflows: history={history_bytes}, session backlog={session_backlog_bytes} bytes"
            ),
            Self::EmptyDestination => {
                formatter.write_str("PCM record-buffer read destination is empty")
            }
            Self::OffsetOverflow { current, samples } => write!(
                formatter,
                "PCM record-buffer offset {current} overflows after {samples} samples"
            ),
            Self::SessionIdOverflow => {
                formatter.write_str("PCM record-buffer session identifier overflowed")
            }
            Self::SessionAlreadyActive { active } => write!(
                formatter,
                "PCM record-buffer session {active:?} is already active"
            ),
            Self::SessionInactive { requested, active } => write!(
                formatter,
                "PCM record-buffer session {requested:?} is inactive; active session is {active:?}"
            ),
            Self::SessionNotFinished { session } => write!(
                formatter,
                "PCM record-buffer session {session:?} has unread or unfinished audio"
            ),
            Self::Shutdown => formatter.write_str("PCM record buffer is shutting down"),
            Self::InternalInvariant(reason) => {
                write!(formatter, "PCM record-buffer invariant failed: {reason}")
            }
        }
    }
}

impl std::error::Error for RecordBufferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocate { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Samples copied by one blocking session read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RecordRead {
    pub(super) samples: usize,
    pub(super) end_of_session: bool,
}

/// Cloneable access to one fixed allocation shared by capture and upload.
#[derive(Clone)]
pub(super) struct RecordBuffer {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    samples: Vec<i16>,
    history_capacity: usize,
    head: usize,
    length: usize,
    start_offset: u64,
    next_offset: u64,
    next_session_id: u64,
    active: Option<ActiveSession>,
    shutdown: bool,
}

#[derive(Clone, Copy)]
struct ActiveSession {
    id: RecordSessionId,
    cursor: u64,
    finish: Option<u64>,
    dropped_samples: u64,
}

impl RecordBuffer {
    pub(super) fn new(
        history_bytes: usize,
        session_backlog_bytes: usize,
    ) -> Result<Self, RecordBufferError> {
        if history_bytes == 0
            || history_bytes % BYTES_PER_SAMPLE != 0
            || session_backlog_bytes % BYTES_PER_SAMPLE != 0
        {
            return Err(RecordBufferError::InvalidCapacity {
                history_bytes,
                session_backlog_bytes,
            });
        }

        let capacity_bytes = history_bytes.checked_add(session_backlog_bytes).ok_or(
            RecordBufferError::CapacityOverflow {
                history_bytes,
                session_backlog_bytes,
            },
        )?;
        let capacity_samples = capacity_bytes / BYTES_PER_SAMPLE;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(capacity_samples)
            .map_err(|source| RecordBufferError::Allocate {
                bytes: capacity_bytes,
                source,
            })?;
        samples.resize(capacity_samples, 0);

        Ok(Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    samples,
                    history_capacity: history_bytes / BYTES_PER_SAMPLE,
                    head: 0,
                    length: 0,
                    start_offset: 0,
                    next_offset: 0,
                    next_session_id: 1,
                    active: None,
                    shutdown: false,
                }),
                changed: Condvar::new(),
            }),
        })
    }

    pub(super) fn write(&self, source: &[i16]) -> Result<(), RecordBufferError> {
        if source.is_empty() {
            return Ok(());
        }

        let mut state = self.lock();
        if state.shutdown {
            return Err(RecordBufferError::Shutdown);
        }
        let next_offset = state.next_offset.checked_add(source.len() as u64).ok_or(
            RecordBufferError::OffsetOverflow {
                current: state.next_offset,
                samples: source.len(),
            },
        )?;
        let capacity = state.retention_capacity();
        if source.len() >= capacity {
            let retained = &source[source.len() - capacity..];
            let new_start = next_offset - capacity as u64;
            state.advance_active_for_overwrite(new_start);
            state.samples[..capacity].copy_from_slice(retained);
            state.head = 0;
            state.length = capacity;
            state.start_offset = new_start;
        } else {
            let displaced = state
                .length
                .checked_add(source.len())
                .ok_or(RecordBufferError::InternalInvariant(
                    "resident sample count overflows",
                ))?
                .saturating_sub(capacity);
            let new_start = state.start_offset.checked_add(displaced as u64).ok_or(
                RecordBufferError::OffsetOverflow {
                    current: state.start_offset,
                    samples: displaced,
                },
            )?;
            state.advance_active_for_overwrite(new_start);
            state.discard_oldest(displaced)?;
            state.append(source)?;
        }
        state.next_offset = next_offset;
        drop(state);
        self.shared.changed.notify_all();

        Ok(())
    }

    pub(super) fn begin_session(&self) -> Result<RecordSessionId, RecordBufferError> {
        let mut state = self.lock();
        if state.shutdown {
            return Err(RecordBufferError::Shutdown);
        }
        if let Some(active) = state.active {
            return Err(RecordBufferError::SessionAlreadyActive { active: active.id });
        }

        let id = RecordSessionId(state.next_session_id);
        state.next_session_id = state
            .next_session_id
            .checked_add(1)
            .ok_or(RecordBufferError::SessionIdOverflow)?;
        state.active = Some(ActiveSession {
            id,
            cursor: state.start_offset,
            finish: None,
            dropped_samples: 0,
        });
        Ok(id)
    }

    pub(super) fn finish_session(&self, session: RecordSessionId) -> Result<(), RecordBufferError> {
        let mut state = self.lock();
        validate_session(&state, session)?;
        let finish = state.next_offset;
        if let Some(active) = state.active.as_mut() {
            active.finish.get_or_insert(finish);
        }
        drop(state);
        self.shared.changed.notify_all();
        Ok(())
    }

    pub(super) fn read_session(
        &self,
        session: RecordSessionId,
        destination: &mut [i16],
    ) -> Result<RecordRead, RecordBufferError> {
        if destination.is_empty() {
            return Err(RecordBufferError::EmptyDestination);
        }

        let mut state = self.lock();
        loop {
            validate_session(&state, session)?;
            let active = state.active.ok_or(RecordBufferError::InternalInvariant(
                "validated session disappeared",
            ))?;
            let boundary = active.finish.unwrap_or(state.next_offset);
            let available = boundary.saturating_sub(active.cursor);
            if available > 0 {
                let count = destination.len().min(u64_to_usize(
                    available,
                    "readable sample count does not fit usize",
                )?);
                state.copy_oldest(&mut destination[..count])?;
                let cursor = active.cursor.checked_add(count as u64).ok_or(
                    RecordBufferError::OffsetOverflow {
                        current: active.cursor,
                        samples: count,
                    },
                )?;
                if let Some(active) = state.active.as_mut() {
                    active.cursor = cursor;
                }
                state.discard_oldest(count)?;
                return Ok(RecordRead {
                    samples: count,
                    end_of_session: active.finish.is_some_and(|finish| cursor >= finish),
                });
            }
            if let Some(finish) = active.finish
                && active.cursor >= finish
            {
                return Ok(RecordRead {
                    samples: 0,
                    end_of_session: true,
                });
            }
            if state.shutdown {
                return Err(RecordBufferError::Shutdown);
            }
            state = self.wait(state);
        }
    }

    pub(super) fn complete_session(
        &self,
        session: RecordSessionId,
    ) -> Result<u64, RecordBufferError> {
        let mut state = self.lock();
        validate_session(&state, session)?;
        let active = state.active.ok_or(RecordBufferError::InternalInvariant(
            "validated session disappeared",
        ))?;
        if active.finish.is_none_or(|finish| active.cursor < finish) {
            return Err(RecordBufferError::SessionNotFinished { session });
        }
        let dropped_samples = active.dropped_samples;
        state.active = None;
        state.trim_idle_history()?;
        Ok(dropped_samples)
    }

    pub(super) fn abort_session(&self, session: RecordSessionId) -> Result<u64, RecordBufferError> {
        let mut state = self.lock();
        validate_session(&state, session)?;
        let active = state.active.ok_or(RecordBufferError::InternalInvariant(
            "validated session disappeared",
        ))?;
        let boundary = active.finish.unwrap_or(state.next_offset);
        let discard = boundary
            .saturating_sub(state.start_offset)
            .min(state.length as u64);
        state.discard_oldest(u64_to_usize(
            discard,
            "aborted session length does not fit usize",
        )?)?;
        let dropped_samples = active.dropped_samples;
        state.active = None;
        state.trim_idle_history()?;
        Ok(dropped_samples)
    }

    pub(super) fn shutdown(&self) {
        let mut state = self.lock();
        state.shutdown = true;
        drop(state);
        self.shared.changed.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        match self.shared.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn wait<'state>(&self, state: MutexGuard<'state, State>) -> MutexGuard<'state, State> {
        match self.shared.changed.wait(state) {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl State {
    fn capacity(&self) -> usize {
        self.samples.len()
    }

    fn retention_capacity(&self) -> usize {
        if self.active.is_some() {
            self.capacity()
        } else {
            self.history_capacity
        }
    }

    fn trim_idle_history(&mut self) -> Result<(), RecordBufferError> {
        let discard = self.length.saturating_sub(self.history_capacity);
        self.discard_oldest(discard)
    }

    fn append(&mut self, source: &[i16]) -> Result<(), RecordBufferError> {
        if source.len() > self.capacity().saturating_sub(self.length) {
            return Err(RecordBufferError::InternalInvariant(
                "append exceeds free record-buffer capacity",
            ));
        }
        let tail = (self.head + self.length) % self.capacity();
        let first = source.len().min(self.capacity() - tail);
        self.samples[tail..tail + first].copy_from_slice(&source[..first]);
        self.samples[..source.len() - first].copy_from_slice(&source[first..]);
        self.length += source.len();
        Ok(())
    }

    fn copy_oldest(&self, destination: &mut [i16]) -> Result<(), RecordBufferError> {
        let length = destination.len();
        if length > self.length {
            return Err(RecordBufferError::InternalInvariant(
                "read exceeds resident record-buffer samples",
            ));
        }
        let first = length.min(self.capacity() - self.head);
        destination[..first].copy_from_slice(&self.samples[self.head..self.head + first]);
        destination[first..].copy_from_slice(&self.samples[..length - first]);
        Ok(())
    }

    fn discard_oldest(&mut self, samples: usize) -> Result<(), RecordBufferError> {
        if samples > self.length {
            return Err(RecordBufferError::InternalInvariant(
                "discard exceeds resident record-buffer samples",
            ));
        }
        self.head = (self.head + samples) % self.capacity();
        self.length -= samples;
        self.start_offset = self.start_offset.checked_add(samples as u64).ok_or(
            RecordBufferError::OffsetOverflow {
                current: self.start_offset,
                samples,
            },
        )?;
        if self.length == 0 {
            self.head = 0;
        }
        Ok(())
    }

    fn advance_active_for_overwrite(&mut self, new_start: u64) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let session_boundary = active.finish.unwrap_or(new_start);
        let advanced_cursor = new_start.min(session_boundary).max(active.cursor);
        let dropped = advanced_cursor - active.cursor;
        active.cursor = advanced_cursor;
        active.dropped_samples = active.dropped_samples.saturating_add(dropped);
    }
}

fn validate_session(state: &State, requested: RecordSessionId) -> Result<(), RecordBufferError> {
    let active = state.active.map(|active| active.id);
    if active == Some(requested) {
        Ok(())
    } else {
        Err(RecordBufferError::SessionInactive { requested, active })
    }
}

fn u64_to_usize(value: u64, reason: &'static str) -> Result<usize, RecordBufferError> {
    usize::try_from(value).map_err(|_| RecordBufferError::InternalInvariant(reason))
}

#[cfg(test)]
mod tests {
    fn buffer(samples: usize) -> super::RecordBuffer {
        buffer_with_backlog(samples, 0)
    }

    fn buffer_with_backlog(
        history_samples: usize,
        session_backlog_samples: usize,
    ) -> super::RecordBuffer {
        super::RecordBuffer::new(
            history_samples * core::mem::size_of::<i16>(),
            session_backlog_samples * core::mem::size_of::<i16>(),
        )
        .expect("test record buffer should allocate")
    }

    fn finish_and_read(
        buffer: &super::RecordBuffer,
        session: super::RecordSessionId,
        destination: &mut [i16],
    ) -> super::RecordRead {
        buffer
            .finish_session(session)
            .expect("test session should finish");
        buffer
            .read_session(session, destination)
            .expect("test session should read")
    }

    #[test]
    fn idle_history_keeps_only_the_newest_samples() {
        let buffer = buffer(4);
        buffer.write(&[1, 2, 3, 4, 5]).expect("write should fit");

        let session = buffer.begin_session().expect("session should start");
        let mut samples = [0_i16; 4];
        let read = finish_and_read(&buffer, session, &mut samples);
        assert_eq!(read.samples, 4);
        assert!(read.end_of_session);
        assert_eq!(samples, [2, 3, 4, 5]);
    }

    #[test]
    fn session_backlog_does_not_expand_idle_history() {
        let buffer = buffer_with_backlog(4, 4);
        buffer
            .write(&[1, 2, 3, 4, 5, 6])
            .expect("idle history should write");

        let session = buffer.begin_session().expect("session should start");
        let mut samples = [0_i16; 4];
        let read = finish_and_read(&buffer, session, &mut samples);
        assert_eq!(read.samples, 4);
        assert_eq!(samples, [3, 4, 5, 6]);
    }

    #[test]
    fn active_reader_drains_preroll_and_live_audio() {
        let buffer = buffer(8);
        buffer.write(&[1, 2]).expect("pre-roll should write");
        let session = buffer.begin_session().expect("session should start");
        buffer.write(&[3, 4]).expect("live audio should write");

        let mut samples = [0_i16; 4];
        let read = finish_and_read(&buffer, session, &mut samples);
        assert_eq!(read.samples, 4);
        assert!(read.end_of_session);
        assert_eq!(samples, [1, 2, 3, 4]);
        buffer
            .complete_session(session)
            .expect("drained session should complete");
    }

    #[test]
    fn active_session_uses_backlog_beyond_the_idle_history() {
        let buffer = buffer_with_backlog(4, 4);
        buffer
            .write(&[1, 2, 3, 4])
            .expect("full pre-roll should write");
        let session = buffer.begin_session().expect("session should start");
        buffer
            .write(&[5, 6, 7, 8])
            .expect("live backlog should write");

        let mut samples = [0_i16; 8];
        let read = finish_and_read(&buffer, session, &mut samples);
        assert_eq!(read.samples, 8);
        assert_eq!(samples, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn finish_boundary_retains_later_audio_for_the_next_session() {
        let buffer = buffer(8);
        buffer.write(&[1, 2]).expect("pre-roll should write");
        let first = buffer.begin_session().expect("session should start");
        buffer.finish_session(first).expect("session should finish");
        buffer.write(&[3, 4]).expect("later audio should write");

        let mut first_samples = [0_i16; 4];
        let first_read = buffer
            .read_session(first, &mut first_samples)
            .expect("first session should read");
        assert_eq!(first_read.samples, 2);
        assert!(first_read.end_of_session);
        assert_eq!(&first_samples[..2], &[1, 2]);
        buffer
            .complete_session(first)
            .expect("first session should complete");

        let second = buffer.begin_session().expect("next session should start");
        let mut second_samples = [0_i16; 2];
        let second_read = finish_and_read(&buffer, second, &mut second_samples);
        assert_eq!(second_read.samples, 2);
        assert_eq!(second_samples, [3, 4]);
    }

    #[test]
    fn completion_trims_post_session_audio_to_idle_history() {
        let buffer = buffer_with_backlog(2, 4);
        buffer.write(&[1, 2]).expect("session audio should write");
        let first = buffer.begin_session().expect("session should start");
        buffer.finish_session(first).expect("session should finish");
        buffer
            .write(&[3, 4, 5, 6])
            .expect("post-session audio should use backlog");

        let mut first_samples = [0_i16; 2];
        let first_read = buffer
            .read_session(first, &mut first_samples)
            .expect("first session should read");
        assert_eq!(first_read.samples, 2);
        buffer
            .complete_session(first)
            .expect("first session should complete");

        let second = buffer.begin_session().expect("next session should start");
        let mut second_samples = [0_i16; 2];
        let second_read = finish_and_read(&buffer, second, &mut second_samples);
        assert_eq!(second_read.samples, 2);
        assert_eq!(second_samples, [5, 6]);
    }

    #[test]
    fn active_overwrite_reports_exact_lost_samples() {
        let buffer = buffer(4);
        buffer.write(&[1, 2]).expect("pre-roll should write");
        let session = buffer.begin_session().expect("session should start");
        buffer.write(&[3, 4, 5]).expect("live audio should write");

        let mut samples = [0_i16; 4];
        let read = finish_and_read(&buffer, session, &mut samples);
        assert_eq!(read.samples, 4);
        assert_eq!(samples, [2, 3, 4, 5]);
        let dropped = buffer
            .complete_session(session)
            .expect("drained session should complete");
        assert_eq!(dropped, 1);
    }

    #[test]
    fn abort_reports_aggregate_lost_samples() {
        let buffer = buffer(4);
        buffer.write(&[1, 2]).expect("pre-roll should write");
        let failed = buffer.begin_session().expect("session should start");
        buffer.write(&[3, 4, 5]).expect("live audio should write");
        buffer.write(&[6, 7]).expect("more live audio should write");

        let dropped = buffer
            .abort_session(failed)
            .expect("active session should abort");
        assert_eq!(dropped, 3);
    }

    #[test]
    fn abort_discards_the_failed_session_audio() {
        let buffer = buffer(4);
        buffer.write(&[1, 2]).expect("pre-roll should write");
        let failed = buffer.begin_session().expect("session should start");
        buffer
            .abort_session(failed)
            .expect("active session should abort");
        buffer.write(&[3]).expect("new pre-roll should write");

        let next = buffer.begin_session().expect("next session should start");
        let mut samples = [0_i16; 2];
        let read = finish_and_read(&buffer, next, &mut samples);
        assert_eq!(read.samples, 1);
        assert_eq!(samples[0], 3);
    }

    #[test]
    fn abort_after_finish_retains_the_next_sessions_preroll() {
        let buffer = buffer(8);
        buffer.write(&[1, 2]).expect("session audio should write");
        let failed = buffer.begin_session().expect("session should start");
        buffer
            .finish_session(failed)
            .expect("failed session should have a boundary");
        buffer.write(&[3, 4]).expect("later audio should write");
        buffer
            .abort_session(failed)
            .expect("failed session should abort");

        let next = buffer.begin_session().expect("next session should start");
        let mut samples = [0_i16; 2];
        let read = finish_and_read(&buffer, next, &mut samples);
        assert_eq!(read.samples, 2);
        assert_eq!(samples, [3, 4]);
    }

    #[test]
    fn stale_session_cannot_finish_a_new_reader() {
        let buffer = buffer(4);
        let first = buffer.begin_session().expect("first session should start");
        buffer
            .finish_session(first)
            .expect("first session should finish");
        let mut sample = [0_i16; 1];
        let _ = buffer
            .read_session(first, &mut sample)
            .expect("empty first session should end");
        buffer
            .complete_session(first)
            .expect("first session should complete");
        let second = buffer.begin_session().expect("second session should start");

        assert!(matches!(
            buffer.finish_session(first),
            Err(super::RecordBufferError::SessionInactive {
                requested,
                active: Some(active)
            }) if requested == first && active == second
        ));
    }
}
