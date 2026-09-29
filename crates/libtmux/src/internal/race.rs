//! Racing futures without `tokio::select!`.
//!
//! `select!`, `join!` and `try_join!` are gated on tokio's `macros` feature,
//! which compiles `tokio-macros` and, with it, `syn`. Each combinator here is
//! `std::future::poll_fn` over pinned futures. Branches are polled in order,
//! so a tie goes to the earlier branch where `select!` picks at random.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

pub(crate) enum Either<A, B> {
    First(A),
    Second(B),
}

pub(crate) enum Either3<A, B, C> {
    First(A),
    Second(B),
    Third(C),
}

/// Complete with whichever future finishes first, dropping the other.
pub(crate) async fn race<A: Future, B: Future>(
    first: A,
    second: B,
) -> Either<A::Output, B::Output> {
    let mut first = pin!(first);
    let mut second = pin!(second);
    poll_fn(|context| {
        if let Poll::Ready(value) = first.as_mut().poll(context) {
            return Poll::Ready(Either::First(value));
        }
        if let Poll::Ready(value) = second.as_mut().poll(context) {
            return Poll::Ready(Either::Second(value));
        }
        Poll::Pending
    })
    .await
}

/// [`race`] over three futures.
pub(crate) async fn race3<A: Future, B: Future, C: Future>(
    first: A,
    second: B,
    third: C,
) -> Either3<A::Output, B::Output, C::Output> {
    let mut first = pin!(first);
    let mut second = pin!(second);
    let mut third = pin!(third);
    poll_fn(|context| {
        if let Poll::Ready(value) = first.as_mut().poll(context) {
            return Poll::Ready(Either3::First(value));
        }
        if let Poll::Ready(value) = second.as_mut().poll(context) {
            return Poll::Ready(Either3::Second(value));
        }
        if let Poll::Ready(value) = third.as_mut().poll(context) {
            return Poll::Ready(Either3::Third(value));
        }
        Poll::Pending
    })
    .await
}

/// Run three fallible futures together; the first error wins and drops the rest.
pub(crate) async fn try_join3<A, B, C, T, U, V, E>(
    first: A,
    second: B,
    third: C,
) -> Result<(T, U, V), E>
where
    A: Future<Output = Result<T, E>>,
    B: Future<Output = Result<U, E>>,
    C: Future<Output = Result<V, E>>,
{
    let mut first = pin!(first);
    let mut second = pin!(second);
    let mut third = pin!(third);
    let (mut done_first, mut done_second, mut done_third) = (None, None, None);
    poll_fn(|context| {
        if done_first.is_none() {
            if let Poll::Ready(result) = first.as_mut().poll(context) {
                match result {
                    Ok(value) => done_first = Some(value),
                    Err(error) => return Poll::Ready(Err(error)),
                }
            }
        }
        if done_second.is_none() {
            if let Poll::Ready(result) = second.as_mut().poll(context) {
                match result {
                    Ok(value) => done_second = Some(value),
                    Err(error) => return Poll::Ready(Err(error)),
                }
            }
        }
        if done_third.is_none() {
            if let Poll::Ready(result) = third.as_mut().poll(context) {
                match result {
                    Ok(value) => done_third = Some(value),
                    Err(error) => return Poll::Ready(Err(error)),
                }
            }
        }
        if done_first.is_some() && done_second.is_some() && done_third.is_some() {
            if let (Some(first), Some(second), Some(third)) =
                (done_first.take(), done_second.take(), done_third.take())
            {
                return Poll::Ready(Ok((first, second, third)));
            }
        }
        Poll::Pending
    })
    .await
}
