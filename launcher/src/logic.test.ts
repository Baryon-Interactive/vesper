import { describe, expect, it, test } from "vitest";
import { cardMedia, desktopInstallable, displayError, formatBytes, formatEta, installedActionState, listingDetail, mediaIsSafe, mergeTaskSnapshot, progressPercent, updateMessage } from "./logic";

describe("launcher display logic", () => {
  it("formats binary download sizes and eta", () => {
    expect(formatBytes(1024 * 1024)).toBe("1.00 MiB");
    expect(formatEta(125)).toBe("2m 5s");
  });
  it("calculates downloaded plus local reuse progress", () => {
    expect(progressPercent({ downloaded_bytes: 20, reused_bytes: 30, remaining_bytes: 50 })).toBe(50);
  });
  it("only accepts hashed http media", () => {
    expect(mediaIsSafe({ url: "https://cdn.example/game.png", blake3: "a".repeat(64) })).toBe(true);
    expect(mediaIsSafe({ url: "javascript:alert(1)", blake3: "a".repeat(64) })).toBe(false);
  });
  it("merges task updates by id", () => {
    const tasks = mergeTaskSnapshot([{ id: "a", status: "paused", downloaded_bytes: 1, reused_bytes: 2, remaining_bytes: 3 }], { id: "a", status: "installing", downloaded_bytes: 4, reused_bytes: 2, remaining_bytes: 0 });
    expect(tasks).toEqual([{ id: "a", status: "installing", downloaded_bytes: 4, reused_bytes: 2, remaining_bytes: 0 }]);
  });
  it("disables rollback without a backup and preserves visible errors", () => {
    expect(installedActionState({ has_backup: false, status: "installed" }).rollback).toBe(false);
    expect(displayError(new Error("disk is full"))).toBe("disk is full");
  });
});

it("selects cover before the first screenshot and rejects unsafe media", () => {
  const cover = { url: "https://example.com/cover.png", blake3: "a".repeat(64) };
  const screenshot = { url: "https://example.com/shot.png", blake3: "b".repeat(64) };
  expect(cardMedia({ cover, screenshots: [screenshot], media: [screenshot] })).toBe(cover);
  expect(cardMedia({ screenshots: [screenshot, cover] })).toBe(screenshot);
  expect(cardMedia({ cover: null, screenshots: [screenshot] })).toBe(screenshot);
  expect(cardMedia({ cover: { url: "javascript:bad" }, screenshots: [screenshot] })).toBeUndefined();
  expect(cardMedia({ media: [cover] })).toBeUndefined();
});

it("shows the game version rather than the metadata version", () => {
  const check = { available: true, target_version: "5", metadata_version: 2, message: "Update available: v2" };
  expect(updateMessage(check)).toBe("Update available: v5");
  expect(updateMessage({ ...check, available: false })).toBe("Up to date");
});

describe("desktopInstallable", () => {
  it("hides install only for listings that name no desktop platform", () => {
    expect(desktopInstallable({ platforms: ["web-wasm32"] })).toBe(false);
    expect(desktopInstallable({ platforms: ["windows-x64", "web-wasm32"] })).toBe(true);
    expect(desktopInstallable({})).toBe(true);
    expect(desktopInstallable({ platforms: [] })).toBe(true);
  });
});

test("listing detail reads the richer store fields and the signed changelog", () => {
  const store = { release: { status: "early-access", date: "2026-11-01" }, publishers: ["Baryon Interactive"], features: ["single-player", "online-co-op"], max_players: 4, languages: { en: {}, ja: {} } };
  const detail = listingDetail(store, { "changelog.md": "## 1.0\n" });
  expect(detail.release).toBe("Early access · 2026-11-01");
  expect(detail.facts).toEqual([
    ["Early access since", "2026-11-01"],
    ["Publisher", "Baryon Interactive"],
    ["Features", "Up to 4 players, Single-player, Online co-op"],
    ["Languages", "English, Japanese"],
  ]);
  expect(detail.changelog).toBe("## 1.0\n");
  expect(listingDetail({}, undefined)).toEqual({ release: "", facts: [], changelog: "" });
});
