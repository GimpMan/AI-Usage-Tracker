/** Initial IPC state is safe to apply only if no event arrived meanwhile. */
export function shouldApplyInitialUpdateState(
  eventVersionAtRequest: number,
  currentEventVersion: number,
): boolean {
  return eventVersionAtRequest === currentEventVersion;
}

/** Normalize updater byte progress for both text and the progress element. */
export function clampUpdateProgressPercent(downloaded: number, total: number): number {
  if (!Number.isFinite(downloaded) || !Number.isFinite(total) || total <= 0) return 0;
  return Math.max(0, Math.min(100, (downloaded / total) * 100));
}
