// Vesper play service worker: serves /run/<app_id>/<path> from verified pack chunks.
// It is deployed only on the play origin, never on the store origin.
//
// The play page asks this worker to prepare an application. The worker
// verifies the catalogue chain and the web build's manifest, and from then on
// answers every request the game makes under /run/<app_id>/ by planning the
// pack frames that cover the requested bytes, fetching missing frames with
// HTTP Range, verifying each chunk and assembling the response. Engines see an
// ordinary static web server, including 206 responses to Range requests.
/* global VesperCore */
"use strict";
importScripts("/vesper/pkg/vesper_web.js", "/vesper/core.js");

const builds = new Map(); // app_id -> Promise<build>
const BUILD_KEY = "/__vesper/state/play-channel-";

const TYPES = {
  html: "text/html; charset=utf-8",
  js: "text/javascript",
  mjs: "text/javascript",
  css: "text/css",
  json: "application/json",
  wasm: "application/wasm",
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  ico: "image/x-icon",
  mp3: "audio/mpeg",
  ogg: "audio/ogg",
  wav: "audio/wav",
  mp4: "video/mp4",
  webm: "video/webm",
  txt: "text/plain; charset=utf-8",
  data: "application/octet-stream",
};

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

async function broadcast(message) {
  for (const client of await self.clients.matchAll({ includeUncontrolled: true })) client.postMessage(message);
}
VesperCore.setProgress((stats) => broadcast({ type: "vesper-progress", stats }));

async function rememberChannel(appId, channel) {
  const cache = await caches.open("vesper-state-v1");
  await cache.put(BUILD_KEY + appId, new Response(channel));
}
async function rememberedChannel(appId) {
  const cache = await caches.open("vesper-state-v1");
  const hit = await cache.match(BUILD_KEY + appId);
  return hit ? hit.text() : "stable";
}

/** Verify and open a web build once per worker lifetime. */
function build(appId, channel) {
  if (!builds.has(appId)) {
    const promise = (async () => VesperCore.openBuild(appId, channel || (await rememberedChannel(appId))))();
    builds.set(appId, promise);
    promise.catch(() => builds.delete(appId));
  }
  return builds.get(appId);
}

self.addEventListener("message", (event) => {
  const { type, appId, channel, id } = event.data || {};
  if (type !== "vesper-prepare") return;
  const reply = (message) => event.source && event.source.postMessage({ ...message, id });
  event.waitUntil(
    (async () => {
      try {
        builds.delete(appId); // a page load always re-verifies the catalogue
        VesperCore.resetStats();
        await rememberChannel(appId, channel || "stable");
        const b = await build(appId, channel);
        reply({ type: "vesper-ready", appId, version: b.summary.version, channel: b.channelName, files: b.summary.files.length, entry: entryOf(b) });
      } catch (error) {
        reply({ type: "vesper-error", appId, message: String(error?.message || error) });
      }
    })(),
  );
});

function entryOf(b) {
  const launch = b.summary.launch;
  return launch && /\.html?$/i.test(launch) ? launch : "index.html";
}

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin || !url.pathname.startsWith("/run/")) return;
  event.respondWith(serve(event.request, url));
});

function parseRange(header, size) {
  const m = /^bytes=(\d*)-(\d*)$/.exec(header || "");
  if (!m || (!m[1] && !m[2])) return null;
  let start;
  let end;
  if (!m[1]) {
    start = Math.max(0, size - Number(m[2]));
    end = size;
  } else {
    start = Number(m[1]);
    end = m[2] ? Math.min(size, Number(m[2]) + 1) : size;
  }
  return start < end ? { start, end } : "unsatisfiable";
}

async function serve(request, url) {
  const parts = url.pathname.slice("/run/".length).split("/");
  const appId = decodeURIComponent(parts.shift() || "");
  let path = parts.map(decodeURIComponent).join("/");
  try {
    const b = await build(appId);
    if (path === "" || path.endsWith("/")) path += entryOf(b);
    if (!b.summary.files.some((f) => f.path === path)) return new Response("Not found in this build", { status: 404 });
    const name = path.toLowerCase();
    // Precompressed engine output (.br/.gz) is decoded here, as a web server with
    // Content-Encoding would have the browser do, so loaders get plain bytes.
    const encoded = name.endsWith(".br") ? "br" : name.endsWith(".gz") ? "gzip" : null;
    const base = encoded ? name.replace(/\.(br|gz)$/, "") : name;
    const ext = base.includes(".") ? base.split(".").pop() : "";
    const config = await VesperCore.config();
    const headers = new Headers({
      "Content-Type": TYPES[ext] || "application/octet-stream",
      "Cache-Control": "no-cache",
      "Cross-Origin-Resource-Policy": "same-origin",
      // Game documents and worker scripts must opt in to cross-origin isolation
      // for the game to get SharedArrayBuffer (docs/publishing.md#threads).
      "Cross-Origin-Embedder-Policy": "credentialless",
      // Game files may only be framed by the launch page on this origin, inside
      // the store (frame-ancestors checks every ancestor, up to the top page).
      "Content-Security-Policy": `frame-ancestors 'self' ${(config.storeOrigins || []).join(" ")}`.trim(),
    });
    if (request.method === "HEAD") return new Response(null, { status: 200, headers });
    if (encoded) {
      const { bytes } = await VesperCore.readFile(b, path);
      const body =
        encoded === "br"
          ? await VesperCore.brotli(bytes)
          : new Uint8Array(await new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"))).arrayBuffer());
      headers.set("Content-Length", String(body.length));
      return new Response(body, { status: 200, headers });
    }
    const size = b.summary.files.find((f) => f.path === path).size;
    headers.set("Accept-Ranges", "bytes");
    const range = parseRange(request.headers.get("Range"), size);
    if (range === "unsatisfiable") {
      headers.set("Content-Range", `bytes */${size}`);
      return new Response(null, { status: 416, headers });
    }
    if (range) {
      const { bytes } = await VesperCore.readFile(b, path, range.start, range.end);
      headers.set("Content-Range", `bytes ${range.start}-${range.end - 1}/${size}`);
      headers.set("Content-Length", String(bytes.length));
      return new Response(bytes, { status: 206, headers });
    }
    const { bytes } = await VesperCore.readFile(b, path);
    headers.set("Content-Length", String(bytes.length));
    return new Response(bytes, { status: 200, headers });
  } catch (error) {
    console.error(`vesper: ${url.pathname}`, error);
    await broadcast({ type: "vesper-error", appId, message: String(error?.message || error) });
    return new Response(`Vesper could not serve this file: ${error?.message || error}`, { status: 502 });
  }
}
