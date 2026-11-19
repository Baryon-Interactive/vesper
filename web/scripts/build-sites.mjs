#!/usr/bin/env node
// Assemble the two deployable static sites after `vite build --outDir dist/store`:
//
//   dist/store  the store (React app, catalogue worker). Runs no game code.
//   dist/play   the play origin: launch page, service worker, chunk runtime.
//               Every game runs here, so it must be a different origin.
//
// Origins come from the environment; without them the build targets the local
// demo ports. Headers are written in Cloudflare Pages `_headers` format.
//   VESPER_STORE_ORIGIN=https://vesper.example VESPER_PLAY_ORIGIN=https://play.vesper.example \
//   VESPER_API_BASE=https://baryon-interactive.github.io/vesper-index/ \
//   VESPER_GITHUB_PROXY=https://gh.vesper.example npm run build
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const store = join(web, "dist", "store");
const play = join(web, "dist", "play");
const origin = (name, fallback) => {
  const value = process.env[name];
  if (!value) {
    console.warn(`${name} is not set; using ${fallback} (local demo build)`);
    return fallback;
  }
  const url = new URL(value);
  if (url.protocol !== "https:" && !/^(localhost|127\.0\.0\.1)$/.test(url.hostname)) throw new Error(`${name} must use https`);
  return url.origin;
};
const storeOrigin = origin("VESPER_STORE_ORIGIN", "http://127.0.0.1:3230");
const playOrigin = origin("VESPER_PLAY_ORIGIN", "http://127.0.0.1:3240");
if (storeOrigin === playOrigin) throw new Error("the store and play sites must be different origins");
const defaults = JSON.parse(readFileSync(join(web, "public", "vesper-config.json"), "utf8"));
const apiBase = process.env.VESPER_API_BASE || defaults.apiBase;
// Optional: origin of the GitHub Release proxy (github-proxy/) for builds without a CORS mirror.
const githubProxy = process.env.VESPER_GITHUB_PROXY ? origin("VESPER_GITHUB_PROXY", "") : undefined;
if (!githubProxy) console.warn("VESPER_GITHUB_PROXY is not set; builds hosted only on GitHub Releases will not play");
for (const file of ["vesper/pkg/vesper_web_bg.wasm", "vesper/root.json"]) {
  if (!existsSync(join(web, "public", file))) throw new Error(`public/${file} is missing; run npm run wasm first`);
}
const json = (value) => JSON.stringify(value, null, 2) + "\n";
const headers = (rules) => Object.entries(rules).map(([path, list]) => `${path}\n${list.map((h) => `  ${h}`).join("\n")}\n`).join("");
const common = ["X-Content-Type-Options: nosniff", "Referrer-Policy: strict-origin-when-cross-origin"];
// Cross-origin isolation, so games get SharedArrayBuffer and WebAssembly threads.
// Every document from the store's /play/ page down to the game must send these;
// the service worker adds COEP to game files. `credentialless` (not `require-corp`)
// still loads cross-origin images, fonts and scripts, without cookies.
// Browsers without `credentialless` ignore it and run games without isolation.
// Only /play/* is isolated on the store: YouTube trailers do not load inside an
// isolated page, so the store enters and leaves the play page with a full load.
const isolation = ["Cross-Origin-Opener-Policy: same-origin", "Cross-Origin-Embedder-Policy: credentialless"];
// Cloudflare Web Analytics: the host injects this beacon script into the HTML
// pages it serves, and the beacon reports to the second origin. Games under the
// play site's /run/ are served by the service worker, so the host never sees or
// injects into them.
const analyticsScript = "https://static.cloudflareinsights.com";
const analyticsReport = "https://cloudflareinsights.com";

// Store: drop local-only files, write its config and a strict CSP.
for (const file of ["vesper-config.dev.json", "vesper/dev-root.json"]) rmSync(join(store, file), { force: true });
// Link previews need an absolute image URL.
const page = join(store, "index.html");
writeFileSync(page, readFileSync(page, "utf8").replace('content="/brand/og-image.png"', `content="${storeOrigin}/brand/og-image.png"`));
writeFileSync(join(store, "vesper-config.json"), json({ ...defaults, apiBase, playOrigin }));
writeFileSync(
  join(store, "_headers"),
  headers({
    "/*": [
      `Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval' ${analyticsScript}; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src 'self' blob: data: https://i.ytimg.com; media-src https:; connect-src 'self' ${new URL(apiBase).origin} ${analyticsReport} https:; worker-src 'self'; frame-src ${playOrigin} https://www.youtube-nocookie.com; object-src 'none'; base-uri 'self'; frame-ancestors 'none'`,
      ...common,
    ],
    "/play/*": isolation,
    // Workers started by the isolated /play/ page must opt in as well, or the
    // browser may refuse to start them (the catalogue worker reads the listing).
    "/vesper/*": ["Cross-Origin-Embedder-Policy: credentialless"],
  }),
);

// Play: the launch page, service worker and shared runtime, nothing from the store.
rmSync(play, { recursive: true, force: true });
mkdirSync(join(play, "vesper"), { recursive: true });
for (const file of ["launch.html", "launch.js", "sw.js"]) copyFileSync(join(web, "play", file), join(play, file));
copyFileSync(join(web, "public", "favicon.svg"), join(play, "favicon.svg"));
copyFileSync(join(web, "public", "vesper", "core.js"), join(play, "vesper", "core.js"));
copyFileSync(join(web, "public", "vesper", "root.json"), join(play, "vesper", "root.json"));
cpSync(join(web, "public", "vesper", "pkg"), join(play, "vesper", "pkg"), { recursive: true });
writeFileSync(join(play, "vesper-config.json"), json({ apiBase, trustedRoot: "/vesper/root.json", storeOrigins: [storeOrigin], githubProxy }));
writeFileSync(join(play, "index.html"), `<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="0;url=${storeOrigin}/"><a href="${storeOrigin}/">Vesper</a>\n`);
// Hosts that map "/" to index.html use the page above; others follow this rule.
writeFileSync(join(play, "_redirects"), `/ ${storeOrigin}/ 302\n`);
// The play site's own pages run only its scripts and the analytics beacon.
// The policy is not set on "/*": game files come from the same origin and
// bring their own scripts, and a page's policy does not reach its frames.
const playPage = [
  `Content-Security-Policy: default-src 'self'; script-src 'self' ${analyticsScript}; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ${analyticsReport}; frame-src 'self'; worker-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'`,
];
writeFileSync(
  join(play, "_headers"),
  headers({
    // Only the store may embed the launch page. Games inherit no permissions
    // beyond what the store's iframe delegates.
    // CORP lets the isolated store page embed the launch page.
    "/*": [`Content-Security-Policy: frame-ancestors ${storeOrigin}`, ...isolation, "Cross-Origin-Resource-Policy: cross-origin", ...common],
    // A second Content-Security-Policy header adds a policy; both apply.
    "/launch.html": playPage,
    "/index.html": playPage,
    "/sw.js": ["Cache-Control: no-cache"],
  }),
);
console.log(`store ${storeOrigin} -> ${store}\nplay  ${playOrigin} -> ${play}\ncatalogue ${apiBase}`);
