// Per-game play origins, shared by the launch page, the service worker and
// the save bridge. Classic script: load it with <script> or importScripts().
//
// The play site's `playOrigin` may be a template such as
// `https://{app}.games.example`. Every game then runs on its own origin, so a
// game cannot read another game's saves, caches or service worker state.
// `{app}` becomes the application id when the id is already a DNS label
// (lowercase letters, digits and inner dashes, at most 63 characters);
// any other id becomes `h-` and the first 24 hex digits of its SHA-256.
// web/src/vesper.ts applies the same rule on the store.
"use strict";

self.VesperOrigins = (() => {
  const LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
  const HASHED = /^h-[0-9a-f]{24}$/;

  /** The DNS label of an application's play origin. */
  async function label(appId) {
    // `xn--` style labels are reserved for IDNA, and `h-<hex>` for hashed ids.
    if (LABEL.test(appId) && !HASHED.test(appId) && appId.slice(2, 4) !== "--") return appId;
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(appId)));
    return "h-" + [...digest.slice(0, 12)].map((b) => b.toString(16).padStart(2, "0")).join("");
  }

  /** Whether `playOrigin` gives every game its own origin. */
  const perGame = (template) => typeof template === "string" && /^https?:\/\/\{app\}\.[^/{}]+$/.test(template.replace(/\/$/, ""));

  /** The origin an application plays on; a template without `{app}` is one shared origin. */
  async function originFor(template, appId) {
    if (!perGame(template)) return new URL(template).origin;
    return new URL(template.replace("{app}", await label(appId))).origin;
  }

  /**
   * Whether this origin may run `appId`. With a per-game template, only the
   * game's own origin may; otherwise (one shared play origin) every game may.
   */
  async function mayRun(template, appId, origin) {
    if (!perGame(template)) return true;
    return (await originFor(template, appId)) === origin;
  }

  return { label, perGame, originFor, mayRun };
})();
