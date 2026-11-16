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
  1.root.json, 2.root.json, ...   (versioned roots, see root renewal below)
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

## Root renewal and rotation

`root.json` is the current root. Every root version after the first is also
published as `<N>.root.json` next to it (`2.root.json`, `3.root.json`, ...),
together with the version it replaced, so the chain is complete from `1.root.json`.
The client starts from the root embedded in its build, or from the newest root it
has already verified, and fetches `N+1.root.json`, `N+2.root.json` and so on until
the index answers 404. Each new root must:

- have exactly the next version (a lower or skipped version is a rollback or gap);
- be signed by a threshold of the previous root's `root` role keys; and
- be signed by a threshold of its own `root` role keys.

The client caches each verified root under `trust/` in its state directory, keyed
by the trust anchor, and starts from that cache next time. Intermediate roots may
have expired; the final one may not. The index's `root.json` must then equal the
newest verified root, so a `root.json` rolled back below a root the client has
already seen is rejected, and so is a newer `root.json` whose versioned file is
missing. When a new root changes the snapshot or timestamp key, the client drops
its cached app snapshot and timestamp versions so the new online key can start a
fresh version sequence. A client release that embeds a newer root skips the older
chain. `VESPER_TRUSTED_ROOT` replaces the embedded anchor for development
catalogues and chains in the same way.

Renew a root before it expires, and rotate keys, offline:

```bash
vesper index root-keygen --out new-root.key        # only to add a new holder; prints the public key
vesper index root-draft --index . --expires-days 365 \
  [--add-root-key <public hex>] [--remove-root-key <key id or public hex>] \
  [--threshold <n>] [--online-pubkey <public hex>]
vesper index root-sign --draft root.draft.json --key-file <root key>   # once per holder
vesper index root-publish --index .
vesper index route-sign --index . --root-key-file <new root key>        # after a root key change
vesper index verify --index .
```

`root-draft` writes an unsigned `root.draft.json` with the next version; Pages
does not serve it. Holders of the current root keys and of the new root keys add
signatures with `root-sign` (a renewal needs only the current threshold, because
the keys are the same). `root-publish` verifies the draft as the successor of
`root.json`, writes `<old>.root.json` if it is missing, `<new>.root.json` and the
new `root.json`, and removes the draft. `routes.json` is signed by the root role, so
after replacing root keys re-sign it until it meets the new threshold.

`index verify` checks the whole versioned chain and that `root.json` equals its
newest file. With `--previous`, the new chain must contain the previous
`root.json` unchanged at its version, so a PR cannot replace or roll back the root.
Root changes go through a maintainer PR with the `maintainer-approved` label.
Commit `root.json` and the versioned files together, and never delete old
`<N>.root.json` files: older clients chain through them.

## Client catalogue verification

Pages copies raw `apps/<id>/store.json` and `description.md` byte-for-byte along
with targets, snapshot, timestamp, root, versioned roots and routes. API pages supply discovery IDs.
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
