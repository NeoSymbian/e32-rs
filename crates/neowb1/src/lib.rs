// SPDX-License-Identifier: Apache-2.0
//! WB1 on-target tests (`NEOWB1.EXE`, docs `browser/wb1-kernel.md`): what Servo needs from the kernel. Reports like neotest,
//! `test <name> ... ok|FAILED: <why>` and `neowb1: N passed, M failed`. Exit code = failures.
//!
//! The FP tests use VFP/NEON from asm. The crate itself is ARMv8 with NEON (US4), so compiled code may use the FP
//! registers too: each test keeps its fill, wait and check of the registers inside one asm block.
#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::hint::black_box;
use e32::dprintln;
use e32::sync::Semaphore;
use e32::sys::{self, TChunkAdjust, TChunkCreate, TOwnerType};
use e32::thread;
use e32::time::ticks;

e32::main!(main);

type Outcome = Result<(), String>;
type Test = (&'static str, fn() -> Outcome);

fn ensure(ok: bool, why: impl FnOnce() -> String) -> Outcome {
    if ok { Ok(()) } else { Err(why()) }
}

const TESTS: &[Test] = &[
    ("fp_math", fp_math),
    ("fp_threads", fp_survives_preemption),
    ("stack_8m", stack_8m),
    ("threads_128", threads_128),
    ("gc_chunk", gc_chunk),
    ("random", secure_random),
];

fn main() -> i32 {
    let mut failed = 0;
    for (name, test) in TESTS {
        match test() {
            Ok(()) => dprintln!("test {} ... ok", name),
            Err(why) => {
                failed += 1;
                dprintln!("test {} ... FAILED: {}", name, why);
            }
        }
    }
    dprintln!(
        "neowb1: {} passed, {} failed",
        TESTS.len() as i32 - failed,
        failed
    );
    failed
}

// ---- FP/NEON ------------------------------------------------------------------------------------------------------

/// VFP double add and multiply give IEEE results: 1.5 + 2.25 = 3.75, 1.5 * 2.25 = 3.375.
fn fp_math() -> Outcome {
    let (a, b) = (1.5f64.to_bits(), 2.25f64.to_bits());
    let (sum_lo, sum_hi, mul_lo, mul_hi): (u32, u32, u32, u32);
    // SAFETY: VFP register arithmetic only; the kernel enables FP for EL0 and switches it per thread.
    unsafe {
        asm!(
            ".fpu neon",
            "vmov d0, {alo}, {ahi}",
            "vmov d1, {blo}, {bhi}",
            "vadd.f64 d2, d0, d1",
            "vmul.f64 d3, d0, d1",
            "vmov {slo}, {shi}, d2",
            "vmov {mlo}, {mhi}, d3",
            alo = in(reg) a as u32, ahi = in(reg) (a >> 32) as u32,
            blo = in(reg) b as u32, bhi = in(reg) (b >> 32) as u32,
            slo = out(reg) sum_lo, shi = out(reg) sum_hi, mlo = out(reg) mul_lo, mhi = out(reg) mul_hi,
            out("d0") _, out("d1") _, out("d2") _, out("d3") _,
            options(nostack),
        );
    }
    let sum = f64::from_bits((sum_hi as u64) << 32 | sum_lo as u64);
    let mul = f64::from_bits((mul_hi as u64) << 32 | mul_lo as u64);
    ensure(sum == 3.75 && mul == 3.375, || {
        format!("sum {sum}, product {mul}")
    })
}

/// One round, all in one asm block so no compiled code runs between filling and checking the registers: save the
/// caller's FPSCR and d8-d15, fill Q0-Q15 (= D0-D31) with `seed`, `seed + 1`, ... per Q register and FPSCR with
/// `fpscr`, spin `spins` integer loops (the 1 ms tick preempts and migrates the thread), then, if `sem` >= 0, block in
/// `SemaphoreWait(sem, 1000 us)` (a real context switch; nobody signals it, so it times out), store D0-D31 to `out`,
/// read FPSCR, restore. Returns (FPSCR seen, the wait's result).
fn fp_round(seed: u32, fpscr: u32, spins: u32, sem: i32, out: &mut [u32; 64]) -> (u32, i32) {
    let (got, waited): (u32, i32);
    // SAFETY: VFP/NEON registers, FPSCR and `out` (256 bytes) only; d8-d15 and FPSCR are restored; the exec call's
    // caller-saved registers are declared by clobber_abi; operands live in r4/r5/r8/r10 (callee-saved) across it.
    unsafe {
        asm!(
            ".fpu crypto-neon-fp-armv8",
            "vmrs r5, fpscr",
            "vpush {{d8-d15}}",
            "vmsr fpscr, {f}",
            "vdup.32 q0, r2", "add r2, r2, #1", "vdup.32 q1, r2", "add r2, r2, #1",
            "vdup.32 q2, r2", "add r2, r2, #1", "vdup.32 q3, r2", "add r2, r2, #1",
            "vdup.32 q4, r2", "add r2, r2, #1", "vdup.32 q5, r2", "add r2, r2, #1",
            "vdup.32 q6, r2", "add r2, r2, #1", "vdup.32 q7, r2", "add r2, r2, #1",
            "vdup.32 q8, r2", "add r2, r2, #1", "vdup.32 q9, r2", "add r2, r2, #1",
            "vdup.32 q10, r2", "add r2, r2, #1", "vdup.32 q11, r2", "add r2, r2, #1",
            "vdup.32 q12, r2", "add r2, r2, #1", "vdup.32 q13, r2", "add r2, r2, #1",
            "vdup.32 q14, r2", "add r2, r2, #1", "vdup.32 q15, r2",
            "2:",
            "subs r10, r10, #1",
            "bne 2b",
            "mov r0, #0",
            "cmp r8, #0",
            "blt 3f",
            "mov r0, r8",
            "movw r1, #1000",
            "swi #{wait}",
            "3:",
            "vstmia r4!, {{d0-d15}}",
            "vstmia r4, {{d16-d31}}",
            "vmrs r1, fpscr",
            "vpop {{d8-d15}}",
            "vmsr fpscr, r5",
            wait = const 0x1f, // slow::SEMAPHORE_WAIT (e32-sys execs.rs)
            out("r5") _,
            inout("r2") seed => _,
            f = in(reg) fpscr,
            inout("r10") spins.max(1) => _,
            inout("r8") sem => _,
            inout("r4") out.as_mut_ptr() => _,
            lateout("r0") waited,
            lateout("r1") got,
            clobber_abi("C"),
        );
    }
    (got, waited)
}

/// FPSCR bits a thread owns here: NZCV (31:28), DN (25), FZ (24), RMode (23:22).
const FPSCR_MASK: u32 = 0xF3C0_0000;

/// Integer loops in about 20 ms (measured once with the kernel tick).
fn spins_for_20ms() -> u32 {
    let mut out = [0u32; 64];
    let n = 4_000_000;
    let t0 = ticks();
    fp_round(0, 0, n, -1, &mut out);
    let ms = ticks().wrapping_sub(t0).max(1);
    (n as u64 * 20 / ms as u64).clamp(1_000_000, 200_000_000) as u32
}

/// "FP patterns survive preemption, migration and blocking": 8 threads (more than the 4 CPUs) each fill every VFP
/// register and FPSCR with their own pattern, busy-spin ~20 ms (so the scheduler preempts and moves them), every 4th
/// round also block 1 ms in a semaphore wait, then check, all inside one asm block (`fp_round`).
fn fp_survives_preemption() -> Outcome {
    const THREADS: u32 = 8;
    const ROUNDS: u32 = 100;
    let spins = spins_for_20ms();
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let h = thread::spawn(move || {
            let sem = sys::semaphore_create(0, TOwnerType::Thread);
            if sem < 0 {
                dprintln!("fp_threads: thread {} semaphore {}", t, sem);
                return 1;
            }
            let mut bad_rounds = 0;
            for round in 0..ROUNDS {
                let seed = (t << 24) | (round << 8);
                let fpscr = ((t * 7 + round) << 22) & FPSCR_MASK;
                let block = if round % 4 == 3 { sem } else { -1 };
                let mut regs = [0u32; 64];
                let (got, waited) = fp_round(seed, fpscr, spins, block, &mut regs);
                let got = got & FPSCR_MASK;
                let bad = regs
                    .iter()
                    .enumerate()
                    .position(|(i, &w)| w != seed + (i as u32 / 4));
                let wait_ok = block < 0 || waited == sys::err::KERR_TIMED_OUT;
                if got != fpscr || bad.is_some() || !wait_ok {
                    dprintln!(
                        "fp_threads: thread {} round {}: word {:?}, fpscr {:#x} want {:#x}, wait {}",
                        t,
                        round,
                        bad,
                        got,
                        fpscr,
                        waited
                    );
                    bad_rounds += 1;
                    break;
                }
            }
            sys::handle_close(sem);
            if bad_rounds == 0 { 0 } else { 1 }
        })
        .map_err(|e| format!("spawn {e}"))?;
        handles.push(h);
    }
    let bad: i32 = handles.into_iter().map(|h| h.join()).sum();
    ensure(bad == 0, || {
        format!("{bad} of {THREADS} threads saw another's registers")
    })
}

