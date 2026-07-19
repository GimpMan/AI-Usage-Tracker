import type { BurnBucket, UsageWindow } from "./types";
import { recentBurnRatePercentPerMs } from "./burn-bars.ts";

export const WEEK_DAYS = 7;
export const DAY_MS = 24 * 60 * 60 * 1000;
export const HOUR_MS = 60 * 60 * 1000;
export const FIVE_HOUR_WINDOW_HOURS = 5;
export const DAILY_PACE_PERCENT = 100 / WEEK_DAYS;
const PERCENT_EPSILON = 0.000001;

/** Recent-history windows for the projection's burn rate. Long enough to
 *  smooth single-bucket noise, short enough that going idle flips the
 *  projection from "runs out" to "left at reset" within hours instead of
 *  days. Tuned to bucket granularity (5h: 12 buckets of 5m; weekly: ~2
 *  buckets of 2.8h). */
export const RECENT_BURN_WINDOW_5H_MS = HOUR_MS;
export const RECENT_BURN_WINDOW_WEEKLY_MS = 6 * HOUR_MS;

export interface WeeklyPace {
  remainingPercent: number;
  daysLeft: number;
  dailyQuotaPercent: number;
  targetRemainingPercent: number;
  /** Blue target minus one day of the currently available %/day —
   *  a dynamic near-term milestone that follows the real burn rate. */
  dailyTargetRemainingPercent: number;
  dayTickPercentages: number[];
}

export interface FiveHourPace {
  remainingPercent: number;
  hoursLeft: number;
  hourlyQuotaPercent: number;
  targetRemainingPercent: number;
  /** Blue target minus one hour of the currently available %/hour —
   *  a dynamic near-term milestone that follows the real burn rate. */
  hourlyTargetRemainingPercent: number;
  hourTickPercentages: number[];
}

export function isWeeklyWindow(label: string): boolean {
  const normalized = label.trim().toLowerCase();
  return (
    normalized === "weekly" ||
    normalized === "wk" ||
    normalized === "7d" ||
    normalized.startsWith("7d")
  );
}

export function isFiveHourWindow(label: string): boolean {
  const normalized = label.trim().toLowerCase();
  return normalized === "5h" || normalized.startsWith("5h ·");
}

function clampPercent(value: number): number {
  return Math.min(100, Math.max(0, value));
}

export function calculateWeeklyPace(
  window: UsageWindow,
  nowMs = Date.now(),
): WeeklyPace | null {
  if (!isWeeklyWindow(window.label) || !Number.isFinite(window.used_percent)) {
    return null;
  }

  if (!window.reset_at) return null;
  const resetMs = Date.parse(window.reset_at);
  if (!Number.isFinite(resetMs) || resetMs <= nowMs) return null;

  const daysLeft = (resetMs - nowMs) / DAY_MS;
  const remainingPercent = clampPercent(100 - window.used_percent);
  const dailyQuotaPercent = remainingPercent / daysLeft;
  const targetRemainingPercent = clampPercent((daysLeft / WEEK_DAYS) * 100);
  const dailyTargetRemainingPercent = clampPercent(
    targetRemainingPercent - dailyQuotaPercent,
  );
  const dayTickPercentages = Array.from(
    { length: WEEK_DAYS - 1 },
    (_, index) => ((index + 1) / WEEK_DAYS) * 100,
  );

  return {
    remainingPercent,
    daysLeft,
    dailyQuotaPercent,
    targetRemainingPercent,
    dailyTargetRemainingPercent,
    dayTickPercentages,
  };
}

