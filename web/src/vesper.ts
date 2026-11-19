// Page-side access to the Vesper workers. Verification runs in workers so the
// page never parses unverified catalogue data.
import type { AppDetail, AppSummary } from "./logic";

let worker: Worker | null = null;
let nextId = 1;
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();

function catalogueWorker(): Worker {
  if (!worker) {
    worker = new Worker("/vesper/catalogue-worker.js");
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

export type SiteConfig = { apiBase: string; playOrigin: string; appDownload?: string };
let configPromise: Promise<SiteConfig> | null = null;

/** Store configuration; pages on localhost prefer the demo's dev configuration. */
export function siteConfig(): Promise<SiteConfig> {
  if (!configPromise) {
    configPromise = (async () => {
      const local = /^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname);
      for (const name of local ? ["/vesper-config.dev.json", "/vesper-config.json"] : ["/vesper-config.json"]) {
        const response = await fetch(name, { cache: "no-store" });
        if (response.ok) {
          const value = await response.json();
          return { ...value, playOrigin: new URL(value.playOrigin).origin };
        }
      }
      throw new Error("vesper-config.json is missing");
    })();
    configPromise.catch(() => (configPromise = null));
  }
  return configPromise;
}

/** Launch page query keys owned by Vesper; every other key is passed to the game. */
export const RESERVED_PLAY_PARAMS = ["app", "channel"];

/**
 * URL of a game's launch page on the separate play origin. `gameQuery` is the
 * play page's own query string (for example an invite's `?code=ROOM`): its keys
 * other than `app` and `channel` are forwarded, and the launch page hands them
 * to the game frame.
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
 * Parse a message from the play frame. Games share the play origin, so these
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
