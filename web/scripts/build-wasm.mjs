#!/usr/bin/env node
// Build crates/vesper-web for the browser and copy the trust anchor into public/.
//
//   node scripts/build-wasm.mjs [--skip-cargo]
//
// Requires the wasm32-unknown-unknown Rust target and wasm-bindgen-cli at the
// version pinned in crates/vesper-web/Cargo.toml. On Windows run it from an MSVC
// developer shell (cargo builds host build scripts with link.exe).
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repo = resolve(web, "..");
const out = join(web, "public", "vesper", "pkg");
const run = (cmd, args) => execFileSync(cmd, args, { cwd: repo, stdio: "inherit" });

if (!process.argv.includes("--skip-cargo")) {
  run("cargo", ["build", "--release", "--target", "wasm32-unknown-unknown", "-p", "vesper-web"]);
}
mkdirSync(out, { recursive: true });
// no-modules output loads with importScripts() in workers and the service worker.
run("wasm-bindgen", [
  "--target",
  "no-modules",
  "--no-typescript",
  "--out-dir",
  out,
  join(repo, "target", "wasm32-unknown-unknown", "release", "vesper_web.wasm"),
]);
// The site ships the same trust anchor as the desktop client.
copyFileSync(join(repo, "crates", "vesper-client", "config", "root.json"), join(web, "public", "vesper", "root.json"));
console.log(`wrote ${out} and public/vesper/root.json`);
