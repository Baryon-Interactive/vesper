# Vesper malformed-input harnesses

This directory contains file-input harnesses for manifest decoding, pack-frame
parsing and path validation. See the [project overview](../README.md) and
[testing guide](../docs/testing.md).

Compile independently from the root workspace:

```powershell
cargo check --manifest-path fuzz/Cargo.toml
cargo run --manifest-path fuzz/Cargo.toml --bin manifest -- ./tmp/manifest-input
cargo run --manifest-path fuzz/Cargo.toml --bin frame -- ./tmp/frame-input
cargo run --manifest-path fuzz/Cargo.toml --bin path -- ./tmp/path-input
```

Create an input file first; `path` reads UTF-8 text, while `manifest` and `frame`
read bytes. These are ordinary stable-Rust replay executables, not registered
libFuzzer/cargo-fuzz targets. A fuzzing driver can supply mutated input files.
Bounds and malformed-input cases also run in the `vesper-format` unit tests.
The local target directory and generated harness lockfile are ignored.
