# Vesper client

The launcher is the Tauri v2 desktop client for Windows, macOS and Linux for the Vesper catalogue and
installed game library. All client logic lives in the window-independent
[`vesper-client`](../crates/vesper-client/) crate; this launcher is its Tauri UI.
The Rust interface contains command wrappers and the `TauriEventSink` adapter,
plus window setup and the automatic updater. Command names, arguments and event
payloads define the frontend contract. See the [project overview](../README.md),
[architecture](../docs/architecture.md) and [testing guide](../docs/testing.md).

The Windows installer, the macOS disk image and the Linux AppImage and `.deb`
are the player client distributed in Vesper releases. The CLI binaries are
separate developer tools; the launcher is not the CLI.

## Build the installer

Use the Rust toolchain in [rust-toolchain.toml](../rust-toolchain.toml), Node.js 22
or later, WebView2 and the Tauri Windows C++ prerequisites. From an x64 Visual
Studio developer PowerShell at the repository root:

```powershell
npm --prefix launcher ci
npm --prefix launcher run build
cargo test --manifest-path launcher/src-tauri/Cargo.toml
cargo clippy --manifest-path launcher/src-tauri/Cargo.toml --all-targets -- -D warnings
npm --prefix launcher test
Push-Location launcher; npx tsc --noEmit; Pop-Location
powershell -ExecutionPolicy Bypass -File launcher/dev/build-exe.ps1
npx tauri build --bundles nsis
```

The NSIS installer is unsigned, so Windows SmartScreen may warn before
installation. CI builds it from the tagged source with the same command.

macOS and Linux bundles are built by CI (see
[the release workflow](../.github/workflows/release.yml)). On a Mac with both
Rust targets `aarch64-apple-darwin` and `x86_64-apple-darwin` installed:

```bash
npm --prefix launcher ci
cd launcher && npx tauri build --target universal-apple-darwin --bundles app,dmg
```

The app is ad-hoc signed and not notarized, so Gatekeeper blocks the first
launch of a downloaded copy: right-click Vesper and choose Open, or run
`xattr -dr com.apple.quarantine /Applications/Vesper.app`. On Ubuntu 22.04 or
later, install `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`,
`librsvg2-dev`, `libxdo-dev`, `libssl-dev`, `patchelf` and `build-essential`,
then run `npx tauri build --bundles appimage,deb` in `launcher`.

## Development

`build-exe.ps1` uses `custom-protocol` and copies the debug executable to
`dev/out/bin/vesper-launcher.exe`, so later cargo test builds do not replace the
demo copy. Scripts use standard VS 2019 Build Tools or `VESPER_VCVARS64` for an
alternate developer command file. For frontend development, run `npm run dev`
inside `launcher`. Generated schemas in `src-tauri/gen` are ignored; the
capability file's `$schema` is an editor hint and is regenerated during builds.

## Local demonstration

From the repository root, generate the ignored demo output:

```powershell
powershell -ExecutionPolicy Bypass -File launcher/dev/start.ps1
```

Start each service in its own terminal, then run the built executable:

```powershell
python launcher/dev/server.py --root launcher/dev/out --port 3210
python launcher/dev/server.py --root launcher/dev/out --port 8787
# In another terminal:
./launcher/dev/out/bin/vesper-launcher.exe
```

Set Index API to `http://127.0.0.1:3210` and Key Gateway to
`http://127.0.0.1:8787`. The demo has free and encrypted paid titles. Its email
code is `123456` and paid redemption code is `DEMO-PAID-2026`; these are local
mock values. The server is not the production Worker. The tour sets the explicit
`VESPER_TRUSTED_ROOT` override to the generated `launcher/dev/out/root.json`;
set that variable yourself when using another development catalogue. Release
builds without the override require the official root shipped in the client.

After installing v1, run:

```powershell
powershell -ExecutionPolicy Bypass -File launcher/dev/publish-v2.ps1
```

This changes part of a 2 MiB file, adds a file and publishes stable v2 while
retaining v1 packs. The demo game displays `VERSION.txt` and remains running for
70 seconds so backup cleanup after 60 seconds can be observed.

Run the tour without other Vesper windows on the desktop; its window lookup
uses the title. For automated native checks, run
`powershell -ExecutionPolicy Bypass -File launcher/dev/tour.ps1`. It sets up and
builds, starts local services, exercises the UI and writes thirteen screenshots
under `dev/out/screens`. `-SkipSetup` reuses prepared output. Tour state stays
inside `dev/out/tour-state`. Leave the separate Vite-time `VESPER_DEMO_TOUR` tab
preview unset for this tour and release builds.

## Client behavior

`vesper-client` verifies signed metadata, manifest size/hash, expiry, monotonic
versions and runtime prerequisites before installation. The store supports search,
tag filtering, verified media and descriptions. Library entries persist on disk
and support launch, updates, channels, rollback, verify/repair and uninstall.
User files are protected unless deletion is explicitly requested. File-lock
errors report Windows Restart Manager owners without forcibly terminating them.

The client installs the channel build for the player's own platform
(`<channel>-<platform>` first, then the plain channel if its platform matches; see
[publishing](../docs/publishing.md)). On macOS and Linux it restores execute
bits for packed executables and the launch target.

Paid installation IPC carries app/channel/metadata/install path only. The client
library reads the session from `Vesper.Launcher.Session` (Windows Credential
Manager; a `0600` file in the state directory on macOS and Linux), obtains the DepotKey,
zeroizes temporary hex and never emits it. Logout deletes the session credential.
Tasks persist under the user's local Vesper state directory; `VESPER_STATE_DIR`
selects an isolated test/demo directory. Pause takes effect between requests;
resume reuses checked chunks, which commit clears. In-flight HTTP requests do
not have an immediate cancellation token. Desktop GitHub OAuth needs a
handoff integration with the gateway.

Runtime entries fail closed until an operator supplies a verified official hash.
No manifest scripts execute. The updater endpoint is configurable.

## Client library tests and trust root

Window-independent Rust tests run from the root workspace with
`cargo test --workspace`, including the signed-index, installation,
progress, library, gateway and runtime tests. The ignored `real_github_index_install`
test belongs to `vesper-client`; set `VESPER_REAL_INDEX` to an explicitly
selected public index and run `cargo test -p vesper-client real_github_index_install
-- --ignored --nocapture`. This only reads the selected remote index and assets.

The trusted root is compiled from
[`crates/vesper-client/config/root.json`](../crates/vesper-client/config/root.json).
Headless callers can use `install_from_index` with the no-op event sink, or
supply an `EventSink` to task and repair APIs. No window handle enters the client
library. Platform services use the credential target, session storage,
process launch and delayed backup cleanup behavior.

## Updater signing

[src-tauri/config/updater-public.key](src-tauri/config/updater-public.key) is an
intentionally tracked development public key, also embedded in the Tauri config.
Private keys belong outside version control. For production, generate a key on
a controlled release machine with `npx tauri signer generate`, store the private
key in a secret manager and sign artifacts in CI. Rotate by shipping trust in
the new public key before retiring the old key.
