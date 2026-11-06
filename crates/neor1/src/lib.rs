// SPDX-License-Identifier: Apache-2.0
//! R1: a Symbian EXE written in Rust only. Prints through `Exec::DebugPrint` and exits with 6 * 7.
#![no_std]

e32::main!(main);

fn main() -> i32 {
    e32::dprintln!("r1: hello from pure Rust");
    let (a, b) = (6, 7);
    e32::dprintln!("r1: {} * {} = {}", a, b, a * b);
    a * b
}
