# Vesper index layout

See the [project overview](../README.md), [architecture](architecture.md),
[publishing guide](publishing.md) and [index template](../index-template/README.md).

An index checkout is static data. The root role signs the route table; each
application is then signed independently by the developer key listed in that
route. Online snapshot and timestamp roles are short-lived and may be
regenerated without changing application targets.

```text
<index>/
  root.json
  routes.json
  apps/<app_id>/description.md
  apps/<app_id>/store.json
  apps/<app_id>/targets.json
  apps/<app_id>/snapshot.json
  apps/<app_id>/timestamp.json
```

`routes.json` has a signed payload with `version`, `expires`, and
`apps.<app_id> = { "keys": [<ed25519 public key hex>], "threshold": <n>, "metadata_url": "https://github.com/example/game-builds/releases/download/vesper-metadata/" }`.
The route keys are public keys; signatures identify them with their SHA-256
key id. Optional `metadata_url` selects developer hosting; omit it for app PRs.
See the [routes schema](../index-template/schemas/routes.schema.json).
Deleting a signed route delists the app even if its old files remain on disk.

Developer hosting serves `targets.json`, `store.json` and `description.md` at
the registered base. No developer snapshot/timestamp is needed. The default is
GitHub Release tag `vesper-metadata`; arbitrary HTTPS bases are also supported.
HTTP 127.0.0.1 is permitted for local mocks only. Bases contain no credentials,
query or fragment. Each file is limited to 1 MiB, with a 15-second timeout,
5-second connect timeout and at most five redirects. Production redirects
require HTTPS; loopback HTTP mocks may also redirect to loopback HTTP.

`vesper index aggregate --index <dir>` verifies root/routes, developer signatures,
expiry and raw store bindings, then compares the prior aggregated targets.
Lower versions and different signed payloads at the same version are rejected.
Complete bundles are staged before replacing app directories, copying the three
raw byte sequences and retaining central online roles for the signing pass.
Rejection preserves an app independently of other apps. Results appear per app
on stdout and in `GITHUB_STEP_SUMMARY`. The [directory layout schema](../index-template/schemas/metadata-layout.schema.json)
defines the developer three-file set and central five-file set shown above.
Snapshot/timestamp always use the central online key.

`aggregate.yml` runs every 15 minutes or by manual dispatch and explicitly
starts `snapshot.yml` after pushing changes. Signing calls reusable Pages with
its resulting commit. Hosted app PRs require maintainer handling to prevent
competing updates through the two publication paths.

An application `targets.json` payload is:

```json
{
  "_type": "targets",
  "spec_version": "1.0.0",
  "version": 7,
  "expires": 1790000000,
  "channels": {
    "stable": {
      "version": 7,
      "platform": "windows-x64",
      "manifest": {
        "release_tag": "v1.2.3",
        "asset_name": "manifest.cbor.zst",
        "size": 1234,
        "blake3": "..."
      },
      "encrypted": false,
      "key_id": 0
    }
  },
  "metadata": {
    "description.md": { "length": 120, "sha256": "<64 hexadecimal digits>" },
    "store.json": { "length": 800, "sha256": "<64 hexadecimal digits>" }
  },
  "sources": [
    { "id": "github", "kind": "github", "url_template": "https://github.com/example/game-builds/releases/download/{release}/{pack_id}", "priority": 0 }
  ]
}
```

Source templates may contain only `{release}` and `{pack_id}`. Clients reject
unknown variables, expired metadata, and versions lower than the locally
recorded version. Required `metadata` entries bind the exact raw bytes of
`store.json` and `description.md` by length and SHA-256, including whitespace and
line endings. Missing entries and old targets formats are rejected. See the
[application targets schema](../index-template/schemas/app-targets.schema.json).
`publish`, `publish-github --index`, `sign`, `promote` and channel `rollback`
compute these entries from both files before signing. Create store files before
publication. Re-sign targets after every store edit; advance the targets version
for a published update.

## Online metadata and publication

The root role binds routes and the online snapshot/timestamp signing roles.
Each app snapshot binds the canonical targets version, length and SHA-256; its
short-lived timestamp binds the snapshot the same way. Full verification checks
all signatures, expiry, canonical links and rollback versions. PR feedback uses
`--targets-only` before trusted online signing, not as published-index verification.

`store.json` follows the [store schema](../index-template/schemas/store.schema.json).
`description.md` carries the app description; actual media and game assets live
outside the index. The [API builder](../index-template/scripts/build-api.mjs)
generates deterministic paginated app records and copies the signed metadata.
See the template's signing-to-Pages workflow before deploying a catalogue.

## Online version recovery

`index verify --previous <base>` rejects lower snapshot/timestamp versions and
missing online files for every app in the base, including with `--targets-only`.
It also checks developer targets rollback for the selected apps.
`index snapshot` and `index timestamp` take the maximum version in current files,
optional `--previous <base>` files, and available full Git history before incrementing.
Deleted files therefore retain their version history. Repository-hosted apps
without history require explicit maintainer `index snapshot --initialize`.
Registered hosted apps may initialize at first aggregation when the checkout has
full Git history; the signer still takes the maximum historical version if
online files were deleted. A shallow checkout cannot enable initialization.
Restore full history or supply a trusted `--previous` index to recover versions.
Automated workflows never pass `--initialize`. Developers do not hold online keys.

## Client catalogue verification

Pages copies raw `apps/<id>/store.json` and `description.md` byte-for-byte along
with targets, snapshot, timestamp, root and routes. API pages supply discovery IDs.
The client verifies the root/routes/developer/online chain, then fetches the raw
store files and checks `metadata` length and SHA-256 before parsing and returning
any store content. It ignores the API's derived store and description records.
Any mismatch returns an explicit error and no catalogue data for that request.


## Direct client update checks

Install/update checks obtain routes only from the central signed registry and
prefer the developer base when present. Developer signatures, expiry and both
store bindings are required; developer online roles are unnecessary. Failures
log the reason and fall back to the central chain. Accepted source and targets
version are logged as `source=developer` or `source=central`.

`metadata/<id>/highest-targets.json` persists the highest accepted payload.
Checks compare that file and the central cache across restarts, rejecting lower
versions and same-version equivocation. A central fallback below the high-water
mark also fails. Catalogue browsing uses central trust and its separate monotonic
chain cache, remaining available while aggregation catches up.
