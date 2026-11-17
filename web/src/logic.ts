// Pure helpers for the web store; tested in logic.test.ts.

export type Media = { url: string; blake3: string; width?: number; height?: number };
export type Store = {
  title?: Record<string, string>;
  summary?: Record<string, string>;
  tags?: string[];
  screenshots?: Media[];
  cover?: Media;
  requirements?: Record<string, string>;
  paid?: boolean;
  price_display?: string;
  developer?: string;
  homepage?: string;
  platforms?: string[];
};
export type Channel = { version: number; platform: string; encrypted: boolean; manifest: { release_tag: string; size: number } };
export type AppSummary = {
  app_id: string;
  store?: Store;
  targets_version?: number;
  channels?: Record<string, Channel>;
  error?: string;
};
export type AppDetail = AppSummary & { description: string; web_channel: string | null };

export const WEB_PLATFORM = "web-wasm32";

/** Pick the best localized string: exact language, base language, English, then any. */
export function localized(values: Record<string, string> | undefined, languages: readonly string[], fallback = ""): string {
  if (!values) return fallback;
  for (const lang of languages) {
    if (values[lang]) return values[lang];
    const base = lang.split("-")[0];
    const match = Object.keys(values).find((key) => key.split("-")[0] === base);
    if (match) return values[match];
  }
  return values.en ?? Object.values(values)[0] ?? fallback;
}

/** Whether any published channel is an unencrypted web build. */
export function playable(app: AppSummary): boolean {
  return Object.values(app.channels ?? {}).some((c) => c.platform === WEB_PLATFORM && !c.encrypted);
}

/** Desktop platforms with a published channel, in a stable order. */
export function desktopPlatforms(app: AppSummary): string[] {
  const order = ["windows-x64", "macos-arm64", "macos-x64", "linux-x64"];
  const published = new Set(Object.values(app.channels ?? {}).map((c) => c.platform));
  return order.filter((p) => published.has(p));
}

export function platformLabel(platform: string): string {
  return (
    {
      "windows-x64": "Windows",
      "macos-arm64": "macOS (Apple silicon)",
      "macos-x64": "macOS (Intel)",
      "linux-x64": "Linux",
      [WEB_PLATFORM]: "Browser",
    } as Record<string, string>
  )[platform] ?? platform;
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

export type Route = { page: "store" } | { page: "app"; appId: string } | { page: "play"; appId: string };

export function parseRoute(pathname: string): Route {
  const parts = pathname.split("/").filter(Boolean).map(decodeURIComponent);
  if (parts[0] === "app" && parts[1]) return { page: "app", appId: parts[1] };
  if (parts[0] === "play" && parts[1]) return { page: "play", appId: parts[1] };
  return { page: "store" };
}

export function routePath(route: Route): string {
  return route.page === "store" ? "/" : `/${route.page}/${encodeURIComponent(route.appId)}`;
}

/** Filter apps by a free-text query over title, summary, developer and tags. */
export function matches(app: AppSummary, query: string, languages: readonly string[]): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  const s = app.store ?? {};
  const haystack = [localized(s.title, languages, app.app_id), localized(s.summary, languages), s.developer ?? "", ...(s.tags ?? []), app.app_id]
    .join(" ")
    .toLowerCase();
  return q.split(/\s+/).every((word) => haystack.includes(word));
}
