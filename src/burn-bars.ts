import type { BurnBucket } from "./types.ts";

/** Reference burn (in percent points) that maps to a full-height bar.
 *  Derived from the even-pace burn per bucket (100% / 60 ≈ 1.67%) scaled up so
 *  steady usage reads as a modest bar and only real spikes hit the top. This
 *  reference is FIXED across refreshes — relative-to-window-max scaling made
 *  every bar rescale whenever the max fluctuated (a spike entering/leaving the
 *  sliding window, or two spikes merging into one bucket as `now` advanced),
 *  which looked like the chart constantly jumping. With a fixed reference, a
 *  given burn always renders at the same height regardless of what other
 *  buckets contain. */
const FULL_HEIGHT_BURN = (100 / 60) * 4; // ~6.67% — 4× even-pace burn per bucket

/** Bar heights 0–100, scaled to a FIXED reference (not the window's own max).
 *  All-zero/empty input → all-zero. Bigger burn always means a taller bar, but
 *  the scale never shifts between refreshes. */
export function burnBarHeights(buckets: BurnBucket[]): number[] {
  return buckets.map((b) => {
    if (!(b.burn > 0)) return 0;
    return Math.min(100, Math.round((b.burn / FULL_HEIGHT_BURN) * 100));
  });
}

/** Seconds covered by one bucket (period_secs / 60). The window label is
 *  accepted for symmetry with other burn-bar helpers; the period is driven
 *  by `isWeekly` so callers do not need to switch on label. */
export function bucketSecsForLabel(_label: string, isWeekly: boolean): number {
  return (isWeekly ? 7 * 24 * 3600 : 5 * 3600) / 60;
}

/** Tooltip for one bar, e.g. "Jul 14, 2:00 PM · 2.1% burned" / "Window reset here". */
export function burnBarTitle(bucket: BurnBucket): string {
  if (bucket.reset) return "Window reset here";
  const when = new Date(bucket.t * 1000).toLocaleString(undefined, {
    month: "short", day: "numeric", hour: "numeric", minute: "2-digit",
  });
  return `${when} · ${bucket.burn.toFixed(1)}% burned`;
}

/** True when at least one bucket has any signal to draw (burn or reset). The
 *  backend always returns 60 buckets per window, so an empty-history window
 *  shows up as 60 zero/reset=false buckets — hiding those keeps the popup
 *  free of dead rows until real history exists. */
export function hasBurnHistory(buckets: BurnBucket[] | undefined): boolean {
  if (!buckets || buckets.length === 0) return false;
  for (const b of buckets) {
    if (b.reset) return true;
    if (b.burn > 0) return true;
  }
  return false;
}

/** Percent of the window's quota burned since local midnight ("used today").
 *  Sums today's buckets; a reset marker zeroes the running sum because burn
 *  recorded before it belonged to the previous window. Returns null when no
 *  bucket falls inside today (nothing to say yet). */
export function burnedTodayPercent(
  buckets: BurnBucket[] | undefined,
  now: Date = new Date(),
): number | null {
  if (!buckets || buckets.length === 0) return null;
  const midnightSecs =
    new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000;
  let sum = 0;
  let sawToday = false;
  for (const b of buckets) {
    if (b.t < midnightSecs) continue;
    sawToday = true;
    if (b.reset) {
      sum = 0;
      continue;
    }
    sum += b.burn;
  }
  return sawToday ? sum : null;
}

/** Burn rate (% of quota per ms) over the recent `windowMs` ending at
 *  `nowMs`, from the per-bucket burn history. Returns null when there is no
 *  history to base a rate on (caller falls back to the whole-period average);
 *  returns 0 when history exists but nothing burned in the window (idle —
 *  the projection should say "left at reset", not "runs out"). A reset marker
 *  contributes no burn but counts as history. */
export function recentBurnRatePercentPerMs(
  buckets: BurnBucket[] | undefined,
  windowMs: number,
  nowMs = Date.now(),
): number | null {
  if (!buckets || buckets.length === 0 || !(windowMs > 0)) return null;
  if (!hasBurnHistory(buckets)) return null;
  const cutoffSecs = (nowMs - windowMs) / 1000;
  let burn = 0;
  for (const b of buckets) {
    if (b.t >= cutoffSecs && b.burn > 0) burn += b.burn;
  }
  return burn / windowMs;
}
