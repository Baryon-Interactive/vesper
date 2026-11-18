export type DownloadProgress = { downloaded_bytes: number; reused_bytes: number; remaining_bytes: number };
export type TaskSnapshot = { id: string; status: string; downloaded_bytes: number; reused_bytes: number; remaining_bytes: number };
export type InstalledActionState = { launch: boolean; update: boolean; channel: boolean; rollback: boolean; verify: boolean; uninstall: boolean };

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = -1;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[unit]}`;
}

export function formatEta(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return "—";
  if (seconds < 60) return `${Math.max(0, Math.round(seconds))}s`;
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(minutes / 60);
  return hours ? `${hours}h ${minutes % 60}m` : `${minutes}m ${Math.round(seconds % 60)}s`;
}

export function progressPercent(progress: DownloadProgress): number {
  const total = progress.downloaded_bytes + progress.reused_bytes + progress.remaining_bytes;
  return total === 0 ? 0 : Math.min(100, ((progress.downloaded_bytes + progress.reused_bytes) / total) * 100);
}

export function mediaIsSafe(media: unknown): media is { url: string; blake3: string } {
  if (!media || typeof media !== "object") return false;
  const value = media as Record<string, unknown>;
  return typeof value.url === "string" && /^https?:\/\//.test(value.url) && typeof value.blake3 === "string" && /^[a-f0-9]{64}$/i.test(value.blake3);
}

export function mergeTaskSnapshot(tasks: TaskSnapshot[], next: TaskSnapshot): TaskSnapshot[] {
  const index = tasks.findIndex(task => task.id === next.id);
  if (index < 0) return [...tasks, next];
  return tasks.map((task, position) => position === index ? next : task);
}

export function installedActionState(game: { has_backup: boolean; status: string }): InstalledActionState {
  const ready = game.status === "installed";
  return { launch: ready, update: ready, channel: ready, rollback: ready && game.has_backup, verify: ready, uninstall: ready };
}

export function displayError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return text.replace(/^Error:\s*/i, "").trim() || "The launcher operation failed";
}

export function cardMedia(store: Record<string, unknown>): { url: string; blake3: string } | undefined {
  const candidate = store.cover ?? (Array.isArray(store.screenshots) ? store.screenshots[0] : undefined);
  return mediaIsSafe(candidate) ? candidate : undefined;
}

export type UpdateStatus = { available: boolean; target_version: string };

export function updateMessage(check: UpdateStatus): string {
  return check.available ? `Update available: v${check.target_version}` : "Up to date";
}

const DESKTOP_PLATFORMS = ["windows-x64", "linux-x64", "macos-arm64", "macos-x64"];

/** False for browser-only games: the signed listing names platforms and none of them is a desktop platform. */
export function desktopInstallable(store: Record<string, unknown>): boolean {
  const platforms = store.platforms;
  if (!Array.isArray(platforms) || platforms.length === 0) return true;
  return platforms.some((platform) => DESKTOP_PLATFORMS.includes(String(platform)));
}

const LISTING_LABELS: Record<string, string> = {
  "coming-soon": "Coming soon", "early-access": "Early access", released: "Released",
  "single-player": "Single-player", "online-co-op": "Online co-op", "local-co-op": "Local co-op", "online-pvp": "Online PvP", "local-pvp": "Local PvP",
  controller: "Controller support", "keyboard-mouse": "Keyboard and mouse", touch: "Touch controls", "cloud-saves": "Cloud saves", achievements: "Achievements",
  "mod-support": "Mod support", "level-editor": "Level editor",
};
const strings = (value: unknown): string[] => (Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : []);

/** Release line, fact rows and changelog for the detail view, from the signed listing and documents. */
export function listingDetail(store: Record<string, unknown>, documents: Record<string, unknown> | undefined): { release: string; facts: [string, string][]; changelog: string } {
  const release = store.release as { status?: string; date?: string } | undefined;
  const status = release?.status && release.status !== "released" ? LISTING_LABELS[release.status] ?? release.status : "";
  const facts: [string, string][] = [];
  if (release?.date) facts.push([release.status === "released" ? "Release date" : "Planned release", release.date]);
  const publishers = strings(store.publishers);
  if (publishers.length) facts.push(["Publisher", publishers.join(", ")]);
  const features = strings(store.features).map((f) => LISTING_LABELS[f] ?? f);
  if (typeof store.max_players === "number" && store.max_players > 1) features.unshift(`Up to ${store.max_players} players`);
  if (features.length) facts.push(["Features", features.join(", ")]);
  const languages = store.languages && typeof store.languages === "object" ? Object.keys(store.languages) : [];
  if (languages.length) {
    const names = new Intl.DisplayNames(["en"], { type: "language" });
    facts.push(["Languages", languages.map((tag) => { try { return names.of(tag) ?? tag; } catch { return tag; } }).join(", ")]);
  }
  const changelog = typeof documents?.["changelog.md"] === "string" ? (documents["changelog.md"] as string) : "";
  return { release: [status, status && release?.date ? release.date : ""].filter(Boolean).join(" · "), facts, changelog };
}
