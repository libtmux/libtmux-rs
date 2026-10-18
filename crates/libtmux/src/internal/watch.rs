//! A versioned latest-value channel for supervisor cancellation.

use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard};

use event_listener::Event;

#[derive(Debug)]
struct State<T> {
    value: T,
    version: u64,
    senders: usize,
    receivers: usize,
}

#[derive(Debug)]
struct Shared<T> {
    state: Mutex<State<T>>,
    changed: Event,
    closed: Event,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Publish replacements of one value without queuing obsolete values.
pub(crate) fn channel<T>(value: T) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            value,
            version: 0,
            senders: 1,
            receivers: 1,
        }),
        changed: Event::new(),
        closed: Event::new(),
    });
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared, seen: 0 },
    )
}

/// The sending side of a latest-value channel.
#[derive(Debug)]
pub(crate) struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.lock().senders += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Sender<T> {
    /// Replace the value if at least one receiver remains.
    pub(crate) fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut state = self.shared.lock();
        if state.receivers == 0 {
            return Err(SendError(value));
        }
        state.value = value;
        state.version = state.version.wrapping_add(1);
        drop(state);
        self.shared.changed.notify(usize::MAX);
        Ok(())
    }

    /// Replace the value even when no receiver remains, returning the old one.
    #[cfg(any(test, feature = "control-mode"))]
    pub(crate) fn send_replace(&self, value: T) -> T {
        let mut previous = value;
        self.send_modify(|current| std::mem::swap(current, &mut previous));
        previous
    }

    /// Modify the value and publish a change even if its value compares equal.
    #[cfg(any(test, feature = "control-mode"))]
    pub(crate) fn send_modify(&self, modify: impl FnOnce(&mut T)) {
        let mut state = self.shared.lock();
        modify(&mut state.value);
        state.version = state.version.wrapping_add(1);
        drop(state);
        self.shared.changed.notify(usize::MAX);
    }

    /// Subscribe at the current version; subsequent changes are unread.
    #[cfg(any(test, feature = "control-mode"))]
    pub(crate) fn subscribe(&self) -> Receiver<T> {
        let mut state = self.shared.lock();
        state.receivers += 1;
        Receiver {
            shared: Arc::clone(&self.shared),
            seen: state.version,
        }
    }

    /// Borrow the current value without changing any receiver's version.
    #[cfg(test)]
    pub(crate) fn borrow(&self) -> Ref<'_, T> {
        Ref(self.shared.lock())
    }

    /// Wait until no receiver remains. Dropping this wait changes nothing.
    #[cfg(test)]
    pub(crate) async fn closed(&self) {
        loop {
            let closed = self.shared.closed.listen();
            if self.shared.lock().receivers == 0 {
                return;
            }
            closed.await;
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.senders -= 1;
        let last = state.senders == 0;
        drop(state);
        if last {
            self.shared.changed.notify(usize::MAX);
        }
    }
}

/// One subscriber's version of a latest-value channel.
#[derive(Debug)]
pub(crate) struct Receiver<T> {
    shared: Arc<Shared<T>>,
    seen: u64,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.lock().receivers += 1;
        Self {
            shared: Arc::clone(&self.shared),
            seen: self.seen,
        }
    }
}

impl<T> Receiver<T> {
    /// Borrow without marking the current version as read.
    pub(crate) fn borrow(&self) -> Ref<'_, T> {
        Ref(self.shared.lock())
    }

    /// Borrow and mark this version as read before releasing the value lock.
    #[cfg(any(test, feature = "control-mode"))]
    pub(crate) fn borrow_and_update(&mut self) -> Ref<'_, T> {
        let state = self.shared.lock();
        self.seen = state.version;
        Ref(state)
    }

    /// Wait for an unread version, returning closure after the last one.
    ///
    /// Cancelling a pending wait does not mark any version as read. A final
    /// unread value is delivered before closure, even if all senders left.
    pub(crate) async fn changed(&mut self) -> Result<(), RecvError> {
        loop {
            let changed = self.shared.changed.listen();
            {
                let state = self.shared.lock();
                if self.seen != state.version {
                    self.seen = state.version;
                    return Ok(());
                }
                if state.senders == 0 {
                    return Err(RecvError);
                }
            }
            changed.await;
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.receivers -= 1;
        let last = state.receivers == 0;
        drop(state);
        if last {
            self.shared.closed.notify(usize::MAX);
        }
    }
}

