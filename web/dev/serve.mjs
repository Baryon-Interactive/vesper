#!/usr/bin/env node
// Local static server for the web demo: byte ranges (206) and CORS, the two
// things a browser needs from any Vesper download source. Several --root
// directories are overlaid in order, so the play site can be served from
// web/play plus the shared files in web/public without a build step.
//   node web/dev/serve.mjs --root web/dev/out --port 3220
//   node web/dev/serve.mjs --root web/play --root web/public --port 3240
import { createReadStream, readFileSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize, resolve, sep } from "node:path";

const arg = (name, fallback) => {
  const i = process.argv.indexOf(name);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const roots = process.argv.flatMap((v, i) => (process.argv[i - 1] === "--root" ? [resolve(v)] : []));
if (!roots.length) roots.push(resolve("web/dev/out"));
const port = Number(arg("--port", "3220"));
const spa = process.argv.includes("--spa"); // serve index.html for unknown extensionless paths
// Apply "/*" rules from a Cloudflare Pages style _headers file, if a root has one.
const extraHeaders = {};
for (const root of roots) {
  try {
    let block = null;
    for (const line of readFileSync(join(root, "_headers"), "utf8").split(/\r?\n/)) {
      if (!line.trim()) continue;
      if (!/^\s/.test(line)) block = line.trim();
      else if (block === "/*") {
        const at = line.indexOf(":");
        extraHeaders[line.slice(0, at).trim()] = line.slice(at + 1).trim();
      }
    }
    break;
  } catch {
    // no _headers in this root
  }
}
const types = {
  ".json": "application/json",
  ".md": "text/markdown; charset=utf-8",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".wasm": "application/wasm",
  ".svg": "image/svg+xml",
  ".css": "text/css",
  ".webmanifest": "application/manifest+json",
};

createServer((req, res) => {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Headers", "Range");
  res.setHeader("Access-Control-Expose-Headers", "Content-Range, Content-Length, Accept-Ranges");
  if (req.method === "OPTIONS") return res.writeHead(204).end();
  const pathname = decodeURIComponent(new URL(req.url, "http://x").pathname);
  let path;
  let stat;
  for (const root of roots) {
    const candidate = normalize(join(root, pathname));
    if (!candidate.startsWith(root + sep)) continue;
    try {
      const found = statSync(candidate);
      if (found.isFile()) {
        path = candidate;
        stat = found;
        break;
      }
    } catch {
      // try the next root
    }
  }
  if (!path && spa && !extname(pathname)) {
    path = join(roots[0], "index.html");
    stat = statSync(path);
  }
  if (!path) return res.writeHead(404).end("not found");
  for (const [name, value] of Object.entries(extraHeaders)) res.setHeader(name, value);
  const headers = { "Content-Type": types[extname(path)] || "application/octet-stream", "Accept-Ranges": "bytes", "Cache-Control": "no-cache" };
  const m = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || "");
  if (m) {
    const start = Number(m[1]);
    const end = Math.min(m[2] ? Number(m[2]) : stat.size - 1, stat.size - 1);
    if (start > end) return res.writeHead(416, { "Content-Range": `bytes */${stat.size}` }).end();
    res.writeHead(206, { ...headers, "Content-Range": `bytes ${start}-${end}/${stat.size}`, "Content-Length": end - start + 1 });
    return req.method === "HEAD" ? res.end() : createReadStream(path, { start, end }).pipe(res);
  }
  res.writeHead(200, { ...headers, "Content-Length": stat.size });
  return req.method === "HEAD" ? res.end() : createReadStream(path).pipe(res);
}).listen(port, "127.0.0.1", () => console.log(`Vesper web demo catalogue: http://127.0.0.1:${port}/ (${roots.join(", ")})`));