export function calculateFiveHourPace(
  window: UsageWindow,
  nowMs = Date.now(),
): FiveHourPace | null {
  if (!isFiveHourWindow(window.label) || !Number.isFinite(window.used_percent)) {
    return null;
  }

  if (!window.reset_at) return null;
  const resetMs = Date.parse(window.reset_at);
  if (!Number.isFinite(resetMs) || resetMs <= nowMs) return null;

  const hoursLeft = (resetMs - nowMs) / HOUR_MS;
  const remainingPercent = clampPercent(100 - window.used_percent);
  const hourlyQuotaPercent = remainingPercent / hoursLeft;
  const targetRemainingPercent = clampPercent(
    (hoursLeft / FIVE_HOUR_WINDOW_HOURS) * 100,
  );
  const hourlyTargetRemainingPercent = clampPercent(
    targetRemainingPercent - hourlyQuotaPercent,
  );
  const hourTickPercentages = Array.from(
    { length: FIVE_HOUR_WINDOW_HOURS - 1 },
    (_, index) => ((index + 1) / FIVE_HOUR_WINDOW_HOURS) * 100,
  );

  return {
    remainingPercent,
    hoursLeft,
    hourlyQuotaPercent,
    targetRemainingPercent,
    hourlyTargetRemainingPercent,
    hourTickPercentages,
  };
}

function paceGradientPercent(
  remainingPercent: number,
  targetRemainingPercent: number,
  paceIntervalPercent: number,
): number {
  // Positive = behind the blue line (warm/red zone), 0 = at pace
  // (bright green), negative = ahead of the blue line (purple zone).
  const behindPercent = targetRemainingPercent - remainingPercent;

  if (behindPercent >= 0) {
    // Behind or at pace: 100 (at pace) down to 30 (one interval behind).
    if (behindPercent >= paceIntervalPercent) {
      // More than one interval behind: clamped to deep red.
      return clampPercent(
        30 - ((behindPercent - paceIntervalPercent) / paceIntervalPercent) * 30,
      );
    }
    return 100 - (behindPercent / paceIntervalPercent) * 70;
  }

  // Ahead of pace: 100 (at the blue line) → 200 (fully purple).
  // Scaled by the "% ahead of even pace" with a 10% window so that even
  // a small lead (e.g. 4% ahead) reads clearly as purple rather than
  // getting lost in a near-pure-green range.
  const aheadPercent = -behindPercent;
  return 100 + Math.min(1, aheadPercent / 10) * 100;
}

export function weeklyPaceGradientPercent(pace: WeeklyPace): number {
  return paceGradientPercent(
    pace.remainingPercent,
    pace.targetRemainingPercent,
    DAILY_PACE_PERCENT,
  );
}

export function fiveHourPaceGradientPercent(pace: FiveHourPace): number {
  return paceGradientPercent(
    pace.remainingPercent,
    pace.targetRemainingPercent,
    100 / FIVE_HOUR_WINDOW_HOURS,
  );
}

/** Percent gap vs even pace (own popup line). */
export function formatPaceGapNote(
  targetRemainingPercent: number,
  remainingPercent: number,
): string {
  const paceGap = targetRemainingPercent - remainingPercent;

  if (paceGap > PERCENT_EPSILON) {
    return `${paceGap.toFixed(1)}% over even pace`;
  }
  if (paceGap < -PERCENT_EPSILON) {
    return `${Math.abs(paceGap).toFixed(1)}% ahead of even pace`;
  }
  return "On even pace";
}

/** Compact human duration for pace-recovery / headroom labels. */
export function formatDurationApprox(ms: number): string {
  const mins = Math.round(Math.abs(ms) / 60000);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  const remMin = mins % 60;
  if (hrs < 48) return remMin ? `${hrs}h ${remMin}m` : `${hrs}h`;
  const days = Math.floor(hrs / 24);
  return `${days}d ${hrs % 24}h`;
}

/**
 * Time-symmetric gap vs even pace for a fixed remaining % (idle model):
 * - over pace → how long until the blue target falls to current remaining
 * - ahead → time-equivalent of how far ahead of even pace you are
 * - on pace → "On even pace"
 */
