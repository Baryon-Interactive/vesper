// Save bridge on the old shared play origin: /migrate.html?app=<app_id>&entry=<path>
//
// Before every game had its own play origin, all games ran on one origin and
// Unity kept their saves there. A game's launch page on its new origin loads
// this page in a hidden frame the first time it starts. The page reads that
// game's Unity save folder and posts it to the parent, but only to the game's
// own origin: postMessage drops the message for any other parent, so a game
// can never receive another game's saves. Nothing is deleted here.
/* global VesperOrigins, VesperSaves */
"use strict";
(async () => {
  const params = new URLSearchParams(location.search);
  const appId = params.get("app") || "";
  const entry = params.get("entry") || "index.html";
  const local = /^(localhost|127\.0\.0\.1|\[::1\]|.+\.localhost)$/.test(location.hostname);
  let config = {};
  for (const name of local ? ["/vesper-config.dev.json", "/vesper-config.json"] : ["/vesper-config.json"]) {
    const response = await fetch(name, { cache: "no-store" });
    if (response.ok) {
      config = await response.json();
      break;
    }
  }
  if (window.parent === window || !config.legacyOrigin || new URL(config.legacyOrigin).origin !== location.origin) return;
  if (!VesperOrigins.perGame(config.playOrigin)) return;
  if (!/^[A-Za-z0-9._-]+$/.test(appId) || appId === "." || appId === "..") return;
  if (entry.split("/").some((part) => part === "" || part === "." || part === "..")) return;
  // Games no longer run here; stop the old service worker from serving them.
  navigator.serviceWorker?.getRegistrations().then((list) => list.forEach((r) => r.unregister()), () => {});
  const target = await VesperOrigins.originFor(config.playOrigin, appId);
  let records = null; // null: could not read; the launch page asks again next time
  try {
    records = await VesperSaves.read(VesperSaves.unityFolder(location.origin, appId, entry));
  } catch (error) {
    console.warn("vesper: could not read the old saves", error);
  }
  window.parent.postMessage({ type: "vesper-saves", appId, records }, target);
})();
