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

The store listing lives in `./metadata`: `store.json`, `description.md` and,
optionally, `changelog.md`. Start it with the CLI, then edit the files:

```powershell
vesper store init --title "My Game" --developer "My Studio" --homepage https://example.com --platform windows-x64
vesper store add-media ./art/header.png --as cover --url https://example.com/media/header.png
vesper store add-media ./art/shot-1.png --as screenshot --url https://example.com/media/shot-1.png
vesper store check
```

`add-media` records an image's BLAKE3 hash and size; host the image itself at
`--url` (your repository, a release or a CDN). Clients show only image bytes
that match the signed hash. `vesper store check` applies the same rules as the
catalogue, which rejects listings that break them. `publish` warns about the
same problems.

### What a listing can hold

`store.json` follows the [store schema](../index-template/schemas/store.schema.json).
Required: `title` and `summary` (maps from language tag to text), `tags`,
`screenshots`, `cover`, `requirements`, `paid`, `price_display`, `developer`,
`homepage` and `platforms`. Optional:

| Field | What the store shows |
|---|---|
| `release` | `{status, date}`: `coming-soon`, `early-access` or `released`, with an optional `YYYY-MM-DD` date. |
| `trailers` | Up to 10, shown before screenshots: `{kind: "youtube", id}` (loads from youtube-nocookie.com only when the player clicks play) or `{kind: "video", url, poster}` for an `.mp4` or `.webm` file you host, with a poster image record. Optional `title` map. |
| `capsule`, `logo`, `background` | Image records: a portrait shelf image, a transparent title logo shown instead of the title text, and the page backdrop. |
| `features` | `single-player`, `online-co-op`, `local-co-op`, `online-pvp`, `local-pvp`, `controller`, `keyboard-mouse`, `touch`, `cloud-saves`, `achievements`, `mod-support`, `level-editor`. |
| `max_players` | 1 to 1000. |
| `languages` | Language tag to `{interface, audio, subtitles}`, each true or false. |
| `system_requirements` | Platform to `{minimum, recommended}`, each with `os`, `cpu`, `gpu`, `memory`, `storage`, `network`, `notes`. |
| `links` | `discord`, `x`, `bluesky`, `youtube`, `twitch`, `reddit`, `steam`, `itch`, `github`, `wiki`, `press-kit`, `support` to https URLs. |
| `publishers` | 1 to 10 names. |
| `content_descriptors` | `violence`, `blood`, `gore`, `strong-language`, `sexual-content`, `nudity`, `drugs`, `alcohol`, `gambling`, `horror`. |
| `accessibility` | `subtitles`, `colorblind-modes`, `remappable-controls`, `adjustable-text-size`, `screen-reader`, `one-handed`, `difficulty-options`, `reduced-motion`, `no-flashing-lights`. |
| `default_language`, `updated` | The listing's main language tag and the date it last changed. |
| `play_params` | Query keys your web build accepts from play links, for example `["code"]` so `/play/<app_id>?code=ROOM` starts the game with `?code=ROOM`. 1 to 16 unique keys of letters, digits, `_` or `-` (at most 32 characters), not `app` or `channel`. The play site forwards only these keys, and drops values longer than 256 characters; without the field it forwards nothing. |

Unknown fields are rejected, so a typo fails `vesper store check` instead of
silently disappearing. `requirements` (free-form name to text) stays for older
listings; prefer `system_requirements`.

### Description and changelog

`description.md` and `changelog.md` are UTF-8 Markdown, up to 64 KiB each. The
store shows the changelog under "Updates", newest entry first as you write it.
Per-language versions are `description.<lang>.md` and `changelog.<lang>.md`
(for example `changelog.ja.md`); players see the one for their language and
fall back to the plain file. Every one of these files is signed: publishing
binds each file's length and SHA-256 into `targets.json`, up to 32 store files.

Signing more than `store.json` and `description.md` needs a catalogue that runs
Vesper 0.8 or later; older catalogues reject the listing. Older clients ignore
the new fields and documents.

Preserve the metadata directory, including its signed targets version history,
between publications. Keep it in your own game repository or durable CI storage.

### First build

```powershell
vesper check ./build --json
vesper pack --input ./build --output ./dist-v1 --app-id com.example.game --platform windows-x64 --version 1.0.0 --release-tag v1.0.0 --user-file 'Saved/**'
vesper inspect --manifest ./dist-v1/manifest.cbor.zst
vesper publish ./dist-v1 --owner example --repo game-builds --tag v1.0.0 --metadata-dir ./metadata --channel stable
```

