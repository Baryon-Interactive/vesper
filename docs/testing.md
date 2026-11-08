# Testing

Run commands from the repository root unless a component directory is stated.
See the [project overview](../README.md) and [script inventory](../scripts/README.md).
Use the toolchain in [rust-toolchain.toml](../rust-toolchain.toml), Node.js 22 or
later, Python 3 and Windows x64 C++ Build Tools for the native launcher. Open a
Visual Studio developer PowerShell before MSVC Cargo commands. Scripts recognize
VS 2019 Build Tools under the standard program-files directory; an alternative
`vcvars64.bat` may be selected with `VESPER_VCVARS64` in the demo/smoke scripts.

## Component checks

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm --prefix launcher ci
npm --prefix launcher run build
cargo test --manifest-path launcher/src-tauri/Cargo.toml
cargo clippy --manifest-path launcher/src-tauri/Cargo.toml --all-targets -- -D warnings
npm --prefix launcher test
Push-Location launcher; npx tsc --noEmit; Pop-Location
node --test index-template/test/
npm --prefix gateway ci
npm --prefix gateway test
npm --prefix gateway run typecheck
python -m unittest discover -s experiments/download-bench -p 'test_*.py'
python -m unittest discover -s experiments/patch-size -p 'test_*.py'
python scripts/check-docs.py
```

On a fresh checkout, build the frontend before native Rust checks because Tauri
embeds `launcher/dist`. Capability `$schema` references are editor hints; Tauri
regenerates `src-tauri/gen/schemas` while building. That directory is ignored and
need not exist in a checkout. Cargo tests and Clippy do not require checked-in
schema files. The [fuzz harnesses](../fuzz/README.md) compile separately.

Root workspace tests use local TCP mocks and isolated install/state trees.
The launcher crate is a Tauri adapter; its Cargo test harness has no independent
client tests. Client tests belong to `vesper-client` in the root workspace.
They cover Range validation, failover, redirect refresh, wire/reuse counters,
chunk caching, encrypted installs, crash boundaries, rollback, user files,
metadata tampering and cross-release packs. Index tests use fixtures, never the
example live catalogue; shell tests require Git Bash and report a skip if absent.
The signing-conflict test uses local Git repositories and the compiled CLI.
Gateway tests use a local Miniflare D1 database with migrations and generated keys.

## Desktop demo and native tour

See [launcher instructions](../launcher/README.md) for manual services and UI.
The tour locates its window by the title `Vesper`; use a desktop without other
Vesper windows so an unrelated session cannot be selected. For a full isolated
local UI run:

```powershell
powershell -ExecutionPolicy Bypass -File launcher/dev/build-exe.ps1
powershell -ExecutionPolicy Bypass -File launcher/dev/tour.ps1
```

The tour also prepares demo content and rebuilds the executable unless
`-SkipSetup` is passed. It starts the index and gateway servers on ports 3210
and 8787, exercises install/update, fresh installation of a cross-release update,
rollback, repair, uninstall and the paid demo, and writes thirteen screenshots
to ignored `launcher/dev/out/screens`. State stays under `out/tour-state`.
`VESPER_DEMO_TOUR=1` is a separate Vite-time automatic tab preview; leave it unset
for the interactive tour and release builds. Screenshots and local automation
are not manual production acceptance.

## Read-only GitHub checks

The smoke script downloads public release assets, installs/verifies, optionally
updates and rolls back. It makes no release or repository writes:

```powershell
powershell -File scripts/github-smoke.ps1 -Owner example -Repo game-builds -Release v1.0.0 -Manifest manifest.cbor.zst
# Optional: -Update <local-path-or-URL-to-v2-manifest> -Rollback
```

It reports elapsed seconds and estimated download bytes, uses a temporary install
and removes it afterwards. Both releases must remain reachable when v2 reuses v1
packs. `-Release` supplies the fallback tag; use an absolute URL for a different
release's update manifest. To contain its temporary files in this checkout, set
`TEMP` and `TMP` to a directory under ignored `tmp` before running it.

The ignored client integration test reads an explicitly selected index API URL:

```powershell
$env:VESPER_REAL_INDEX = 'https://catalogue.example/api'
cargo test -p vesper-client real_github_index_install -- --ignored --nocapture
```

The selected API must contain `ghtest.free` on `stable`, with publicly readable
Vesper/1 content. Unset configuration returns without a network install; that is
not a live pass. Older public fixtures may use an incompatible format. Select a
compatible fixture before running; no external publication is part of local tests.

## Measured results

Local regression tests verify that an update fetches new compressed frames while
reusing verified unchanged local bytes; counters are compared with bytes served
by the HTTP mock. Fresh-install tests resolve retained packs from each original
release rather than assuming all packs belong to the newest release. The native
tour independently exercises a fresh v2 install with v1 packs hosted.

Deterministic packing tests compare repeated outputs for identical input and
parameters. A repeated quick-start pack produced byte-identical manifest and pack
files. In a separate 2 MiB deterministic synthetic file, overwriting 4 KiB with
32/64/128 KiB min/average/max chunks produced 99,531 bytes of new pack containers;
the local update and full installed-file byte comparison passed. This is local
pack output size, not a measured HTTP transfer or a real-game patch ratio.
The quick-start example ran in a newly created temporary directory: packing,
installation and verification succeeded for its 22-byte input file.
These results concern local data and mocks; they do not measure real UE5 patch
size, Internet throughput, deployed payments or public catalogue availability.
Release-binary byte identity additionally depends on the compiler, SDK and linker;
see [release reproducibility](releases.md).

For bounded-memory synthetic patch and download measurements and their limitations,
see [experiments](../experiments/README.md). The larger synthetic e2e script emits
actual transfer bytes, rollback checks and tampered-metadata rejection; its two
512 MiB pak files are intentionally not representative of every real engine build.
