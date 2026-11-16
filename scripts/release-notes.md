# Vesper @VERSION@

## Players: download the Vesper client

The client is the app for browsing the official catalogue and installing games.
Pick the file for your computer:

| Asset | Platform | Notes |
| --- | --- | --- |
| `Vesper-Setup-@VERSION@-windows-x64.exe` | Windows 10/11 x64 | Installer |
| `Vesper-@VERSION@-macos-universal.dmg` | macOS 11 or later, Apple Silicon and Intel | Open the disk image and drag Vesper to Applications |
| `Vesper-@VERSION@-linux-x64.AppImage` | Linux x64 | Portable; run `chmod +x` on it, then start it |
| `Vesper-@VERSION@-linux-x64.deb` | Debian, Ubuntu and derivatives, x64 | `sudo apt install ./Vesper-@VERSION@-linux-x64.deb` |

The clients are not signed with a paid certificate in this release. Verify
`SHA256SUMS` first. Windows SmartScreen may warn before the installer runs.
macOS may say the app cannot be opened: right-click Vesper in Applications and
choose Open, or run `xattr -dr com.apple.quarantine /Applications/Vesper.app`.

## Developers: vesper CLI

The CLI is a command-line tool for publishing and installing verified builds.
It is not the player client.

| Asset | Platform | Notes |
| --- | --- | --- |
| `vesper-cli-linux-x64` | Linux x64 | Static musl binary |
| `vesper-cli-windows-x64.exe` | Windows x64 | Command-line tool |
| `vesper-cli-macos-arm64` | macOS arm64 | Unsigned; remove quarantine with `xattr -d com.apple.quarantine <file>` |
| `vesper-cli-macos-x64` | macOS x64 | Unsigned; remove quarantine with `xattr -d com.apple.quarantine <file>` |

Publish one channel per platform, for example `stable-windows-x64`,
`stable-macos-arm64` and `stable-linux-x64`; the client picks the build that
matches the player's computer.

All assets are covered by `SHA256SUMS`. This release was built from source
commit `@COMMIT@`.
