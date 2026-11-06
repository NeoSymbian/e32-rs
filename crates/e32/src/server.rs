// SPDX-License-Identifier: Apache-2.0
//! Server side of Symbian client-server IPC: the Rust `CServer2`.
//!
//! A [`Server`] is a named kernel server object; [`Server::receive`] is a future yielding the next [`Message`]. For
//! the usual shape, implement [`Service`] and run [`serve`]: it handles connect (version check, session object),
//! disconnect (session freed), and bad clients, and hands every other message to [`Service::message`].
//!
//! ```ignore
//! let server = Server::create("MyServer")?;
//! e32::process::rendezvous(0); // a client waiting in RProcess::Rendezvous may connect now
//! e32::rt::block_on(serve(&server, &mut MyService::default()))?;
//! ```
//!
//! Kernel behaviour this follows: `sipc.cpp` (DServer/DSession/RMessageK) and euser `ub_svr.cpp` (CServer2::RunL).

use crate::request::{TRequestStatus, wait_for_request};
use crate::sys::{self, RMessage2, SIpcCopyInfo, TExitType, TPtrC8, TVersion, err, ipc};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::marker::PhantomData;
use core::ops::ControlFlow;
use core::pin::Pin;
use core::task::{Context, Poll};

/// A named server. The handle belongs to the creating thread, and only that thread may receive, so it stays there.
pub struct Server {
    handle: i32,
    _thread_bound: PhantomData<*const ()>,
}

impl Server {
    /// Create the server `name` (ASCII; a leading `!` needs ProtServ). Sessions may be shared within a process,
    /// as `RServer2::CreateGlobal`. `Err(KErrAlreadyExists)` if the name is taken.
    pub fn create(name: &str) -> Result<Self, i32> {
        let n = TPtrC8::new(name.as_bytes());
        let h = ipc::server_create(&n, ipc::EIPC_SESSION_SHARABLE);
        if h < 0 {
            return Err(h);
        }
        Ok(Self {
            handle: h,
            _thread_bound: PhantomData,
        })
    }

    /// The next message. `Err(KErrCancel)` if the server is being closed.
    pub fn receive(&self) -> Receive<'_> {
        Receive {
            server: self,
            started: false,
            status: TRequestStatus::pending(),
            msg: RMessage2::default(),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Outstanding clients see KErrServerTerminated.
        sys::handle_close(self.handle);
    }
}

/// Future of [`Server::receive`]. The kernel writes the message into this future, so it must stay pinned while the
/// receive is outstanding; dropping it early cancels the receive first.
pub struct Receive<'a> {
    server: &'a Server,
    started: bool,
    status: TRequestStatus,
    msg: RMessage2,
}

impl Future for Receive<'_> {
    type Output = Result<Message, i32>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: nothing is moved out; `status` and `msg` stay put while the receive runs (the future is pinned).
        let this = unsafe { self.get_unchecked_mut() };
        if !this.started {
            this.started = true;
            this.status = TRequestStatus::pending();
            // SAFETY: both live inside the pinned future; Drop cancels and waits before they go away.
            unsafe { ipc::server_receive(this.server.handle, &mut this.status, &mut this.msg) };
            return Poll::Pending;
        }
        match this.status.get() {
            sys::KREQUEST_PENDING => Poll::Pending,
            r if r < 0 => {
                this.started = false;
                Poll::Ready(Err(r))
            }
            _ => {
                this.started = false;
                Poll::Ready(Ok(Message { raw: this.msg }))
            }
        }
    }
}

impl Drop for Receive<'_> {
    fn drop(&mut self) {
        if !self.started {
            return;
        }
        if self.status.is_pending() {
            ipc::server_cancel(self.server.handle);
            wait_for_request(&self.status);
        }
        // A message that arrived but was never polled: complete it (Message's Drop) so its client is not left hanging.
        if self.status.get() == 0 {
            drop(Message { raw: self.msg });
        }
    }
}

/// One client request. Complete it exactly once with [`Message::complete`] (it consumes the message, so a stale
/// handle can never complete a newer message); dropping it uncompleted completes it with `KErrGeneral`. Handles are
/// valid in every thread of the server's process, so a message may be passed to another thread for a later reply.
#[derive(Debug)]
pub struct Message {
    raw: RMessage2,
}

