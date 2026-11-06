// SPDX-License-Identifier: Apache-2.0
//! A `critical-section` implementation (feature `critical-section`) for crates that need one on ARMv5TE, which has
//! no compare-and-swap (Slint's `portable-atomic`, `once_cell`, ...). It is a process-wide kernel mutex — recursive
//! for its holder, so nested critical sections work — created by [`crate::init`].

use crate::sys::{self, TOwnerType};
use core::cell::UnsafeCell;

/// The mutex handle: written once by `init` on the main thread before anything can run concurrently, then only read.
struct Handle(UnsafeCell<i32>);
// SAFETY: see above — no write after other threads can exist.
unsafe impl Sync for Handle {}
static LOCK: Handle = Handle(UnsafeCell::new(0));

fn lock() -> i32 {
    // SAFETY: a plain read of a value fixed before any concurrency (see `Handle`).
    unsafe { *LOCK.0.get() }
}

pub(crate) fn init() {
    let h = sys::mutex_create(TOwnerType::Process);
    if h < 0 {
        crate::process::panic("RUST-CS", h);
    }
    // SAFETY: `init` runs once, first, on the main thread.
    unsafe { *LOCK.0.get() = h };
}

struct KernelMutex;
critical_section::set_impl!(KernelMutex);

// SAFETY: acquire/release pair up on one thread; the kernel mutex excludes other threads of the process.
unsafe impl critical_section::Impl for KernelMutex {
    unsafe fn acquire() -> critical_section::RawRestoreState {
        sys::mutex_wait(lock(), 0);
    }
    unsafe fn release(_: critical_section::RawRestoreState) {
        sys::mutex_signal(lock());
    }
}