Each channel holds one build for one platform. To ship several platforms, pack
each build with its `--platform` (`windows-x64`, `macos-arm64`, `macos-x64` or
`linux-x64`) and publish it to a channel named `<channel>-<platform>`, for
example `stable-windows-x64`, `stable-macos-arm64` and `stable-linux-x64`. When a
player installs `stable`, the client first looks for `stable-<platform>` for its
own computer, then accepts plain `stable` if that build's platform matches. Apple
Silicon Macs fall back to `macos-x64` builds, which run under Rosetta 2. On macOS
and Linux, files packed with an execute bit, and the manifest's launch
executable, are installed as executable.

The publisher uploads the build manifest and packs to the immutable build
release, checks remote assets, increments the local targets version, binds the
exact lengths and SHA-256 hashes of every store file, and signs with the developer
key. It creates the `vesper-metadata` Release if absent, without marking it as
the repository's latest release, and uploads `targets.json` and every store file
it signs, removing store files it no longer signs. Targets upload last; a reader
seeing mixed generations rejects the store binding and retains valid data. The
printed `metadata_url` is the location clients and aggregation read. `--metadata-dir` defaults to `./metadata`; `--metadata-tag` overrides the
metadata Release tag and must match the registered route. Build and metadata
tags must differ. `--dry-run` performs no upload or metadata write.

Use `GITHUB_TOKEN` or authenticated `gh` with permission to upload to your own
repository. Assets must be anonymously readable by clients. The CLI does not
create a repository, register routes, submit PRs or dispatch catalogue workflows.
For custom HTTPS hosting, copy `targets.json` and every store file it signs to
the registered base directory. No Pages setup is needed for Release hosting.

## Web builds

A `web-wasm32` build plays in the browser on the Vesper web store, with no
install. Pack the folder a static web server would host (`index.html` at the
top, relative paths) and publish it on its own channel:

```powershell
vesper pack --input ./build-web --output ./dist-web --app-id com.example.game --platform web-wasm32 --version 1.0.0 --release-tag v1.0.0-web
vesper publish ./dist-web --owner example --repo game-builds --tag v1.0.0-web --metadata-dir ./metadata --channel stable-web-wasm32
```

Add `web-wasm32` to `platforms` in `store.json`. Browsers download packs from
another origin, so every host they read must send CORS headers and answer
byte ranges with `206`.

**Host web builds on R2 or another S3-compatible store.** GitHub Releases
still hold every build, but GitHub sends no CORS headers. Browsers can reach
a GitHub-only build only through the catalogue's
[GitHub proxy](../github-proxy/README.md), which every game shares and which
is slower and quota-limited. With a mirror, the player downloads straight
from the bucket and uses the proxy only as a fallback. Desktop clients use
both sources as before.

### One-time setup

Do this once per game (or once for all your games, with one prefix each).
After that, every `vesper publish` uploads to GitHub and the bucket, signs
both sources and checks browser access, with nothing else to configure.

