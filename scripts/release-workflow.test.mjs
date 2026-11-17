import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const workflow = fs.readFileSync(path.join(root, ".github/workflows/release.yml"), "utf8");
const notes = fs.readFileSync(path.join(root, "scripts/release-notes.md"), "utf8");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const section = (name, next) => workflow.slice(workflow.indexOf(`\n  ${name}:`), next ? workflow.indexOf(`\n  ${next}:`) : undefined);

const clientAssets = [
  "Vesper-Setup-$VESPER_VERSION-windows-x64.exe",
  "Vesper-$VESPER_VERSION-macos-universal.dmg",
  "Vesper-$VESPER_VERSION-linux-x64.AppImage",
  "Vesper-$VESPER_VERSION-linux-x64.deb",
];
const cliAssets = ["vesper-cli-linux-x64", "vesper-cli-windows-x64.exe", "vesper-cli-macos-arm64", "vesper-cli-macos-x64"];

test("release workflow has tag, pull request, and manual build triggers", () => {
  assert.match(workflow, /push:\s*\n\s*tags:\s*\["vesper-v\*"\]/);
  assert.match(workflow, /\n\s*pull_request:\s*\n/);
  assert.match(workflow, /\n\s*workflow_dispatch:\s*\n/);
  assert.match(workflow, /if: startsWith\(github\.ref, 'refs\/tags\/vesper-v'\)/);
});

test("all release jobs and actions are present and actions are pinned", () => {
  for (const name of [
    "linux-cli",
    "windows-cli",
    "macos-cli-arm64",
    "macos-cli-x64",
    "windows-client",
    "macos-client",
    "linux-client",
    "publish",
  ]) assert.match(workflow, new RegExp(`^  ${name}:`, "m"));
  for (const line of workflow.split("\n")) {
    if (/uses:/.test(line) && !/uses: \.\//.test(line))
      assert.match(line, /uses: [\w/-]+@[0-9a-f]{40} # v[\d.]+$/);
  }
  assert.match(workflow, /permissions:\s*\n\s*contents: read/);
  assert.match(workflow, /publish:[\s\S]*?permissions:\s*\n\s*contents: write/);
  assert.match(
    section("publish"),
    /needs: \[linux-cli, windows-cli, macos-cli-arm64, macos-cli-x64, windows-client, macos-client, linux-client\]/,
  );
});

test("workflow builds, checks and uploads every required asset", () => {
  for (const asset of [...clientAssets, ...cliAssets, "LICENSE", "NOTICE", "SHA256SUMS"])
    assert.match(workflow, new RegExp(escape(asset)));
  assert.match(workflow, /find \. -maxdepth 1 -type f ! -name SHA256SUMS/);
  assert.match(workflow, /test "\$\(wc -l < dist\/SHA256SUMS\)" -eq 10/);
  // Each SHA256SUMS line is "<64 hex>  <name>"; names are compared literally so "." in the version is not a wildcard.
  assert.ok(workflow.includes("NF == 2 && length($1) == 64 && $1 ~ /^[0-9a-f]+$/ && $2 == n"));
  const checked = workflow.slice(workflow.indexOf("for name in"), workflow.indexOf("awk -v n="));
  const uploaded = workflow.slice(workflow.indexOf("gh release create"));
  for (const name of [...clientAssets, ...cliAssets, "LICENSE", "NOTICE"]) {
    assert.ok(checked.includes(name), `${name} is checked`);
    assert.ok(uploaded.includes(name), `${name} is uploaded`);
  }
  assert.ok(uploaded.includes("dist/SHA256SUMS"));
  assert.match(workflow, /--target "\$GITHUB_SHA"/);
});

test("release notes separate the player client from the developer CLI", () => {
  assert.match(notes, /## Players: download the Vesper client/);
  assert.match(notes, /## Developers: vesper CLI/);
  for (const asset of clientAssets) assert.ok(notes.includes(asset.replace("$VESPER_VERSION", "@VERSION@")), asset);
  for (const asset of cliAssets) assert.ok(notes.includes(asset), asset);
  assert.match(notes, /xattr -d com\.apple\.quarantine <file>/);
  assert.match(notes, /chmod \+x/);
  assert.ok(notes.indexOf("Players:") < notes.indexOf("Developers:"));
});

test("tag validation and CLI version checks use the workspace version", () => {
  assert.match(workflow, /tag_version="\$\{GITHUB_REF_NAME#vesper-v\}"/);
  assert.match(workflow, /text = Path\("Cargo\.toml"\)/);
  assert.match(workflow, /test "\$tag_version" = "\$workspace_version"/);
  assert.match(workflow, /test "\$workspace_version" = "\$VESPER_VERSION"/);
  assert.equal((workflow.match(/"vesper \$VESPER_VERSION \(\$VESPER_GIT_SHA\)"/g) ?? []).length, 3);
  assert.match(workflow, /"vesper \$\(\$env:VESPER_VERSION\) \(\$env:VESPER_GIT_SHA\)"/);
  assert.match(workflow, /Unexpected CLI version/);
  const version = workflow.match(/^  VESPER_VERSION: "([^"]+)"$/m)?.[1];
  const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
  const workspace = cargo.split("[workspace.package]")[1].match(/^version = "([^"]+)"/m)[1];
  const tauri = JSON.parse(fs.readFileSync(path.join(root, "launcher/src-tauri/tauri.conf.json"), "utf8"));
  const pkg = JSON.parse(fs.readFileSync(path.join(root, "launcher/package.json"), "utf8"));
  assert.equal(version, workspace);
  assert.equal(tauri.version, workspace);
  assert.equal(pkg.version, workspace);
});

test("macOS and Linux client jobs build and check native bundles", () => {
  const mac = section("macos-client", "linux-client");
  const linux = section("linux-client", "publish");
  assert.match(mac, /runs-on: macos-latest/);
  assert.match(mac, /--target universal-apple-darwin --bundles app,dmg/);
  assert.match(mac, /lipo -archs/);
  assert.match(mac, /arm64/);
  assert.match(mac, /x86_64/);
  assert.match(linux, /runs-on: ubuntu-22\.04/);
  assert.match(linux, /libwebkit2gtk-4\.1-dev/);
  assert.match(linux, /--bundles appimage,deb/);
  assert.match(linux, /dpkg-deb --contents/);
  for (const body of [mac, linux]) assert.match(body, /-p vesper-client -p vesper-format -p vesper-install/);
  // Every step runs with pipefail: a "cmd | grep -q" pipe fails when grep exits early and cmd gets SIGPIPE.
  assert.doesNotMatch(workflow, /\|\s*grep\s+(-\w*q|-\w+\s+-q)/);
});

test("publish is the only write-permission job and never runs for pull requests", () => {
  const publish = section("publish");
  assert.match(publish, /if: startsWith\(github\.ref, 'refs\/tags\/vesper-v'\)/);
  assert.match(publish, /contents: write/);
  assert.doesNotMatch(workflow.slice(0, workflow.indexOf("\n  publish:")), /contents: write/);
  assert.match(publish, /actions\/download-artifact@[0-9a-f]{40}/);
  assert.match(publish, /gh release create/);
});
