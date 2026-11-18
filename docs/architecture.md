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
| `vesper-publish` | GitHub release publishing, optional SigV4 R2 mirroring and asset verification |
| `vesper-cli` | Packaging, inspection, publishing, index maintenance and installation commands |
| `vesper-client` | All client logic: verified catalogue, settings, library, tasks, installation orchestration, gateway, credentials, media, runtime rules and game launch |
| `vesper-web` | WebAssembly build of the metadata, manifest and chunk checks for browsers |
| [Launcher](../launcher/README.md) | Tauri command wrappers, event forwarding, window setup and automatic updater |
| [Web](../web/README.md) | Web store, catalogue worker and the service worker that streams `web-wasm32` builds |
| [GitHub proxy](../github-proxy/README.md) | Worker that streams GitHub Release assets of catalogue apps to browsers with CORS and byte ranges |
| [Gateway](../gateway/README.md) | Paid-game authentication, grants and DepotKey delivery through Cloudflare Worker/D1 |
| [Index template](../index-template/README.md) | Registration, developer metadata aggregation, app PR validation, online signing and Pages |

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
It is the trust anchor only: the client follows renewed or rotated roots published
as `<N>.root.json` and caches the newest verified root under `trust/`
([root renewal](index-layout.md#root-renewal-and-rotation)).

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
   channel targets, binding store and description files. These three files live
   in the developer's metadata Release or HTTPS directory. A single registration
   PR adds developer keys and `metadata_url` to routes.
4. Central Actions aggregate every 15 minutes or on manual dispatch, checking
   signatures, store bindings, expiry and monotonic versions per app. Rejected
   apps retain their last valid data. Changes explicitly dispatch online signing
   and Pages deployment. Apps without a metadata location use app PRs.
5. Clients verify trust and manifest size/hash before fetching. Sources can be
   GitHub releases, Hugging Face, HTTP or R2; templates use `{release}` and `{pack_id}`.
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

## Download source selection

Priority establishes the initial ordering and breaks throughput ties. The client
persists source IDs and a timestamp per app in `metadata/<app_id>/source-ranking.json`;
a ranking at most 24 hours old supplies the next initial order. Disabled sources
are filtered before ranking. Manifest requests try all enabled sources in that
order, rejecting HTTP failures and incorrect length/BLAKE3 with stderr logs.

For content downloads with multiple sources, the fetcher probes each source in
parallel using a Range of up to 256 KiB (at most 1/128 of the sampled pack,
rounded up) from a referenced pack and a three-second
total timeout. Failed probes sort last without contributing failure counters.
Client setup is excluded from throughput timing. Measurements within 10% are
tied using the initial order. A single source sends
no probe. Probe bytes are additional traffic, are not reused, and are reported
separately by `probe_bytes()` and stderr; overhead is at most 256 KiB per source
for a conforming response. Chunk integrity checks still apply to all content.

During downloads a shared byte reservation budget routes useful ranges to
non-primary sources in round-robin order. Sampling accounts for at most 5% of
scheduled range bytes, reported by `sampling_bytes()` and stderr alongside the
budget. These bytes satisfy content requests and introduce no duplicate traffic;
startup probe bytes are separate. Single-source downloads neither probe nor
sample. A large range waits until sufficient budget accrues; no measurement is
used for switching when it becomes stale. Available backups receive periodic
ranges as the download proceeds, without requiring a slow-tail hedge.
Multi-source materialization dispatches the first merged range's first 40
frames separately before returning to the usual 16 MiB coalescing limit. This
gives the byte-budgeted sampler early opportunities on small downloads; each
frame is still fetched once and the 5% reservation limit applies unchanged.

Successful content ranges, including hedges and samples, update per-source EWMA
with alpha 0.5 using bytes per elapsed second of one request, never aggregate
worker throughput. Probe rates establish only the initial ranking; the first
content observation replaces that rate. Switching requires content observations
from both sources within 30 seconds, a candidate exceeding the primary by 1.5x
for three observations, and an eight-completion cooldown. A candidate at least
5x faster requires one observation and bypasses that cooldown, allowing the
first fresh backup sample to correct a noisy probe ranking. The same rule
applies after startup when a source slows sharply. Close speeds retain the
ordinary hysteresis. Stale observations reset their EWMA on the next sample;
probe-only or stale rates cannot supply switch votes. Ranking, sampling and
switch decisions are state transitions with injected rates and monotonic time.
Failover to a
successful backup also changes the primary. Probe outcomes, sampling bytes,
primary choices and switches are logged to stderr and ranking changes refresh
the app cache. Consecutive-failure demotion, slow-tail hedging and loser
cancellation remain active; cancellation does not count as a source failure.
Hedging requires an incomplete network body,
and observed per-request progress below one tenth of the completed-request
median after the existing tail deadline. A fully received body awaiting decode
is not hedged. With multiple sources a hedge requires an available alternate
and cannot repeat its original source. With a single source it repeats the
stalled range on a new connection to that source, retaining the same progress
threshold and loser cancellation. Healthy single-source downloads send no
duplicate content and still skip probing and sampling.

An R2 mirror is an availability/performance provider. It requires no content
trust: developer-signed targets bind manifest length/BLAKE3, the manifest binds
pack and chunk hashes, and decoded chunks are verified before installation.
All targets channels are mirrored before signing, with retained GitHub manifests
verified against targets before pack enumeration. HEAD checks both size and
BLAKE3 metadata, which is included in SigV4 signed PUT headers. Files are hashed
and uploaded in bounded streams; recovered packs are verified staging files.
Connection deadlines are 30 seconds, read/write idle deadlines 60 seconds, with
no total upload deadline. Transport failures and 408/429/5xx responses retry
with exponential backoff. GitHub retries delete any same-name asset and reopen
the file. Dry-run never reads remote object bodies and defers remote manifest
pack enumeration to actual publication. R2-backed `promote` returns an explicit
error directing the caller to verified `publish --channel` instead. GitHub
remains the default source.

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

Install/update checks prefer developer hosting using routes from central trust,
validating signatures and both store bindings. Failures log their reason and
fall back to the full central chain. `highest-targets.json` persists each app's
highest accepted payload; neither path permits rollback or same-version changes.
Catalogue browsing uses a separate central cache while aggregation catches up.
Deleting a route delists the app and prevents further direct checks.

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
files for routes without `metadata_url` qualify for automatic merge; hosted app
and online-role changes require a maintainer.
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

The desktop WebView uses a self-only default CSP and scripts, no eval, and
`img-src 'self' blob:`. The client returns verified image bytes over IPC; the UI
creates and revokes blob URLs instead of fetching external images. Inline styles
support React's computed progress widths; scripts do not permit unsafe-inline.
The native tour reuses the tracked application icon as a hash-verified cover.
