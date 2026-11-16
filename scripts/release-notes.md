# Vesper @VERSION@

## Players: download Vesper-Setup-@VERSION@-windows-x64.exe (Windows)

Download the Windows client installer to browse the official catalogue and
install games. The installer is unsigned in this release, so Windows SmartScreen
may warn before it runs; verify `SHA256SUMS` before continuing.

## Developers: vesper CLI

The CLI is a command-line tool for publishing and installing verified builds.
It is separate from the Windows client installer.

| Asset | Platform | Notes |
| --- | --- | --- |
| `vesper-cli-linux-x64` | Linux x64 | Static musl binary |
| `vesper-cli-windows-x64.exe` | Windows x64 | Command-line tool |
| `vesper-cli-macos-arm64` | macOS arm64 | Unsigned; remove quarantine with `xattr -d com.apple.quarantine <file>` |
| `vesper-cli-macos-x64` | macOS x64 | Unsigned; remove quarantine with `xattr -d com.apple.quarantine <file>` |

All assets are covered by `SHA256SUMS`. This release was built from source
commit `@COMMIT@`.
