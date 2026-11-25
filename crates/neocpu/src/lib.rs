// SPDX-License-Identifier: Apache-2.0
//! US6a on-target tests (`NEOCPU.EXE`, docs `user-side-smp.md`): the ARMv8 AArch32 CRC32, AES, SHA-1 and SHA-256
//! instructions give the known answers on every CPU, the Pixel's A55 and A75 clusters both. The thread is pinned to each
//! CPU in turn (`EKernelHalLockThreadToCpu`). Reports like neotest: `cpu N: crc32 ok aes ok sha1 ok sha256 ok` (or
//! `cpu N: <test> FAILED: <why>`) and `neocpu: N cpus, P passed, F failed`. Exit code = failures.
#![cfg_attr(target_arch = "arm", no_std)]
#![cfg_attr(
    target_arch = "arm",
    feature(stdarch_aarch32_crc32, stdarch_arm_neon_intrinsics)
)]

extern crate alloc;

#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
pub mod kat;

#[cfg(target_arch = "arm")]
mod device {
    use super::kat::TESTS;
    use alloc::string::String;
    use e32::{dprintln, thread};

    e32::main!(main);

    fn main() -> i32 {
        let cpus = thread::cpu_count();
        let (mut passed, mut failed) = (0, 0);
        for cpu in 0..cpus {
            if let Err(r) = thread::lock_to_cpu(cpu) {
                failed += TESTS.len() as i32;
                dprintln!("cpu {}: can't pin the thread: {}", cpu, r);
                continue;
            }
            let mut line = String::new();
            for (name, test) in TESTS {
                match test() {
                    Ok(()) => {
                        passed += 1;
                        line.push_str(&alloc::format!(" {name} ok"));
                    }
                    Err(why) => {
                        failed += 1;
                        dprintln!("cpu {}: {} FAILED: {}", cpu, name, why);
                    }
                }
            }
            thread::unlock_cpu();
            dprintln!("cpu {}:{}", cpu, line);
        }
        dprintln!(
            "neocpu: {} cpus, {} passed, {} failed",
            cpus,
            passed,
            failed
        );
        failed
    }
}
