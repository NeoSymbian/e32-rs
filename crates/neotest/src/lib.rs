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
    ("timer_high_res", high_res_timer_waits),
    ("async", async_joins),
    ("threads", threads_share_a_mutex),
    ("semaphore", semaphores_ping_pong),
    ("fs", file_round_trip),
    ("fs_ops", file_operations),
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
    })?;
    // the fast counter measures a 100 ms sleep as 100 ms, give or take a tick
    let hz = e32::time::fast_counter_hz();
    let c0 = e32::time::fast_counter();
    sleep(100_000);
    let us = e32::time::counter_us(c0, e32::time::fast_counter(), hz);
    dprintln!("neotest: fast counter {} Hz, 100 ms = {} us", hz, us);
    ensure(hz > 0 && (80_000..=150_000).contains(&us), || {
        format!("fast counter {hz} Hz measured 100 ms as {us} us")
    })
}

/// Zero and short waits reuse one thread-owned high-resolution timer.
fn high_res_timer_waits() -> Outcome {
    let mut timer = e32::time::Timer::new().map_err(|e| format!("high-res timer create {e}"))?;
    ensure(timer.wait_high_res(-1) == Err(err::KERR_ARGUMENT), || {
        "high-res timer accepted a negative interval".into()
    })?;
    timer
        .wait_high_res(0)
        .map_err(|e| format!("zero wait {e}"))?;
    let hz = e32::time::fast_counter_hz();
    let c0 = e32::time::fast_counter();
    for _ in 0..20 {
        timer
            .wait_high_res(1_000)
            .map_err(|e| format!("high-res timer wait {e}"))?;
    }
    let us = e32::time::counter_us(c0, e32::time::fast_counter(), hz);
    dprintln!("neotest: high-res 20 x 1 ms = {} us", us);
    // A broad completion/reuse check; this does not establish timer precision under emulator load.
    ensure(hz > 0 && (10_000..=1_000_000).contains(&us), || {
        format!("20 high-res waits measured {us} us at {hz} Hz")
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

/// Positional I/O, metadata and replacement use the real F32 wire ABI.
fn file_operations() -> Outcome {
    const DIR: &str = "C:\\neofsops\\";
    const PATH: &str = "C:\\neofsops\\first.tmp";
    const FINAL: &str = "C:\\neofsops\\final.txt";
    const MODIFIED: i64 = 63_000_000_000_000_000; // a whole FAT two-second interval
    let fs = Fs::connect().map_err(|e| format!("connect {e}"))?;
    let _ = fs.delete(PATH);
    let _ = fs.delete(FINAL);
    let _ = fs.rmdir(DIR);
    fs.mkdir(DIR).map_err(|e| format!("mkdir {e}"))?;
    {
        let f =
            File::create(&fs, PATH, e32::fs::mode::BIG_FILE).map_err(|e| format!("create {e}"))?;
        ensure(
            matches!(File::create(&fs, PATH, 0), Err(err::KERR_ALREADY_EXISTS)),
            || "exclusive create did not report already-exists".into(),
        )?;
        let mut buf = [0u8; 8];
        ensure(
            f.read_at64(u64::MAX, &mut buf) == Err(err::KERR_ARGUMENT)
                && f.write_at64(u64::MAX, b"x") == Err(err::KERR_ARGUMENT)
                && f.set_size64(u64::MAX) == Err(err::KERR_ARGUMENT),
            || "unsigned offsets above TInt64 were accepted".into(),
        )?;
        ensure(f.set_att(1, 1) == Err(err::KERR_ARGUMENT), || {
            "overlapping attribute masks were accepted".into()
        })?;
        // RFile64 writes past EOF append at EOF rather than creating a hole. Reserve the range before testing
        // out-of-order positional writes, just as a native RFile caller must.
        f.set_size64(6).map_err(|e| format!("reserve size: {e}"))?;
        f.write_at64(3, b"XYZ")
            .map_err(|e| format!("write at 3: {e}"))?;
        f.write_at64(0, b"abc")
            .map_err(|e| format!("write at 0: {e}"))?;
        f.flush().map_err(|e| format!("flush {e}"))?;
        let n = f
            .read_at64(0, &mut buf)
            .map_err(|e| format!("read64 {e}"))?;
        ensure(&buf[..n] == b"abcXYZ", || {
            format!("positional read: {buf:?}")
        })?;
        let n = f
            .read_at64(1 << 31, &mut buf)
            .map_err(|e| format!("high read {e}"))?;
        ensure(n == 0, || format!("read beyond EOF: {n}"))?;
        f.set_size64(3).map_err(|e| format!("truncate {e}"))?;
        f.set_modified(MODIFIED)
            .map_err(|e| format!("set modified {e}"))?;
        f.set_att(e32::fs::att::HIDDEN, 0)
            .map_err(|e| format!("set hidden {e}"))?;
        f.flush().map_err(|e| format!("flush truncated {e}"))?;
    }
    let entry = fs.entry(PATH).map_err(|e| format!("entry {e}"))?;
    ensure(
        entry.size == 3 && entry.modified == MODIFIED && entry.att & e32::fs::att::HIDDEN != 0,
        || format!("metadata after flush: {entry:?}"),
    )?;
    {
        let old = File::replace(&fs, FINAL, 0).map_err(|e| format!("destination {e}"))?;
        old.write(b"old").map_err(|e| format!("old content {e}"))?;
        old.flush().map_err(|e| format!("old flush {e}"))?;
    }
    let all = fs.read_dir(DIR).map_err(|e| format!("list {e}"))?;
    let mut paged = fs
        .read_dir_page(DIR, 0, 1)
        .map_err(|e| format!("first page {e}"))?;
    paged.extend(
        fs.read_dir_page(DIR, 1, 1)
            .map_err(|e| format!("second page {e}"))?,
    );
    ensure(all.len() == 2 && all == paged, || {
        format!("paged listing: {all:?} versus {paged:?}")
    })?;
    ensure(
        fs.read_dir_page(DIR, 2, 1)
            .map_err(|e| format!("last page {e}"))?
            .is_empty(),
        || "page beyond the directory end was nonempty".into(),
    )?;
    ensure(
        fs.read_dir_page(DIR, 0, 0)
            .map_err(|e| format!("empty page {e}"))?
            .is_empty()
            && fs.read_dir_page(DIR, 0, 257) == Err(err::KERR_ARGUMENT),
        || "page size validation".into(),
    )?;
    // More than two 1024-byte server batches: exercise skipping and limiting across batch boundaries.
    for i in 0..40 {
        let path = format!("{DIR}page{i:02}.tmp");
        File::create(&fs, &path, 0).map_err(|e| format!("page fixture {i}: {e}"))?;
    }
    let all = fs.read_dir(DIR).map_err(|e| format!("batch list {e}"))?;
    let mut paged = Vec::new();
    for offset in (0..all.len()).step_by(7) {
        let page = fs
            .read_dir_page(DIR, offset as u32, 7)
            .map_err(|e| format!("batch page {offset}: {e}"))?;
        ensure(page.len() <= 7, || "page exceeded its bound".into())?;
        paged.extend(page);
    }
    ensure(all.len() == 42 && all == paged, || {
        format!(
            "batch pagination: {} versus {} entries",
            all.len(),
            paged.len()
        )
    })?;
    let late = fs
        .read_dir_page(DIR, 25, 7)
        .map_err(|e| format!("late page {e}"))?;
    ensure(late == all[25..32], || "skip across server batches".into())?;
    for i in 0..40 {
        fs.delete(&format!("{DIR}page{i:02}.tmp"))
            .map_err(|e| format!("page cleanup {i}: {e}"))?;
    }
    fs.replace_file(PATH, FINAL)
        .map_err(|e| format!("replace-file {e}"))?;
    let f = File::open(&fs, FINAL, 0).map_err(|e| format!("reopen {e}"))?;
    let mut buf = [0u8; 8];
    let n = f
        .read_at64(0, &mut buf)
        .map_err(|e| format!("read replacement {e}"))?;
    ensure(&buf[..n] == b"abc", || "replacement contents".into())?;
    drop(f);
    fs.delete(FINAL).map_err(|e| format!("cleanup file {e}"))?;
    fs.rmdir(DIR).map_err(|e| format!("cleanup directory {e}"))
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
