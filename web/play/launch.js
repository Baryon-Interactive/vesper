// Vesper play site launch page: /launch.html?app=<app_id>&channel=<channel>[&<game params>]
//
// Other query keys are passed on, URL-encoded, in the query of the game frame
// (for example an invite's `code=ROOM`), but only the keys the game lists in
// `play_params` of its signed store listing, and only values of at most 256
// characters. Nothing else reaches the game; fragments are never forwarded.
//
// This page lives on the play origin, separate from the store, because games
// are third-party code: anything under /run/ shares this origin and must never
// share one with the store. The page registers the service worker, asks it to
// verify and open the web build, then runs the game in a nested frame. It
// reports progress to the embedding store with postMessage, and only to a
// store origin listed in this site's configuration.
//
// With per-game play origins (play/origins.js) the page runs only the game
// whose origin it is on, and on a game's first start it brings that game's
// Unity saves over from the old shared origin (bringSaves below).
//
// The page never waits forever on the worker. The worker that gets the
// prepare request can be replaced before it answers (a new sw.js activates and
// claims the page), so the request is sent again to each newly activated
// worker. An attempt without an answer for PREPARE_TIMEOUT_MS is sent once
// more, and the page then shows an error that asks for a reload.
"use strict";
const PREPARE_TIMEOUT_MS = 45000;
const READY_TIMEOUT_MS = 30000;
const SAVES_TIMEOUT_MS = 8000;
const RELOAD_HINT = "Reload the page to try again.";

/**
 * Copy the game's Unity saves from the old shared play origin, once. The
 * bridge page there posts them only to this game's own origin. A failed or
 * silent bridge is asked again on the next start; saves already made on this
 * origin are never replaced (play/saves.js).
 */
async function bringSaves(config, appId, entry) {
  if (!config.legacyOrigin || !VesperOrigins.perGame(config.playOrigin)) return;
  const legacy = new URL(config.legacyOrigin).origin;
  const done = `vesper-saves-moved:${appId}`;
  if (legacy === location.origin || localStorage.getItem(done)) return;
  const records = await new Promise((resolve) => {
    const bridge = document.createElement("iframe");
    let timer = 0;
    const finish = (value) => {
      clearTimeout(timer);
      removeEventListener("message", onMessage);
      bridge.remove();
      resolve(value);
    };
    const onMessage = (event) => {
      const data = event.data || {};
      if (event.origin !== legacy || event.source !== bridge.contentWindow || data.type !== "vesper-saves" || data.appId !== appId) return;
      finish(Array.isArray(data.records) ? data.records : null);
    };
    addEventListener("message", onMessage);
    timer = setTimeout(() => finish(null), SAVES_TIMEOUT_MS);
    bridge.hidden = true;
    bridge.src = `${legacy}/migrate.html?${new URLSearchParams({ app: appId, entry })}`;
    document.body.appendChild(bridge);
  });
  if (!records) return;
  try {
    const moved = await VesperSaves.write(VesperSaves.unityFolder(location.origin, appId, entry), records);
    if (moved) console.info(`vesper: moved ${moved} saved files for ${appId} to this origin`);
    localStorage.setItem(done, "1");
  } catch (error) {
    console.warn("vesper: could not move the old saves", error);
  }
}

