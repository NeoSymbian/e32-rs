// SPDX-License-Identifier: Apache-2.0
//! R2/R3 on-target test suite (`NEOTEST.EXE`): runs on the 2010 Symbian user side and reports over the debug port,
//! `test <name> ... ok|FAILED: <why>` per test and `neotest: N passed, M failed` at the end. Exit code = failures.
#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use e32::dprintln;
use e32::fs::{File, Fs};
use e32::ipc::{Session, TIpcArgs, TVersion};
use e32::server::{Server, serve};
use e32::sync::{Mutex, Semaphore};
use e32::sys::{TPtr8, TPtr16, TPtrC8, TPtrC16, err};
use e32::time::{delay, sleep, ticks};
use e32::{rt, thread};
use neoecho::op;

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
    ("fs_dirs", directories_and_drives),
    ("ipc_server", ipc_server),
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

/// Drives, volume size, listings (`Z:\\sys\\bin\\` too), and directory changes on C:: make, list, rename, delete.
fn directories_and_drives() -> Outcome {
    const DIR: &str = "C:\\neotest8\\";
    let fs = Fs::connect().map_err(|e| format!("connect {e}"))?;
    let drives = fs.drives().map_err(|e| format!("drives {e}"))?;
    ensure(drives[2] != 0 && drives[25] != 0, || {
        format!("C: {} Z: {}", drives[2], drives[25])
    })?;
    let c = fs.volume(2).map_err(|e| format!("volume C: {e}"))?;
    ensure(c.size > 0 && c.free > 0 && c.free <= c.size, || {
        format!("C: size {} free {}", c.size, c.free)
    })?;
    let bin = fs
        .read_dir("Z:\\sys\\bin\\")
        .map_err(|e| format!("list Z:\\sys\\bin {e}"))?;
    ensure(bin.len() > 10, || format!("Z:\\sys\\bin has {}", bin.len()))?;
    ensure(
        bin.iter().any(|e| e.name.eq_ignore_ascii_case("euser.dll")),
        || "no euser.dll in Z:\\sys\\bin".into(),
    )?;
    dprintln!(
        "neotest: Z:\\sys\\bin has {} entries, C: {} of {} bytes free",
        bin.len(),
        c.free,
        c.size
    );

    // a leftover from an interrupted run
    let _ = fs.delete("C:\\neotest8\\b.txt");
    let _ = fs.delete("C:\\neotest8\\a.txt");
    let _ = fs.rmdir(DIR);
    fs.mkdir(DIR).map_err(|e| format!("mkdir {e}"))?;
    ensure(fs.mkdir(DIR) == Err(err::KERR_ALREADY_EXISTS), || {
        "mkdir twice".into()
    })?;
    File::replace(&fs, "C:\\neotest8\\a.txt", e32::fs::mode::SHARE_EXCLUSIVE)
        .and_then(|f| f.write(b"12345"))
        .map_err(|e| format!("write {e}"))?;
    let list = fs.read_dir(DIR).map_err(|e| format!("list {e}"))?;
    ensure(
        list.len() == 1 && list[0].name == "a.txt" && list[0].size == 5 && !list[0].is_dir(),
        || format!("listing {list:?}"),
    )?;
    let root = fs.read_dir("C:\\").map_err(|e| format!("list C: {e}"))?;
    ensure(
        root.iter().any(|e| e.name == "neotest8" && e.is_dir()),
        || "C:\\neotest8 not listed as a directory".into(),
    )?;
    fs.rename("C:\\neotest8\\a.txt", "C:\\neotest8\\b.txt")
        .map_err(|e| format!("rename {e}"))?;
    ensure(fs.rmdir(DIR) == Err(err::KERR_IN_USE), || {
        "rmdir of a non-empty dir".into()
    })?;
    fs.delete("C:\\neotest8\\b.txt")
        .map_err(|e| format!("delete {e}"))?;
    fs.rmdir(DIR).map_err(|e| format!("rmdir {e}"))?;
    let gone = fs.read_dir(DIR);
    ensure(gone.is_err(), || format!("{DIR} still lists: {gone:?}"))?;
    let z = fs.mkdir("Z:\\neotest8\\");
    ensure(z == Err(err::KERR_ACCESS_DENIED), || {
        format!("mkdir on Z: gave {z:?}")
    })
}

