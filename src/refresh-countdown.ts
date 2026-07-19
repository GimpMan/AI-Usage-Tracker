/** Coerce a refresh-interval value from config / Tauri events. */
export function normalizeRefreshIntervalSecs(value: unknown): number | null {
  const n = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(n) || n <= 0) return null;
  return Math.round(n);
}

function parseFetchedAtMs(fetchedAtIso: string): number | null {
  if (!fetchedAtIso) return null;
  const ms = Date.parse(fetchedAtIso);
  if (Number.isNaN(ms)) return null;
  return ms;
}

/**
 * Fraction of the refresh cycle still remaining for a provider
 * (1 = just refreshed, 0 = due or overdue).
 *
 * Uses that provider's `fetched_at` and the **configured** refresh interval
 * (same value as Settings → Refresh interval).
 */
export function refreshRemainingFraction(
  fetchedAtIso: string,
  intervalSecs: number,
  nowMs: number = Date.now(),
): number {
  const fetchedAt = parseFetchedAtMs(fetchedAtIso);
  const interval = normalizeRefreshIntervalSecs(intervalSecs);
  if (fetchedAt == null || interval == null) return 0;
  const intervalMs = interval * 1000;
  const age = Math.max(0, nowMs - fetchedAt);
  // Slight epsilon so a brand-new fetch never rounds to an empty ring.
  if (age <= 50) return 1;
  return Math.max(0, Math.min(1, 1 - age / intervalMs));
}

/** Used stroke length on a unit circle (pathLength=1): 1 = full, 0 = empty. */
export function refreshRingDash(remaining: number): number {
  return Math.max(0, Math.min(1, remaining));
}

function formatLeft(leftMs: number): string {
  const totalSec = Math.ceil(leftMs / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  if (m <= 0) return `${s}s`;
  return `${m}m ${s.toString().padStart(2, "0")}s`;
}

/**
 * Short tooltip for the countdown ring, e.g. "Next refresh in 1m 12s".
 * `cycleIntervalSecs` is the interval for the *current* cycle (frozen when
 * Settings changes mid-cycle). Optional `settingsIntervalSecs` notes a
 * pending change that applies after the next refresh.
 */
export function refreshCountdownLabel(
  fetchedAtIso: string,
  cycleIntervalSecs: number,
  nowMs: number = Date.now(),
  settingsIntervalSecs?: number,
): string {
  const fetchedAt = parseFetchedAtMs(fetchedAtIso);
  const interval = normalizeRefreshIntervalSecs(cycleIntervalSecs);
  if (fetchedAt == null || interval == null) {
    return "Refresh timing unknown";
  }
  const dueMs = fetchedAt + interval * 1000;
  const leftMs = dueMs - nowMs;
  const base =
    leftMs <= 0 ? "Refresh due" : `Next refresh in ${formatLeft(leftMs)}`;
  const settings = normalizeRefreshIntervalSecs(settingsIntervalSecs);
  if (settings != null && settings !== interval) {
    return `${base} · new interval (${formatLeft(settings * 1000)}) after next refresh`;
  }
  return base;
}