1. Create an R2 bucket and connect a custom domain to it, for example
   `cdn.example.com` (`r2.dev` addresses are rate limited). R2 needs a payment
   method; see [pricing](#optional-r2-mirror).
2. Add this CORS policy to the bucket (R2: Settings, CORS Policy):

   ```json
   [{"AllowedOrigins": ["*"], "AllowedMethods": ["GET", "HEAD"], "AllowedHeaders": ["Range"],
     "ExposeHeaders": ["Content-Range", "Content-Length", "Accept-Ranges", "ETag"], "MaxAgeSeconds": 86400}]
   ```

   Allowing every origin is safe: the objects are public anyway, and players
   verify every byte against the signed metadata.
3. Let Cloudflare cache the custom domain. It does not cache `.vpack` files by
   default, so every request would read the bucket. Add a Cache Rule (Caching,
   Cache Rules) with the expression `(http.host eq "cdn.example.com")`,
   Eligible for cache, Edge TTL "Ignore cache-control header and use this
   TTL" of one month, and a status code TTL of No store for codes 400 and
   above. Release assets never change under the same name, so a long TTL is
   safe.
4. Create an API token for that bucket with Object Read & Write permission.
5. Save its S3 credentials as `VESPER_R2_ACCESS_KEY_ID` and
   `VESPER_R2_SECRET_ACCESS_KEY` (environment variables or CI secrets), and the
   non-secret settings in `vesper.toml`:

   ```toml
   [r2]
   bucket = "game-builds"
   account_id = "your-account-id"
   prefix = "game"
   public_base = "https://cdn.example.com/game"
   ```

When a web build is published, `vesper publish` reads its manifest from
`public_base` as a browser on another origin would. It prints
`r2 browser access ok`, or a warning naming what is missing (a `206`
response, `Access-Control-Allow-Origin`, or `Content-Range` in
`Access-Control-Expose-Headers`).

Other S3-compatible stores work the same way. Set `endpoint` to the store's S3
endpoint and `region` to its signing region, for example
`endpoint = "https://s3.eu-west-1.amazonaws.com"` and `region = "eu-west-1"`
for AWS S3; R2 uses the default region `auto`. Requests use path-style
addresses (`<endpoint>/<bucket>/<key>`), which the store must accept.
`public_base` must be a public HTTPS address of the same objects, and the
bucket needs public read access and the CORS rule above. The credential
variables keep their `VESPER_R2_` names for every store.

Store media is read by browsers too. Put the cover and screenshots where
cross-origin reads work: the bucket, or files committed to a public
repository and linked through `raw.githubusercontent.com`. GitHub Release
asset URLs do not work in the browser.

### How browsers reach a build

The player tries the signed sources in this order and uses the first that
answers:

| Source | Run by | Limits |
|---|---|---|
| R2 or S3 mirror | You | Storage and request prices of the store; R2 has free egress (see [pricing](#optional-r2-mirror)). No GitHub and no Worker in the path, and no shared quota. |
| Your own relay | You | The relay's Cloudflare Workers plan (free: 100,000 requests a day per account). Every byte crosses the Worker and comes from GitHub at GitHub's speed. |
| The catalogue's GitHub proxy | The catalogue operator | Shared by every game in the catalogue, on the operator's Workers plan. A fallback, not a host. |

The player merges the ranges it needs, so a cold start sends few requests:
an 11 MB Unity build took 5. Returning players load most files from their
browser cache. On the shared proxy's free plan that is roughly 15,000 cold
starts a day for the whole catalogue, so games with many players should have
a mirror. GitHub may also slow down heavy download traffic from one source,
which all proxy and relay traffic is.

Measured on one connection with an 11 MB Unity web build: through the
proxy the browser downloaded it in 14 seconds; from R2 with the Cache Rule
from the setup above, in 3 to 5 seconds, with the first byte after 0.6
seconds instead of 1.6 to 3.3. Without that rule R2 was no faster than the
proxy, and a 103 MB desktop install took 55 to 84 seconds instead of 11.

### Your own relay

If you cannot use a bucket, run the GitHub proxy in relay mode in your own
Cloudflare account. It serves one repository, so its quota is yours alone:

```bash
git clone https://github.com/Baryon-Interactive/vesper && cd vesper
npx wrangler deploy --config github-proxy/wrangler.toml --name my-game-relay --var REPO:example/game-builds
```

Wrangler prints the relay address, for example
`https://my-game-relay.example.workers.dev` (a custom domain works too). Add it
to `vesper.toml` or pass `--relay` to `vesper publish`:

```toml
relay = "https://my-game-relay.example.workers.dev"
```

Every `vesper publish` with `--owner` and `--repo` then signs the relay as an
extra `http` source and checks that a browser can read the build through it
(`relay browser access ok`). The relay needs no trust: players verify every
byte against your signed metadata.

### Threads and SharedArrayBuffer

`SharedArrayBuffer`, and with it WebAssembly threads (Unity multithreading,
Godot thread support, Emscripten `-pthread`), is only available to pages that
are *cross-origin isolated*. This is a browser rule. A page is isolated only
when it and every page that embeds it are served with:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: credentialless
```

The Vesper store sends both headers on its play pages (`/play/*`), the play
site on every page, and the service worker adds the embedder policy to every
game file. Other store pages are not isolated, so they can embed YouTube
trailers; links into and out of a play page load a new page. In the game, `crossOriginIsolated` is
`true` and threaded builds run. The play bar's tooltip shows whether the
current browser isolated the game, and the play site logs it to the console.

- **Browser support.** Chromium-based browsers and Firefox support
  `credentialless`. Browsers that do not (Safari, at the time of writing)
  ignore it and run games without isolation, so `crossOriginIsolated` is
  `false` and `SharedArrayBuffer` is missing. Ship a single-threaded build,
  or check `crossOriginIsolated` and fall back.
- **What changes for game code.** Cross-origin requests that do not use CORS
  (`<img>`, `<script>` or `<link>` without `crossorigin`) still load, but
  without cookies. A cross-origin frame inside the game loads only if it
  sends an embedder policy of its own. Popups opened with `window.open` get no
  `window.opener` link back to the game, so sign-in flows must redirect
  instead of relying on a popup.
- **Hosting the sites yourself.** `npm run build` in `web/` writes these
  headers into `_headers` for both sites. Other hosts must send them too, and
  the launch page also needs `Cross-Origin-Resource-Policy: cross-origin` so
  the isolated store play page may embed it.

## Optional R2 mirror

Clients choose between GitHub and enabled mirrors using probes followed by
fresh content throughput. Useful backup sampling is limited to 5% of scheduled
bytes. Early small ranges allow a noisy initial ranking to be corrected:
a fresh source at least 5x faster switches in one observation, bypassing the
normal eight-completion cooldown; ordinary switches require over 1.5x for three
observations. Both measurements must be at most 30 seconds old. See
[download source selection](architecture.md#download-source-selection).

GitHub Releases is the default, with zero hosting configuration. An optional
Cloudflare R2 mirror suits large games or high download volumes, and web
builds should always have one (see [Web builds](#web-builds)). Standard R2
storage includes 10 GB-month free each month, then costs $0.015/GB-month. Class A
operations include 1 million free requests per month; Class B includes 10 million.
Egress traffic is free. Enabling R2 requires a payment method. Public `r2.dev`
addresses are rate limited and unsuitable for production; bind a custom domain.
See [Cloudflare pricing](https://developers.cloudflare.com/r2/pricing/) and
[public bucket access](https://developers.cloudflare.com/r2/buckets/public-buckets/).

Create a bucket in the R2 dashboard, then create an API token scoped to that
bucket with Object Read & Write permissions. Supply its S3 access key ID and
secret access key as `VESPER_R2_ACCESS_KEY_ID` and
`VESPER_R2_SECRET_ACCESS_KEY` through the process environment or CI secrets.
Credentials belong only in these environment variables; never put them in
`vesper.toml`, command arguments or committed files.

```powershell
vesper publish ./dist-v1 --owner example --repo game-builds --tag v1.0.0 --metadata-dir ./metadata --r2-bucket game-builds --r2-account-id <account-id> --r2-prefix game --r2-public-base https://cdn.example.com/game
```

The same mirror flags work with `--index ./index`. Optional non-secret defaults
come from `[r2]` in `vesper.toml` in the current directory; CLI values take
precedence independently for each field:

```toml
[r2]
bucket = "game-builds"
account_id = "your-account-id"
prefix = "game"
public_base = "https://cdn.example.com/game"
```

`endpoint` / `--r2-endpoint` overrides the S3 endpoint; the default is
`https://<account-id>.r2.cloudflarestorage.com`. Endpoint and public base require
HTTPS, with HTTP on `127.0.0.1` allowed for local tests. The public base includes
the prefix's public path. Objects use `<prefix>/<release_tag>/<asset_name>`, and
the signed source template is `<public_base>/{release}/{pack_id}`.

The mirror contains the manifest and packs referenced by every channel in targets,
including reused packs owned by older releases. HEAD skips an object only when
both size and `x-amz-meta-vesper-blake3` match; missing or different hash metadata
requires replacement, including same-size republished manifests. The metadata
header is included in SigV4 signed headers and checked again after upload.
Current-channel files come from the local build. Other channel manifests are
recovered anonymously from GitHub by their signed release tag and asset name,
checked against targets size/BLAKE3, then decoded to enumerate packs. Missing
packs use local files or streamed GitHub recovery into temporary files, with
length/BLAKE3 verification before upload. Staging files are removed on success
and failure. Any incomplete channel stops publication before targets change. Channels that
bind different content to the same release/asset key require distinct tags.
Successful publication signs GitHub at priority 0 and R2 at priority 1. Without
mirror configuration, publication signs only GitHub.

Uploads stream files in 64 KiB buffers after a streaming SHA-256 pass for SigV4;
pack bodies are never loaded into memory in full. Connections have a 30-second
deadline and transfers have a 60-second read/write idle deadline, with no total
transfer limit. R2 and GitHub retry transport failures, HTTP 408/429 and all 5xx
responses up to four attempts, waiting 250/500/1000 ms. Each upload attempt opens
the file afresh. Before a GitHub upload retry, any same-name asset, including a
starter asset, is deleted. If developer metadata publication fails after signing,
the error identifies the unpublished local targets version and directs the caller
to rerun `publish`; keep the local metadata directory for that rerun.

`--dry-run` performs mirror HEAD checks and GitHub asset existence/size checks,
prints `skipped`, `would-upload` or `would-recover-from-github`, and performs no
pack or manifest body download, upload or metadata write. Local files can be
hashed without network traffic. For retained remote channel manifests it lists
the manifest recovery plan; their pack inventory and content verification run
during actual publication because enumerating them requires reading the body.
Dry-run requires the same environment credentials to inspect the bucket.
`promote` rejects targets advertising R2 because it cannot verify mirror
completeness; use `publish --channel <destination>` with the retained build and
R2 settings instead. Both metadata hosting modes enforce the same rules.
Clients probe enabled sources and use the fastest; players can disable `r2`
in Settings. Mirror operators cannot authorize modified content: signed targets
bind the manifest, and manifests bind pack/chunk content hashes.

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
