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
