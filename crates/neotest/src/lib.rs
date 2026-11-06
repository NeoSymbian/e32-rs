// SPDX-License-Identifier: Apache-2.0
//! R2 on-target test suite (`NEOTEST.EXE`): runs on the 2010 Symbian user side and reports over the debug port,
//! `test <name> ... ok|FAILED: <why>` per test and `neotest: N passed, M failed` at the end. Exit code = failures.
#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use e32::dprintln;
use e32::fs::{File, Fs};
use e32::sync::{Mutex, Semaphore};
use e32::time::{delay, sleep, ticks};
use e32::{rt, thread};

e32::main!(main);

type Outcome = Result<(), String>;
type Test = (&'static str, fn() -> Outcome);

fn ensure(ok: bool, why: impl FnOnce() -> String) -> Outcome {
    if ok { Ok(()) } else { Err(why()) }
}

const TESTS: &[Test] = &[
    ("alloc", alloc_works),
    ("timer", timer_waits),
    ("async", async_joins),
    ("threads", threads_share_a_mutex),
    ("semaphore", semaphores_ping_pong),
    ("fs", file_round_trip),
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
        "neotest: {} passed, {} failed",
        TESTS.len() as i32 - failed,
        failed
    );
    failed
}

/// The heap: small and large allocations, and growth past the first 64 KB commit.
fn alloc_works() -> Outcome {
    let v: Vec<u32> = (0..10_000).collect();
    ensure(v.iter().sum::<u32>() == 49_995_000, || "vec sum".into())?;
    let s = format!("{}-{}", "neo", 42);
    ensure(s == "neo-42", || format!("format gave {s}"))?;
    let b = Box::new([7u8; 1000]);
    ensure(b.iter().all(|&x| x == 7), || "box contents".into())?;
    let mut big = alloc::vec![0u8; 2 << 20];
    for (i, x) in big.iter_mut().enumerate().step_by(4096) {
        *x = (i >> 12) as u8;
    }
    let sum: u32 = big.iter().step_by(4096).map(|&x| x as u32).sum();
    ensure(sum == (0..512u32).map(|i| i & 0xFF).sum(), || {
        format!("2 MB block sum {sum}")
    })
}

/// A blocking sleep takes time, and twice as long takes about twice the ticks.
fn timer_waits() -> Outcome {
    let t0 = ticks();
    sleep(100_000);
    let d1 = ticks().wrapping_sub(t0);
    let t1 = ticks();
    sleep(200_000);
    let d2 = ticks().wrapping_sub(t1);
    dprintln!("neotest: 100 ms = {} ticks, 200 ms = {} ticks", d1, d2);
    ensure(d1 > 0 && d2 >= d1 + d1 / 2, || {
        format!("100 ms = {d1} ticks, 200 ms = {d2} ticks")
    })
}

/// Two 100 ms delays joined in the executor run concurrently: well under the 200 ms they take one after the other.
fn async_joins() -> Outcome {
    let t0 = ticks();
    rt::block_on(delay(100_000));
    let one = ticks().wrapping_sub(t0);
    let t1 = ticks();
    rt::block_on(rt::join(delay(100_000), delay(100_000)));
    let both = ticks().wrapping_sub(t1);
    dprintln!(
        "neotest: one delay = {} ticks, two joined = {} ticks",
        one,
        both
    );
    ensure(one > 0 && both < one + one / 2, || {
        format!("one = {one} ticks, two joined = {both}")
    })
}

/// Four threads bump one counter behind a kernel mutex; each returns its own exit code.
fn threads_share_a_mutex() -> Outcome {
    let counter: &'static Mutex<u32> =
        Box::leak(Box::new(Mutex::new(0).map_err(|e| format!("mutex {e}"))?));
    let mut handles = Vec::new();
    for i in 0..4 {
        let h = thread::spawn(move || {
            for _ in 0..250 {
                *counter.lock() += 1;
            }
            10 + i
        })
        .map_err(|e| format!("spawn {e}"))?;
        handles.push(h);
    }
    let codes: i32 = handles.into_iter().map(|h| h.join()).sum();
    let total = *counter.lock();
    ensure(total == 1000 && codes == 10 + 11 + 12 + 13, || {
        format!("counter {total}, exit codes sum {codes}")
    })
}

/// Two threads hand control back and forth ten times through two semaphores.
fn semaphores_ping_pong() -> Outcome {
    let ping: &'static Semaphore =
        Box::leak(Box::new(Semaphore::new(0).map_err(|e| format!("sem {e}"))?));
    let pong: &'static Semaphore =
        Box::leak(Box::new(Semaphore::new(0).map_err(|e| format!("sem {e}"))?));
    let h = thread::spawn(move || {
        for _ in 0..10 {
            ping.wait();
            pong.signal();
        }
        7
    })
    .map_err(|e| format!("spawn {e}"))?;
    for round in 0..10 {
        ping.signal();
        ensure(pong.wait_timeout(2_000_000), || {
            format!("no pong in round {round}")
        })?;
    }
    let code = h.join();
    ensure(code == 7, || format!("thread exit code {code}"))
}

/// Write a file on C: through the file server, read it back, check its size; reopen and read again.
fn file_round_trip() -> Outcome {
    const PATH: &str = "C:\\neotest.txt";
    const DATA: &[u8] = b"hello from Rust on Symbian";
    let fs = Fs::connect().map_err(|e| format!("connect {e}"))?;
    {
        let f = File::replace(&fs, PATH, e32::fs::mode::SHARE_EXCLUSIVE)
            .map_err(|e| format!("replace {e}"))?;
        f.write(DATA).map_err(|e| format!("write {e}"))?;
        let mut buf = [0u8; 64];
        let n = f.read_at(0, &mut buf).map_err(|e| format!("read {e}"))?;
        ensure(&buf[..n] == DATA, || format!("read back {n} bytes"))?;
        let size = f.size().map_err(|e| format!("size {e}"))?;
        ensure(size == DATA.len() as i64, || format!("size {size}"))?;
    }
    let f = File::open(&fs, PATH, e32::fs::mode::SHARE_READERS_ONLY)
        .map_err(|e| format!("open {e}"))?;
    let mut buf = [0u8; 64];
    let n = f.read_at(0, &mut buf).map_err(|e| format!("reread {e}"))?;
    ensure(&buf[..n] == DATA, || format!("reopened file has {n} bytes"))
}
