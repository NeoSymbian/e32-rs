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

**R0 is done: Rust runs inside a real Symbian EXE.** `crates/rust-hello` is linked with a small C++ `E32Main`
(`toolchain/rust/neo_glue.cpp` in symbian-2010) into `NEOHELLO.EXE` in the ROM. Typed into ESHELL on Syborg, it
prints:

```
[proc] create NEOHELLO.EXE
neo: start
hello from Rust
neo: rust_hello(6, 7) = 42
```

Next is **R1**: an EXE written entirely in Rust, with exec calls generated from the kernel's `execs.txt`.

## Building

`rust-toolchain.toml` pins a nightly with `rust-src`. `armv5te-none-eabi` is a tier 3 target, so `core` is built from
source.

```sh
cargo build --release      # -> target/armv5te-none-eabi/release/librust_hello.a
```

To run it on Syborg (needs Docker; see the CI in `.github/workflows/syborg.yml`):

```sh
git clone https://github.com/NeoSymbian/symbian-2010 && cd symbian-2010
mkdir -p rust && cp ../e32-rs/target/armv5te-none-eabi/release/librust_hello.a rust/
./fetch.sh && ./build.sh && RUN=neohello ./boot.sh   # -> out/serial.log, out/fb.png
```

**ABI rule:** never use an `eabihf` target. The Symbian user side passes floating-point values in integer registers;
`eabihf` passes them in VFP registers and breaks every call into a Symbian library.

## License

Apache-2.0.
