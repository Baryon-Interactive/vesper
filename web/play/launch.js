// Vesper play site launch page: /launch.html?app=<app_id>&channel=<channel>[&<game params>]
//
// Query keys other than `app` and `channel` belong to the game: they are passed
// on, URL-encoded, in the query of the game frame (for example an invite's
// `code=ROOM`). Fragments are not forwarded.
//
// This page lives on the play origin, separate from the store, because games
// are third-party code: anything under /run/ shares this origin and must never
// share one with the store. The page registers the service worker, asks it to
// verify and open the web build, then runs the game in a nested frame. It
// reports progress to the embedding store with postMessage, and only to a
// store origin listed in this site's configuration.
"use strict";
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
  const gameParams = new URLSearchParams();
  for (const [key, value] of params) if (key !== "app" && key !== "channel") gameParams.append(key, value);
  const gameQuery = gameParams.toString();
  const local = /^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname);

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
    const registration = await navigator.serviceWorker.ready;
    navigator.serviceWorker.addEventListener("message", (event) => {
      const data = event.data || {};
      if (data.type === "vesper-progress") report({ type: "vesper-progress", stats: data.stats });
      if (data.type === "vesper-error" && !data.id) {
        report({ type: "vesper-error", message: data.message });
        if (!frame) show(data.message, true);
      }
    });
    const id = Math.random().toString(36).slice(2);
    const prepared = await new Promise((resolve, reject) => {
      const listener = (event) => {
        if (event.data?.id !== id) return;
        navigator.serviceWorker.removeEventListener("message", listener);
        if (event.data.type === "vesper-ready") resolve(event.data);
        else reject(new Error(event.data.message));
      };
      navigator.serviceWorker.addEventListener("message", listener);
      registration.active.postMessage({ type: "vesper-prepare", appId, channel, id });
    });
    report({ type: "vesper-ready", version: prepared.version, channel: prepared.channel });
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