// ---- stacks and threads --------------------------------------------------------------------------------------------

/// An 8 MiB stack (Servo's script and style threads) used to the end; 32 MiB is over the kernel's cap.
fn stack_8m() -> Outcome {
    let h = thread::spawn_with_stack(8 << 20, || {
        let mut a = [0u8; (8 << 20) - (64 << 10)];
        let a = black_box(&mut a);
        for i in (0..a.len()).step_by(4096) {
            a[i] = i as u8 | 1;
        }
        black_box(a)
            .iter()
            .step_by(4096)
            .filter(|&&b| b & 1 == 0)
            .count() as i32
    })
    .map_err(|e| format!("spawn 8 MiB: {e}"))?;
    let r = h.join();
    let big = thread::spawn_with_stack(32 << 20, || 0).map(|h| h.join());
    ensure(r == 0 && big == Err(KERR_TOO_BIG), || {
        format!("8 MiB thread exit {r}, 32 MiB spawn {big:?}")
    })
}
const KERR_TOO_BIG: i32 = -40;

/// 128 threads alive at once (Servo runs 40-130), each with a 64 KB stack.
fn threads_128() -> Outcome {
    let go: &'static Semaphore = Box::leak(Box::new(
        Semaphore::new(0).map_err(|e| format!("semaphore {e}"))?,
    ));
    let mut handles = Vec::new();
    for i in 0..128 {
        let h = thread::spawn_with_stack(64 << 10, move || {
            go.wait();
            i
        })
        .map_err(|e| format!("spawn #{i}: {e}"))?;
        handles.push(h);
    }
    for _ in 0..128 {
        go.signal();
    }
    let sum: i32 = handles.into_iter().map(|h| h.join()).sum();
    ensure(sum == 127 * 128 / 2, || format!("exit codes sum {sum}"))
}

