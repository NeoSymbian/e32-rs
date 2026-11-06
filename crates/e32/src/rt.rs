// SPDX-License-Identifier: Apache-2.0
//! A one-thread async runtime, the Rust replacement for Symbian's active scheduler.
//!
//! Every pending future here is waiting on a kernel request (a `TRequestStatus`); each completion signals the
//! thread's request semaphore once. So the executor polls, and when the future is still pending it waits for one
//! signal (`WaitForAnyRequest`) and polls again. A future that is pending without an outstanding request would wait
//! forever — the futures in this crate never are.

use crate::sys;
use core::future::Future;
use core::pin::{Pin, pin};
use core::task::{Context, Poll, Waker};

/// Run `fut` to completion on this thread.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        sys::wait_for_any_request();
    }
}

/// One of two outcomes, from [`select`].
#[derive(Debug, PartialEq, Eq)]
pub enum Either<A, B> {
    Left(A),
    Right(B),
}

/// Wait for whichever of two futures finishes first; the other is dropped (which cancels its kernel request).
pub async fn select<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    core::future::poll_fn(|cx| {
        if let Poll::Ready(v) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(v));
        }
        if let Poll::Ready(v) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(v));
        }
        Poll::Pending
    })
    .await
}

/// Wait for two futures together; outputs in order.
pub async fn join<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    Join {
        a: Slot::Running(a),
        b: Slot::Running(b),
    }
    .await
}

enum Slot<F: Future> {
    Running(F),
    Done(F::Output),
    Taken,
}

impl<F: Future> Slot<F> {
    /// # Safety
    /// `self` must be pinned.
    unsafe fn poll(&mut self, cx: &mut Context<'_>) -> bool {
        if let Slot::Running(f) = self {
            // SAFETY: the slot is pinned (caller contract) and `f` is not moved until it is replaced below.
            if let Poll::Ready(v) = unsafe { Pin::new_unchecked(f) }.poll(cx) {
                *self = Slot::Done(v);
            }
        }
        !matches!(self, Slot::Running(_))
    }
    fn take(&mut self) -> F::Output {
        match core::mem::replace(self, Slot::Taken) {
            Slot::Done(v) => v,
            _ => unreachable!("join polled after completion"),
        }
    }
}

struct Join<A: Future, B: Future> {
    a: Slot<A>,
    b: Slot<B>,
}

impl<A: Future, B: Future> Future for Join<A, B> {
    type Output = (A::Output, B::Output);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: the fields are never moved while running (structural pinning by hand).
        let this = unsafe { self.get_unchecked_mut() };
        // SAFETY: `this` is pinned.
        let (da, db) = unsafe { (this.a.poll(cx), this.b.poll(cx)) };
        if da && db {
            Poll::Ready((this.a.take(), this.b.take()))
        } else {
            Poll::Pending
        }
    }
}
