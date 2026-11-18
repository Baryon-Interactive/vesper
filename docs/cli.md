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
| `publish` | Read a packed directory or manifest; `--owner`, `--repo`, `--tag`, `--channel`; `--metadata-dir` (default `./metadata`) and `--metadata-tag` (default `vesper-metadata`) for self-hosting, or `--index` for repository hosting; `--dry-run` previews |
| `publish-github` | Lower-level upload with `--owner`, `--repo`, `--tag`, `--manifest`, `--packs-dir`; optional `--index` and `--channel` |
| `promote` | Copy a channel entry: positional source/destination channels and `--index`; R2 sources require `publish --channel` instead |
| `rollback` | Decrement the channel metadata version: positional channel and `--index`; does not select a previous manifest |
| `sign` | Re-sign application targets (recomputes store hashes) or flat targets with `--targets` and optional `--key-file` |
| `gc` | Report packs in `<index>/packs`; `--index` required, `--yes` deletes reported packs |

`publish` with owner/repo and no `--index` uploads build assets and the three
signed developer metadata files to a separate mutable Release. Preserve
`--metadata-dir` and its increasing targets versions between publications;
`store.json` and `description.md` must exist there. The metadata tag must differ
from the build tag. It prints the public `metadata_url` base to register.
`--metadata-dir` conflicts with `--index`. With `--index`, the publisher writes
`apps/<id>/targets.json` for an app PR; omit owner/repo for local-only signing.
With neither an index nor owner/repo, publication only reports a dry run.
`GITHUB_TOKEN`, explicit `--token`, or authenticated `gh` supplies upload credentials.
`--api-base` and `--upload-base` override GitHub endpoints for local mocks.
Signing selects `VESPER_APP_ID` or `vesper.toml`; `VESPER_DEV_KEY` supplies a CI key.

```powershell
vesper publish ./dist-v2 --owner example --repo game-builds --tag v1.1.0 --metadata-dir ./metadata --channel stable
vesper publish ./dist-v1 --owner example --repo game-builds --tag v1.0.0 --metadata-dir ./metadata --channel stable
vesper publish ./dist-v2 --owner example --repo game-builds --tag v1.1.0 --index ./index --channel beta
vesper promote beta stable --index ./index
```

An explicit build tag is required if pack tags exclude the manifest version.
Ambiguous tags fail before network calls. Missing local reused packs are checked
on their remote release. Redeploying an older build with its retained tag
increments the targets version and binds that older manifest. `promote` and
channel `rollback` require a single-app index and online metadata renewal;
`rollback` only decrements channel metadata, not the selected game manifest.
Publication does not create an index PR or trigger aggregation itself.
`gc` currently passes an empty retained-manifest set: use it only on disposable
pack directories, never as a production retention planner. It does not repack.

### R2 publishing options

