import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const workflow = fs.readFileSync(path.join(root, ".github/workflows/release.yml"), "utf8");
const notes = fs.readFileSync(path.join(root, "scripts/release-notes.md"), "utf8");

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
    "publish",
  ]) assert.match(workflow, new RegExp(`^  ${name}:`, "m"));
  for (const line of workflow.split("\n")) {
    if (/uses:/.test(line) && !/uses: \.\//.test(line))
      assert.match(line, /uses: [\w/-]+@[0-9a-f]{40} # v[\d.]+$/);
  }
  assert.match(workflow, /permissions:\s*\n\s*contents: read/);
  assert.match(workflow, /publish:[\s\S]*?permissions:\s*\n\s*contents: write/);
});

test("workflow builds and checks every required asset", () => {
  for (const asset of [
    "Vesper-Setup-0.7.0-windows-x64.exe",
    "vesper-cli-linux-x64",
    "vesper-cli-windows-x64.exe",
    "vesper-cli-macos-arm64",
    "vesper-cli-macos-x64",
    "LICENSE",
    "NOTICE",
    "SHA256SUMS",
  ]) assert.match(workflow + notes, new RegExp(asset.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  assert.match(workflow, /find \. -maxdepth 1 -type f ! -name SHA256SUMS/);
  assert.match(workflow, /test "\$\(wc -l < dist\/SHA256SUMS\)" -eq 7/);
  // SHA256SUMS lines are "<64 hex>  <name>"; each asset check must match that whole line, not the bare name.
  assert.doesNotMatch(workflow, /grep -Fxq/);
  for (const name of [String.raw`Vesper-Setup-0\.7\.0-windows-x64\.exe`, "vesper-cli-linux-x64", String.raw`vesper-cli-windows-x64\.exe`, "vesper-cli-macos-arm64", "vesper-cli-macos-x64"]) {
    assert.ok(workflow.includes(`grep -Eq '^[0-9a-f]{64}  ${name}$' dist/SHA256SUMS`), name);
  }
  assert.match(notes, /Players: download Vesper-Setup-@VERSION@-windows-x64\.exe/);
  assert.match(notes, /Developers: vesper CLI/);
  assert.match(notes, /xattr -d com\.apple\.quarantine <file>/);
  assert.match(workflow, /--target "\$GITHUB_SHA"/);
});

test("tag validation and CLI version checks use workspace 0.7.0", () => {
  assert.match(workflow, /tag_version="\$\{GITHUB_REF_NAME#vesper-v\}"/);
  assert.match(workflow, /text = Path\("Cargo\.toml"\)/);
  assert.match(workflow, /test "\$tag_version" = "\$workspace_version"/);
  assert.match(workflow, /test "\$workspace_version" = "0\.7\.0"/);
  assert.equal((workflow.match(/vesper 0\.7\.0 \(\$VESPER_GIT_SHA\)/g) ?? []).length, 3);
  assert.match(workflow, /Unexpected CLI version/);
});

test("publish is the only write-permission job and never runs for pull requests", () => {
  const publish = workflow.slice(workflow.indexOf("\n  publish:"));
  assert.match(publish, /if: startsWith\(github\.ref, 'refs\/tags\/vesper-v'\)/);
  assert.match(publish, /contents: write/);
  assert.doesNotMatch(workflow.slice(0, workflow.indexOf("\n  publish:")), /contents: write/);
  assert.match(publish, /actions\/download-artifact@[0-9a-f]{40}/);
  assert.match(publish, /gh release create/);
});
