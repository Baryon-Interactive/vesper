# Vesper

Vesper is Baryon Interactive's game distribution platform with no fixed hosting cost.
Game builds default to GitHub Releases, with optional R2 mirrors or Hugging Face storage; a signed
index repository on GitHub provides the catalogue. Clients verify metadata and
download only the pack sections needed for an install or update. Hosting remains
subject to each provider's quotas and terms.

Status: **pre-release**. The Vesper/1 format and interfaces may change. The project is
licensed under Apache-2.0.

## Download

Players download the Vesper client from the latest Release: the Windows
installer (`Vesper-Setup-<version>-windows-x64.exe`), the macOS disk image
(`Vesper-<version>-macos-universal.dmg`, Apple Silicon and Intel) or the Linux
AppImage or `.deb` (`Vesper-<version>-linux-x64.*`). Developers download the
`vesper` command-line tool (`vesper-cli-*`) for Linux, Windows or macOS. The CLI
is a command-line tool, not the player app.

## How it works

```text
Developer: vesper pack -> vesper publish
                           | release assets + signed targets
                           v
                      Developer metadata Release
                           | Central aggregation verifies registered keys and store bindings
                           v
                      Sign snapshot/timestamp -> deploy Pages
                           |
Client: verify root -> routes -> targets + snapshot/timestamp
        -> download required pack ranges -> atomic file installation/update
        -> journal recovery or rollback
```

## Features

- Incremental updates: content-addressed chunks reuse packs across versions.
- Multiple sources: GitHub, optional R2, Hugging Face and HTTP, with throughput probes, adaptive primary selection and direct-link refresh.
- Signed metadata: threshold Ed25519 root/routes/targets and online snapshot/timestamp roles.
- Paid builds: HKDF-derived AES-256-GCM encryption, with DepotKeys delivered through a gateway.
- Installation journals: stage changed files, recover interrupted transactions and roll back backups.
- User files: declared path globs survive updates, repairs and ordinary uninstall.

## Repository layout

| Directory | Purpose |
| --- | --- |
| [crates/](crates/) | Rust core and CLI |
| [crates/vesper-client/](crates/vesper-client/) | Window-independent client logic shared by UI and headless callers |
| [launcher/](launcher/) | Tauri desktop launcher; local demo tooling in `dev/` |
| [gateway/](gateway/) | Cloudflare Workers paid-game key gateway |
| [index-template/](index-template/) | Template for a catalogue index repository |
| [docs/](docs/) | Architecture, CLI, publishing, releases and testing |
| [scripts/](scripts/README.md) | Build, release and test scripts |
| [experiments/](experiments/README.md) | Patch-size and download measurements |
| [fuzz/](fuzz/) | Malformed-input harnesses |
| [skills/](skills/) | Agent publishing skill |

## Quick start

Use the Rust `stable` toolchain declared in [rust-toolchain.toml](rust-toolchain.toml),
Node.js 22 or later for JavaScript components, and Python 3 for measurement tools.
The desktop launcher builds on Windows with the Tauri prerequisites, WebView2,
and Visual Studio C++ Build Tools. Open an x64 developer PowerShell for MSVC builds.
Docker Desktop in Linux-container mode is needed for Linux release binaries;
see [releases](docs/releases.md) for the release script's pinned toolchains.

From the repository root, this PowerShell example creates a small build and
installs it entirely locally, without credentials or a catalogue:

```powershell
cargo build -p vesper-cli
if ($LASTEXITCODE -ne 0) { throw 'CLI build failed' }
$vesper = (Resolve-Path ./target/debug/vesper.exe).Path
$scratch = Join-Path $PWD ('tmp/quick-start-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path "$scratch/build" | Out-Null
Set-Content -Encoding UTF8 -Path "$scratch/build/hello.txt" -Value 'Hello from Vesper'
Push-Location $scratch
try {
  & $vesper pack --input ./build --output ./dist --app-id demo --platform windows-x64 --version 1.0.0
  if ($LASTEXITCODE -ne 0) { throw 'Pack failed' }
  & $vesper install --manifest ./dist/manifest.cbor.zst --source ./dist --target ./library/demo
  if ($LASTEXITCODE -ne 0) { throw 'Install failed' }
  & $vesper verify --manifest ./dist/manifest.cbor.zst --target ./library/demo
  if ($LASTEXITCODE -ne 0) { throw 'Verify failed' }
} finally { Pop-Location }
```

For larger deterministic v1/v2 builds, use [make-synth-build.py](scripts/make-synth-build.py).
See [the CLI reference](docs/cli.md) for HTTP sources, encryption and updates.

## Desktop launcher

Run `powershell -ExecutionPolicy Bypass -File launcher/dev/start.ps1`, then serve
`launcher/dev/out` with `launcher/dev/server.py` on ports 3210 and 8787 in separate
terminals. Build with `launcher/dev/build-exe.ps1` and run
`launcher/dev/out/bin/vesper-launcher.exe`. The automated local tour is
`powershell -ExecutionPolicy Bypass -File launcher/dev/tour.ps1`.
See [launcher setup and demo instructions](launcher/README.md).

## Publishing a game

Initialize a developer identity, pack a build, and publish release assets plus
signed targets into your own metadata Release. A single registration PR adds
your public key and `metadata_url` to the central registry. Aggregation runs
every 15 minutes; subsequent releases need no app PR. Repository-hosted metadata
is also supported. Updates can reuse packs from earlier releases. Optional R2
mirrors include every channel manifest and all referenced packs and use environment-only credentials; see
[R2 setup](docs/publishing.md#optional-r2-mirror).
Follow [the publishing guide](docs/publishing.md), including key registration.

## Running your own catalogue

Start with [index-template](index-template/README.md). Configure offline root
trust, developer routes, online signing secrets, a verified CLI binary pin and
Pages deployment before accepting submissions. The example catalogue is a
layout example, not a production trust root.

## Development

Run these from the repository root after `npm ci` in `launcher` and `gateway`:

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --manifest-path launcher/src-tauri/Cargo.toml
cargo clippy --manifest-path launcher/src-tauri/Cargo.toml --all-targets -- -D warnings
npm --prefix launcher test
# Run TypeScript in its component directory:
Push-Location launcher; npx tsc --noEmit; Pop-Location
node --test index-template/test/
npm --prefix gateway test
npm --prefix gateway run typecheck
```

Build the frontend with `npm --prefix launcher run build` before the launcher
Rust checks on a fresh checkout. [Testing](docs/testing.md) describes the native
tour, live read-only checks, experiment tests and measured results.
[Architecture](docs/architecture.md) describes data flow and trust boundaries.

## License

Vesper is licensed under the [Apache License 2.0](LICENSE); see [NOTICE](NOTICE).
The Vesper name and logo are not covered by the license. Third-party dependencies
keep their own licenses; see [third-party licenses](docs/third-party-licenses.md).
