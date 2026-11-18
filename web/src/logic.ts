// Pure helpers for the web store; tested in logic.test.ts.

export type Media = { url: string; blake3: string; width?: number; height?: number };
export type Trailer =
  | { kind: "youtube"; id: string; title?: Record<string, string> }
  | { kind: "video"; url: string; poster: Media; title?: Record<string, string> };
export type LanguageSupport = { interface?: boolean; audio?: boolean; subtitles?: boolean };
export type Spec = Partial<Record<"os" | "cpu" | "gpu" | "memory" | "storage" | "network" | "notes", string>>;
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
  default_language?: string;
  release?: { status: "coming-soon" | "early-access" | "released"; date?: string };
  updated?: string;
  features?: string[];
  max_players?: number;
  languages?: Record<string, LanguageSupport>;
  system_requirements?: Record<string, { minimum?: Spec; recommended?: Spec }>;
  capsule?: Media;
  logo?: Media;
  background?: Media;
  trailers?: Trailer[];
  links?: Record<string, string>;
  publishers?: string[];
  content_descriptors?: string[];
  accessibility?: string[];
};
export type Channel = { version: number; platform: string; encrypted: boolean; manifest: { release_tag: string; size: number } };
export type AppSummary = {
  app_id: string;
  store?: Store;
  targets_version?: number;
  channels?: Record<string, Channel>;
  error?: string;
};
export type AppDetail = AppSummary & {
  description: string;
  /** Signed Markdown documents other than description.md, such as changelog.md. */
  documents?: Record<string, string>;
  web_channel: string | null;
};

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

// Store vocabulary in English; phase two of the roadmap translates these.
const LABELS: Record<string, string> = {
  "coming-soon": "Coming soon",
  "early-access": "Early access",
  released: "Released",
  "single-player": "Single-player",
  "online-co-op": "Online co-op",
  "local-co-op": "Local co-op",
  "online-pvp": "Online PvP",
  "local-pvp": "Local PvP",
  controller: "Controller support",
  "keyboard-mouse": "Keyboard and mouse",
  touch: "Touch controls",
  "cloud-saves": "Cloud saves",
  achievements: "Achievements",
  "mod-support": "Mod support",
  "level-editor": "Level editor",
  violence: "Violence",
  blood: "Blood",
  gore: "Gore",
  "strong-language": "Strong language",
  "sexual-content": "Sexual content",
  nudity: "Nudity",
  drugs: "Drugs",
  alcohol: "Alcohol",
  gambling: "Gambling",
  horror: "Horror",
  subtitles: "Subtitles",
  "colorblind-modes": "Colorblind modes",
  "remappable-controls": "Remappable controls",
  "adjustable-text-size": "Adjustable text size",
  "screen-reader": "Screen reader support",
  "one-handed": "Playable with one hand",
  "difficulty-options": "Difficulty options",
  "reduced-motion": "Reduced motion",
  "no-flashing-lights": "No flashing lights",
  discord: "Discord",
  x: "X",
  bluesky: "Bluesky",
  youtube: "YouTube",
  twitch: "Twitch",
  reddit: "Reddit",
  steam: "Steam",
  itch: "itch.io",
  github: "GitHub",
  wiki: "Wiki",
  "press-kit": "Press kit",
  support: "Support",
  os: "OS",
  cpu: "Processor",
  gpu: "Graphics",
  memory: "Memory",
  storage: "Storage",
  network: "Network",
  notes: "Notes",
};

/** English label for a store slug (feature, descriptor, link, ...); unknown slugs pass through. */
export const label = (slug: string): string => LABELS[slug] ?? slug;

/** A YYYY-MM-DD date as a readable day, without time-zone shifts. */
export function formatDate(date: string, locale?: string): string {
  const [y, m, d] = date.split("-").map(Number);
  if (!y || !m || !d) return date;
  return new Date(Date.UTC(y, m - 1, d)).toLocaleDateString(locale, { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });
}

/** The release line under the title: "Early access", "Coming soon · Mar 1, 2027", or nothing when simply released. */
export function releaseLine(release: Store["release"], locale?: string): string {
  if (!release) return "";
  const date = release.date ? formatDate(release.date, locale) : "";
  if (release.status === "released") return date ? `Released ${date}` : "";
  return [label(release.status), date].filter(Boolean).join(" · ");
}

/** Display name of a language tag in the reader's language, e.g. "zh-Hans" -> "Chinese (Simplified)". */
export function languageName(tag: string, locale = "en"): string {
  try {
    return new Intl.DisplayNames([locale], { type: "language" }).of(tag) ?? tag;
  } catch {
    return tag;
  }
}

/** Trailers first, then screenshots: the order of the media gallery. */
export type GalleryItem = { kind: "trailer"; trailer: Trailer } | { kind: "image"; media: Media };
export function gallery(store: Store): GalleryItem[] {
  return [
    ...(store.trailers ?? []).map((trailer) => ({ kind: "trailer" as const, trailer })),
    ...(store.screenshots ?? []).map((media) => ({ kind: "image" as const, media })),
  ];
}

/** The privacy-enhanced YouTube player, loaded only after the player clicks. */
export const youtubeEmbed = (id: string) => `https://www.youtube-nocookie.com/embed/${encodeURIComponent(id)}?autoplay=1&rel=0`;
export const youtubeThumbnail = (id: string) => `https://i.ytimg.com/vi/${encodeURIComponent(id)}/hqdefault.jpg`;

/** Changelog for the reader's language, falling back to changelog.md. */
export function changelog(documents: Record<string, string> | undefined, languages: readonly string[]): string {
  if (!documents) return "";
  for (const lang of languages) {
    const exact = documents[`changelog.${lang}.md`];
    if (exact) return exact;
  }
  return documents["changelog.md"] ?? "";
}
