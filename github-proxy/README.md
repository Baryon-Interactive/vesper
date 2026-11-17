# Vesper GitHub Release proxy

A Cloudflare Worker that lets the [web player](../web/README.md) read builds
published only to GitHub Releases. See the [project overview](../README.md)
and [publishing guide](../docs/publishing.md#web-builds).

Browsers cannot download GitHub Release assets directly: `github.com` and its
asset host send no CORS headers. The proxy streams them with CORS and byte
ranges:

```
GET /<app_id>/<release>/<asset>
```

It reads `apps/<app_id>/targets.json` from the catalogue (`CATALOGUE`), takes
the app's `github` source template and fills in the release and asset name.
It never fetches a repository that the catalogue does not list for that app,
and it never forwards cookies or other request headers except `Range`.

The proxy needs no trust. The player checks the manifest against the
developer-signed targets and every chunk against its BLAKE3 id, so a changed
byte fails in the browser. A broken or hostile proxy can stop a game from
loading, but cannot change what runs.

The proxy is a fallback. Every byte crosses the Worker, so request quotas and
GitHub's download speed limit how many players it can serve. Web builds
should be mirrored to R2 or another S3-compatible store with CORS, which the
player uses first; see the [publishing guide](../docs/publishing.md#web-builds).

## Deploy

```bash
npm --prefix github-proxy test
npx wrangler deploy --config github-proxy/wrangler.toml
```

Set `CATALOGUE` in `wrangler.toml` and add a route or custom domain for the
host name. Then build the web sites with `VESPER_GITHUB_PROXY` set to that
origin (see [web deployment](../web/README.md#deployment)).