export function formatPaceTimeNote(
  remainingPercent: number,
  targetRemainingPercent: number,
  periodMs: number,
): string {
  if (!(periodMs > 0) || !Number.isFinite(periodMs)) {
    return "On even pace";
  }
  const gapPercent = targetRemainingPercent - remainingPercent;
  if (Math.abs(gapPercent) <= PERCENT_EPSILON) {
    return "On even pace";
  }
  const dur = formatDurationApprox((Math.abs(gapPercent) / 100) * periodMs);
  if (gapPercent > 0) {
    return `~${dur} until even pace`;
  }
  return `~${dur} ahead of even pace`;
}

/** Quota burn-rate line only (gap % is rendered separately).
 *
 *  The divisor is floored at one unit (day/hour): with less than a day/hour
 *  left the exact sustainable rate (remaining ÷ time-left) diverges toward
 *  infinity — e.g. 71% left with 27m to reset reads "~160%/hour", which looks
 *  like a bug. Flooring keeps the displayed rate ≤ remaining% (a conservative
 *  but truthful "at least this much is available") and ≤ 100%. The struct's
 *  raw `dailyQuotaPercent`/`hourlyQuotaPercent` is untouched — the sub-target
 *  line clamps the huge value to 0 harmlessly. */
export function formatWeeklyPaceNote(pace: WeeklyPace): string {
  const rate = pace.remainingPercent / Math.max(pace.daysLeft, 1);
  return `~${rate.toFixed(1)}%/day available until reset`;
}

export function formatFiveHourPaceNote(pace: FiveHourPace): string {
  const rate = pace.remainingPercent / Math.max(pace.hoursLeft, 1);
  return `~${rate.toFixed(1)}%/hour available until reset`;
}

/**
 * Projected run-out / leftover line. By default uses the average burn rate
 * since the period started, from a single snapshot. When
 * `recentRatePercentPerMs` is provided (from the per-bucket burn history, see
 * `recentProjectionRate`), that recent rate is extrapolated instead — a burst
 * right after reset no longer dominates the projection for days, and going
 * idle flips the line to "left at reset" instead of a stale "runs out".
 * Returns "" when the period just reset or an input is degenerate (the caller
 * hides the line).
 */
export function formatProjectionNote(
  remainingPercent: number,
  usedPercent: number,
  timeLeftMs: number,
  periodMs: number,
  recentRatePercentPerMs?: number | null,
): string {
  const elapsedMs = periodMs - timeLeftMs;
  if (
    elapsedMs <= 0 ||
    !Number.isFinite(remainingPercent) ||
    !Number.isFinite(usedPercent) ||
    !Number.isFinite(timeLeftMs) ||
    !Number.isFinite(periodMs)
  ) {
    return "";
  }
  if (remainingPercent <= 0) return "Projection: exhausted";
  if (usedPercent <= 0.05) return "Projection: no usage yet this period";
  const avgRate = recentRatePercentPerMs ?? usedPercent / elapsedMs; // % per ms
  const runOutMs = remainingPercent / avgRate;
  if (runOutMs < timeLeftMs) {
    return `Projection: runs out in ~${formatDurationApprox(runOutMs)} — ${formatDurationApprox(timeLeftMs - runOutMs)} before reset`;
  }
  return `Projection: ~${(remainingPercent - avgRate * timeLeftMs).toFixed(0)}% left at reset`;
}

// ============================================================
// Normalized even-pace shape — one render-ready interface for every
// paced window type (weekly, 5h, OpenRouter daily/monthly).
// ============================================================

