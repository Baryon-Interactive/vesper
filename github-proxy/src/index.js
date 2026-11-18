// Vesper GitHub Release proxy for the web player.
//
// Browsers cannot download GitHub Release assets: github.com and its asset
// host send no CORS headers. This Worker streams them with CORS and byte
// ranges. It runs in one of two modes:
//
//   catalogue mode (CATALOGUE set)  GET /<app_id>/<release>/<asset>
//     The shared proxy for every app in a catalogue. It reads the app's
//     targets, takes its `github` source template and fills in the release and
//     asset name, so it never fetches a repository the catalogue does not list.
//
//   relay mode (REPO = "owner/repo")  GET /<release>/<asset>
//     A developer's own relay for one repository, signed into their targets
//     as an ordinary `http` source (vesper publish --relay).
//
// Neither mode needs trust: the player verifies the manifest against the
// signed targets and every chunk against its hash, so a modified byte is
// rejected in the browser.

const SEGMENT = /^[A-Za-z0-9._+-]{1,200}$/;
const APP_ID = /^[A-Za-z0-9._-]{1,200}$/;
const REPO = /^([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)$/;
const GITHUB_TEMPLATE = /^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)\/releases\/download\/\{release\}\/\{pack_id\}$/;
const TARGETS_TTL_MS = 5 * 60 * 1000;
const PASS_HEADERS = ["content-type", "content-length", "content-range", "accept-ranges", "etag", "last-modified"];
const CORS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, HEAD, OPTIONS",
  "Access-Control-Allow-Headers": "Range",
  "Access-Control-Expose-Headers": "Content-Range, Content-Length, Accept-Ranges, ETag",
  "Access-Control-Max-Age": "86400",
  // Lets cross-origin isolated pages (COEP) load assets even without CORS mode.
  "Cross-Origin-Resource-Policy": "cross-origin",
};

const plain = (status, text) =>
  new Response(text + "\n", { status, headers: { ...CORS, "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store" } });

const safeSegment = (value) => SEGMENT.test(value) && value !== "." && value !== "..";

/** Split `/<app_id>/<release>/<asset>` into decoded, validated parts, or null. */
export function parsePath(pathname) {
  const parts = pathname.split("/").slice(1);
  if (parts.length !== 3) return null;
  let decoded;
  try {
    decoded = parts.map(decodeURIComponent);
  } catch {
    return null;
  }
  const [appId, release, asset] = decoded;
  if (!APP_ID.test(appId) || appId === "." || appId === ".." || !safeSegment(release) || !safeSegment(asset)) return null;
  return { appId, release, asset };
}

/** Split `/<release>/<asset>` (relay mode) into decoded, validated parts, or null. */
export function parseRelayPath(pathname) {
  const parts = pathname.split("/").slice(1);
  if (parts.length !== 2) return null;
  let decoded;
  try {
    decoded = parts.map(decodeURIComponent);
  } catch {
    return null;
  }
  const [release, asset] = decoded;
  return safeSegment(release) && safeSegment(asset) ? { release, asset } : null;
}

const releaseUrl = (owner, repo, release, asset) =>
  `https://github.com/${owner}/${repo}/releases/download/${encodeURIComponent(release)}/${encodeURIComponent(asset)}`;

/** The GitHub download URL for an asset, from the app's signed targets payload, or null. */
export function githubUrl(targets, release, asset) {
  const sources = Array.isArray(targets?.signed?.sources) ? targets.signed.sources : [];
  for (const source of sources) {
    if (source?.kind !== "github" || typeof source.url_template !== "string") continue;
    const match = GITHUB_TEMPLATE.exec(source.url_template);
    if (!match) continue;
    return releaseUrl(match[1], match[2], release, asset);
  }
  return null;
}

/** Create the Worker. `fetchImpl` and `now` are injectable for tests. */
export function createProxy({ fetchImpl = (...args) => fetch(...args), now = () => Date.now() } = {}) {
  const targetsCache = new Map();

  async function targetsFor(catalogue, appId) {
    const hit = targetsCache.get(appId);
    if (hit && now() - hit.at < TARGETS_TTL_MS) return hit.value;
    const url = new URL(`apps/${encodeURIComponent(appId)}/targets.json`, catalogue).href;
    const response = await fetchImpl(url, { cf: { cacheTtl: 300, cacheEverything: true } });
    const value = response.ok ? await response.json().catch(() => null) : null;
    targetsCache.set(appId, { at: now(), value });
    return value;
  }

  /** The upstream GitHub URL for a request, or an error response. */
  async function resolve(url, env) {
    const repo = env?.REPO ? REPO.exec(env.REPO) : null;
    if (env?.REPO && !repo) return plain(500, "REPO must be owner/repo");
    if (repo) {
      if (url.pathname === "/") return plain(200, `Vesper relay for ${env.REPO}: GET /<release>/<asset>`);
      const parts = parseRelayPath(url.pathname);
      if (!parts || url.search) return plain(400, "expected /<release>/<asset>");
      return releaseUrl(repo[1], repo[2], parts.release, parts.asset);
    }
    if (url.pathname === "/") return plain(200, "Vesper GitHub Release proxy: GET /<app_id>/<release>/<asset>");
    const parts = parsePath(url.pathname);
    if (!parts || url.search) return plain(400, "expected /<app_id>/<release>/<asset>");
    if (!env?.CATALOGUE) return plain(500, "set CATALOGUE (catalogue mode) or REPO (relay mode)");
    const targets = await targetsFor(env.CATALOGUE, parts.appId);
    if (!targets) return plain(404, "unknown app");
    return githubUrl(targets, parts.release, parts.asset) ?? plain(404, "app has no GitHub Release source");
  }

  return {
    async fetch(request, env) {
      if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
      if (request.method !== "GET" && request.method !== "HEAD") return plain(405, "method not allowed");
      const url = new URL(request.url);
      const upstream = await resolve(url, env);
      if (upstream instanceof Response) return upstream;

      const headers = new Headers();
      const range = request.headers.get("Range");
      if (range) headers.set("Range", range);
      const response = await fetchImpl(upstream, { method: request.method, headers, redirect: "follow" });
      const out = new Headers(CORS);
      for (const name of PASS_HEADERS) {
        const value = response.headers.get(name);
        if (value !== null) out.set(name, value);
      }
      // Release assets are immutable per tag and name; the player verifies them anyway.
      out.set("Cache-Control", response.ok ? "public, max-age=3600" : "no-store");
      out.set("X-Content-Type-Options", "nosniff");
      return new Response(request.method === "HEAD" ? null : response.body, { status: response.status, headers: out });
    },
  };
}

export default createProxy();
