import { expect, test } from "vitest";
import {
  changelog,
  desktopPlatforms,
  formatBytes,
  gallery,
  label,
  localized,
  matches,
  parseRoute,
  playable,
  releaseDateLabel,
  releaseLine,
  routePath,
  youtubeEmbed,
  type AppSummary,
} from "./logic";

const channel = (platform: string, encrypted = false) => ({ version: 1, platform, encrypted, manifest: { release_tag: "r", size: 1 } });

test("localized picks the closest language", () => {
  const values = { en: "Hello", "zh-CN": "Ni hao" };
  expect(localized(values, ["zh-TW"])).toBe("Ni hao");
  expect(localized(values, ["fr"])).toBe("Hello");
  expect(localized(undefined, ["en"], "x")).toBe("x");
});

test("web builds are playable unless encrypted", () => {
  const app: AppSummary = { app_id: "a", channels: { stable: channel("windows-x64"), "stable-web-wasm32": channel("web-wasm32") } };
  expect(playable(app)).toBe(true);
  expect(desktopPlatforms(app)).toEqual(["windows-x64"]);
  expect(playable({ app_id: "b", channels: { stable: channel("web-wasm32", true) } })).toBe(false);
});

test("routes round-trip", () => {
  expect(parseRoute("/play/demo.web")).toEqual({ page: "play", appId: "demo.web" });
  expect(routePath({ page: "app", appId: "a b" })).toBe("/app/a%20b");
  expect(parseRoute("/nothing")).toEqual({ page: "store" });
});

test("search and byte formatting", () => {
  const app: AppSummary = { app_id: "x", store: { title: { en: "Night Heist" }, tags: ["co-op"] } };
  expect(matches(app, "heist co-op", ["en"])).toBe(true);
  expect(matches(app, "racing", ["en"])).toBe(false);
  expect(formatBytes(11 * 1024 * 1024)).toBe("11 MB");
  expect(formatBytes(1536)).toBe("1.5 KB");
});

test("play frame messages are reduced to plain values", async () => {
  const { playEvent, playUrl } = await import("./vesper");
  expect(playEvent({ type: "vesper-progress", stats: { downloaded: "12", cached: -5, requests: "x" } })).toEqual({
    type: "progress",
    progress: { downloaded: 12, cached: 0, requests: 0 },
  });
  expect(playEvent({ type: "vesper-error", message: { toString: () => "boom" } })).toEqual({ type: "error", message: "boom" });
  expect(playEvent({ type: "vesper-error", message: "x".repeat(1000) })).toMatchObject({ message: "x".repeat(300) });
  expect(playEvent({ type: "vesper-started", isolated: true })).toEqual({ type: "started", isolated: true });
  expect(playEvent({ type: "vesper-started", isolated: "yes" })).toEqual({ type: "started", isolated: false });
  expect(playEvent({ type: "other" })).toBeNull();
  expect(playEvent("vesper-started")).toBeNull();
  expect(playUrl("https://play.example", "a b")).toBe("https://play.example/launch.html?app=a+b&channel=stable");
  expect(playUrl("https://play.example", "game", "stable", "?code=AB12&app=evil&channel=beta&name=a%20b&code=2")).toBe(
    "https://play.example/launch.html?app=game&channel=stable&code=AB12&name=a+b&code=2",
  );
});

test("listing helpers: release line, gallery order, labels, changelog language", () => {
  expect(releaseLine({ status: "early-access" }, "en-US")).toBe("Early access");
  expect(releaseLine({ status: "coming-soon", date: "2027-03-01" }, "en-US")).toBe("Coming soon · Mar 1, 2027");
  expect(releaseLine({ status: "released" }, "en-US")).toBe("");
  expect([releaseDateLabel("coming-soon"), releaseDateLabel("early-access"), releaseDateLabel("released")]).toEqual(["Planned release", "Early access since", "Release date"]);
  const shot = { url: "https://x/1.png", blake3: "a".repeat(64) };
  const items = gallery({ screenshots: [shot], trailers: [{ kind: "youtube", id: "dQw4w9WgXcQ" }] });
  expect(items.map((i) => i.kind)).toEqual(["trailer", "image"]);
  expect(youtubeEmbed("dQw4w9WgXcQ")).toBe("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?autoplay=1&rel=0");
  expect(label("online-co-op")).toBe("Online co-op");
  expect(label("unknown-slug")).toBe("unknown-slug");
  const docs = { "changelog.md": "en", "changelog.ja.md": "ja" };
  expect(changelog(docs, ["ja", "en"])).toBe("ja");
  expect(changelog(docs, ["de"])).toBe("en");
  expect(changelog(undefined, ["en"])).toBe("");
});