`publish` accepts optional `--r2-bucket`, `--r2-account-id`, `--r2-endpoint`,
`--r2-region`, `--r2-prefix` and `--r2-public-base`. Non-secret defaults are read from the
current directory's `vesper.toml` `[r2]` fields `bucket`, `account_id`, `endpoint`,
`region`, `prefix` and `public_base`; CLI values override each field. Any
S3-compatible store works with `endpoint` and `region` (default `auto`, for R2).
For `web-wasm32` builds, `publish` then checks that a browser on another origin
can read the uploaded manifest (`206`, CORS, exposed `Content-Range`) and warns
otherwise; see [web builds](publishing.md#web-builds). Bucket and public
base are required when R2 is configured; supply account ID or endpoint.
Prefix defaults to empty. R2 requires `--owner` and `--repo` for a complete
GitHub-backed mirror, in both `--metadata-dir` and `--index` modes.

### Relay option

`publish --relay <URL>` (or `relay = "<URL>"` at the top of `vesper.toml`)
signs a developer's own GitHub relay, the `github-proxy` Worker in relay
mode, as an extra `http` source with template `<URL>/{release}/{pack_id}`.
It requires `--owner` and `--repo`. After the upload, `publish` checks that a
browser on another origin can read the manifest through the relay and warns
otherwise; see [your own relay](publishing.md#your-own-relay).

Only `VESPER_R2_ACCESS_KEY_ID` and `VESPER_R2_SECRET_ACCESS_KEY` supply credentials.
The publisher uses path-style SigV4 PUT/HEAD, region `auto`, service `s3`.
HEAD compares size and signed BLAKE3 object metadata before skipping and after
uploading. All targets channels, their verified manifests and every referenced
pack must be mirrored before signing. Recovery and uploads stream through files;
30-second connection and 60-second read/write idle deadlines permit long uploads.
Transport errors, 408/429 and 5xx responses use four attempts with exponential
backoff; GitHub retries delete same-name assets and reopen files from byte zero.
A metadata failure identifies the signed but unpublished local targets version;
rerun `publish` with the retained metadata directory. `promote` rejects R2-backed
targets; use `publish --channel <destination>` with R2 settings for verification.
`--dry-run` prints `skipped`, `would-upload` and `would-recover-from-github`, uses
HEAD for asset checks, and never downloads pack/manifest bodies. Retained remote
manifest pack inventories are verified during actual publication.
See [R2 setup and pricing](publishing.md#optional-r2-mirror).

Content source selection uses parallel startup probes and useful backup ranges
within a 5% sampling budget. Fresh single-request EWMA rates drive switches:
over 1.5x for three observations with eight completions of cooldown, or at least
5x for one observation bypassing cooldown. Both sources need content samples
at most 30 seconds old. Multi-source installs dispatch the first 40 frames
separately to enable early correction of noisy probes. A single source skips
probes and samples but can hedge a stalled range on a new connection; healthy
progress suppresses that hedge. See [source selection](architecture.md#download-source-selection).

## Index maintenance

| Subcommand under `index` | Purpose and key arguments |
| --- | --- |
| `online-pubkey` | Print the public key derived from `VESPER_ONLINE_KEY` |
| `init` | `--index`, `--root-keys`, `--threshold`, `--out-keys`; `--online-pubkey` or `VESPER_ONLINE_KEY` establishes online trust |
| `route-add` | Register `--app`, `--pubkey` and optional `--metadata-url` in `--index`; requires offline signing next |
| `aggregate` | Fetch developer bundles in `--index`; per-app `updated` / `unchanged` / `rejected: <reason>`, also in `GITHUB_STEP_SUMMARY`; rejected apps retain old files and do not fail other apps |
| `route-sign` | Sign routes with `--root-key-file`; repeat the command with required root keys |
| `root-keygen` | Write a new offline root key to `--out` (never overwrites) and print its public key |
| `root-draft` | Draft the next root of `--index` as `root.draft.json`; `--expires-days`, repeatable `--add-root-key` / `--remove-root-key`, `--threshold`, `--online-pubkey` |
| `root-sign` | Add the `--key-file` signature to `--draft`; repeat for each previous and new root key holder |
| `root-publish` | Verify the draft chains from `root.json` in `--index`, then write `<N>.root.json` and `root.json` ([root renewal](index-layout.md#root-renewal-and-rotation)) |
| `verify` | Verify `--index` including its versioned root chain; repeat `--app`, use `--previous` for monotonic and root-chain checks; `--targets-only` is for untrusted pre-signing PR feedback |
| `pending-snapshots` | List apps with changed targets or snapshots within two days of expiry |
| `snapshot` | Renew changed/near-expiry `--app` in `--index` and its timestamp with `VESPER_ONLINE_KEY` |
| `timestamp` | Renew timestamps in `--index --all` with the online key |

```powershell
vesper index online-pubkey
vesper index init --index ./index --root-keys 3 --threshold 2 --out-keys ./root-keys --online-pubkey <hex-public-key>
vesper index aggregate --index ./index
vesper index verify --index ./index
vesper index pending-snapshots --index ./index
vesper index snapshot --index ./index --app demo
vesper index timestamp --index ./index --all
```

Aggregation verifies the root-signed registry before fetching. Each file is limited
to 1 MiB, with a 15-second total and 5-second connect timeout and at most five
redirects. HTTPS is required throughout; HTTP 127.0.0.1 is permitted for local
mock origins only. Same-version different payloads and lower versions are rejected.
A rejection is a per-app result; invalid registry trust or local report I/O fails
the command. Workflows run aggregation every 15 minutes and on manual dispatch.

Root keys are offline secrets; online signing keys belong only in trusted branch
workflows. See [index layout](index-layout.md) for canonical metadata links.

## Store listing

```powershell
vesper store init --title "My Game" --developer "My Studio" --homepage https://example.com --platform web-wasm32
vesper store add-media ./art/header.png --as cover --url https://example.com/media/header.png
vesper store check
```

All three take `--dir` (default `./metadata`).

- `init` writes `store.json`, `description.md` and `changelog.md`, keeping any
  that already exist. Repeat `--platform` for each platform.
- `add-media <file> --as cover|capsule|logo|background|screenshot --url <https URL>`
  reads the image size from a PNG, JPEG, WebP or GIF header, hashes the file
  with BLAKE3 and records it. Screenshots are appended; the other roles are
  replaced. `store.json` is rewritten with sorted keys.
- `check` reports every problem with `store.json` and the Markdown files next
  to it, using the catalogue's rules, and exits non-zero if there are any.

The fields are described in [publishing](publishing.md#what-a-listing-can-hold).

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
