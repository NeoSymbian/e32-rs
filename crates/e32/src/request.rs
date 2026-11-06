// SPDX-License-Identifier: Apache-2.0
//! Waiting for an asynchronous request.
//!
//! The kernel completes a request by writing its `TRequestStatus` and signalling the thread's request semaphore once.
//! Waiting for one request may consume signals meant for others; they are given back afterwards, exactly like
//! euser's `User::WaitForRequest` (`us_exec.cpp`).

use crate::sys;
pub use crate::sys::TRequestStatus;

/// Block until `status` is no longer pending; returns its completion value.
pub fn wait_for_request(status: &TRequestStatus) -> i32 {
    let mut extra = -1;
    loop {
        extra += 1;
        sys::wait_for_any_request();
        if !status.is_pending() {
            break;
        }
    }
    if extra > 0 {
        sys::request_signal(extra);
    }
    status.get()
}
