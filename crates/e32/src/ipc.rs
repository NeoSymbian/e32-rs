// SPDX-License-Identifier: Apache-2.0
//! Client side of Symbian client-server IPC (`RSessionBase`).

use crate::request::{TRequestStatus, wait_for_request};
use crate::sys::{self, TPtrC8, ipc};
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

pub use crate::sys::{TIpcArgs, TVersion};

/// A session with a server. Unsharable: the handle belongs to the thread that connected, so it stays there.
pub struct Session {
    handle: i32,
    _thread_bound: PhantomData<*const ()>,
}

impl Session {
    /// Connect to the server named `name` (ASCII) on the global message pool, sending `version` as
    /// `RSessionBase::CreateSession` does.
    pub fn connect(name: &str, version: TVersion) -> Result<Self, i32> {
        let n = TPtrC8::new(name.as_bytes());
        let h = ipc::session_create(&n, -1);
        if h < 0 {
            return Err(h);
        }
        let s = Self {
            handle: h,
            _thread_bound: PhantomData,
        };
        let mut a = TIpcArgs::new();
        a.int(0, version.packed());
        s.send_receive(ipc::ECONNECT, &a)?; // on error `s` drops and closes the handle
        Ok(s)
    }

    /// Send a message and wait for the reply: `Ok(value >= 0)` or `Err(negative error)`.
    /// Everything `args` points at must stay alive for the call (it does: the call blocks).
    pub fn send_receive(&self, function: i32, args: &TIpcArgs<'_>) -> Result<i32, i32> {
        let mut status = TRequestStatus::pending();
        // SAFETY: `args`, its descriptors and `status` live on the caller's/this frame until the wait returns.
        let mut r = unsafe { ipc::session_send_sync(self.handle, function, args, &mut status) };
        if r == 0 {
            r = wait_for_request(&status);
        }
        if r < 0 { Err(r) } else { Ok(r) }
    }
}

impl Session {
    /// Send a message without blocking: the future completes with the server's reply. The session must have message
    /// slots (`connect` uses the global pool). Dropping the future before the reply blocks until it arrives — a
    /// message cannot be withdrawn and the server may still write into `args`' descriptors.
    pub fn send<'a>(&'a self, function: i32, args: &'a TIpcArgs<'a>) -> SendFuture<'a> {
        SendFuture {
            session: self,
            function,
            args,
            started: false,
            status: TRequestStatus::pending(),
        }
    }
}

pub struct SendFuture<'a> {
    session: &'a Session,
    function: i32,
    args: &'a TIpcArgs<'a>,
    started: bool,
    status: TRequestStatus,
}

impl Future for SendFuture<'_> {
    type Output = Result<i32, i32>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: nothing is moved out; `status` stays put while the message is outstanding (the future is pinned).
        let this = unsafe { self.get_unchecked_mut() };
        if !this.started {
            this.status = TRequestStatus::pending();
            // SAFETY: `args` outlives the future; `status` is inside the pinned future and Drop waits for it.
            let r = unsafe {
                ipc::session_send(
                    this.session.handle,
                    this.function,
                    this.args,
                    &mut this.status,
                )
            };
            if r != 0 {
                return Poll::Ready(Err(r));
            }
            this.started = true;
            return Poll::Pending;
        }
        match this.status.get() {
            sys::KREQUEST_PENDING => Poll::Pending,
            r if r < 0 => Poll::Ready(Err(r)),
            r => Poll::Ready(Ok(r)),
        }
    }
}

impl Drop for SendFuture<'_> {
    fn drop(&mut self) {
        if self.started && self.status.is_pending() {
            wait_for_request(&self.status);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The kernel sends the disconnect message; the server cleans up the session's subsessions.
        sys::handle_close(self.handle);
    }
}