/// A borrowed value; release it before awaiting a channel operation.
#[derive(Debug)]
pub(crate) struct Ref<'a, T>(MutexGuard<'a, State<T>>);

impl<T> Deref for Ref<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.value
    }
}

/// No receiver remained to accept this value.
#[derive(Debug)]
pub(crate) struct SendError<T>(pub(crate) T);

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("watch channel has no receivers")
    }
}

/// Every sender has closed and the last published value has been read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("watch channel has no senders")
    }
}

impl std::error::Error for RecvError {}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use super::{RecvError, channel};

    fn poll<F: Future>(future: &mut std::pin::Pin<&mut F>) -> Poll<F::Output> {
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    }

    #[derive(Default)]
    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn publications_and_last_handle_drops_wake_registered_waiters() {
        let wake_count = Arc::new(WakeCount::default());
        let waker = Waker::from(Arc::clone(&wake_count));
        let mut context = Context::from_waker(&waker);
        let (sender, mut receiver) = channel(0);
        {
            let mut changed = pin!(receiver.changed());
            assert!(changed.as_mut().poll(&mut context).is_pending());
            sender.send_replace(1);
            assert_eq!(wake_count.0.load(Ordering::SeqCst), 1);
            assert_eq!(changed.as_mut().poll(&mut context), Poll::Ready(Ok(())));
        }
        {
            let mut changed = pin!(receiver.changed());
            assert!(changed.as_mut().poll(&mut context).is_pending());
            drop(sender);
            assert_eq!(wake_count.0.load(Ordering::SeqCst), 2);
            assert_eq!(
                changed.as_mut().poll(&mut context),
                Poll::Ready(Err(RecvError))
            );
        }
        let (sender, receiver) = channel(());
        let mut closed = pin!(sender.closed());
        assert!(closed.as_mut().poll(&mut context).is_pending());
        drop(receiver);
        assert_eq!(wake_count.0.load(Ordering::SeqCst), 3);
        assert_eq!(closed.as_mut().poll(&mut context), Poll::Ready(()));
    }

    #[test]
    fn cancellation_preserves_unseen_replacements_and_final_value() {
        let (sender, mut receiver) = channel(0);
        {
            let mut wait = pin!(receiver.changed());
            assert!(poll(&mut wait).is_pending());
        }
        sender.send_replace(1);
        sender.send_replace(2);
        assert_eq!(*receiver.borrow(), 2);
        assert_eq!(poll(&mut pin!(receiver.changed())), Poll::Ready(Ok(())));
        assert!(poll(&mut pin!(receiver.changed())).is_pending());
        sender.send_replace(3);
        drop(sender);
        assert_eq!(poll(&mut pin!(receiver.changed())), Poll::Ready(Ok(())));
        assert_eq!(*receiver.borrow(), 3);
        assert_eq!(
            poll(&mut pin!(receiver.changed())),
            Poll::Ready(Err(RecvError))
        );
    }

    #[test]
    fn generations_survive_equal_values_and_receiver_clones() {
        let (sender, mut receiver) = channel(false);
        sender.send_replace(false);
        let mut cloned = receiver.clone();
        let mut subscribed = sender.subscribe();
        assert_eq!(poll(&mut pin!(receiver.changed())), Poll::Ready(Ok(())));
        assert_eq!(poll(&mut pin!(cloned.changed())), Poll::Ready(Ok(())));
        assert!(poll(&mut pin!(subscribed.changed())).is_pending());
        sender.send_modify(|value| *value = true);
        assert!(*subscribed.borrow_and_update());
        assert!(poll(&mut pin!(subscribed.changed())).is_pending());
        assert_eq!(poll(&mut pin!(receiver.changed())), Poll::Ready(Ok(())));
    }

    #[test]
    fn last_receiver_wakes_closed_and_subscribe_reads_retained_value() {
        let (sender, receiver) = channel(1);
        let other = receiver.clone();
        let mut closed = pin!(sender.closed());
        assert!(poll(&mut closed).is_pending());
        drop(receiver);
        assert!(poll(&mut closed).is_pending());
        drop(other);
        assert_eq!(poll(&mut closed), Poll::Ready(()));
        assert!(sender.send(2).is_err());
        assert_eq!(*sender.borrow(), 1);
        assert_eq!(sender.send_replace(3), 1);
        assert_eq!(*sender.subscribe().borrow(), 3);
    }
}