export interface EvenPace {
  remainingPercent: number;
  /** Blue even-pace target line position (% of bar width). */
  targetRemainingPercent: number;
  /** Red sub-target: blue target minus one day/hour of the currently
   *  available burn rate (the %/day or %/hour until reset) — dynamic. */
  subTargetRemainingPercent: number;
  /** Subdivision tick positions. */
  tickPercentages: number[];
  /** Fill-color driver: 0–100 green→red, 100–200 green→purple (ahead). */
  gradientPercent: number;
  /** Quota burn-rate line (~X%/day available until reset). */
  note: string;
  /** Percent gap vs even pace (own line under the quota note). */
  gapNote: string;
  /**
   * Time until even pace when over, or time-equivalent headroom when ahead
   * (idle model). Always set when EvenPace is non-null.
   */
  timeNote: string;
  /** Projected run-out / leftover-at-reset line ("" = hide the line). */
  projectionNote: string;
  /** Lowercase cadence word for the target's title attribute. */
  targetLabel: string;
  /** Which sub-target wording to use: "daily" → "Today's", "hourly" → "This hour's". */
  subTargetKind: "daily" | "hourly";
}

/** Normalize a weekly window into the shared EvenPace shape.
 *  `recentRatePercentPerMs` (from `recentProjectionRate`) switches the
 *  projection from the whole-period average to the recent burn rate. */
export function weeklyEvenPace(
  window: UsageWindow,
  nowMs = Date.now(),
  recentRatePercentPerMs?: number | null,
): EvenPace | null {
  const pace = calculateWeeklyPace(window, nowMs);
  if (!pace) return null;
  const periodMs = WEEK_DAYS * DAY_MS;
  return {
    remainingPercent: pace.remainingPercent,
    targetRemainingPercent: pace.targetRemainingPercent,
    subTargetRemainingPercent: pace.dailyTargetRemainingPercent,
    tickPercentages: pace.dayTickPercentages,
    gradientPercent: weeklyPaceGradientPercent(pace),
    note: formatWeeklyPaceNote(pace),
    gapNote: formatPaceGapNote(
      pace.targetRemainingPercent,
      pace.remainingPercent,
    ),
    timeNote: formatPaceTimeNote(
      pace.remainingPercent,
      pace.targetRemainingPercent,
      periodMs,
    ),
    projectionNote: formatProjectionNote(
      pace.remainingPercent,
      window.used_percent,
      pace.daysLeft * DAY_MS,
      periodMs,
      recentRatePercentPerMs,
    ),
    targetLabel: "weekly",
    subTargetKind: "daily",
  };
}

/** Normalize a 5-hour window into the shared EvenPace shape.
 *  `recentRatePercentPerMs` works like in `weeklyEvenPace`. */
export function fiveHourEvenPace(
  window: UsageWindow,
  nowMs = Date.now(),
  recentRatePercentPerMs?: number | null,
): EvenPace | null {
  const pace = calculateFiveHourPace(window, nowMs);
  if (!pace) return null;
  const periodMs = FIVE_HOUR_WINDOW_HOURS * HOUR_MS;
  return {
    remainingPercent: pace.remainingPercent,
    targetRemainingPercent: pace.targetRemainingPercent,
    subTargetRemainingPercent: pace.hourlyTargetRemainingPercent,
    tickPercentages: pace.hourTickPercentages,
    gradientPercent: fiveHourPaceGradientPercent(pace),
    note: formatFiveHourPaceNote(pace),
    gapNote: formatPaceGapNote(
      pace.targetRemainingPercent,
      pace.remainingPercent,
    ),
    timeNote: formatPaceTimeNote(
      pace.remainingPercent,
      pace.targetRemainingPercent,
      periodMs,
    ),
    projectionNote: formatProjectionNote(
      pace.remainingPercent,
      window.used_percent,
      pace.hoursLeft * HOUR_MS,
      periodMs,
      recentRatePercentPerMs,
    ),
    targetLabel: "hourly",
    subTargetKind: "hourly",
  };
}

// ============================================================
// Daily/monthly calendar pace + the shared resolver.
// OpenRouter's weekly window is caught by isWeeklyWindow above; this
// handles daily (24h) and monthly (~days-in-month) windows.
// Daily/monthly are provider-gated via resolveEvenPace (see
// CALENDAR_PACE_PROVIDERS) so that, e.g., Z.ai's monthly tool-use quota
// is not given a pace line.
// ============================================================

