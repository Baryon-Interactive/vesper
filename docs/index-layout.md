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
`apps.<app_id> = { "keys": [<ed25519 public key hex>], "threshold": <n> }`.
The route keys are public keys; signatures identify them with their SHA-256
key id.

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
Deleted files therefore retain their version history. Signing an app with no
recoverable prior online version fails by default. For a genuinely new app a
maintainer explicitly runs `index snapshot --index <index> --app <id> --initialize`.
The automated signing workflow never passes `--initialize`; it fails if history
is unavailable rather than guessing version 1. A shallow clone, rewritten history,
or standalone directory cannot prove an app was never published: restore the
full history or provide a trusted prior index using `--previous`. Initial online
files require maintainer handling, as contributor PRs cannot modify them.

## Client catalogue verification

Pages copies raw `apps/<id>/store.json` and `description.md` byte-for-byte along
with targets, snapshot, timestamp, root and routes. API pages supply discovery IDs.
The client verifies the root/routes/developer/online chain, then fetches the raw
store files and checks `metadata` length and SHA-256 before parsing and returning
any store content. It ignores the API's derived store and description records.
Any mismatch returns an explicit error and no catalogue data for that request.
