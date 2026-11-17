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
//   VESPER_API_BASE=https://baryon-interactive.github.io/vesper-index/ npm run build
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
for (const file of ["vesper/pkg/vesper_web_bg.wasm", "vesper/root.json"]) {
  if (!existsSync(join(web, "public", file))) throw new Error(`public/${file} is missing; run npm run wasm first`);
}
const json = (value) => JSON.stringify(value, null, 2) + "\n";
const headers = (rules) => Object.entries(rules).map(([path, list]) => `${path}\n${list.map((h) => `  ${h}`).join("\n")}\n`).join("");
const common = ["X-Content-Type-Options: nosniff", "Referrer-Policy: strict-origin-when-cross-origin"];

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
      `Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src 'self' blob: data:; connect-src 'self' ${new URL(apiBase).origin} https:; worker-src 'self'; frame-src ${playOrigin}; object-src 'none'; base-uri 'self'; frame-ancestors 'none'`,
      ...common,
    ],
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
writeFileSync(join(play, "vesper-config.json"), json({ apiBase, trustedRoot: "/vesper/root.json", storeOrigins: [storeOrigin] }));
writeFileSync(join(play, "index.html"), `<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="0;url=${storeOrigin}/"><a href="${storeOrigin}/">Vesper</a>\n`);
// Hosts that map "/" to index.html use the page above; others follow this rule.
writeFileSync(join(play, "_redirects"), `/ ${storeOrigin}/ 302\n`);
writeFileSync(
  join(play, "_headers"),
  headers({
    // Only the store may embed the launch page. Games inherit no permissions
    // beyond what the store's iframe delegates.
    "/*": [`Content-Security-Policy: frame-ancestors ${storeOrigin}`, ...common],
    "/sw.js": ["Cache-Control: no-cache"],
  }),
);
console.log(`store ${storeOrigin} -> ${store}\nplay  ${playOrigin} -> ${play}\ncatalogue ${apiBase}`);
