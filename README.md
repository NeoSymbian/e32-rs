# e32-rs

Rust for the Symbian EKA2 user side, part of [NeoSymbian](https://github.com/NeoSymbian).

The crates here build `no_std` Rust for the 2010 Symbian user-side ABI: 32-bit ARMv5TE with soft-float. The same
binaries run on two platforms:
- **now**, the 2010 ARM user side on QEMU Syborg, through
  [NeoSymbian/symbian-2010](https://github.com/NeoSymbian/symbian-2010);
- **later**, the Google Pixel 3a XL, once the [AArch64 kernel port](https://github.com/NeoSymbian/symbian-pixel)
  boots the user side.

The full plan is in [rust-integration.md](https://github.com/NeoSymbian/symbian-pixel/blob/main/docs/rust-integration.md).

## Status

**R2 is done: the `e32` core.** Rust programs on the 2010 Symbian user side now have:
- a heap behind `alloc`, in a private chunk that grows on demand;
- request waiting, timers, and a small async runtime that replaces Symbian's active objects;
- threads, mutexes and semaphores;
- client-server IPC, with a Rust file-server client.

They also load from the C: drive, not only the ROM. The on-device suite `NEOTEST.EXE`, on QEMU Syborg:

```
test alloc ... ok          (Vec, String, Box, and a 2 MB block, so the heap grew past its first 64 KB)
test timer ... ok          (100 ms = 11 ticks, 200 ms = 20 ticks)
test async ... ok          (two 100 ms delays joined in block_on take 11 ticks, not 22)
test threads ... ok        (4 threads × 250 increments under a kernel mutex = 1000; exit codes 10..13)
test semaphore ... ok      (10 rounds of ping-pong between two threads)
test fs ... ok             (C:\neotest.txt written, read back, sized and reopened through the file server)
neotest: 6 passed, 0 failed
```

R1 (`NEOR1.EXE` exits 42; `NEOPANIC.EXE` panics with category `RUST`) and R0 (`NEOHELLO.EXE`) still run.
`NEOR1C.EXE` is `neor1` installed on C:. Next is **R3**, an IPC server written in Rust.

## Crates

| Crate | What |
|---|---|
| `e32-sys` | Raw executive calls. Numbers come from `src/execs.rs`, generated from the kernel's `execs.txt` by `tools/gen_execs.py`; all 315 match the 2010 `genexec.pl`. Also here: the kernel ABI types (`TRequestStatus`, `TChunkCreate`, the 64-byte thread create info, the `TPtrC8`/`TPtr8`/`TPtrC16` descriptors, `TIpcArgs`, `TVersion`) and the `SessionSendSync` stub, which passes extended arguments in r4–r8 like `uc_exec.cia` |
| `e32` | The program-facing API. `dprintln!`; `process::{exit, panic}`; `e32::main!` (`E32Main`, heap set-up, panic handler with category `RUST`). `heap`: the global allocator. `request::wait_for_request`. `time::{Timer, sleep, delay, ticks}`. `rt::{block_on, join}`. `thread::spawn`/`join`. `sync::{Mutex, Semaphore}`. `ipc::Session`. `fs::{Fs, File}` |
| `neotest` | The R2 on-device test suite |
| `neor1`, `neopanic` | R1 test programs |
| `rust-hello` | R0 spike, called from C++ glue |

The EXEs are listed in `symbian/exes.txt` (name, uid3, staticlib, optional `drive=c`), which symbian-2010's
`emitter/build_rust_exe.py` reads.

## Building

`rust-toolchain.toml` pins a nightly with `rust-src`. `armv5te-none-eabi` is a tier 3 target, so `core` is built from
source.

```sh
cargo arm      # Symbian staticlibs -> target/armv5te-none-eabi/release/lib*.a
cargo test -p e32-sys -p e32      # host unit tests
python3 tools/gen_execs.py ../oss.FCL.sf.os.kernelhwsrv/kernel/eka/kernel/execs.txt   # regenerate execs.rs
```

To run them on Syborg (needs Docker; the CI in `.github/workflows/syborg.yml` does exactly this):

```sh
git clone https://github.com/NeoSymbian/symbian-2010 && cd symbian-2010
mkdir -p rust && cp ../e32-rs/target/armv5te-none-eabi/release/lib*.a ../e32-rs/symbian/exes.txt rust/
./fetch.sh && ./build.sh && RUN=neohello,neor1,neopanic,neotest,neor1c SECS=200 ./boot.sh   # -> out/serial.log, out/fb.png
```

**ABI rule:** never use an `eabihf` target. The Symbian user side passes floating-point values in integer registers;
`eabihf` passes them in VFP registers and breaks every call into a Symbian library.

## License

Apache-2.0.
