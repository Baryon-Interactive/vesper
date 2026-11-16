# Building and publishing vesper binaries

See the [project overview](../README.md), [script inventory](../scripts/README.md)
and [index template](../index-template/README.md).

Binary releases are published on this repository, `Baryon-Interactive/vesper`,
as `vesper-v<version>` Releases whose tag points to the source commit they were
built from. A maintainer publishes them manually; the build script never writes
to GitHub. Releases 0.4.0 to 0.6.0 were first published in `EthanInMel/gsp-releases`;
the copies here are byte-identical, and index repositories still pinned to the old
repository keep working until they are re-pinned. Release 0.3.0 predates this
repository's history and remains only in `EthanInMel/gsp-releases`.

## Build from a committed tree

Run on Windows with Docker Desktop in Linux container mode, Git, tar, Rust
`1.99.1-x86_64-pc-windows-msvc`, and the VS 2019 x64 Build Tools in the standard program-files directory.
The development workspace uses the same compiler via [rust-toolchain.toml](../rust-toolchain.toml):

```powershell
powershell -NoProfile -File scripts/build-vesper-release.ps1 -Version 0.6.0 -OutDir ./target/release-out
```

`-Version` must match the workspace version in the archived commit. The script
rejects a dirty working tree unless `-AllowDirty` is supplied; that flag still
builds only `git archive HEAD`, never uncommitted source. Scratch files stay
under the ignored `target/vesper-release` directory and are removed on completion
or failure. The output directory contains `vesper-linux-x64`,
`vesper-windows-x64.exe`, `LICENSE` and `NOTICE` (copied from the archived commit),
and `SHA256SUMS` covering all four in standard `sha256sum` format with bare
filenames. Release versions print `vesper <version> (<12-digit commit SHA>)`.
Ordinary Cargo builds read the 12-digit SHA from the checkout; archived builds require `VESPER_GIT_SHA`.

Linux uses the image reference and digest committed in the script:
`rust:1.99.1-alpine@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383`.
It was obtained with `docker image inspect rust:1-alpine --format '{{json .RepoDigests}}'`.
The image already contains musl headers and binutils; builds check these are
present and never update APK packages. The target is
`x86_64-unknown-linux-musl`, with locked Cargo dependencies and stripped symbols.
Linux intermediates stay in the container filesystem; only the finished binary
is copied to the exported source directory on the Windows bind mount.
Windows uses the pinned Rust toolchain and calls `VsDevCmd.bat -arch=x64` before
the locked release build. Source paths are remapped and `/Brepro` removes PE
link timestamp variation. `SOURCE_DATE_EPOCH` comes from the source commit.

For comparison builds, keep the same commit, image digest, Rust toolchain and
MSVC/Windows SDK versions. The digest pins the Linux compiler and system
packages; Windows Build Tools and SDK binaries are local prerequisites, not
vendored or pinned by this repository. A different MSVC/SDK can change Windows
bytes. Record actual repeat-build results rather than assuming cross-machine
byte identity. Cargo may download locked crates from its registries; no GitHub
source access is needed after the archive is exported.

## Verify and publish manually

Verify checksums from the output directory and run the Linux binary in both
musl and glibc containers (PowerShell uses Windows absolute bind mount paths):

```powershell
docker run --rm --mount type=bind,source=${PWD}/target/release-out,target=/out,readonly -w /out alpine sh -c 'sha256sum -c SHA256SUMS && ./vesper-linux-x64 --version'
docker run --rm --mount type=bind,source=${PWD}/target/release-out,target=/out,readonly -w /out ubuntu sh -c 'sha256sum -c SHA256SUMS && ./vesper-linux-x64 --version'
./target/release-out/vesper-windows-x64.exe --version
```

After review, the **maintainer** runs the following from the output directory
against this repository:

```powershell
gh release create vesper-v0.6.0 vesper-linux-x64 vesper-windows-x64.exe LICENSE NOTICE SHA256SUMS --repo Baryon-Interactive/vesper --target <full-commit-sha> --title 'vesper 0.6.0' --notes 'Built from source commit <full-commit-sha>; see SHA256SUMS.'
```

Replace the version and commit with the reviewed build provenance. Do not
upload source archives or source tokens. Then set workflow-level `VESPER_VERSION`
and `VESPER_SHA256` in `verify-pr.yml`, `aggregate.yml`, `snapshot.yml`, and `timestamp.yml` to the
same release version and **Linux binary** hash. Keep
`VESPER_RELEASES_REPO: Baryon-Interactive/vesper`. Run the template tests, then test
snapshot and timestamp Actions in the index repository and verify a PR changing
`.github/workflows/` remains for manual merge. The placeholder pins are the only
intentional pre-publication placeholders and must be replaced before use.
