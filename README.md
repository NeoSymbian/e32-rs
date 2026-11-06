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

**R1 is done: Symbian EXEs written in Rust only.** They talk to the kernel through executive calls (`swi`), print,
exit with their own code, and turn a Rust `panic!` into a Symbian panic. ESHELL on Syborg:

```
C:\>neor1                                   serial log
Exit type 0,42,Kill                         r1: hello from pure Rust
C:\>neopanic                                r1: 6 * 7 = 42
Exit type 2,0,RUST                          [EKERN] exit 'NEOR1.EXE[e0001002]0001::Main' reason=0x0000002a
C:\>                                        neopanic: about to panic
                                            rust panic: deliberate panic 7 at crates/neopanic/src/lib.rs:10
                                            [EKERN] panic in 'NEOPANIC.EXE[e0001003]0001::Main' category='RUST' ...
```

R0, a C++ `E32Main` calling Rust (`rust-hello`, `NEOHELLO.EXE`), still runs as well. Next is **R2**: a memory
allocator, threads, timers, and a Rust file-server client.

## Crates

| Crate | What |
|---|---|
| `e32-sys` | Raw executive calls. Numbers come from `src/execs.rs`, generated from the kernel's `execs.txt` by `tools/gen_execs.py`; all 315 match the 2010 `genexec.pl`. The `swi` wrapper and the ABI types (`TPtrC8`, `TExitType`, handles) are here too. |
| `e32` | The program-facing API: `dprintln!` (like `RDebug::Printf`, 256-byte buffer), `process::exit`, `process::panic`, and `e32::main!(fn)`, which makes a `fn() -> i32` the EXE's `E32Main` and installs the panic handler (category `RUST`) |
| `neor1`, `neopanic` | R1 test programs |
| `rust-hello` | R0 spike, called from C++ glue |

The EXEs are listed in `symbian/exes.txt` (name, uid3, staticlib), which symbian-2010's `emitter/build_rust_exe.py`
reads.

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
./fetch.sh && ./build.sh && RUN=neohello,neor1,neopanic ./boot.sh   # -> out/serial.log, out/fb.png
```

**ABI rule:** never use an `eabihf` target. The Symbian user side passes floating-point values in integer registers;
`eabihf` passes them in VFP registers and breaks every call into a Symbian library.

## License

Apache-2.0.
