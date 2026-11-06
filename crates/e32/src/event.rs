// SPDX-License-Identifier: Apache-2.0
//! Raw input (pointer, keys) straight from the kernel's event queue — the window server's job. Only one process may
//! take it, the one registered as the window server; so this works when nothing else is (e.g. our UI started as
//! `SYSSTART.EXE` instead of `EWSRV.EXE`).

use crate::request::{TRequestStatus, wait_for_request};
use crate::sys::ui::{self, TRawEventBuf};
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

pub use crate::sys::ui::{TRawEvent, raw};

/// The captured event hook. Thread-bound; released on drop.
pub struct EventHook {
    _thread_bound: PhantomData<*const ()>,
}

impl EventHook {
    /// Register this process as the window server and capture the event hook. Panics the process (KERN-EXEC 1)
    /// if another process is already the window server.
    pub fn capture() -> Self {
        ui::ws_register_thread();
        ui::capture_event_hook();
        Self {
            _thread_bound: PhantomData,
        }
    }

    /// The next raw event. Dropping the future while it waits cancels the request; queued events are kept.
    pub fn next(&self) -> NextEvent<'_> {
        NextEvent {
            _hook: self,
            started: false,
            status: TRequestStatus::pending(),
            buf: TRawEventBuf::default(),
        }
    }
}

impl Drop for EventHook {
    fn drop(&mut self) {
        ui::release_event_hook();
    }
}

pub struct NextEvent<'a> {
    _hook: &'a EventHook,
    started: bool,
    status: TRequestStatus,
    buf: TRawEventBuf,
}

impl Future for NextEvent<'_> {
    type Output = Result<TRawEvent, i32>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: nothing is moved out; `status` and `buf` stay put while the request runs (the future is pinned).
        let this = unsafe { self.get_unchecked_mut() };
        if !this.started {
            this.started = true;
            this.status = TRequestStatus::pending();
            // SAFETY: both live inside the pinned future; Drop cancels and waits before they go away.
            unsafe { ui::request_event(&mut this.buf, &mut this.status) };
            return Poll::Pending;
        }
        match this.status.get() {
            crate::sys::KREQUEST_PENDING => Poll::Pending,
            r => {
                this.started = false;
                Poll::Ready(if r < 0 { Err(r) } else { Ok(this.buf.event) })
            }
        }
    }
}

impl Drop for NextEvent<'_> {
    fn drop(&mut self) {
        if self.started && self.status.is_pending() {
            ui::request_event_cancel();
            wait_for_request(&self.status);
        }
    }
}
