// SPDX-License-Identifier: Apache-2.0
//! R1: a Rust `panic!` becomes a Symbian panic (category "RUST") that the kernel reports.
#![no_std]

e32::main!(main);

fn main() -> i32 {
    e32::dprintln!("neopanic: about to panic");
    let n = core::hint::black_box(7);
    panic!("deliberate panic {}", n);
}
