// SPDX-License-Identifier: Apache-2.0
//! R0 spike: Rust code inside a real Symbian EXE.
//!
//! The C++ side (`toolchain/rust/neo_glue.cpp` in NeoSymbian/symbian-2010) defines `E32Main`, provides
//! `neo_debug_print` on top of `RDebug::Printf`, and calls `rust_hello`.
#![no_std]

unsafe extern "C" {
    fn neo_debug_print(ptr: *const u8, len: i32);
}

fn print(s: &str) {
    // SAFETY: the C++ side reads exactly `len` bytes from `ptr` and keeps no reference to them.
    unsafe { neo_debug_print(s.as_ptr(), s.len() as i32) }
}

/// Called by the C++ `E32Main`. Returns a value the C++ side prints, to show data flows both ways.
#[unsafe(no_mangle)]
pub extern "C" fn rust_hello(a: i32, b: i32) -> i32 {
    print("hello from Rust");
    a.wrapping_mul(b)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // R1 replaces this with User::Panic through an exec call.
    print("rust panic");
    loop {}
}
