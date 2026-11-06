// SPDX-License-Identifier: Apache-2.0
//! The Rust side of a Symbian EXE, all on executive calls: debug output, exit, panic, `E32Main`, a heap for `alloc`,
//! request waiting, timers, a small async runtime, threads, locks, client-server IPC and a file-server client.
//!
//! ```ignore
//! #![no_std]
//! e32::main!(main);
//! fn main() -> i32 {
//!     e32::dprintln!("hello from {}", "Rust");
//!     0
//! }
//! ```
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub use e32_sys as sys;

pub mod fs;
#[cfg(target_arch = "arm")]
pub mod heap;
pub mod ipc;
pub mod request;
pub mod rt;
pub mod sync;
pub mod thread;
pub mod time;

pub mod debug {
    use crate::sys;
    use core::fmt;

    /// The size of `RDebug::Printf`'s buffer (`TBuf8<0x100>`): longer output is truncated, never split.
    pub const BUF_SIZE: usize = 0x100;

    /// Print bytes as one line on the kernel debug port (the kernel ends the line).
    pub fn print_bytes(bytes: &[u8]) {
        let des = sys::TPtrC8::new(bytes);
        // SAFETY: `des` points at `bytes`, both alive for the call.
        unsafe { sys::debug_print(&des, 0) };
    }

    pub fn print(s: &str) {
        print_bytes(s.as_bytes());
    }

    /// A fixed buffer `core::fmt` writes into; what does not fit is dropped.
    pub struct Buf {
        data: [u8; BUF_SIZE],
        len: usize,
    }

    impl Buf {
        pub const fn new() -> Self {
            Self {
                data: [0; BUF_SIZE],
                len: 0,
            }
        }
        pub fn as_bytes(&self) -> &[u8] {
            &self.data[..self.len]
        }
    }

    impl Default for Buf {
        fn default() -> Self {
            Self::new()
        }
    }

    impl fmt::Write for Buf {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let n = s.len().min(BUF_SIZE - self.len);
            self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
            self.len += n;
            Ok(())
        }
    }
}

/// Format and print one line on the kernel debug port, like `RDebug::Printf`.
#[macro_export]
macro_rules! dprintln {
    ($($arg:tt)*) => {{
        let mut buf = $crate::debug::Buf::new();
        let _ = core::fmt::Write::write_fmt(&mut buf, format_args!($($arg)*));
        $crate::debug::print_bytes(buf.as_bytes());
    }};
}

pub mod process {
    use crate::sys;

    /// End the current thread with `reason`; for the main thread that ends the process (`User::Exit`).
    pub fn exit(reason: i32) -> ! {
        sys::user_thread_exiting(reason);
        // SAFETY: a null category is allowed for EExitKill.
        unsafe {
            sys::thread_kill(
                sys::KCURRENT_THREAD_HANDLE,
                sys::TExitType::Kill,
                reason,
                core::ptr::null(),
            )
        };
        // ThreadKill on the current thread does not return.
        loop {
            core::hint::spin_loop();
        }
    }

    /// Panic the current thread with a category (at most 16 bytes kept) and reason (`User::Panic`).
    pub fn panic(category: &str, reason: i32) -> ! {
        let cat = &category.as_bytes()[..category.len().min(sys::KMAX_EXIT_CATEGORY_NAME)];
        let des = sys::TPtrC8::new(cat);
        // SAFETY: `des` points at `cat`, alive for the call (which does not return).
        unsafe {
            sys::thread_kill(
                sys::KCURRENT_THREAD_HANDLE,
                sys::TExitType::Panic,
                reason,
                &des,
            )
        };
        // ThreadKill on the current thread does not return.
        loop {
            core::hint::spin_loop();
        }
    }
}

/// The panic category Rust panics show up under.
pub const PANIC_CATEGORY: &str = "RUST";

/// Body of the `#[panic_handler]` that [`main!`] installs: print the message, then a Symbian panic.
pub fn on_panic(info: &core::panic::PanicInfo) -> ! {
    match info.location() {
        Some(l) => dprintln!(
            "rust panic: {} at {}:{}",
            info.message(),
            l.file(),
            l.line()
        ),
        None => dprintln!("rust panic: {}", info.message()),
    }
    process::panic(PANIC_CATEGORY, 0)
}

/// Set up what the Rust side needs before `main` runs: the heap behind `alloc`. Called by [`main!`].
pub fn init() {
    #[cfg(target_arch = "arm")]
    heap::init();
}

/// Make `fn() -> i32` the EXE's `E32Main` (the Symbian start-up code calls it, then `User::Exit` with its result),
/// set up the heap, and install the panic handler.
#[macro_export]
macro_rules! main {
    ($main:path) => {
        #[unsafe(export_name = "_Z7E32Mainv")]
        pub extern "C" fn __e32_main() -> i32 {
            $crate::init();
            $main()
        }

        #[panic_handler]
        fn __e32_panic(info: &core::panic::PanicInfo) -> ! {
            $crate::on_panic(info)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::debug::{BUF_SIZE, Buf};
    use core::fmt::Write;

    #[test]
    fn buf_formats() {
        let mut b = Buf::new();
        write!(b, "{} * {} = {}", 6, 7, 6 * 7).unwrap();
        assert_eq!(b.as_bytes(), b"6 * 7 = 42");
    }

    #[test]
    fn buf_truncates_at_rdebug_size() {
        let mut b = Buf::new();
        for _ in 0..100 {
            write!(b, "0123456789").unwrap();
        }
        assert_eq!(b.as_bytes().len(), BUF_SIZE);
        assert!(b.as_bytes().starts_with(b"0123456789"));
    }
}
