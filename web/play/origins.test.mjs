import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { expect, test } from "vitest";
import { perGameOrigins, playLabel, playOriginFor } from "../src/vesper";

const load = (name) => {
  const scope = { crypto: globalThis.crypto, TextEncoder, URL, encodeURIComponent };
  scope.self = scope;
  runInNewContext(readFileSync(new URL(`./${name}`, import.meta.url), "utf8"), scope);
  return scope;
};
const { VesperOrigins } = load("origins.js");
const { VesperSaves } = load("saves.js");
const sha = (id) => `h-${createHash("sha256").update(id).digest("hex").slice(0, 24)}`;

test("app ids that are DNS labels keep their name; others are hashed", async () => {
  const kept = ["no-doors-allowed", "a", "x".repeat(63), "h-0123"];
  const hashed = ["x".repeat(64), "No-Doors", "ember.drift", "ember_drift", "-edge", "edge-", "xn--abc", "h-0123456789abcdef01234567"];
  for (const [id, label] of [...kept.map((id) => [id, id]), ...hashed.map((id) => [id, sha(id)])]) {
    expect(await VesperOrigins.label(id)).toBe(label);
    expect(await playLabel(id)).toBe(label); // the store applies the same rule
  }
});

test("play origins come from the template on the store and the play site", async () => {
  for (const [template, id, origin] of [
    ["https://{app}.games.example", "no-doors-allowed", "https://no-doors-allowed.games.example"],
    ["https://{app}.games.example/", "Ember_Drift", `https://${sha("Ember_Drift")}.games.example`],
    ["http://{app}.localhost:3240", "ember-drift", "http://ember-drift.localhost:3240"],
    ["https://play.example", "anything", "https://play.example"],
  ]) {
    expect(await VesperOrigins.originFor(template, id)).toBe(origin);
    expect(await playOriginFor(template.replace(/\/$/, ""), id)).toBe(origin);
  }
  expect(perGameOrigins("https://games.example/{app}")).toBe(false);
  expect(VesperOrigins.perGame("https://x{app}.games.example")).toBe(false);
  expect(await VesperOrigins.mayRun("https://play.example", "any", "https://play.example")).toBe(true);
  expect(await VesperOrigins.mayRun("https://{app}.play.example", "a", "https://b.play.example")).toBe(false);
});

test("md5 matches node for every padding length and for non-ASCII text", () => {
  for (const text of ["", "a", "é漢字", ..."x".repeat(130).split("").map((_, i) => "x".repeat(i))]) {
    expect(VesperSaves.md5(text)).toBe(createHash("md5").update(text).digest("hex"));
  }
});

test("Unity's save folder is the md5 of the game's directory URL", () => {
  // The folder No Doors Allowed used on the shared play origin.
  expect(VesperSaves.unityFolder("https://games.vesperplay.xyz", "no-doors-allowed", "index.html")).toBe(
    "/idbfs/fe53b856829178db12f7f41aad40f850",
  );
  const md5 = (s) => createHash("md5").update(s).digest("hex");
  expect(VesperSaves.unityFolder("https://a.example", "a b", "Build/web/index.html")).toBe(`/idbfs/${md5("https://a.example/run/a%20b/Build/web")}`);
});
