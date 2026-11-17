# Working in the Vesper repository

Vesper distributes games from GitHub Releases with a signed catalogue (index). This file is the map for
agents. Read it instead of exploring the tree, and open only the files your task needs. The
[architecture](docs/architecture.md) and [testing guide](docs/testing.md) hold the details.

## Where things are

| Path | What |
|---|---|
| `crates/vesper-format` | Pack format: chunking, `build_from_directory`, manifests, file modes. |
| `crates/vesper-meta` | Signed metadata roles (root, routes, targets, snapshot, timestamp) and verification. |
| `crates/vesper-fetch` | Multi-source download (GitHub, R2, Hugging Face, HTTP, local), probes, range merging. |
| `crates/vesper-install` | Journaled install, update, rollback, repair; symlink guard inside the install root. |
| `crates/vesper-client` | Window-independent client library: catalogue, install tasks, library, settings, gateway, platform (`src/platform/`: host platform and channel choice, credentials, launch). Trusted root: `config/root.json`. |
| `crates/vesper-publish` | Publishing builds and metadata to GitHub Releases and mirrors. |
| `crates/vesper-cli` | The `vesper` command-line tool ([docs/cli.md](docs/cli.md)). |
| `crates/vesper-web` | WebAssembly build of the verification and manifest code for the web store and player. |
| `launcher/` | Tauri v2 desktop client: `src/` (TypeScript UI), `src-tauri/` (own Cargo workspace and lockfile), `dev/` (demo catalogue, tour scripts). See [launcher/README.md](launcher/README.md). |
| `web/` | Web store (`src/`) and play site (`play/`: service worker, launch page) for `web-wasm32` builds. See [web/README.md](web/README.md). |
| `index-template/` | Template repository for a catalogue: workflows, schemas, `test/` (Node tests). |
| `gateway/` | Key gateway Worker for paid builds. |
| `scripts/` | Release build, docs check, license inventory, e2e and smoke scripts, release notes, workflow test. |
| `.github/workflows/release.yml` | CI release: CLI for Linux, Windows, macOS; client for Windows, macOS, Linux; publishes on `vesper-v*` tags. |
| `docs/` | Architecture, CLI, publishing, index layout, releases, testing, third-party licenses. |

## Commands

- Rust: `cargo test --workspace --locked` and `cargo test --manifest-path launcher/src-tauri/Cargo.toml --locked`.
  On Windows, run cargo from a Visual Studio x64 developer environment (`VsDevCmd.bat -arch=x64`); some
  dependencies need `cl.exe`.
- Launcher front end: `npm --prefix launcher ci`, `npm --prefix launcher run build`, `npm --prefix launcher test`.
- Web: `npm --prefix web ci`, `npm --prefix web run wasm` (needs `wasm32-unknown-unknown` and `wasm-bindgen-cli`), `npm --prefix web test`, `npm --prefix web run build`.
- `node --test scripts/release-workflow.test.mjs` and `node --test index-template/test/*.test.mjs`.
- `python scripts/check-docs.py` (no BOM, LF only, no Chinese, no absolute machine paths, README under 200 lines).
- `python scripts/third-party-licenses.py --check`; regenerate without `--check` after any lockfile change.

## Rules

- English everywhere in the repository. UTF-8 without BOM, LF line endings.
- A release bumps the version in `Cargo.toml` (`[workspace.package]`), `launcher/src-tauri/Cargo.toml`,
  `launcher/src-tauri/tauri.conf.json`, `launcher/package.json` and `VESPER_VERSION` in the release
  workflow; the workflow test checks they agree. See [docs/releases.md](docs/releases.md).
- Players get the client; developers get `vesper-cli-*`. Never blur the two in names or docs.
- Channels are per platform: `<channel>-<platform>`, see [docs/publishing.md](docs/publishing.md).
- No private keys, tokens or signing material in the repository. Root keys stay offline.
- A test must drive the code under test and assert its result.
