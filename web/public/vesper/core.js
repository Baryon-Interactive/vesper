// Vesper browser core, shared by the catalogue worker and the play service worker.
// Classic script: load it with importScripts() after vesper/pkg/vesper_web.js.
//
// JavaScript only moves bytes. Every trust decision (root chain, routes,
// developer and online signatures, expiry, rollback, store bytes, manifest
// hash, chunk hashes) is made by the Rust code in vesper_web.wasm, the same
// checks the desktop client runs.
/* global wasm_bindgen */
"use strict";

self.VesperCore = (() => {
  const STATE_CACHE = "vesper-state-v1";
  const CHUNK_CACHE = "vesper-chunks-v1";
  const MAX_FILE = 2 * 1024 * 1024 * 1024; // refuse decompressed files above 2 GiB
  const MAX_RANGE = 16 * 1024 * 1024; // coalesce frames into requests up to 16 MiB
  const MAX_GAP = 64 * 1024; // read through gaps this small instead of splitting
  const PARALLEL = 6;
  const ROOT_TTL_MS = 60_000;

  let wasmReady = null;
  let configPromise = null;
  let rootPromise = null;
  let rootAt = 0;
  const appCache = new Map();
  const inflight = new Map();
  const stats = { downloaded: 0, cached: 0, requests: 0 };
  let onProgress = () => {};

  const wasm = () => {
    if (!wasmReady) wasmReady = wasm_bindgen({ module_or_path: "/vesper/pkg/vesper_web_bg.wasm" }).then(() => wasm_bindgen);
    return wasmReady;
  };
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  const local = /^(localhost|127\.0\.0\.1|\[::1\])$/.test(self.location.hostname);

  /** Site configuration: catalogue base URL and the trust anchor shipped with the site. */
  function config() {
    if (!configPromise) {
      configPromise = (async () => {
        // A local development catalogue may override the production configuration.
        const names = local ? ["/vesper-config.dev.json", "/vesper-config.json"] : ["/vesper-config.json"];
        for (const name of names) {
          const response = await fetch(name, { cache: "no-store" });
          if (response.ok) {
            const value = await response.json();
            const apiBase = new URL(value.apiBase, self.location.origin).href.replace(/\/?$/, "/");
            return { ...value, apiBase };
          }
        }
        throw new Error("vesper-config.json is missing");
      })();
      configPromise.catch(() => (configPromise = null));
    }
    return configPromise;
  }

  async function fetchBytes(url, { optional = false } = {}) {
    const response = await fetch(url, { cache: "no-cache" });
    if (optional && (response.status === 404 || response.status === 410)) return null;
    if (!response.ok) throw new Error(`${response.status} ${response.statusText} for ${url}`);
    return new Uint8Array(await response.arrayBuffer());
  }

  // --- local state (Cache Storage, available to workers and service workers) ---
  async function stateGet(key) {
    const cache = await caches.open(STATE_CACHE);
    const hit = await cache.match(`/__vesper/state/${key}`);
    return hit ? new Uint8Array(await hit.arrayBuffer()) : null;
  }
  async function statePut(key, bytes) {
    const cache = await caches.open(STATE_CACHE);
    await cache.put(`/__vesper/state/${key}`, new Response(bytes));
  }
  async function stateDeletePrefix(prefix) {
    const cache = await caches.open(STATE_CACHE);
    for (const request of await cache.keys()) {
      if (new URL(request.url).pathname.startsWith(`/__vesper/state/${prefix}`)) await cache.delete(request);
    }
  }
  async function sha256Hex(bytes) {
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
    return Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
  }

  /**
   * TUF root chaining, as in the desktop client: start from the shipped anchor
   * (or the newest root verified from it), follow N+1.root.json until 404, then
   * require root.json to equal the newest root.
   */
  async function verifiedRoot() {
    if (rootPromise && Date.now() - rootAt < ROOT_TTL_MS) return rootPromise;
    rootAt = Date.now();
    rootPromise = (async () => {
      const w = await wasm();
      const { apiBase, trustedRoot } = await config();
      const anchor = await fetchBytes(new URL(trustedRoot, self.location.origin).href);
      w.rootAnchor(anchor);
      const anchorKey = `root-${(await sha256Hex(anchor)).slice(0, 16)}`;
      let trusted = anchor;
      const cached = await stateGet(anchorKey);
      if (cached) {
        try {
          if (w.rootAnchor(cached) > w.rootAnchor(anchor)) trusted = cached;
        } catch (error) {
          console.warn("ignoring unusable cached catalogue root", error);
        }
      }
      for (let i = 0; i < 256; i++) {
        const version = JSON.parse(dec.decode(trusted)).signed.version + 1;
        const next = await fetchBytes(`${apiBase}${version}.root.json`, { optional: true });
        if (!next) break;
        const step = JSON.parse(w.rootSuccessor(trusted, next));
        if (step.online_roles_changed) await stateDeletePrefix("seen-");
        await statePut(anchorKey, next);
        trusted = next;
      }
      w.rootCurrent(trusted, await fetchBytes(`${apiBase}root.json`));
      const routes = await fetchBytes(`${apiBase}routes.json`);
      const apps = JSON.parse(w.verifyRoutes(trusted, routes));
      return { root: trusted, routes, apps };
    })();
    rootPromise.catch(() => (rootPromise = null));
    return rootPromise;
  }

  /** Verify one application and record its versions so later visits reject rollbacks. */
  async function verifyApp(appId) {
    const w = await wasm();
    const { apiBase } = await config();
    const { root, routes } = await verifiedRoot();
    const dir = `${apiBase}apps/${encodeURIComponent(appId)}/`;
    const names = ["targets.json", "snapshot.json", "timestamp.json", "store.json", "description.md"];
    const [targets, snapshot, timestamp, store, description] = await Promise.all(names.map((n) => fetchBytes(dir + n)));
    const seenKey = `seen-${appId}`;
    const seen = await stateGet(seenKey);
    const result = JSON.parse(
      w.verifyApp(root, routes, appId, targets, snapshot, timestamp, store, description, seen ? dec.decode(seen) : ""),
    );
    await statePut(seenKey, enc.encode(JSON.stringify(result.seen)));
    appCache.set(appId, result);
    return result;
  }

  /** Catalogue: discovery ids from the static API, each verified independently. */
  async function listApps() {
    const { apiBase } = await config();
    const { apps: routed } = await verifiedRoot();
    const page = JSON.parse(dec.decode(await fetchBytes(`${apiBase}api/v1/apps/page-1.json`)));
    const ids = (page.apps || []).map((a) => a.app_id).filter((id) => routed.includes(id));
    const results = await Promise.allSettled(ids.map((id) => verifyApp(id)));
    return results.map((r, i) =>
      r.status === "fulfilled" ? summarize(r.value) : { app_id: ids[i], error: String(r.reason?.message || r.reason) },
    );
  }

  async function webChannel(result, requested = "stable") {
    const w = await wasm();
    return w.webChannel(JSON.stringify(result.targets), requested) ?? null;
  }

  function summarize(result) {
    return { app_id: result.app_id, store: result.store, targets_version: result.targets.version, channels: result.targets.channels };
  }

  async function appDetail(appId) {
    const result = await verifyApp(appId);
    return { ...summarize(result), description: result.description, web_channel: await webChannel(result) };
  }

  /** Download store media and check its signed BLAKE3 before display. */
  async function media(url, blake3) {
    const w = await wasm();
    const bytes = await fetchBytes(url);
    if (w.blake3Hex(bytes) !== String(blake3).toLowerCase()) throw new Error(`media hash mismatch for ${url}`);
    return bytes;
  }

  // --- builds ---

  const GITHUB_TEMPLATE = /^https:\/\/github\.com\/[^/]+\/[^/]+\/releases\/download\/\{release\}\/\{pack_id\}$/;

  /**
   * Download sources in the order the browser tries them. Sources that serve
   * CORS themselves (R2 and other S3-compatible stores, Hugging Face, HTTP)
   * come first, by priority. GitHub Releases send no CORS headers, so they go
   * last, through the GitHub proxy (`githubProxy` in the site configuration),
   * and are skipped when no proxy is configured.
   */
  function sourceUrls(apiBase, targets, appId, githubProxy) {
    const direct = [];
    const proxied = [];
    for (const s of [...targets.sources].sort((a, b) => a.priority - b.priority)) {
      if (s.kind === "github") {
        if (githubProxy && GITHUB_TEMPLATE.test(s.url_template)) {
          const base = githubProxy.replace(/\/+$/, "");
          proxied.push({ id: s.id, template: `${base}/${encodeURIComponent(appId)}/{release}/{pack_id}` });
        }
        continue;
      }
      direct.push({ id: s.id, template: /^https?:\/\//.test(s.url_template) ? s.url_template : apiBase + s.url_template });
    }
    return [...direct, ...proxied];
  }
  const fill = (template, release, name) =>
    template.replace("{release}", encodeURIComponent(release)).replace("{pack_id}", encodeURIComponent(name));

  /** Verify an application and open the manifest of its web build. */
  async function openBuild(appId, requested = "stable") {
    const w = await wasm();
    const { apiBase, githubProxy } = await config();
    const result = await verifyApp(appId);
    const channelName = await webChannel(result, requested);
    if (!channelName) throw new Error(`${appId} has no web build on the ${requested} channel`);
    const channel = result.targets.channels[channelName];
    const sources = sourceUrls(apiBase, result.targets, appId, githubProxy);
    let lastError = new Error("no download source the browser can read (GitHub Releases need the GitHub proxy)");
    for (const source of sources) {
      try {
        const bytes = await fetchBytes(fill(source.template, channel.manifest.release_tag, channel.manifest.asset_name));
        const manifest = w.WebManifest.open(bytes, BigInt(channel.manifest.size), channel.manifest.blake3);
        const summary = JSON.parse(manifest.summary());
        return { appId, channelName, channel, manifest, summary, sources, release: channel.manifest.release_tag, store: result.store };
      } catch (error) {
        console.warn(`${appId}: manifest source ${source.id} rejected`, error);
        lastError = error;
      }
    }
    throw lastError;
  }

  /** Fetch one coalesced byte range of a pack, trying each source in order. Only 206 is accepted. */
  async function fetchRange(build, pack, release, start, end) {
    let lastError;
    for (const source of build.sources) {
      const url = fill(source.template, release, pack);
      try {
        const response = await fetch(url, { headers: { Range: `bytes=${start}-${end - 1}` }, cache: "no-store" });
        if (response.status !== 206) throw new Error(`expected 206 from ${source.id}, got ${response.status}`);
        const range = /^bytes (\d+)-(\d+)\//.exec(response.headers.get("Content-Range") || "");
        if (!range || Number(range[1]) !== start || Number(range[2]) !== end - 1) throw new Error(`bad Content-Range from ${source.id}`);
        const bytes = new Uint8Array(await response.arrayBuffer());
        if (bytes.length !== end - start) throw new Error(`short range from ${source.id}`);
        stats.requests++;
        return bytes;
      } catch (error) {
        console.warn(`pack ${pack}: source ${source.id} failed`, error);
        lastError = error;
      }
    }
    throw lastError || new Error("no download source");
  }

  async function runLimited(tasks, limit) {
    let next = 0;
    const workers = Array.from({ length: Math.min(limit, tasks.length) }, async () => {
      while (next < tasks.length) await tasks[next++]();
    });
    await Promise.all(workers);
  }

  /**
   * Return verified raw bytes for every planned chunk. Cached chunks come from
   * Cache Storage; missing frames are grouped per pack, coalesced into Range
   * requests, decoded and verified in WebAssembly, then cached by chunk id.
   */
  async function loadChunks(build, chunks) {
    const w = await wasm();
    const cache = await caches.open(CHUNK_CACHE);
    const out = new Map();
    const missing = [];
    await Promise.all(
      chunks.map(async (c) => {
        if (out.has(c.id)) return;
        if (inflight.has(c.id)) return out.set(c.id, await inflight.get(c.id));
        const key = `/__vesper/chunk/${c.id}`;
        const hit = await cache.match(key);
        if (hit) {
          // Every game on the play origin can write to Cache Storage, so a
          // cached chunk is trusted only if it still hashes to its id.
          const raw = new Uint8Array(await hit.arrayBuffer());
          if (raw.length === c.raw_size && w.blake3Hex(raw) === c.id) {
            out.set(c.id, raw);
            stats.cached += c.raw_size;
            return;
          }
          console.warn(`vesper: discarding cached chunk ${c.id} that failed verification`);
          await cache.delete(key);
        }
        if (!missing.some((m) => m.id === c.id)) missing.push(c);
      }),
    );
    if (!missing.length) {
      onProgress({ ...stats });
      return out;
    }
    const groups = new Map();
    for (const c of missing) {
      const release = c.release || build.release;
      const key = `${release}\u0000${c.pack}`;
      if (!groups.has(key)) groups.set(key, { release, pack: c.pack, frames: [] });
      groups.get(key).frames.push(c);
    }
    const requests = [];
    for (const group of groups.values()) {
      group.frames.sort((a, b) => a.offset - b.offset);
      let current = null;
      for (const frame of group.frames) {
        const end = frame.offset + frame.frame_len;
        if (current && frame.offset - current.end <= MAX_GAP && end - current.start <= MAX_RANGE) {
          current.end = Math.max(current.end, end);
          current.frames.push(frame);
        } else {
          current = { ...group, start: frame.offset, end, frames: [frame] };
          requests.push(current);
        }
      }
    }
    const tasks = requests.map((r) => async () => {
      const work = (async () => {
        const bytes = await fetchRange(build, r.pack, r.release, r.start, r.end);
        stats.downloaded += bytes.length;
        for (const frame of r.frames) {
          const slice = bytes.subarray(frame.offset - r.start, frame.offset - r.start + frame.frame_len);
          const raw = w.decodeFrame(slice, frame.id, BigInt(frame.raw_size));
          out.set(frame.id, raw);
          await cache.put(`/__vesper/chunk/${frame.id}`, new Response(raw));
        }
        onProgress({ ...stats });
      })();
      for (const frame of r.frames) inflight.set(frame.id, work.then(() => out.get(frame.id)));
      try {
        await work;
      } finally {
        for (const frame of r.frames) inflight.delete(frame.id);
      }
    });
    await runLimited(tasks, PARALLEL);
    return out;
  }

  /** Read bytes start..end of a file in the build (end exclusive, clamped). */
  async function readFile(build, path, start = 0, end = Number.MAX_SAFE_INTEGER) {
    const plan = JSON.parse(build.manifest.plan(path, BigInt(start), BigInt(Math.min(end, Number.MAX_SAFE_INTEGER))));
    const stop = Math.min(end, plan.size);
    const length = Math.max(0, stop - start);
    const chunks = await loadChunks(build, plan.chunks);
    const out = new Uint8Array(length);
    for (const c of plan.chunks) {
      const raw = chunks.get(c.id);
      const from = Math.max(start, c.file_offset);
      const to = Math.min(stop, c.file_offset + c.raw_size);
      out.set(raw.subarray(from - c.file_offset, to - c.file_offset), from - start);
    }
    return { bytes: out, size: plan.size };
  }

  async function brotli(bytes) {
    const w = await wasm();
    return w.brotliDecompress(bytes, BigInt(MAX_FILE));
  }

  return {
    wasm,
    config,
    verifiedRoot,
    verifyApp,
    listApps,
    appDetail,
    media,
    openBuild,
    sourceUrls,
    readFile,
    brotli,
    stats,
    setProgress: (fn) => (onProgress = fn),
    resetStats: () => Object.assign(stats, { downloaded: 0, cached: 0, requests: 0 }),
  };
})();