impl Message {
    pub fn function(&self) -> i32 {
        self.raw.i_function
    }

    /// Argument word `i` (0-3) as the client sent it (for descriptor arguments, a client-side address).
    pub fn int(&self, i: usize) -> i32 {
        self.raw.i_args[i]
    }

    /// Length of the client's descriptor argument `param`, in characters.
    pub fn des_len(&self, param: usize) -> Result<usize, i32> {
        ok_len(ipc::message_get_des_length(self.raw.i_handle, param as i32))
    }

    /// Capacity of the client's descriptor argument `param`, in characters.
    pub fn des_max_len(&self, param: usize) -> Result<usize, i32> {
        ok_len(ipc::message_get_des_max_length(
            self.raw.i_handle,
            param as i32,
        ))
    }

    /// Copy the client's 8-bit descriptor `param`, from `offset`, into `buf`; returns the bytes copied.
    pub fn read8(&self, param: usize, buf: &mut [u8], offset: usize) -> Result<usize, i32> {
        self.copy(param, buf.as_mut_ptr(), buf.len(), 0, offset)
    }

    /// Copy the client's 16-bit descriptor `param`, from `offset`, into `buf`; returns the characters copied.
    pub fn read16(&self, param: usize, buf: &mut [u16], offset: usize) -> Result<usize, i32> {
        self.copy(
            param,
            buf.as_mut_ptr().cast(),
            buf.len(),
            ipc::KIPC_16BIT,
            offset,
        )
    }

    /// The whole of the client's 8-bit descriptor `param`.
    pub fn read8_vec(&self, param: usize) -> Result<Vec<u8>, i32> {
        let mut v = vec![0; self.des_len(param)?];
        let n = self.read8(param, &mut v, 0)?;
        v.truncate(n);
        Ok(v)
    }

    /// The whole of the client's 16-bit descriptor `param`.
    pub fn read16_vec(&self, param: usize) -> Result<Vec<u16>, i32> {
        let mut v = vec![0; self.des_len(param)?];
        let n = self.read16(param, &mut v, 0)?;
        v.truncate(n);
        Ok(v)
    }

    /// Write `data` into the client's 8-bit descriptor `param` at `offset`; its length becomes `offset + data.len()`
    /// when the message completes. `Err(KErrOverflow)` past its capacity.
    pub fn write8(&self, param: usize, data: &[u8], offset: usize) -> Result<(), i32> {
        let flags = ipc::KIPC_DIR_WRITE;
        self.copy(param, data.as_ptr().cast_mut(), data.len(), flags, offset)
            .map(|_| ())
    }

    /// Write `data` into the client's 16-bit descriptor `param` at `offset` (characters).
    pub fn write16(&self, param: usize, data: &[u16], offset: usize) -> Result<(), i32> {
        let flags = ipc::KIPC_DIR_WRITE | ipc::KIPC_16BIT;
        self.copy(
            param,
            data.as_ptr().cast_mut().cast(),
            data.len(),
            flags,
            offset,
        )
        .map(|_| ())
    }

    fn copy(
        &self,
        param: usize,
        ptr: *mut u8,
        chars: usize,
        flags: u32,
        offset: usize,
    ) -> Result<usize, i32> {
        let info = SIpcCopyInfo {
            i_local_ptr: ptr,
            i_local_len: chars as i32,
            i_flags: flags,
        };
        // SAFETY: `ptr` is valid for `chars` characters (the caller's slice); a write only reads from it.
        let r =
            unsafe { ipc::message_ipc_copy(self.raw.i_handle, param as i32, &info, offset as i32) };
        ok_len(r)
    }

    /// Reply to the client: `reason` becomes its request's completion value.
    pub fn complete(mut self, reason: i32) {
        ipc::message_complete(self.raw.i_handle, reason);
        self.raw.i_handle = 0;
    }

