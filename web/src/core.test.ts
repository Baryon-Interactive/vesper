import { expect, test } from "vitest";
import coreSource from "../public/vesper/core.js?raw";

// Run the classic worker script the way importScripts() would, with its own `self`.
function loadCore() {
  const self: { location: { hostname: string }; VesperCore?: Record<string, unknown> } = {
    location: { hostname: "games.example" },
  };
  new Function("self", coreSource)(self);
  return self.VesperCore as unknown as {
    sourceUrls: (apiBase: string, targets: unknown, appId: string, proxy?: string) => { id: string; template: string }[];
  };
}

const targets = {
  sources: [
    { id: "github", kind: "github", url_template: "https://github.com/example/builds/releases/download/{release}/{pack_id}", priority: 0 },
    { id: "r2", kind: "r2", url_template: "https://cdn.example/game/{release}/{pack_id}", priority: 1 },
    { id: "local", kind: "http", url_template: "packs/{release}/{pack_id}", priority: 2 },
    { id: "relay", kind: "http", url_template: "https://relay.example/{release}/{pack_id}", priority: 3 },
  ],
};

test("sources with CORS come first and GitHub goes through the proxy", () => {
  const { sourceUrls } = loadCore();
  expect(sourceUrls("https://catalogue.example/", targets, "a b", "https://gh.example/")).toEqual([
    { id: "r2", template: "https://cdn.example/game/{release}/{pack_id}" },
    { id: "local", template: "https://catalogue.example/packs/{release}/{pack_id}" },
    { id: "relay", template: "https://relay.example/{release}/{pack_id}" },
    { id: "github", template: "https://gh.example/a%20b/{release}/{pack_id}" },
  ]);
});

test("GitHub sources are skipped without a proxy or with an unexpected template", () => {
  const { sourceUrls } = loadCore();
  expect(sourceUrls("https://catalogue.example/", targets, "a").map((s) => s.id)).toEqual(["r2", "local", "relay"]);
  const odd = { sources: [{ id: "github", kind: "github", url_template: "https://evil.example/{release}/{pack_id}", priority: 0 }] };
  expect(sourceUrls("https://catalogue.example/", odd, "a", "https://gh.example")).toEqual([]);
});