/// A Rust IPC server (NeoEcho on a thread) driven by a Rust client: version refusal, two concurrent sessions with
/// their own state, 8- and 16-bit descriptor reads and writes, overflow, an unknown function, and the server ending
/// when the last session closes.
fn ipc_server() -> Outcome {
    const NAME: &str = "neotest.echo";
    let ready: &'static Semaphore =
        Box::leak(Box::new(Semaphore::new(0).map_err(|e| format!("sem {e}"))?));
    let server = thread::spawn(move || {
        let server = match Server::create(NAME) {
            Ok(s) => s,
            Err(e) => {
                ready.signal();
                return e;
            }
        };
        ready.signal();
        match rt::block_on(serve(&server, &mut neoecho::Echo::default())) {
            Ok(()) => 0,
            Err(e) => e,
        }
    })
    .map_err(|e| format!("spawn {e}"))?;
    ensure(ready.wait_timeout(2_000_000), || {
        "server never started".into()
    })?;

    let v = neoecho::VERSION;
    let old = TVersion { major: 9, ..v };
    let refused = Session::connect(NAME, old).err();
    ensure(refused == Some(err::KERR_NOT_SUPPORTED), || {
        format!("version 9 connect gave {refused:?}")
    })?;
    let a = Session::connect(NAME, v).map_err(|e| format!("connect a {e}"))?;
    let b = Session::connect(NAME, v).map_err(|e| format!("connect b {e}"))?;

    let mut args = TIpcArgs::new();
    args.int(0, 40).int(1, 2);
    let r = a.send_receive(op::ADD, &args);
    ensure(r == Ok(42), || format!("ADD gave {r:?}"))?;

    let mut out = [0u8; 16];
    let mut outd = TPtr8::new(&mut out);
    let input8 = TPtrC8::new(b"symbian");
    let mut args = TIpcArgs::new();
    args.desc8(0, &input8).des8(1, &mut outd);
    let r = b.send_receive(op::REVERSE8, &args);
    let n = outd.len();
    ensure(r == Ok(7) && &out[..n] == b"naibmys", || {
        format!("REVERSE8 gave {r:?}, {:?}", &out[..n])
    })?;

    let input: Vec<u16> = "Neo-3".encode_utf16().collect();
    let mut wide = [0u16; 8];
    let mut wided = TPtr16::new(&mut wide);
    let input16 = TPtrC16::new(&input);
    let mut args = TIpcArgs::new();
    args.desc16(0, &input16).des16(1, &mut wided);
    let r = b.send_receive(op::UPPER16, &args);
    let n = wided.len();
    let want: Vec<u16> = "NEO-3".encode_utf16().collect();
    ensure(r == Ok(5) && wide[..n] == want[..], || {
        format!("UPPER16 gave {r:?}, {:?}", &wide[..n])
    })?;

    let mut small = [0u8; 4];
    let mut smalld = TPtr8::new(&mut small);
    let mut args = TIpcArgs::new();
    args.des8(0, &mut smalld).int(1, 0);
    let r = a.send_receive(op::WRITE_AT, &args);
    let n = smalld.len();
    ensure(r == Ok(0) && &small[..n] == b"xyz", || {
        format!("WRITE_AT 0 gave {r:?}, {:?}", &small[..n])
    })?;
    let mut smalld = TPtr8::new(&mut small);
    let mut args = TIpcArgs::new();
    args.des8(0, &mut smalld).int(1, 2);
    let r = a.send_receive(op::WRITE_AT, &args);
    ensure(r == Err(err::KERR_OVERFLOW), || {
        format!("WRITE_AT 2 gave {r:?}")
    })?;

    let r = a.send_receive(99, &TIpcArgs::new());
    ensure(r == Err(err::KERR_NOT_SUPPORTED), || {
        format!("function 99 gave {r:?}")
    })?;
    // a: ADD, WRITE_AT x2, 99, COUNT = 5; b: REVERSE8, UPPER16, COUNT = 3.
    let counts = (
        a.send_receive(op::COUNT, &TIpcArgs::new()),
        b.send_receive(op::COUNT, &TIpcArgs::new()),
    );
    ensure(counts == (Ok(5), Ok(3)), || {
        format!("COUNT gave {counts:?}")
    })?;

    drop(a);
    drop(b);
    let code = server.join();
    ensure(code == 0, || format!("server thread ended with {code}"))
}
