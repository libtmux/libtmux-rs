//! Bounded control queues with cancellation-safe capacity reservations.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_lock::{Semaphore, SemaphoreGuardArc};
use futures_core::Stream;
use futures_util::FutureExt as _;

pub(super) use async_channel::TryRecvError;

type Envelope<T> = (T, SemaphoreGuardArc);

pub(super) fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let (send, receive) = async_channel::bounded(capacity);
    (
        Sender {
            send,
            slots: Arc::new(Semaphore::new(capacity)),
        },
        Receiver {
            receive: Box::pin(receive),
        },
    )
}

#[derive(Debug)]
pub(super) struct Sender<T> {
    send: async_channel::Sender<Envelope<T>>,
    slots: Arc<Semaphore>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            send: self.send.clone(),
            slots: Arc::clone(&self.slots),
        }
    }
}

impl<T> Sender<T> {
    /// Reserve one slot without moving the message into a cancellable future.
    pub(super) async fn reserve(&self) -> Result<Permit<'_, T>, Closed> {
        let slot = futures_util::select_biased! {
            () = self.send.closed().fuse() => return Err(Closed),
            slot = self.slots.acquire_arc().fuse() => slot,
        };
        if self.send.is_closed() {
            return Err(Closed);
        }
        Ok(Permit { sender: self, slot })
    }

    #[cfg(test)]
    pub(super) async fn send(&self, value: T) -> Result<(), T> {
        let Ok(permit) = self.reserve().await else {
            return Err(value);
        };
        permit.send(value)
    }

    pub(super) fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        if self.send.is_closed() {
            return Err(TrySendError::Closed(value));
        }
        let Some(slot) = self.slots.try_acquire_arc() else {
            return Err(TrySendError::Full(value));
        };
        self.send
            .try_send((value, slot))
            .map_err(|error| match error {
                async_channel::TrySendError::Closed((value, _slot)) => TrySendError::Closed(value),
                async_channel::TrySendError::Full((value, _slot)) => TrySendError::Full(value),
            })
    }

    pub(super) fn is_closed(&self) -> bool {
        self.send.is_closed()
    }

    pub(super) fn is_full(&self) -> bool {
        self.send.is_full()
    }

    #[cfg(test)]
    pub(super) async fn closed(&self) {
        self.send.closed().await;
    }
}

pub(super) struct Permit<'a, T> {
    sender: &'a Sender<T>,
    slot: SemaphoreGuardArc,
}

impl<T> Permit<'_, T> {
    /// Publish into the reserved slot, returning the value if the receiver left.
    pub(super) fn send(self, value: T) -> Result<(), T> {
        self.sender
            .send
            .try_send((value, self.slot))
            .map_err(|error| error.into_inner().0)
    }
}

#[derive(Debug)]
pub(super) struct Receiver<T> {
    receive: Pin<Box<async_channel::Receiver<Envelope<T>>>>,
}

impl<T> Receiver<T> {
    pub(super) async fn recv(&mut self) -> Option<T> {
        self.receive.recv().await.ok().map(|(value, _slot)| value)
    }

    pub(super) fn try_recv(&mut self) -> Result<T, TryRecvError> {
        self.receive.try_recv().map(|(value, _slot)| value)
    }

    pub(super) fn poll_recv(&mut self, context: &mut Context<'_>) -> Poll<Option<T>> {
        self.receive
            .as_mut()
            .poll_next(context)
            .map(|value| value.map(|(value, _slot)| value))
    }

    pub(super) fn close(&mut self) {
        self.receive.close();
    }
}

#[derive(Debug)]
pub(super) struct Closed;

#[derive(Debug)]
pub(super) enum TrySendError<T> {
    Full(T),
    Closed(T),
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::{TrySendError, channel};

    #[test]
    fn reservation_drop_returns_exact_capacity_without_publishing() {
        let (sender, mut receiver) = channel(1);
        let mut reserve = pin!(sender.reserve());
        let Poll::Ready(Ok(permit)) = reserve
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("empty queue has room");
        };
        assert!(matches!(sender.try_send(1), Err(TrySendError::Full(1))));
        drop(permit);
        assert!(receiver.try_recv().is_err());
        sender
            .try_send(2)
            .expect("cancelled reservation released its slot");
        assert!(matches!(sender.try_send(3), Err(TrySendError::Full(3))));
        assert_eq!(receiver.try_recv(), Ok(2));
        sender.try_send(3).expect("receiving released its slot");
    }

    #[test]
    fn receiver_close_wakes_a_reservation_blocked_behind_a_full_queue() {
        let (sender, mut receiver) = channel(1);
        sender.try_send(1).expect("empty queue has room");
        let mut reserve = pin!(sender.reserve());
        assert!(
            reserve
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        receiver.close();
        assert!(matches!(
            reserve
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Err(_))
        ));
        assert_eq!(receiver.try_recv(), Ok(1));
    }
}
