# Publishing a game

Developers host their signed metadata alongside their game releases. The central
catalogue registers developer keys and hosting locations, aggregates verified
metadata, and signs the online snapshot/timestamp chain. See the [CLI reference](cli.md),
[index layout](index-layout.md) and [index template](../index-template/README.md).
Commands assume PowerShell with `vesper` and authenticated `gh` on PATH.

## Register once

Initialize your game identity from the game project:

```powershell
vesper init --app-id com.example.game
```

This creates `vesper.toml`, stores the developer seed in the system key store and
prints its public key. Supply that public key and a metadata base URL to the
catalogue maintainer in a registration PR. A maintainer prepares the route:

```powershell
vesper index route-add --index ./index --app com.example.game --pubkey <public-key> --metadata-url https://github.com/example/game-builds/releases/download/vesper-metadata/
vesper index route-sign --index ./index --root-key-file <offline-root-key-file>
```

Collect the root threshold signatures and merge this single registration PR.
The root-signed route authorizes the developer keys and metadata location.
Subsequent releases require no app PR. Key/location changes require maintainer
review and root signatures; deleting the route delists the app. Keep private
keys out of the index. CI uses secret `VESPER_DEV_KEY` and `VESPER_APP_ID`.

## Prepare metadata and the first build

Create `./metadata/store.json` following the [store schema](../index-template/schemas/store.schema.json)
and `./metadata/description.md` as UTF-8 Markdown. Media records contain HTTPS
URLs, BLAKE3 hashes and dimensions; host actual media in your own repository or
release. The launcher displays the exact image bytes verified by the client.
Preserve the metadata directory, including its signed targets version history,
between publications. Keep it in your own game repository or durable CI storage.

```powershell
vesper check ./build --json
vesper pack --input ./build --output ./dist-v1 --app-id com.example.game --platform windows-x64 --version 1.0.0 --release-tag v1.0.0 --user-file 'Saved/**'
vesper inspect --manifest ./dist-v1/manifest.cbor.zst
vesper publish ./dist-v1 --owner example --repo game-builds --tag v1.0.0 --metadata-dir ./metadata --channel stable
```

The publisher uploads the build manifest and packs to the immutable build
release, checks remote assets, increments the local targets version, binds the
exact store/description lengths and SHA-256 hashes, and signs with the developer
key. It creates the `vesper-metadata` Release if absent and replaces its three
metadata assets: `targets.json`, `store.json`, `description.md`. Targets upload
last; a reader seeing mixed generations rejects the store binding and retains
valid data. The printed `metadata_url` is the location clients and aggregation
read. `--metadata-dir` defaults to `./metadata`; `--metadata-tag` overrides the
metadata Release tag and must match the registered route. Build and metadata
tags must differ. `--dry-run` performs no upload or metadata write.

Use `GITHUB_TOKEN` or authenticated `gh` with permission to upload to your own
repository. Assets must be anonymously readable by clients. The CLI does not
create a repository, register routes, submit PRs or dispatch catalogue workflows.
For custom HTTPS hosting, copy the same three local metadata files to the
registered base directory. No Pages setup is needed for Release hosting.

## Aggregation and client updates

The central `aggregate.yml` workflow runs every 15 minutes and supports manual
Actions dispatch. Scheduling and hosting caches can delay visibility. A
maintainer can run `vesper index aggregate --index ./index` locally or trigger
that workflow. It reports `updated`, `unchanged` or `rejected: <reason>` per app
and writes the same report to the Actions summary. Signature, expiry, store
binding, transport and version checks run in the CLI. One failed app keeps its
last valid data while other apps proceed. Lower targets versions and different
content at the same version are rejected.

Changed aggregation commits explicitly dispatch central snapshot signing and
Pages deployment. Only the centre holds the online key. For first publication,
a full index Git history allows the signer to initialize a registered hosted
app; deleted online documents recover their highest historical versions.
The binary checksum pins and online key must be configured before deployment;
the template's zero checksum placeholder fails closed at execution time.

Catalogue browsing reads central Pages and verifies its full signed chain.
Update/install checks can read the developer location using keys from the
central signed route and verify both store files. Invalid or unavailable direct
data logs a rejection and falls back to the central signed chain. Clients
persist the highest accepted targets payload/version per app; neither path can
lower it or substitute different content at the same version. An older central
catalogue remains browsable while aggregation catches up, but cannot authorize
an update below that high-water mark.

## Publish updates and redeploy retained builds

```powershell
vesper pack --input ./build-v2 --output ./dist-v2 --app-id com.example.game --platform windows-x64 --version 1.1.0 --release-tag v1.1.0 --previous ./dist-v1/manifest.cbor.zst --user-file 'Saved/**'
vesper diff-check --current ./dist-v2/manifest.cbor.zst --against ./dist-v1/manifest.cbor.zst --json
vesper publish ./dist-v2 --owner example --repo game-builds --tag v1.1.0 --metadata-dir ./metadata --channel stable
```

Keep previous manifests and release packs: unchanged chunks refer to their
original releases, including for fresh installs. Repeat `--previous` for more
than one retained manifest. To return a channel to an earlier game build,
publish the retained packed directory again with its original build tag:

```powershell
vesper publish ./dist-v1 --owner example --repo game-builds --tag v1.0.0 --metadata-dir ./metadata --channel stable
```

The metadata version increases even when the game build version decreases.
Installer rollback (`vesper install-rollback --target ./library/game`) restores
a local backup independently of published metadata. The channel `rollback`
command only decrements the channel metadata version; it does not choose an
older manifest. `promote` and channel `rollback` operate on a single-app index.

For paid builds, `pack` accepts `--depot-key-hex` and `--key-id`; high-level
`publish` reads unencrypted manifests. Encrypted publication uses
`publish-github` with decryption flags plus app-target signing integration and
[gateway provisioning](../gateway/README.md). See the local
[encrypted demonstration](../launcher/README.md#local-demonstration).

## Repository-hosted metadata alternative

Omit `metadata_url` from the signed route to manage metadata inside the central
index. Create `apps/com.example.game/store.json` and `description.md`, then:

```powershell
vesper publish ./dist-v1 --owner example --repo game-builds --tag v1.0.0 --index ./index --channel stable
```

Review and commit only that app's `store.json`, `description.md` and signed
`targets.json`, and submit an index PR. Store edits require a higher targets
version and a fresh signature. `vesper sign --targets <path>` recomputes store
bindings. Eligible single-app PRs auto-merge after trusted default-branch path
and exact-head checks. Snapshot/timestamp files, routes, multiple apps and other
paths require maintainer handling. New repository-hosted apps require explicit
maintainer online initialization; see [version recovery](index-layout.md#online-version-recovery).
A route with `metadata_url` requires maintainer handling for any app PR, so the
PR and aggregation paths cannot compete. The `maintainer-approved` label enables
PR verification feedback for such changes; it never permits automatic merging.
