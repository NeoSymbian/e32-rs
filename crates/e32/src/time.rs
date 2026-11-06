// SPDX-License-Identifier: Apache-2.0
//! Timers (`RTimer`) and the tick counter.

use crate::request::{TRequestStatus, wait_for_request};
use crate::sys;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Nanokernel ticks since boot (`NTickCount`).
pub fn ticks() -> u32 {
    sys::ntick_count()
}

/// A kernel timer. Timer handles are owned by the thread that creates them, so a `Timer` stays on its thread.
pub struct Timer {
    handle: i32,
    _thread_bound: PhantomData<*const ()>,
}

impl Timer {
    pub fn new() -> Result<Self, i32> {
        let h = sys::timer_create();
        if h < 0 {
            Err(h)
        } else {
            Ok(Self {
                handle: h,
                _thread_bound: PhantomData,
            })
        }
    }

    /// Start the timer; `status` completes after `micros` microseconds.
    ///
    /// # Safety
    /// `status` must not move or be dropped until it completes (or [`Timer::cancel`] and a wait).
    pub unsafe fn after(&self, micros: i32, status: &mut TRequestStatus) {
        *status = TRequestStatus::pending();
        // SAFETY: forwarded caller contract.
        unsafe { sys::timer_after(self.handle, status, micros) };
    }

    /// Complete a pending request early with `KErrCancel`.
    pub fn cancel(&self) {
        sys::timer_cancel(self.handle);
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        sys::handle_close(self.handle);
    }
}

/// Block the thread for `micros` microseconds.
pub fn sleep(micros: i32) {
    let t = Timer::new().unwrap_or_else(|e| crate::process::panic("RUST-TIMER", e));
    let mut s = TRequestStatus::pending();
    // SAFETY: `s` lives on this frame until the wait below returns.
    unsafe { t.after(micros, &mut s) };
    wait_for_request(&s);
}

/// A future that completes after `micros` microseconds; for [`crate::rt::block_on`].
pub fn delay(micros: i32) -> Delay {
    Delay {
        micros,
        timer: None,
        status: TRequestStatus::pending(),
    }
}

pub struct Delay {
    micros: i32,
    timer: Option<Timer>,
    status: TRequestStatus,
}

impl Future for Delay {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        // SAFETY: nothing is moved out; `status` stays where it is while the request runs (the future is pinned).
        let this = unsafe { self.get_unchecked_mut() };
        match &this.timer {
            None => {
                let t = Timer::new().unwrap_or_else(|e| crate::process::panic("RUST-TIMER", e));
                // SAFETY: `status` is inside the pinned future; Drop cancels and waits before it goes away.
                unsafe { t.after(this.micros, &mut this.status) };
                this.timer = Some(t);
                Poll::Pending
            }
            Some(_) if this.status.is_pending() => Poll::Pending,
            Some(_) => Poll::Ready(()),
        }
    }
}

impl Drop for Delay {
    fn drop(&mut self) {
        if let Some(t) = &self.timer
            && self.status.is_pending()
        {
            t.cancel();
            wait_for_request(&self.status);
        }
    }
}
