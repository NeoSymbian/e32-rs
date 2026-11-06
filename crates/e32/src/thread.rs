// SPDX-License-Identifier: Apache-2.0
//! Threads in the current process.
//!
//! `Exec::ThreadCreate` starts the thread (suspended) at the EXE's entry point; Symbian's start-up code (`RunThread`)
//! sets up euser's per-thread state from the 64-byte create info, calls `iFunction(iPtr)` and exits the thread with
//! its return value. The function here is a trampoline that runs a boxed Rust closure.

use crate::request::{TRequestStatus, wait_for_request};
use crate::sys::{self, SThreadCreateInfo, TOwnerType, TPtrC8};
use alloc::boxed::Box;
use core::ffi::c_void;
use core::ptr;

/// User stack of a spawned thread (the kernel wants at least 512 bytes).
pub const STACK_SIZE: i32 = 16 << 10;

type Entry = Box<dyn FnOnce() -> i32 + Send>;

extern "C" fn trampoline(p: *mut c_void) -> i32 {
    // SAFETY: `p` is the `Box<Entry>` leaked by `spawn`, handed to exactly this one thread.
    let f = unsafe { Box::from_raw(p as *mut Entry) };
    f()
}

/// A running thread; [`JoinHandle::join`] waits for it and returns its exit reason.
pub struct JoinHandle {
    handle: i32,
    // Boxed: the kernel writes it when the thread exits, so it must not move. Leaked if never joined.
    status: Option<Box<TRequestStatus>>,
}

/// Start `f` on a new thread sharing this process's heap. Its return value becomes the thread's exit reason.
pub fn spawn<F: FnOnce() -> i32 + Send + 'static>(f: F) -> Result<JoinHandle, i32> {
    let entry: Box<Entry> = Box::new(Box::new(f));
    let arg = Box::into_raw(entry) as *mut c_void;
    let name = TPtrC8::new(b""); // anonymous
    let mut info = SThreadCreateInfo {
        i_handle: 0,
        i_type: 0,
        i_function: Some(trampoline),
        i_ptr: arg,
        i_supervisor_stack: ptr::null_mut(),
        i_supervisor_stack_size: 0,
        i_user_stack: ptr::null_mut(),
        i_user_stack_size: STACK_SIZE,
        i_initial_thread_priority: 0,
        i_name: name,
        i_total_size: size_of::<SThreadCreateInfo>() as i32,
        // euser's runtime in the new thread shares the creator's RHeap (what RThread::Create does by default)
        i_allocator: sys::heap(),
        i_heap_initial_size: 0,
        i_heap_max_size: 0,
        i_flags: 0,
    };
    // SAFETY: `info` is a complete 64-byte create info; the trampoline owns `arg` once the thread runs.
    let h = unsafe { sys::thread_create(&name, TOwnerType::Process, &mut info) };
    if h < 0 {
        // SAFETY: the thread was not created, so `arg` is still ours.
        drop(unsafe { Box::from_raw(arg as *mut Entry) });
        return Err(h);
    }
    let mut status = Box::new(TRequestStatus::pending());
    // SAFETY: `status` is boxed (does not move) and kept until the logon completes (join) or leaked (drop).
    unsafe { sys::thread_logon(h, &mut *status) };
    sys::thread_resume(h);
    Ok(JoinHandle {
        handle: h,
        status: Some(status),
    })
}

impl JoinHandle {
    /// Wait for the thread to end; its exit reason (the closure's return value, or the panic reason).
    pub fn join(mut self) -> i32 {
        let status = self.status.take().expect("joined once");
        wait_for_request(&status)
    }
}

impl Drop for JoinHandle {
    fn drop(&mut self) {
        if let Some(s) = self.status.take() {
            // shortcut: a detached thread's logon request still completes later, so its status is leaked rather
            // than cancelled (ThreadLogonCancel would also need its completion consumed).
            Box::leak(s);
        }
        sys::handle_close(self.handle);
    }
}