    /// Reply with a handle: the client's request completes with a new handle, in its thread, to the object `handle`
    /// refers to here (`RMessagePtr2::Complete(RHandleBase)`, `sipc.cpp:2086-2109`) — e.g. a global chunk.
    pub fn complete_with_handle(mut self, handle: i32) {
        ipc::message_complete_with_handle(self.raw.i_handle, handle);
        self.raw.i_handle = 0;
    }

    /// Panic the client thread (`RMessage2::Panic`), then complete the message.
    pub fn panic_client(self, category: &str, reason: i32) {
        let cat = &category.as_bytes()[..category.len().min(sys::KMAX_EXIT_CATEGORY_NAME)];
        ipc::message_kill(
            self.raw.i_handle,
            TExitType::Panic,
            reason,
            &TPtrC8::new(cat),
        );
        self.complete(err::KERR_NONE);
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        if self.raw.i_handle != 0 {
            ipc::message_complete(self.raw.i_handle, err::KERR_GENERAL);
        }
    }
}

fn ok_len(r: i32) -> Result<usize, i32> {
    if r < 0 { Err(r) } else { Ok(r as usize) }
}

/// What a server does with its clients. One `Session` value per connected client session.
pub trait Service {
    type Session;

    /// A client connects with `version`: return its session state, or an error to refuse (e.g. `KErrNotSupported`).
    fn connect(&mut self, version: TVersion) -> Result<Self::Session, i32>;

    /// A request on `session`. Complete `msg` now or keep it for a later reply — but complete every message of a
    /// session before that session's [`Service::disconnect`] returns.
    fn message(&mut self, session: &mut Self::Session, msg: Message);

    /// The client closed its session. `Break` ends [`serve`] (a transient server that exits when idle).
    fn disconnect(&mut self, session: Self::Session) -> ControlFlow<()>;
}

/// Panic category for clients that break the protocol, and its reasons (CServer2 uses `CSession2` / `CServer2`).
pub const BAD_CLIENT_CATEGORY: &str = "E32-SERVER";
/// A connect message on a session that is already connected.
pub const BAD_CLIENT_RECONNECT: i32 = 1;
/// A negative function number, or a request on a session whose connect failed.
pub const BAD_CLIENT_MESSAGE: i32 = 2;

/// Receive and dispatch messages on `server` until [`Service::disconnect`] says `Break`. Run it (with
/// [`crate::rt::block_on`]) on the thread that created the server.
pub async fn serve<S: Service>(server: &Server, service: &mut S) -> Result<(), i32> {
    loop {
        let msg = server.receive().await?;
        if dispatch(service, msg).is_break() {
            return Ok(());
        }
    }
}

/// Handle one received message the way [`serve`] does (connect, disconnect, bad clients, then
/// [`Service::message`]) — for servers that run their own loop, e.g. alongside input and timers.
pub fn dispatch<S: Service>(service: &mut S, msg: Message) -> ControlFlow<()> {
    let cookie = msg.raw.i_session_ptr as *mut S::Session;
    match msg.function() {
        ipc::ECONNECT if !cookie.is_null() => {
            msg.panic_client(BAD_CLIENT_CATEGORY, BAD_CLIENT_RECONNECT)
        }
        ipc::ECONNECT => match service.connect(TVersion::from_packed(msg.int(0))) {
            Ok(s) => {
                ipc::set_session_ptr(msg.raw.i_handle, Box::into_raw(Box::new(s)) as usize);
                msg.complete(err::KERR_NONE);
            }
            Err(e) => msg.complete(e),
        },
        // Sent for every session that closes, connected or not; the reason is ignored.
        ipc::EDISCONNECT => {
            let flow = if cookie.is_null() {
                ControlFlow::Continue(())
            } else {
                // SAFETY: the cookie is the Box set at connect; the kernel sends one disconnect per session.
                service.disconnect(*unsafe { Box::from_raw(cookie) })
            };
            msg.complete(err::KERR_NONE);
            return flow;
        }
        f if f < 0 || cookie.is_null() => msg.panic_client(BAD_CLIENT_CATEGORY, BAD_CLIENT_MESSAGE),
        // SAFETY: a live session's Box (freed only at its disconnect); one message at a time on this thread.
        _ => service.message(unsafe { &mut *cookie }, msg),
    }
    ControlFlow::Continue(())
}
