# Vesper web

The web store and in-browser player. Players browse the signed catalogue and
start web builds without installing anything. Desktop builds still install
through the [Vesper app](../launcher/README.md). See the
[project overview](../README.md) and [architecture](../docs/architecture.md).

## How it works

The browser runs the same checks as the desktop client. The
[`vesper-web`](../crates/vesper-web/) crate compiles `vesper-meta` and
`vesper-format` to WebAssembly. JavaScript only moves bytes; Rust decides what
to trust.

Games are third-party code, so they never run on the store's origin. The web
version is two static sites:

- The **store** (`src/`, `public/`) shows the catalogue and runs no game code
  and no service worker.
- The **play site** (`play/`, plus the shared runtime from `public/vesper/`)
  holds the service worker, the chunk cache and the launch page. Every game
  runs here, inside a frame the store embeds.

The store accepts messages only from that frame and the configured play
origin, and treats them as untrusted status text. The play site can be
embedded only by the configured store origins (`frame-ancestors`).

| Part | Role |
| --- | --- |
| `public/vesper/core.js` | Shared worker code: fetches, root chaining, rollback state, Range requests, chunk cache |
| `public/vesper/catalogue-worker.js` | Store: verifies the catalogue, app details and store media |
| `src/` | Store: React store, detail and play pages |
| `play/launch.html`, `play/launch.js` | Play site: registers the worker, opens the build, runs the game frame, reports progress to the store |
| `play/sw.js` | Play site: serves `/run/<app_id>/<path>` from verified chunks |
| `public/brand/` | Vesper mark (SVG), app icons and share image; the name and logo are excluded from the license, see [NOTICE](../NOTICE) |

1. The site ships a trust anchor (`public/vesper/root.json`, copied from the
   desktop client's [root](../crates/vesper-client/config/root.json) by
   `npm run wasm`). Workers follow `N+1.root.json`, require `root.json` to equal
   the newest root, then verify routes, developer-signed targets,
   snapshot/timestamp and the exact `store.json` and `description.md` bytes.
   Accepted versions are kept in Cache Storage, so a later visit rejects
   rollbacks. Store media is shown only after its BLAKE3 hash matches.
2. A game is playable in the browser when it publishes an unencrypted
   `web-wasm32` build, on `<channel>-web-wasm32` (preferred) or on `<channel>`.