(async () => {
  const status = document.getElementById("status");
  const show = (text, error = false) => {
    status.textContent = text;
    status.className = error ? "error" : "";
    status.hidden = false;
  };
  let frame = null;
  const params = new URLSearchParams(location.search);
  const appId = params.get("app") || "";
  const channel = params.get("channel") || "stable";
  const local = /^(localhost|127\.0\.0\.1|\[::1\]|.+\.localhost)$/.test(location.hostname);

  let config = {};
  for (const name of local ? ["/vesper-config.dev.json", "/vesper-config.json"] : ["/vesper-config.json"]) {
    const response = await fetch(name, { cache: "no-store" });
    if (response.ok) {
      config = await response.json();
      break;
    }
  }
  const embedder =
    (location.ancestorOrigins && location.ancestorOrigins.length && location.ancestorOrigins[0]) ||
    (document.referrer ? new URL(document.referrer).origin : "");
  const store = window.parent !== window && (config.storeOrigins || []).includes(embedder) ? embedder : null;
  const report = (message) => store && window.parent.postMessage({ ...message, appId }, store);

  if (!/^[A-Za-z0-9._-]+$/.test(appId) || appId === "." || appId === "..") {
    show("No game selected.", true);
    return report({ type: "vesper-error", message: "invalid application id" });
  }
  // With per-game play origins, this origin runs only its own game.
  if (config.playOrigin && !(await VesperOrigins.mayRun(config.playOrigin, appId, location.origin))) {
    show("This game does not play on this address.", true);
    return report({ type: "vesper-error", message: "this play origin belongs to another game" });
  }
  if (!("serviceWorker" in navigator)) {
    show("This browser cannot stream games: service workers are unavailable.", true);
    return report({ type: "vesper-error", message: "service workers are unavailable in this browser" });
  }
  try {
    const registered = await navigator.serviceWorker.register("/sw.js", { scope: "/" });
    // Take a new service worker before starting: an old one may serve game files
    // without the headers this page now requires, and the frame would be blocked.
    await registered.update().catch(() => {});
    const pending = registered.installing || registered.waiting;
    if (pending) {
      await new Promise((resolve) => {
        const done = () => ["activated", "redundant"].includes(pending.state) && resolve();
        pending.addEventListener("statechange", done);
        setTimeout(resolve, 10000); // never block the launch on a stuck update
        done();
      });
    }
    const workers = navigator.serviceWorker;
    // Messages from the worker are held until the page starts them.
    if (workers.startMessages) workers.startMessages();
    const registration = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`The game service worker did not start. ${RELOAD_HINT}`)), READY_TIMEOUT_MS);
      workers.ready.then((ready) => {
        clearTimeout(timer);
        resolve(ready);
      }, reject);
    });
    navigator.serviceWorker.addEventListener("message", (event) => {
      const data = event.data || {};
      if (data.type === "vesper-progress") report({ type: "vesper-progress", stats: data.stats });
      if (data.type === "vesper-error" && !data.id) {
        report({ type: "vesper-error", message: data.message });
        if (!frame) show(data.message, true);
      }
    });
    const prepared = await new Promise((resolve, reject) => {
      const ids = new Set(); // every request sent; the first answer wins
      let target = null;
      let timer = 0;
      let retried = false;
      const send = (again) => {
        const worker = registration.active || workers.controller;
        if (!worker || (worker === target && !again)) return;
        target = worker;
        const id = Math.random().toString(36).slice(2);
        ids.add(id);
        clearTimeout(timer);
        timer = setTimeout(() => {
          if (!retried) {
            retried = true;
            send(true);
          } else finish(new Error(`Checking signatures took too long. ${RELOAD_HINT}`));
        }, PREPARE_TIMEOUT_MS);
        worker.postMessage({ type: "vesper-prepare", appId, channel, id });
      };
      const onMessage = (event) => {
        const data = event.data || {};
        if (!ids.has(data.id)) return;
        if (data.type === "vesper-ready") finish(null, data);
        else finish(new Error(data.message));
      };
      const onChange = () => send(false);
      // A worker still installing (the wait above gives up after 10 s) or one
      // found while preparing gets the request once it is active.
      const follow = (worker) =>
        worker && worker.addEventListener("statechange", () => worker.state === "activated" && send(false));
      const onUpdate = () => follow(registration.installing);
      const finish = (error, data) => {
        clearTimeout(timer);
        workers.removeEventListener("message", onMessage);
        workers.removeEventListener("controllerchange", onChange);
        registration.removeEventListener("updatefound", onUpdate);
        if (error) reject(error);
        else resolve(data);
      };
      workers.addEventListener("message", onMessage);
      workers.addEventListener("controllerchange", onChange);
      registration.addEventListener("updatefound", onUpdate);
      follow(registration.installing || registration.waiting);
      send(true);
    });
    report({ type: "vesper-ready", version: prepared.version, channel: prepared.channel });
    const accepted = Array.isArray(prepared.playParams) ? prepared.playParams : [];
    const gameParams = new URLSearchParams();
    for (const [key, value] of params) {
      if (key !== "app" && key !== "channel" && accepted.includes(key) && value.length <= 256) gameParams.append(key, value);
    }
    const gameQuery = gameParams.toString();
    await bringSaves(config, appId, prepared.entry);
    show("Streaming the first files…");
    frame = document.createElement("iframe");
    frame.id = "game";
    frame.title = appId;
    frame.allow = "autoplay; fullscreen; gamepad; microphone; cross-origin-isolated";
    frame.src =
      `/run/${encodeURIComponent(appId)}/${prepared.entry.split("/").map(encodeURIComponent).join("/")}` +
      (gameQuery ? `?${gameQuery}` : "");
    frame.addEventListener("load", () => {
      status.hidden = true;
      frame.focus();
      // Whether the game got SharedArrayBuffer and wasm threads.
      let isolated = false;
      try {
        isolated = frame.contentWindow.crossOriginIsolated === true;
      } catch {
        // An error page in the frame (for example a blocked document) is cross-origin.
      }
      console.info(`vesper: ${appId} ${isolated ? "is" : "is not"} cross-origin isolated`);
      report({ type: "vesper-started", isolated });
    });
    document.body.appendChild(frame);
  } catch (error) {
    const message = String(error?.message || error);
    show(message, true);
    report({ type: "vesper-error", message });
  }
})();
