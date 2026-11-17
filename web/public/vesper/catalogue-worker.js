// Catalogue worker for the store pages: verified catalogue, app details and media.
// Requests are { id, method, args } and replies { id, ok, value | error }.
/* global VesperCore */
"use strict";
importScripts("/vesper/pkg/vesper_web.js", "/vesper/core.js");

const methods = {
  listApps: () => VesperCore.listApps(),
  appDetail: (appId) => VesperCore.appDetail(appId),
  media: async (url, blake3) => {
    const bytes = await VesperCore.media(url, blake3);
    return bytes.buffer;
  },
};

self.addEventListener("message", async (event) => {
  const { id, method, args } = event.data || {};
  try {
    if (!methods[method]) throw new Error(`unknown method ${method}`);
    const value = await methods[method](...(args || []));
    self.postMessage({ id, ok: true, value }, value instanceof ArrayBuffer ? [value] : []);
  } catch (error) {
    self.postMessage({ id, ok: false, error: String(error?.message || error) });
  }
});
