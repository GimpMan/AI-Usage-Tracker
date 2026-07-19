import type { UsageSnapshot } from "./types";

/** Floor for the mini-bar stale hide (ms). Startup rehydrated state.json
 *  older than this is suppressed until a live fetch lands. */
export const MIN_STALE_THRESHOLD_MS = 120_000;

/** Grace after one scheduler period: multi-provider fetch duration + the
 *  frontend 5s poll lag, so cards never blink right as a refresh is due. */
export const STALE_GRACE_MS = 90_000;

/** Default refresh interval (seconds) before the config is loaded. */
export const DEFAULT_REFRESH_INTERVAL_SECS = 60;

/**
 * How old a snapshot may be before the mini bar hides it.
 * Must stay strictly above the user-configured refresh interval, otherwise
 * cards vanish every tick (e.g. 2m interval vs a fixed 2m threshold).
 */
export function staleThresholdMs(
  refreshIntervalSecs: number = DEFAULT_REFRESH_INTERVAL_SECS,
): number {
  const intervalMs =
    Math.max(0, Number.isFinite(refreshIntervalSecs) ? refreshIntervalSecs : 0) *
    1000;
  return Math.max(MIN_STALE_THRESHOLD_MS, intervalMs + STALE_GRACE_MS);
}

/** A snapshot is stale if its `fetched_at` is older than the interval-aware
 *  threshold. Used to hide persisted (loaded-from-disk) values on the mini
 *  bar until the scheduler delivers a fresh fetch for the current session —
 *  without blanking cards mid-cycle when the user picks a longer interval. */
export function isStaleSnapshot(
  snap: Pick<UsageSnapshot, "fetched_at">,
  thresholdMs: number = staleThresholdMs(),
  nowMs: number = Date.now(),
): boolean {
  const fetchedAt = new Date(snap.fetched_at).getTime();
  if (Number.isNaN(fetchedAt)) return true;
  return nowMs - fetchedAt > thresholdMs;
}