const OPENROUTER_DAILY_TICK_HOURS = [4, 8, 12, 16, 20];

function daysInCurrentMonth(nowMs: number): number {
  const d = new Date(nowMs);
  // day 0 of next month = last day of this month.
  return new Date(d.getFullYear(), d.getMonth() + 1, 0).getDate();
}

/** Even-pace calculation for a daily or monthly limit window (OpenRouter
 *  spend caps, Grok's monthly included pool). */
export function openrouterEvenPace(
  window: UsageWindow,
  nowMs = Date.now(),
): EvenPace | null {
  const label = window.label.trim().toLowerCase();
  if (label !== "daily" && label !== "monthly") return null;
  if (!Number.isFinite(window.used_percent) || !window.reset_at) return null;
  const resetMs = Date.parse(window.reset_at);
  if (!Number.isFinite(resetMs) || resetMs <= nowMs) return null;

  const remainingPercent = clampPercent(100 - window.used_percent);
  const timeLeftMs = resetMs - nowMs;

  if (label === "daily") {
    const periodMs = DAY_MS; // 24h
    const hoursLeft = timeLeftMs / HOUR_MS;
    const hourlyQuotaPercent = remainingPercent / hoursLeft;
    const targetRemainingPercent = clampPercent((timeLeftMs / periodMs) * 100);
    const paceIntervalPercent = 100 / 24; // one hour (gradient scaling)
    return {
      remainingPercent,
      targetRemainingPercent,
      subTargetRemainingPercent: clampPercent(
        targetRemainingPercent - hourlyQuotaPercent,
      ),
      tickPercentages: OPENROUTER_DAILY_TICK_HOURS.map((h) => (h / 24) * 100),
      gradientPercent: paceGradientPercent(
        remainingPercent,
        targetRemainingPercent,
        paceIntervalPercent,
      ),
      note: `~${(remainingPercent / Math.max(hoursLeft, 1)).toFixed(1)}%/hour available until reset`,
      gapNote: formatPaceGapNote(targetRemainingPercent, remainingPercent),
      timeNote: formatPaceTimeNote(
        remainingPercent,
        targetRemainingPercent,
        periodMs,
      ),
      projectionNote: formatProjectionNote(
        remainingPercent,
        window.used_percent,
        timeLeftMs,
        periodMs,
      ),
      targetLabel: "daily",
      subTargetKind: "hourly",
    };
  }

  // monthly
  const dim = daysInCurrentMonth(nowMs);
  const periodMs = dim * DAY_MS;
  const daysLeft = timeLeftMs / DAY_MS;
  const dailyQuotaPercent = remainingPercent / daysLeft;
  const targetRemainingPercent = clampPercent((timeLeftMs / periodMs) * 100);
  const paceIntervalPercent = 100 / dim; // one day (gradient scaling)
  return {
    remainingPercent,
    targetRemainingPercent,
    subTargetRemainingPercent: clampPercent(
      targetRemainingPercent - dailyQuotaPercent,
    ),
    tickPercentages: [1, 2, 3, 4, 5].map((n) => (n / 6) * 100),
    gradientPercent: paceGradientPercent(
      remainingPercent,
      targetRemainingPercent,
      paceIntervalPercent,
    ),
    note: `~${(remainingPercent / Math.max(daysLeft, 1)).toFixed(1)}%/day available until reset`,
    gapNote: formatPaceGapNote(targetRemainingPercent, remainingPercent),
    timeNote: formatPaceTimeNote(
      remainingPercent,
      targetRemainingPercent,
      periodMs,
    ),
    projectionNote: formatProjectionNote(
      remainingPercent,
      window.used_percent,
      timeLeftMs,
      periodMs,
    ),
    targetLabel: "monthly",
    subTargetKind: "daily",
  };
}

