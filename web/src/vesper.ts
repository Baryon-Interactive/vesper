// Page-side access to the Vesper workers. Verification runs in workers so the
// page never parses unverified catalogue data.
import type { AppDetail, AppSummary } from "./logic";

let worker: Worker | null = null;
let nextId = 1;
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();

function catalogueWorker(): Worker {
  if (!worker) {
    worker = new Worker("/vesper/catalogue-worker.js");
    // A worker that cannot load (blocked script, network error) fails every
    // waiting call instead of leaving it pending; the next call starts a new one.
    worker.onerror = (event: ErrorEvent) => {
      event.preventDefault();
      worker = null;
      const error = new Error(event.message || "The catalogue worker could not start");
      for (const entry of pending.values()) entry.reject(error);
      pending.clear();
    };
    worker.onmessage = (event: MessageEvent) => {
      const { id, ok, value, error } = event.data;
      const entry = pending.get(id);
      if (!entry) return;
      pending.delete(id);
      if (ok) entry.resolve(value);
      else entry.reject(new Error(error));
    };
  }
  return worker;
}

function call<T>(method: string, ...args: unknown[]): Promise<T> {
  const id = nextId++;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
    catalogueWorker().postMessage({ id, method, args });
  });
}

export const listApps = () => call<AppSummary[]>("listApps");
export const appDetail = (appId: string) => call<AppDetail>("appDetail", appId);

const mediaUrls = new Map<string, Promise<string>>();
/** Object URL for media whose bytes matched the signed BLAKE3 hash. */
export function verifiedMedia(url: string, blake3: string): Promise<string> {
  const key = `${url}#${blake3}`;
  if (!mediaUrls.has(key)) {
    const promise = call<ArrayBuffer>("media", url, blake3).then((buffer) => URL.createObjectURL(new Blob([buffer])));
    promise.catch(() => mediaUrls.delete(key));
    mediaUrls.set(key, promise);
  }
  return mediaUrls.get(key)!;
}

export type Progress = { downloaded: number; cached: number; requests: number };

/**
 * `playOrigin` is one origin for every game (`https://play.example`) or a
 * template that gives each game its own (`https://{app}.play.example`).
 */
export type SiteConfig = { apiBase: string; playOrigin: string; appDownload?: string };
let configPromise: Promise<SiteConfig> | null = null;

/** Store configuration; pages on localhost prefer the demo's dev configuration. */
export function siteConfig(): Promise<SiteConfig> {
  if (!configPromise) {
    configPromise = (async () => {
      const local = /^(localhost|127\.0\.0\.1|\[::1\]|.+\.localhost)$/.test(location.hostname);
      for (const name of local ? ["/vesper-config.dev.json", "/vesper-config.json"] : ["/vesper-config.json"]) {
        const response = await fetch(name, { cache: "no-store" });
        if (response.ok) {
          const value = await response.json();
          const template = String(value.playOrigin).replace(/\/$/, "");
          return { ...value, playOrigin: perGameOrigins(template) ? template : new URL(template).origin };
        }
      }
      throw new Error("vesper-config.json is missing");
    })();
    configPromise.catch(() => (configPromise = null));
  }
  return configPromise;
}

const LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
const HASHED = /^h-[0-9a-f]{24}$/;

/**
 * The DNS label of a game's play origin: the application id when it already
 * is a label, otherwise `h-` and 24 hex digits of its SHA-256. The play site
 * applies the same rule (play/origins.js).
 */
export async function playLabel(appId: string): Promise<string> {
  if (LABEL.test(appId) && !HASHED.test(appId) && appId.slice(2, 4) !== "--") return appId;
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(appId)));
  return "h-" + [...digest.slice(0, 12)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Whether a `playOrigin` template gives every game its own origin. */
export const perGameOrigins = (template: string) => /^https?:\/\/\{app\}\.[^/{}]+$/.test(template);

/** The origin a game plays on. */
export async function playOriginFor(template: string, appId: string): Promise<string> {
  if (!perGameOrigins(template)) return new URL(template).origin;
  return new URL(template.replace("{app}", await playLabel(appId))).origin;
}

/** Launch page query keys owned by Vesper; a game can never accept them. */
export const RESERVED_PLAY_PARAMS = ["app", "channel"];

/**
 * URL of a game's launch page on the separate play origin. `gameQuery` is the
 * play page's own query string (for example an invite's `?code=ROOM`); every
 * key but `app` and `channel` is passed on. The launch page forwards to the
 * game only the keys in `play_params` of the verified listing, so the store
 * does not need the listing (or its catalogue worker) before the game starts.
 */
export function playUrl(playOrigin: string, appId: string, channel = "stable", gameQuery = ""): string {
  const url = new URL("/launch.html", playOrigin);
  url.searchParams.set("app", appId);
  url.searchParams.set("channel", channel);
  for (const [key, value] of new URLSearchParams(gameQuery)) {
    if (!RESERVED_PLAY_PARAMS.includes(key)) url.searchParams.append(key, value);
  }
  return url.href;
}

export type PlayEvent =
  | { type: "progress"; progress: Progress }
  | { type: "ready"; version: string; channel: string }
  | { type: "started"; isolated: boolean }
  | { type: "error"; message: string };

/**
 * Parse a message from the play frame. Game code runs on the play origin, so these
 * messages are untrusted: they only drive the status line and are reduced to
 * plain numbers and strings here.
 */
export function playEvent(data: unknown): PlayEvent | null {
  if (!data || typeof data !== "object") return null;
  const d = data as Record<string, unknown>;
  const text = (v: unknown) => String(v ?? "").slice(0, 300);
  const num = (v: unknown) => (Number.isFinite(Number(v)) && Number(v) >= 0 ? Number(v) : 0);
  switch (d.type) {
    case "vesper-progress": {
      const s = (d.stats ?? {}) as Record<string, unknown>;
      return { type: "progress", progress: { downloaded: num(s.downloaded), cached: num(s.cached), requests: num(s.requests) } };
    }
    case "vesper-ready":
      return { type: "ready", version: text(d.version), channel: text(d.channel) };
    case "vesper-started":
      return { type: "started", isolated: d.isolated === true };
    case "vesper-error":
      return { type: "error", message: text(d.message) };
    default:
      return null;
  }
}