3. "Play in browser" opens `/play/<app_id>` on the store, which frames
   `<play origin>/launch.html?app=<app_id>`. The play site's service worker
   verifies the catalogue again, downloads the manifest and checks its signed
   size and BLAKE3 hash. The launch page then loads `/run/<app_id>/index.html`
   (or the manifest's `.html` launch entry) in a nested frame.
4. Each request under `/run/` is planned against the manifest. Missing pack
   frames are fetched with coalesced HTTP Range requests (only `206` with a
   matching `Content-Range` is accepted), decoded and verified chunk by chunk,
   then cached by chunk id. All games share the play origin's Cache Storage,
   so a cached chunk is used only if it still hashes to its id; anything
   else is discarded and downloaded again. Range requests from the engine get `206`
   responses. Files ending in `.br` or `.gz` are decompressed first, as a web
   server sending `Content-Encoding` would do. The engine never sees bytes
   that failed verification.

Measured locally with a single-threaded Unity 6 WebGL build (11 MB packed):
the first visit downloaded 11 MB and Unity was ready in about 1.4 s. A reload
read everything from the chunk cache (0 bytes downloaded) and was ready in
about 1 s. Flipping one byte of the served pack made the page report
`stored tag mismatch`, and the engine failed to start instead of running bad
code. Overwriting every cached chunk of a game with a script payload, as a
hostile game on the play origin could, had no effect: the next load discarded
the entries and fetched verified bytes.

## Publishing a web build

```bash
vesper pack --input ./build-web --output ./dist-web --app-id com.example.game --platform web-wasm32 --version 1.0.0 --release-tag v1.0.0-web
vesper publish ./dist-web --owner example --repo game-builds --tag v1.0.0-web --metadata-dir ./metadata --channel stable-web-wasm32
```

Add `web-wasm32` to `platforms` in `store.json`. The build folder is what a
static web server would host: `index.html` at the top level and relative
paths for everything else. Engine output without its own compression packs
best, because Vesper already compresses each chunk, and unchanged chunks are
reused across updates. Brotli or gzip output still works but changes as a
whole on every build.

Browsers download packs directly, so each source must allow cross-origin
`GET` with a `Range` header and expose `Content-Range`. Cloudflare R2 and
Hugging Face can do this. GitHub Release downloads send no CORS headers, so a
GitHub-only game needs a CORS proxy source before it can play in the browser.
Some static hosts ignore `Range` and always send the whole file with `200`
(Cloudflare Workers static assets do); put a small Worker in front that
answers single ranges with `206`, or use R2.

## Development

```powershell
npm --prefix web ci
npm --prefix web run wasm      # from an x64 Visual Studio developer shell on Windows
npm --prefix web test
Push-Location web; npx tsc --noEmit; Pop-Location
```

`npm run wasm` needs the `wasm32-unknown-unknown` Rust target and
`wasm-bindgen-cli` at the version pinned in
[the crate manifest](../crates/vesper-web/Cargo.toml).

Local demo with a signed catalogue (development keys only):

```powershell
powershell -ExecutionPolicy Bypass -File web/dev/start.ps1
node web/dev/serve.mjs --root web/dev/out --port 3220
npm --prefix web run dev       # http://127.0.0.1:3230
```

The script publishes the bundled demo game, Ember Drift, as a web build.
Pass `-GameId`, `-GameTitle`, `-GameSummary`, `-GameWeb`, `-GameWindows` and
`-GameCover` to add one of your own builds alongside it. It writes
`public/vesper-config.dev.json` and `play/vesper-config.dev.json`, which only
pages served from localhost read. Serve the play site on its own port so it is
a separate origin, as in production:

```powershell
node web/dev/serve.mjs --root web/play --root web/public --port 3240
```

`serve.mjs` serves byte ranges with CORS, as a production source must, and
overlays several `--root` folders.

## Deployment

```bash
VESPER_STORE_ORIGIN=https://store.example VESPER_PLAY_ORIGIN=https://play.example VESPER_API_BASE=https://baryon-interactive.github.io/vesper-index/ npm --prefix web run build
```

This writes two static sites, `web/dist/store` and `web/dist/play`. Each
gets its own `vesper-config.json` and a Cloudflare Pages `_headers` file. The
store's Content Security Policy allows only its own scripts and WebAssembly,
the catalogue and https media, and frames from the play origin. The play
site allows framing only by the store. Deploy them to two different origins,
each at the root of its host name, because the service worker and runtime use
absolute paths. The store is a single-page app: unknown paths must serve
`index.html`, which Cloudflare Pages does by default when there is no
`404.html`. The play site also gets a `_redirects` rule that sends `/` to the
store. To test a production build locally, build with `127.0.0.1`
origins and serve each folder with `serve.mjs` (`--spa` for the store); it
applies the `/*` rules from `_headers`.

## Not done yet

- Paid builds: encrypted manifests are refused in the browser, and DepotKey
  delivery to the browser is not designed yet.
- Multi-threaded engines need `SharedArrayBuffer`, which needs COOP/COEP
  headers on the play site, and a store frame that allows cross-origin isolation.
- Games share one play origin, so they share its storage (saves, Cache
  Storage). Verified chunks cannot be poisoned, but one game can read or clear
  another's saves, or clear the rollback records. A per-game origin would
  remove this.
- No synchronous file access for engines that read with `fread`
  (Emscripten/WASI bridging). Engines must load files with `fetch` or XHR.
- The chunk cache uses Cache Storage, with no quota management or eviction.
- No CORS proxy for GitHub Releases, no pre-rendered store pages for search
  engines, no cloud saves, and no deployment workflow yet. The public store at
  <https://vesperplay.xyz> is deployed by hand.
