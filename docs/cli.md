# CLI reference

`vesper` is built by `cargo build -p vesper-cli`. Start with the
[quick start](../README.md#quick-start) or [publishing guide](publishing.md).
Run `vesper <command> --help` for all flags and defaults of the installed binary.
Commands below assume the executable is on PATH. Formats are pre-release Vesper/1
and have no previous-format compatibility.

## Build inspection and packaging

| Command | Purpose and key arguments |
| --- | --- |
| `init` | Write `vesper.toml` and store a developer key. `--output`, `--app-id`, optional `--index` to print route-registration instructions |
| `check` | Inspect an unpacked directory: positional build or `--input`, `--max-file-size` (default 4 GiB), `--json` |
| `diff-check` | Compare manifests: positional current manifest or `--current`, required `--against`, optional decryption flags, `--json` |
| `pack` | Required `--input`, `--output`, `--app-id`, `--platform`, `--version`; repeatable `--previous` and `--user-file`; `--release-tag`, `--max-pack-size`, chunk-size and encryption flags |
| `inspect` | Print identity and counts from `--manifest`, with decryption flags when needed |

```powershell
vesper check ./build --json
vesper pack --input ./build --output ./dist-v1 --app-id demo --platform windows-x64 --version 1.0.0 --release-tag v1.0.0 --user-file 'Saved/**'
vesper pack --input ./build-v2 --output ./dist-v2 --app-id demo --platform windows-x64 --version 1.1.0 --release-tag v1.1.0 --previous ./dist-v1/manifest.cbor.zst --user-file 'Saved/**'
vesper diff-check --current ./dist-v2/manifest.cbor.zst --against ./dist-v1/manifest.cbor.zst --json
```

Previous arguments accept manifest files or packed directories and may repeat.
Keep earlier release packs accessible. Default pack size is 512 MiB. Chunking
uses the minimum, average and maximum sizes exposed by `pack --help`.

For paid packaging, add `--depot-key-hex <64-hex-secret> --key-id <number>`.
`inspect`, `diff-check`, `install`, `verify`, `uninstall` and `publish-github`
also accept the key and optional key ID. Keep keys out of logs and shell history.
The high-level `publish` command currently reads unencrypted manifests; encrypted
publication requires the lower-level upload command plus signed app metadata
integration, as demonstrated by the launcher demo helper.

## Installation

| Command | Purpose and key arguments |
| --- | --- |
| `install` | Required `--manifest`, `--target`; repeat `--source` for priority-ordered local/HTTP sources; `--release` supplies a fallback tag; `--dry-run --json` estimates disk and network bytes |
| `verify` | Check installed files with `--manifest` and `--target` |
| `install-rollback` | Restore the latest installer backup at `--target` |
| `uninstall` | Remove manifest-owned files at `--target`; `--delete-user-files` explicitly removes protected user files too |
| `launch` | Wait `--seconds` (default 60) then clean the target's backup; it does not start a game executable |

```powershell
vesper install --manifest ./dist-v1/manifest.cbor.zst --source ./dist-v1 --target ./library/demo --dry-run --json
vesper install --manifest ./dist-v1/manifest.cbor.zst --source ./dist-v1 --target ./library/demo
vesper install --manifest ./dist-v2/manifest.cbor.zst --source ./dist-v2 --source ./dist-v1 --target ./library/demo
vesper verify --manifest ./dist-v2/manifest.cbor.zst --target ./library/demo
vesper install-rollback --target ./library/demo
vesper verify --manifest ./dist-v1/manifest.cbor.zst --target ./library/demo
```

A remote source can be
`https://github.com/example/game-builds/releases/download/{release}/{pack_id}`.
Release substitution uses each pack's original tag, falling back to `--release`.
Keep the manifest trusted: direct CLI installation does not bootstrap an index
signature chain for you. Dry-run fields include `changed_bytes`, `download_bytes`,
`peak_bytes` and `hedged_bytes`; download counters measure compressed wire bytes.

## Publication and channels

| Command | Purpose and key arguments |
| --- | --- |
| `publish` | Read a packed directory or manifest; `--index`, `--channel`, `--owner`, `--repo`, `--tag`; `--dry-run` previews without publication |
| `publish-github` | Lower-level upload with `--owner`, `--repo`, `--tag`, `--manifest`, `--packs-dir`; optional `--index` and `--channel` |
| `promote` | Copy a channel entry: positional source/destination channels and `--index` |
| `rollback` | Decrement the channel metadata version: positional channel and `--index`; does not select a previous manifest |
| `sign` | Re-sign application targets (recomputes store hashes) or flat targets with `--targets` and optional `--key-file` |
| `gc` | Report packs in `<index>/packs`; `--index` required, `--yes` deletes reported packs |

`publish` needs `--index` to perform publication; without it, or with `--dry-run`,
it only reports. Supply both owner and repo for GitHub upload; omit both for a
local index-only write. `GITHUB_TOKEN` or authenticated `gh` supplies upload
credentials. Endpoint overrides `--api-base` and `--upload-base` support mocks.
Index signing uses the developer key selected by `VESPER_APP_ID` or the current
`vesper.toml`, with `VESPER_DEV_KEY` available for CI.

```powershell
vesper publish ./dist-v2 --owner example --repo game-builds --tag v1.1.0 --index ./index --channel beta
vesper promote beta stable --index ./index
vesper rollback stable --index ./index
vesper publish-github --owner example --repo game-builds --tag v1.0.0 --manifest ./dist-v1/manifest.cbor.zst --packs-dir ./dist-v1/packs
```

An explicit tag is required if reused pack tags do not include the manifest
version. Ambiguity fails before network calls. Missing local reused packs are
checked on the remote release. Publication writes local targets; it does not
create an index PR. Channel promotion and rollback need online metadata renewal.
Both currently require an index containing exactly one application's targets.
`rollback` only decrements that channel entry's metadata version; it does not
restore an older game manifest. To redeploy an earlier build, publish its retained
packed directory again with its original release tag and submit the new targets.
`gc` currently passes an empty retained-manifest set: use it only on disposable
pack directories, never as a production retention planner. It does not repack.

## Index maintenance

| Subcommand under `index` | Purpose and key arguments |
| --- | --- |
| `online-pubkey` | Print the public key derived from `VESPER_ONLINE_KEY` |
| `init` | `--index`, `--root-keys`, `--threshold`, `--out-keys`; `--online-pubkey` or `VESPER_ONLINE_KEY` establishes online trust |
| `route-add` | Register `--app` and `--pubkey` in `--index`; requires offline signing next |
| `route-sign` | Sign routes with `--root-key-file`; repeat the command with required root keys |
| `verify` | Verify `--index`; repeat `--app`, use `--previous` for monotonic checks; `--targets-only` is for untrusted pre-signing PR feedback |
| `pending-snapshots` | List apps with changed targets or snapshots within two days of expiry |
| `snapshot` | Renew changed/near-expiry `--app` in `--index` and its timestamp with `VESPER_ONLINE_KEY` |
| `timestamp` | Renew timestamps in `--index --all` with the online key |

```powershell
vesper index online-pubkey
vesper index init --index ./index --root-keys 3 --threshold 2 --out-keys ./root-keys --online-pubkey <hex-public-key>
vesper index verify --index ./index
vesper index pending-snapshots --index ./index
vesper index snapshot --index ./index --app demo
vesper index timestamp --index ./index --all
```

Root keys are offline secrets; online signing keys belong only in trusted branch
workflows. See [index layout](index-layout.md) for canonical metadata links.

## Flat metadata helpers

These operate on the flat root/targets metadata family, separate from the
per-application catalogue route layout:

```powershell
vesper meta-init --output ./metadata
vesper meta-add-target --targets ./metadata/targets.json --key-file ./metadata/targets.key --name demo/windows-x64/stable/1.0.0 --file ./dist-v1/manifest.cbor.zst --app-id demo --platform windows-x64 --version 1.0.0 --channel stable
vesper meta-verify --root ./metadata/root.json --targets ./metadata/targets.json --snapshot ./metadata/snapshot.json --timestamp ./metadata/timestamp.json
vesper meta-refresh --dir ./metadata --snapshot-key-file ./metadata/snapshot.key --timestamp-key-file ./metadata/timestamp.key --snapshot-version 2 --timestamp-version 2
```

`meta-init` writes development role keys and metadata. `meta-add-target` signs a
file description. `meta-verify` accepts `--previous-targets-version` and optional
online documents. `meta-refresh` accepts a shared `--key-file` fallback or
separate online role keys; choose increasing versions explicitly.