/** "Projected ~$X by month-end vs $Y limit" for a monthly window carrying
 *  typed absolute USD counters (OpenRouter's per-key monthly cap). Linear
 *  projection from the elapsed fraction of the current calendar month. */
export function dollarMonthlyProjectionNote(
  window: UsageWindow,
  nowMs = Date.now(),
): string | null {
  if (window.label.trim().toLowerCase() !== "monthly") return null;
  const used = window.used_absolute;
  const limit = window.limit_absolute;
  if (used == null || limit == null || !window.reset_at) return null;
  const resetMs = Date.parse(window.reset_at);
  if (!Number.isFinite(resetMs) || resetMs <= nowMs) return null;
  const periodMs = daysInCurrentMonth(nowMs) * DAY_MS;
  const elapsedMs = periodMs - (resetMs - nowMs);
  if (elapsedMs <= 0) return null;
  const projected = used * (periodMs / elapsedMs);
  return `Projected ~$${projected.toFixed(2)} by month-end vs $${limit.toFixed(2)} limit`;
}

/** Providers whose daily/monthly windows are calendar-paced: OpenRouter
 *  spend caps and Grok's monthly included pool are plain usage pools.
 *  Z.ai's monthly window is a tool-use quota and stays unpaced. */
const CALENDAR_PACE_PROVIDERS = ["openrouter", "grok"];

/** Resolve any window to its EvenPace, or null when no pace applies.
 *  5h and weekly are label-driven (any provider); daily/monthly are gated
 *  to CALENDAR_PACE_PROVIDERS via the provider label.
 *  `recentRatePercentPerMs` (from `recentProjectionRate`) is forwarded to the
 *  weekly/5h projection; calendar-paced windows never get it (the burn
 *  history only covers weekly/5h windows). */
export function resolveEvenPace(
  window: UsageWindow,
  providerLabel: string,
  nowMs = Date.now(),
  recentRatePercentPerMs?: number | null,
): EvenPace | null {
  if (isFiveHourWindow(window.label))
    return fiveHourEvenPace(window, nowMs, recentRatePercentPerMs);
  if (isWeeklyWindow(window.label))
    return weeklyEvenPace(window, nowMs, recentRatePercentPerMs);
  const provider = providerLabel.toLowerCase();
  if (CALENDAR_PACE_PROVIDERS.some((p) => provider.includes(p))) {
    return openrouterEvenPace(window, nowMs);
  }
  return null;
}

/** Recent burn rate (% per ms) for the projection line, from the per-bucket
 *  burn history — or null to fall back to the whole-period average (no
 *  history, degenerate reset time, or a non-weekly/5h window). The lookback
 *  is capped at min(RECENT_BURN_WINDOW_*, elapsed this period) so a window
 *  that just reset doesn't dilute the rate over time it wasn't running. */
export function recentProjectionRate(
  window: UsageWindow,
  buckets: BurnBucket[] | undefined,
  nowMs = Date.now(),
): number | null {
  const weekly = isWeeklyWindow(window.label);
  if (!weekly && !isFiveHourWindow(window.label)) return null;
  if (!window.reset_at) return null;
  const resetMs = Date.parse(window.reset_at);
  if (!Number.isFinite(resetMs)) return null;
  const periodMs = weekly
    ? WEEK_DAYS * DAY_MS
    : FIVE_HOUR_WINDOW_HOURS * HOUR_MS;
  const elapsedMs = periodMs - (resetMs - nowMs);
  if (elapsedMs <= 0) return null;
  const capMs = weekly
    ? RECENT_BURN_WINDOW_WEEKLY_MS
    : RECENT_BURN_WINDOW_5H_MS;
  return recentBurnRatePercentPerMs(
    buckets,
    Math.min(capMs, elapsedMs),
    nowMs,
  );
}
