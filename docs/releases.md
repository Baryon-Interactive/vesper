# Building and publishing Vesper

See the [project overview](../README.md), [script inventory](../scripts/README.md)
and [index template](../index-template/README.md).

## CI release flow

Push a tag named `vesper-v<workspace-version>` to the public repository. The
release workflow checks that the tag version matches the workspace, builds the
CLI for Linux, Windows and macOS, builds the player client for Windows (NSIS
installer), macOS (universal disk image) and Linux (AppImage and `.deb`), and
publishes a GitHub Release targeted at the tagged commit. Pull
requests and manual dispatch build the same jobs and upload workflow artifacts;
they never create a Release.

The published assets are:

| Asset | Audience |
| --- | --- |
| `Vesper-Setup-<version>-windows-x64.exe` | Players; Windows client installer |
| `Vesper-<version>-macos-universal.dmg` | Players; macOS client (Apple Silicon and Intel) |
| `Vesper-<version>-linux-x64.AppImage` | Players; portable Linux client |
| `Vesper-<version>-linux-x64.deb` | Players; Linux client package for Debian and Ubuntu |
| `vesper-cli-linux-x64` | Developers and index workflows; static musl CLI |
| `vesper-cli-windows-x64.exe` | Developers; Windows CLI |
| `vesper-cli-macos-arm64` | Developers; unsigned macOS arm64 CLI |
| `vesper-cli-macos-x64` | Developers; unsigned macOS x64 CLI |
| `LICENSE`, `NOTICE` | All users |
| `SHA256SUMS` | Checksums for every other asset |

`SHA256SUMS` uses bare filenames. Verify it after downloading with
`sha256sum -c SHA256SUMS` (or an equivalent SHA-256 tool on Windows). macOS
may quarantine the unsigned CLI; remove that attribute with
`xattr -d com.apple.quarantine <file>` after downloading from a trusted Release.
The Windows installer is unsigned, so SmartScreen may warn before it runs. The
macOS app is ad-hoc signed and not notarized; players open it the first time
with right-click Open, or `xattr -dr com.apple.quarantine /Applications/Vesper.app`.
The CLI is a command-line tool and is separate from the player client.

The workflow's `VESPER_VERSION` must equal the workspace version; bump both
(plus `launcher/package.json` and `tauri.conf.json`) before tagging.

## Local reproducibility check

Run this on Windows with Docker Desktop in Linux-container mode, Git, tar, Rust
`1.99.1-x86_64-pc-windows-msvc`, and the VS 2019 x64 Build Tools:

```powershell
powershell -NoProfile -File scripts/build-vesper-release.ps1 -Version 0.7.0 -OutDir ./target/release-out
```

The script rejects a dirty tree unless `-AllowDirty` is supplied; it still
archives only `HEAD`. It is a reproducibility check for the Linux and Windows
CLI, not the CI publication path. Scratch files stay below the ignored
`target/vesper-release` directory. The output contains the two CLI assets,
`LICENSE`, `NOTICE` and `SHA256SUMS`.

Linux uses the pinned image
`rust:1.99.1-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383`,
the `x86_64-unknown-linux-musl` target, locked dependencies and stripped
symbols. Windows uses the pinned Rust toolchain, `VsDevCmd.bat -arch=x64`,
path remapping and `/Brepro`. Release binaries report
`vesper <version> (<12-digit commit SHA>)`; archived builds receive the SHA
through `VESPER_GIT_SHA`.

For a local check, run the binaries and verify the sums:

```powershell
sha256sum -c ./target/release-out/SHA256SUMS
./target/release-out/vesper-cli-windows-x64.exe --version
```

The CI workflow is the source of the macOS binaries and every client bundle;
those jobs are not reproduced by the local script. The macOS client job builds a
universal binary with `--target universal-apple-darwin` and checks both
architectures with `lipo`; the Linux client job builds on Ubuntu 22.04 so the
bundles run on that release and later.
Both macOS CLI jobs use the `macos-latest` (Apple Silicon) runner; the x64 job adds the
`x86_64-apple-darwin` target explicitly so it does not depend on an Intel runner.
