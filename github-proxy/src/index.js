// Vesper GitHub Release proxy for the web player.
//
// Browsers cannot download GitHub Release assets: github.com and its asset
// host send no CORS headers. This Worker streams them with CORS and byte
// ranges, for catalogue apps only:
//
//   GET /<app_id>/<release>/<asset>
//
// The Worker reads the app's targets from the catalogue, takes its `github`
// source template and fills in the release and asset name, so it never fetches
// a repository the catalogue does not list. It needs no trust: the player
// verifies the manifest against the signed targets and every chunk against
// its hash, so a modified byte is rejected in the browser.

const SEGMENT = /^[A-Za-z0-9._+-]{1,200}$/;
const APP_ID = /^[A-Za-z0-9._-]{1,200}$/;
const GITHUB_TEMPLATE = /^https:\/\/github\.com\/([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)\/releases\/download\/\{release\}\/\{pack_id\}$/;
const TARGETS_TTL_MS = 5 * 60 * 1000;
const PASS_HEADERS = ["content-type", "content-length", "content-range", "accept-ranges", "etag", "last-modified"];
const CORS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, HEAD, OPTIONS",
  "Access-Control-Allow-Headers": "Range",
  "Access-Control-Expose-Headers": "Content-Range, Content-Length, Accept-Ranges, ETag",
  "Access-Control-Max-Age": "86400",
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

/** The GitHub download URL for an asset, from the app's signed targets payload, or null. */
export function githubUrl(targets, release, asset) {
  const sources = Array.isArray(targets?.signed?.sources) ? targets.signed.sources : [];
  for (const source of sources) {
    if (source?.kind !== "github" || typeof source.url_template !== "string") continue;
    const match = GITHUB_TEMPLATE.exec(source.url_template);
    if (!match) continue;
    return `https://github.com/${match[1]}/${match[2]}/releases/download/${encodeURIComponent(release)}/${encodeURIComponent(asset)}`;
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

  return {
    async fetch(request, env) {
      if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
      if (request.method !== "GET" && request.method !== "HEAD") return plain(405, "method not allowed");
      const url = new URL(request.url);
      if (url.pathname === "/") return plain(200, "Vesper GitHub Release proxy: GET /<app_id>/<release>/<asset>");
      const parts = parsePath(url.pathname);
      if (!parts || url.search) return plain(400, "expected /<app_id>/<release>/<asset>");
      if (!env?.CATALOGUE) return plain(500, "CATALOGUE is not configured");
      const targets = await targetsFor(env.CATALOGUE, parts.appId);
      if (!targets) return plain(404, "unknown app");
      const upstream = githubUrl(targets, parts.release, parts.asset);
      if (!upstream) return plain(404, "app has no GitHub Release source");

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