// ---- GC-style memory (SpiderMonkey: reserve, aligned commit, decommit) ---------------------------------------------

const EDISCONNECTED: u32 = 2;
const MIB: usize = 1 << 20;

/// Reserve 256 MiB (and 1 GiB), commit 1 MiB pieces at 1 MiB-aligned addresses, decommit half; a decommitted page
/// faults (the GC's "no access"), and a commit again gives fresh memory.
fn gc_chunk() -> Outcome {
    let reserve = |size: usize| {
        let info = TChunkCreate {
            i_att: EDISCONNECTED,
            i_max_size: size as i32,
            ..TChunkCreate::default()
        };
        sys::chunk_create(TOwnerType::Process, &info)
    };
    let big = reserve(1024 * MIB);
    ensure(big > 0, || format!("reserve 1 GiB: {big}"))?;
    sys::handle_close(big);
    let h = reserve(256 * MIB);
    ensure(h > 0, || format!("reserve 256 MiB: {h}"))?;
    let base = sys::chunk_base(h) as usize;
    let first = base.next_multiple_of(MIB) - base; // offset of the first 1 MiB-aligned address
    dprintln!(
        "gc_chunk: base {:#x}, first aligned offset {:#x}",
        base,
        first
    );
    let piece = |i: usize| first + i * MIB;
    for i in 0..16 {
        let r = sys::chunk_adjust(h, TChunkAdjust::Commit, piece(i) as i32, MIB as i32);
        ensure(r == 0, || format!("commit #{i}: {r}"))?;
        // SAFETY: committed just now, 1 MiB.
        unsafe { core::ptr::write_bytes((base + piece(i)) as *mut u8, 0xAB, MIB) };
    }
    for i in (1..16).step_by(2) {
        let r = sys::chunk_adjust(h, TChunkAdjust::Decommit, piece(i) as i32, MIB as i32);
        ensure(r == 0, || format!("decommit #{i}: {r}"))?;
    }
    // a read of a decommitted page must fault (KERN-EXEC 3 ends the reading thread)
    let addr = base + piece(1);
    let r = thread::spawn(move || {
        // SAFETY: none intended: the read is expected to fault.
        unsafe { core::ptr::read_volatile(addr as *const u8) as i32 + 1000 }
    })
    .map_err(|e| format!("spawn {e}"))?
    .join();
    ensure(r == 3, || {
        format!("reading a decommitted page: exit {r}, want 3 (KERN-EXEC 3)")
    })?;
    // even pieces kept their data; a recommitted piece holds none of the old data
    // SAFETY: piece 2 is committed.
    let kept = unsafe { core::ptr::read_volatile((base + piece(2) + 12345) as *const u8) };
    let c = sys::chunk_adjust(h, TChunkAdjust::Commit, piece(1) as i32, MIB as i32);
    // SAFETY: piece 1 was committed again just now (if c == 0).
    let fresh = c == 0
        && unsafe { core::slice::from_raw_parts((base + piece(1)) as *const u8, MIB) }
            .iter()
            .all(|&b| b != 0xAB);
    sys::handle_close(h);
    ensure(kept == 0xAB && fresh, || {
        format!("kept {kept:#x}, recommit {c}, fresh {fresh}")
    })
}

// ---- RNG -----------------------------------------------------------------------------------------------------------

/// `Math::RandomL`'s source: seeded after boot (interrupt timing; the phone's PRNG too), two draws differ.
fn secure_random() -> Outcome {
    let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
    let ra = sys::math_secure_random(&mut a);
    let rb = sys::math_secure_random(&mut b);
    ensure(ra == 0 && rb == 0 && a != b && a != [0; 32], || {
        format!("results {ra} {rb}, a {a:02x?}")
    })
}
