//! Active-notification cancellation state.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

struct ActiveNotification {
    cancel: Arc<AtomicBool>,
    id: u64,
    lease: NotificationLease,
}

struct NotificationInner {
    active: Option<ActiveNotification>,
    next_lease: u64,
}

/// Unique ownership token for one notification task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NotificationLease(u64);

/// Result of reserving the notification worker.
pub(crate) struct Activation {
    /// Lease owned by the new task.
    pub(crate) lease: NotificationLease,
    /// Whether an older task was marked for cancellation.
    pub(crate) replaced: bool,
}

/// Result of trying to cancel one notification ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CancelOutcome {
    /// The requested notification was marked for cancellation.
    Cancelled,
    /// A notification is active, but it has another ID.
    DifferentId,
    /// There is no active notification to cancel.
    NoActiveNotification,
}

/// Tracks the notification currently exposed to cancellation requests.
pub(crate) struct NotificationState {
    inner: Mutex<NotificationInner>,
}

impl NotificationState {
    /// Creates empty notification state.
    pub(crate) const fn new() -> Self {
        Self {
            inner: Mutex::new(NotificationInner {
                active: None,
                next_lease: 0,
            }),
        }
    }

    /// Reserves the worker and cancels the previous reservation.
    pub(crate) fn activate(&self, id: u64, cancel: Arc<AtomicBool>) -> Activation {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let replaced = inner.active.is_some();
        if let Some(active) = inner.active.as_ref() {
            active.cancel.store(true, Ordering::Release);
        }

        inner.next_lease = inner.next_lease.wrapping_add(1);
        let lease = NotificationLease(inner.next_lease);
        inner.active = Some(ActiveNotification { cancel, id, lease });
        Activation { lease, replaced }
    }

    /// Marks the matching active notification as cancelled.
    pub(crate) fn cancel(&self, id: u64) -> CancelOutcome {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(active) = inner.active.as_ref() else {
            return CancelOutcome::NoActiveNotification;
        };
        if active.id != id {
            return CancelOutcome::DifferentId;
        }

        active.cancel.store(true, Ordering::Release);
        CancelOutcome::Cancelled
    }

    /// Removes the active notification only when its lease still owns it.
    pub(crate) fn clear(&self, lease: NotificationLease) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !inner
            .active
            .as_ref()
            .is_some_and(|active| active.lease == lease)
        {
            return false;
        }

        inner.active = None;
        true
    }
}

#[cfg(all(test, not(target_os = "espidf")))]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::{CancelOutcome, NotificationState};

    #[test]
    fn cancels_only_the_matching_active_notification() {
        let state = NotificationState::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let activation = state.activate(42, Arc::clone(&cancel));

        assert_eq!(state.cancel(41), CancelOutcome::DifferentId);
        assert!(!cancel.load(Ordering::Acquire));
        assert_eq!(state.cancel(42), CancelOutcome::Cancelled);
        assert!(cancel.load(Ordering::Acquire));
        assert!(state.clear(activation.lease));
    }

    #[test]
    fn replaces_and_conditionally_clears_notification_ownership() {
        let state = NotificationState::new();
        let first_cancel = Arc::new(AtomicBool::new(false));
        let second_cancel = Arc::new(AtomicBool::new(false));

        assert_eq!(state.cancel(42), CancelOutcome::NoActiveNotification);
        let first = state.activate(42, Arc::clone(&first_cancel));
        assert!(!first.replaced);
        let second = state.activate(42, Arc::clone(&second_cancel));
        assert!(second.replaced);
        assert!(first_cancel.load(Ordering::Acquire));
        assert!(!second_cancel.load(Ordering::Acquire));

        assert!(!state.clear(first.lease));
        assert_eq!(state.cancel(42), CancelOutcome::Cancelled);
        assert!(second_cancel.load(Ordering::Acquire));
        assert!(state.clear(second.lease));
        assert_eq!(state.cancel(42), CancelOutcome::NoActiveNotification);
    }
}
