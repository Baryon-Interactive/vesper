# Architecture

Vesper distributes immutable game content using object hosting and a signed
static catalogue. See the [project overview](../README.md),
[index layout](index-layout.md) and [publishing guide](publishing.md).

## Components

| Component | Responsibility |
| --- | --- |
| `vesper-format` | FastCDC chunking, BLAKE3 identities, deterministic CBOR manifests, VSPK frames and encryption |
| `vesper-meta` | Ed25519 roles, thresholds, expiry, monotonic versions and metadata links |
| `vesper-fetch` | Local/HTTP Range reads, source resolution, direct-link refresh, verification and chunk reuse |
| `vesper-install` | Preflight, staging, journal recovery, file replacement, rollback and user-file preservation |
| `vesper-publish` | GitHub release upload and remote asset verification |
| `vesper-cli` | Packaging, inspection, publishing, index maintenance and installation commands |
| `vesper-client` | All client logic: verified catalogue, settings, library, tasks, installation orchestration, gateway, credentials, media, runtime rules and game launch |
| [Launcher](../launcher/README.md) | Tauri command wrappers, event forwarding, window setup and automatic updater |
| [Gateway](../gateway/README.md) | Paid-game authentication, grants and DepotKey delivery through Cloudflare Worker/D1 |
| [Index template](../index-template/README.md) | App PR validation, trusted merge, online signing and static Pages API |

The [agent skill](../skills/vesper-publish/SKILL.md) documents the publishing
boundary. [Experiments](../experiments/README.md) measure patch sizes and transfer
performance separately from the runtime.

`vesper-client` depends on the core crates and has no Tauri dependency. Its public
API is grouped into `settings`, `state`, `metadata`, `store`, `install`, `library`,
`tasks`, `gateway`, `credentials`, `media`, `runtime` and `events`. Every public
item is documented; `missing_docs` is denied. Root re-exports keep the command
wrappers and headless callers simple.

The client reports `task-status`, `download-progress`, `disk-preflight` and
`repair-completed` as JSON payloads through a thread-safe `EventSink`.
`NoopEventSink` discards events for headless calls. The launcher implements the
trait by forwarding events with Tauri's emitter; it owns no client state logic.
Event callbacks can run on workers while task state is locked, so sinks should
forward or record events without calling back into task operations.

Settings are stored in `settings.json`, tasks in `tasks/<id>.json` and verified
metadata in `metadata/<app_id>/` under the state directory. `VESPER_STATE_DIR`
overrides that directory. Installation control files are in `.vesper`.
The client's trusted bootstrap root is
[`crates/vesper-client/config/root.json`](../crates/vesper-client/config/root.json);
replace it with the offline production root before building a pinned client.

Private platform modules contain session credential access and game process
launch/backup cleanup. Windows Credential Manager calls are isolated with
`cfg(windows)`; non-Windows sessions use a file fallback.
Restart Manager and disk-space platform handling are in `vesper-install`,
which the client invokes for every installation, repair and rollback.
Compilation portability does not add support for non-Windows game manifests:
manifests require the `windows-x64` platform identity.

## Content and installation flow

1. Packaging splits files into content-addressed chunks, deduplicates them and
   compresses frames into immutable `.vpack` files. A manifest records files,
   chunk locations, pack identities and the release owning each pack.
2. `pack --previous` retains references to unchanged chunks in earlier packs;
   only new content needs new pack files. Identical inputs and parameters produce
   identical packs and manifests. `SOURCE_DATE_EPOCH` controls creation time.
3. Publication uploads new packs and the manifest to a release and signs the
   channel targets. A separate index PR carries metadata and store descriptions.
4. Trusted Actions validate permitted paths, merge eligible single-app changes,
   sign snapshot/timestamp metadata and deploy the signed revision to Pages.
5. Clients verify trust and manifest size/hash before fetching. Sources can be
   GitHub releases, Hugging Face or HTTP; templates use `{release}` and `{pack_id}`.
   Every chunk is verified after decoding. HTTP requires a matching `206`
   `Content-Range`; a full `200` is refused.
6. The installer stages changed files and journals same-volume file renames.
   Recovery handles interrupted transactions; backups support rollback. This is
   file-granular atomic replacement, not a simultaneous whole-tree swap.

The fetcher merges adjacent missing ranges, bounds concurrency and its in-memory
chunk cache (64 MiB), tracks source health and can hedge slow requests. Verified
chunks persist under `.vesper/chunks` during a transaction for restart reuse;
commit clears this temporary store. Declared user-file globs are preserved by
update, repair and default uninstall. Windows lock failures report Restart
Manager owners rather than force-killing processes.

## Signature chain and key responsibilities

```text
Pinned root keys -> root.json -> routes.json -> developer keys -> targets.json
                       |                                           ^
                       +-> online keys -> timestamp -> snapshot ----+
```

- Offline root maintainers establish role keys and thresholds and sign developer
  route registrations. Root private keys stay offline.
- Each developer signs only their application's targets using the registered
  route keys. `vesper init` stores a developer seed in the platform key store;
  CI can use `VESPER_DEV_KEY` with `VESPER_APP_ID`.
- The separate `VESPER_ONLINE_KEY` signs short-lived snapshots and timestamps.
  Initialization binds its public key to both online roles at threshold one.
  It cannot authorize a new developer or replace the developer's signature.
- Snapshot descriptions bind canonical targets version, length and SHA-256;
  timestamps similarly bind snapshots. Clients enforce signatures, expiry and
  locally recorded version monotonicity. App targets bind the manifest's size
  and BLAKE3 hash. Required targets `metadata` entries bind the raw store and
  description bytes by length and SHA-256. Clients use API IDs for discovery,
  verify the signed chain, and hash raw files copied by Pages before display.
  Derived API store fields do not authorize displayed content.

A PR uses `--targets-only --previous` before online signing. Full published-index
verification also checks the online chain. A configured production root rejects
missing required online roles; an unpinned bootstrap index is a weaker trust mode.

## Paid builds and security boundaries

Paid content uses a DepotKey with HKDF-derived AES-256-GCM keys for pack frames
and encrypted manifests (VSPM). Keys are not embedded in published content.
The Worker stores encrypted DepotKeys and hashes of opaque credentials in D1,
checks grants and verifies developer requests against root-signed routes.
The client library obtains keys with the user's session, zeroizes the
temporary hex buffer and never returns keys to frontend IPC or progress events.

Untrusted PR code never runs with merge or online signing secrets. The trusted
merge workflow inspects the complete file list and exact head commit using
scripts from the default branch. Only targets and their signed store/description
files qualify for automatic merge; online-role files require a maintainer.
PR verification rejects store changes without matching developer signatures.
Signing retries regenerate metadata on current
main rather than rebasing signed output; Pages deploys the resulting commit.

The client permits runtime downloads only after an operator records an
allowlisted official hash; manifests cannot run arbitrary scripts. Updater
signing uses a separate key pair; its checked-in public key is for development.
Transport and storage providers can deny service; signatures detect tampering,
not availability failures. Encryption controls distribution access and does not
prevent an authorized player from copying decrypted game files.

Production operation requires trusted root distribution, provider
credentials, a mail adapter, payment configuration and gateway rate limiting.
Local tests do not establish deployed OAuth, payments, Pages or WAN performance.

The desktop WebView uses a self-only default CSP and scripts, no eval, HTTPS
images plus Tauri asset origins and the loopback demo cover server. Inline styles
support React's computed progress widths; scripts do not permit unsafe-inline.
The native tour reuses the tracked application icon as a hash-verified cover.
