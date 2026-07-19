import assert from "node:assert/strict";
import * as weeklyPaceModule from "../src/weekly-pace.ts";
import {
  calculateFiveHourPace,
  calculateWeeklyPace,
  dollarMonthlyProjectionNote,
  fiveHourEvenPace,
  formatDurationApprox,
  formatFiveHourPaceNote,
  formatPaceGapNote,
  formatPaceTimeNote,
  formatProjectionNote,
  formatWeeklyPaceNote,
  isFiveHourWindow,
  isWeeklyWindow,
  recentProjectionRate,
  weeklyEvenPace,
  RECENT_BURN_WINDOW_WEEKLY_MS,
  WEEK_DAYS,
} from "../src/weekly-pace.ts";

const DAY_MS = 24 * 60 * 60 * 1000;
const HOUR_MS = 60 * 60 * 1000;
const WEEK_MS = WEEK_DAYS * DAY_MS;
const now = Date.parse("2026-07-10T12:00:00.000Z");
const resetAfter = (days) => new Date(now + days * DAY_MS).toISOString();
const closeTo = (actual, expected) =>
  assert.ok(Math.abs(actual - expected) < 0.001, `${actual} != ${expected}`);

assert.equal(isWeeklyWindow("weekly"), true);
assert.equal(isWeeklyWindow("wk"), true);
assert.equal(isWeeklyWindow("7d"), true);
assert.equal(isWeeklyWindow("7d · 12M tokens"), true);
assert.equal(isWeeklyWindow("5h"), false);
assert.equal(isFiveHourWindow("5h"), true);
assert.equal(isFiveHourWindow("5h · 80K"), true);
assert.equal(isFiveHourWindow("weekly"), false);

const fiveHour = calculateFiveHourPace(
  { label: "5h", used_percent: 1, reset_at: resetAfter(4.25 / 24), bar_visible: true },
  now,
);
assert.ok(fiveHour);
closeTo(fiveHour.hoursLeft, 4.25);
closeTo(fiveHour.remainingPercent, 99);
closeTo(fiveHour.hourlyQuotaPercent, 99 / 4.25);
closeTo(fiveHour.targetRemainingPercent, 85);
// red sub-target = blue target minus one hour of the available %/hour.
closeTo(
  fiveHour.hourlyTargetRemainingPercent,
  fiveHour.targetRemainingPercent - fiveHour.hourlyQuotaPercent,
);
assert.equal(fiveHour.hourTickPercentages.length, 4);
closeTo(fiveHour.hourTickPercentages[0], 20);
closeTo(fiveHour.hourTickPercentages[3], 80);
assert.equal(
  formatFiveHourPaceNote(fiveHour),
  "~23.3%/hour available until reset",
);
assert.equal(
  formatPaceGapNote(fiveHour.targetRemainingPercent, fiveHour.remainingPercent),
  "14.0% ahead of even pace",
);

const fiveHourPaceForOverage = (overagePercent) =>
  calculateFiveHourPace(
    {
      label: "5h · 80K",
      used_percent: 20 + overagePercent,
      reset_at: resetAfter(4 / 24),
      bar_visible: true,
    },
    now,
  );

assert.equal(typeof weeklyPaceModule.fiveHourPaceGradientPercent, "function");
const { fiveHourPaceGradientPercent } = weeklyPaceModule;
assert.equal(fiveHourPaceGradientPercent(fiveHourPaceForOverage(0)), 100);
assert.equal(fiveHourPaceGradientPercent(fiveHourPaceForOverage(10)), 65);
assert.equal(fiveHourPaceGradientPercent(fiveHourPaceForOverage(20)), 30);
assert.equal(fiveHourPaceGradientPercent(fiveHourPaceForOverage(30)), 15);
assert.equal(fiveHourPaceGradientPercent(fiveHourPaceForOverage(40)), 0);

const fullWeek = calculateWeeklyPace(
  { label: "wk", used_percent: 0, reset_at: resetAfter(7), bar_visible: true },
  now,
);
assert.ok(fullWeek);
assert.equal(fullWeek.daysLeft, 7);
closeTo(fullWeek.remainingPercent, 100);
closeTo(fullWeek.dailyQuotaPercent, 100 / 7);
closeTo(fullWeek.targetRemainingPercent, 100);
assert.equal(fullWeek.dayTickPercentages.length, 6);
assert.equal(
  formatWeeklyPaceNote(fullWeek),
  "~14.3%/day available until reset",
);
assert.equal(
  formatPaceGapNote(fullWeek.targetRemainingPercent, fullWeek.remainingPercent),
  "On even pace",
);

const midWeek = calculateWeeklyPace(
  { label: "weekly", used_percent: 40, reset_at: resetAfter(4), bar_visible: true },
  now,
);
assert.ok(midWeek);
assert.equal(midWeek.daysLeft, 4);
closeTo(midWeek.remainingPercent, 60);
closeTo(midWeek.dailyQuotaPercent, 15);
closeTo(midWeek.targetRemainingPercent, (4 / 7) * 100);
// red sub-target = blue target minus one day of the available %/day.
closeTo(
  midWeek.dailyTargetRemainingPercent,
  midWeek.targetRemainingPercent - midWeek.dailyQuotaPercent,
);
assert.equal(
  formatWeeklyPaceNote(midWeek),
  "~15.0%/day available until reset",
);
assert.equal(
  formatPaceGapNote(midWeek.targetRemainingPercent, midWeek.remainingPercent),
  "2.9% ahead of even pace",
);

const fractionalWeek = calculateWeeklyPace(
  { label: "weekly", used_percent: 40, reset_at: resetAfter(4.25), bar_visible: true },
  now,
);
assert.ok(fractionalWeek);
closeTo(fractionalWeek.daysLeft, 4.25);
closeTo(fractionalWeek.targetRemainingPercent, (4.25 / 7) * 100);

const overPace = calculateWeeklyPace(
  { label: "wk", used_percent: 37, reset_at: resetAfter(6), bar_visible: true },
  now,
);
assert.ok(overPace);
assert.equal(
  formatWeeklyPaceNote(overPace),
  "~10.5%/day available until reset",
);
assert.equal(
  formatPaceGapNote(overPace.targetRemainingPercent, overPace.remainingPercent),
  "22.7% over even pace",
);

// Time gap vs even pace (idle model): over → until even; ahead → headroom.
assert.equal(formatDurationApprox(45 * 60 * 1000), "45m");
assert.equal(formatDurationApprox(2 * HOUR_MS + 15 * 60 * 1000), "2h 15m");
assert.equal(formatDurationApprox(2 * DAY_MS + 5 * HOUR_MS), "2d 5h");
assert.equal(
  formatPaceTimeNote(100, 100, WEEK_MS),
  "On even pace",
);
// 10% over a 7-day period → 0.7 day = 16h 48m → rounds to 16h 48m
assert.equal(
  formatPaceTimeNote(50, 60, WEEK_MS),
  `~${formatDurationApprox(0.1 * WEEK_MS)} until even pace`,
);
// 10% ahead → same duration, opposite wording
assert.equal(
  formatPaceTimeNote(60, 50, WEEK_MS),
  `~${formatDurationApprox(0.1 * WEEK_MS)} ahead of even pace`,
);
assert.equal(
  formatPaceTimeNote(overPace.remainingPercent, overPace.targetRemainingPercent, WEEK_MS),
  `~${formatDurationApprox(
    ((overPace.targetRemainingPercent - overPace.remainingPercent) / 100) * WEEK_MS,
  )} until even pace`,
);
assert.equal(
  formatPaceTimeNote(midWeek.remainingPercent, midWeek.targetRemainingPercent, WEEK_MS),
  `~${formatDurationApprox(
    ((midWeek.remainingPercent - midWeek.targetRemainingPercent) / 100) * WEEK_MS,
  )} ahead of even pace`,
);

const dailyPacePercent = 100 / 7;
const paceForOverage = (overagePercent) =>
  calculateWeeklyPace(
    {
      label: "wk",
      used_percent: 100 - (6 / 7) * 100 + overagePercent,
      reset_at: resetAfter(6),
      bar_visible: true,
    },
    now,
  );

assert.equal(typeof weeklyPaceModule.weeklyPaceGradientPercent, "function");
const { weeklyPaceGradientPercent } = weeklyPaceModule;
assert.equal(weeklyPaceGradientPercent(paceForOverage(0)), 100);
closeTo(weeklyPaceGradientPercent(paceForOverage(dailyPacePercent)), 30);
closeTo(weeklyPaceGradientPercent(paceForOverage(dailyPacePercent * 2)), 0);

const lastDay = calculateWeeklyPace(
  { label: "7d · 12M", used_percent: 60, reset_at: resetAfter(1), bar_visible: false },
  now,
);
assert.ok(lastDay);
assert.equal(lastDay.daysLeft, 1);
closeTo(lastDay.dailyQuotaPercent, 40);

assert.equal(
  calculateWeeklyPace(
    { label: "wk", used_percent: 40, reset_at: null, bar_visible: true },
    now,
  ),
  null,
);
assert.equal(
  calculateWeeklyPace(
    { label: "wk", used_percent: 40, reset_at: "not-a-date", bar_visible: true },
    now,
  ),
  null,
);
assert.equal(
  calculateWeeklyPace(
    { label: "wk", used_percent: 40, reset_at: resetAfter(-1), bar_visible: true },
    now,
  ),
  null,
);
assert.equal(
  calculateWeeklyPace(
    { label: "5h", used_percent: 40, reset_at: resetAfter(1), bar_visible: true },
    now,
  ),
  null,
);

// --- EvenPace adapters normalize weekly/5h into one render-ready shape ---

const midWeekWindow = {
  label: "weekly",
  used_percent: 40,
  reset_at: resetAfter(4),
  bar_visible: true,
};
const midWeekPace = calculateWeeklyPace(midWeekWindow, now);
const midWeekEven = weeklyEvenPace(midWeekWindow, now);
assert.ok(midWeekEven);
assert.equal(midWeekEven.remainingPercent, midWeekPace.remainingPercent);
assert.equal(
  midWeekEven.targetRemainingPercent,
  midWeekPace.targetRemainingPercent,
);
assert.equal(
  midWeekEven.subTargetRemainingPercent,
  midWeekPace.dailyTargetRemainingPercent,
);
assert.deepEqual(midWeekEven.tickPercentages, midWeekPace.dayTickPercentages);
assert.equal(midWeekEven.gradientPercent, weeklyPaceGradientPercent(midWeekPace));
assert.equal(midWeekEven.note, formatWeeklyPaceNote(midWeekPace));
assert.equal(
  midWeekEven.gapNote,
  formatPaceGapNote(
    midWeekPace.targetRemainingPercent,
    midWeekPace.remainingPercent,
  ),
);
assert.equal(
  midWeekEven.timeNote,
  formatPaceTimeNote(
    midWeekPace.remainingPercent,
    midWeekPace.targetRemainingPercent,
    WEEK_MS,
  ),
);
assert.equal(midWeekEven.targetLabel, "weekly");
assert.equal(midWeekEven.subTargetKind, "daily");

const fiveHourWindow = {
  label: "5h · 80K",
  used_percent: 20,
  reset_at: resetAfter(4 / 24),
  bar_visible: true,
};
const fiveHourPace = calculateFiveHourPace(fiveHourWindow, now);
const fiveHourEven = fiveHourEvenPace(fiveHourWindow, now);
assert.ok(fiveHourEven);
assert.equal(fiveHourEven.targetLabel, "hourly");
assert.equal(fiveHourEven.subTargetKind, "hourly");
assert.deepEqual(fiveHourEven.tickPercentages, fiveHourPace.hourTickPercentages);
assert.equal(fiveHourEven.note, formatFiveHourPaceNote(fiveHourPace));
assert.equal(
  fiveHourEven.gapNote,
  formatPaceGapNote(
    fiveHourPace.targetRemainingPercent,
    fiveHourPace.remainingPercent,
  ),
);
assert.equal(fiveHourEven.remainingPercent, fiveHourPace.remainingPercent);
assert.equal(
  fiveHourEven.targetRemainingPercent,
  fiveHourPace.targetRemainingPercent,
);
assert.equal(
  fiveHourEven.subTargetRemainingPercent,
  fiveHourPace.hourlyTargetRemainingPercent,
);
assert.equal(
  fiveHourEven.gradientPercent,
  fiveHourPaceGradientPercent(fiveHourPace),
);
assert.equal(
  fiveHourEven.timeNote,
  formatPaceTimeNote(
    fiveHourPace.remainingPercent,
    fiveHourPace.targetRemainingPercent,
    5 * HOUR_MS,
  ),
);

// Adapters return null when the underlying calculator does.
assert.equal(
  weeklyEvenPace(
    { label: "5h", used_percent: 40, reset_at: resetAfter(1), bar_visible: true },
    now,
  ),
  null,
);
assert.equal(
  fiveHourEvenPace(
    { label: "weekly", used_percent: 40, reset_at: resetAfter(1), bar_visible: true },
    now,
  ),
  null,
);

// --- Projection notes: run-out / leftover from a single snapshot ---

// Direct formatter cases over a weekly period (elapsed = 4d, left = 3d).
assert.equal(
  formatProjectionNote(20, 80, 3 * DAY_MS, WEEK_MS),
  "Projection: runs out in ~24h — 2d 0h before reset",
  "heavy burn runs out before reset",
);
assert.equal(
  formatProjectionNote(88, 12, 3 * DAY_MS, WEEK_MS),
  "Projection: ~79% left at reset",
  "light burn lands with quota left at reset",
);
assert.equal(
  formatProjectionNote(100, 0, WEEK_MS, WEEK_MS),
  "",
  "just reset (no elapsed time) hides the line",
);
assert.equal(
  formatProjectionNote(100, 0, 3 * DAY_MS, WEEK_MS),
  "Projection: no usage yet this period",
);
assert.equal(
  formatProjectionNote(0, 90, 3 * DAY_MS, WEEK_MS),
  "Projection: exhausted",
);
assert.equal(
  formatProjectionNote(NaN, 10, 3 * DAY_MS, WEEK_MS),
  "",
  "non-finite input hides the line",
);

// Adapter-level: projectionNote is populated with a fixed clock.
const runsOutEven = weeklyEvenPace(
  { label: "wk", used_percent: 80, reset_at: resetAfter(3), bar_visible: true },
  now,
);
assert.ok(runsOutEven);
assert.equal(
  runsOutEven.projectionNote,
  "Projection: runs out in ~24h — 2d 0h before reset",
);

const landsEven = weeklyEvenPace(
  { label: "wk", used_percent: 12, reset_at: resetAfter(3), bar_visible: true },
  now,
);
assert.ok(landsEven);
assert.equal(landsEven.projectionNote, "Projection: ~79% left at reset");

const justResetEven = weeklyEvenPace(
  { label: "wk", used_percent: 0, reset_at: resetAfter(7), bar_visible: true },
  now,
);
assert.ok(justResetEven);
assert.equal(justResetEven.projectionNote, "");

const noUsageEven = weeklyEvenPace(
  { label: "wk", used_percent: 0, reset_at: resetAfter(3.5), bar_visible: true },
  now,
);
assert.ok(noUsageEven);
assert.equal(
  noUsageEven.projectionNote,
  "Projection: no usage yet this period",
);

const exhaustedEven = weeklyEvenPace(
  { label: "wk", used_percent: 100, reset_at: resetAfter(3), bar_visible: true },
  now,
);
assert.ok(exhaustedEven);
assert.equal(exhaustedEven.projectionNote, "Projection: exhausted");

// 5h window: 90% used in 3h of 5h → 10% left runs out 20m in, 1h 40m early.
const fiveHourRunsOut = fiveHourEvenPace(
  { label: "5h", used_percent: 90, reset_at: resetAfter(2 / 24), bar_visible: true },
  now,
);
assert.ok(fiveHourRunsOut);
assert.equal(
  fiveHourRunsOut.projectionNote,
  "Projection: runs out in ~20m — 1h 40m before reset",
);

// --- Near-reset rate cap: the displayed burn rate never exceeds 100% ---
// With <1 unit left, remaining ÷ time-left diverges (e.g. 71% left with 27m
// to reset would read "~157%/hour"). The note floors the divisor at one unit
// so it reads as a conservative "at least this much available" instead. The
// struct's raw quota stays exact for the sub-target line.

const fiveHourNearReset = calculateFiveHourPace(
  { label: "5h", used_percent: 29, reset_at: resetAfter(0.45 / 24), bar_visible: true },
  now,
);
assert.ok(fiveHourNearReset);
closeTo(fiveHourNearReset.hoursLeft, 0.45);
closeTo(fiveHourNearReset.hourlyQuotaPercent, 71 / 0.45); // raw: ~157.8 (sub-target)
assert.equal(
  formatFiveHourPaceNote(fiveHourNearReset),
  "~71.0%/hour available until reset",
  "note must cap the rate at remaining% when <1h left, not exceed 100%",
);

const weeklyNearReset = calculateWeeklyPace(
  { label: "weekly", used_percent: 60, reset_at: resetAfter(0.5), bar_visible: true },
  now,
);
assert.ok(weeklyNearReset);
closeTo(weeklyNearReset.dailyQuotaPercent, 80); // raw: 40 / 0.5 (sub-target)
assert.equal(
  formatWeeklyPaceNote(weeklyNearReset),
  "~40.0%/day available until reset",
  "note must cap the rate at remaining% when <1d left, not exceed 100%",
);

// --- OpenRouter monthly dollar projection ---
// now = 2026-07-10T12:00Z; July has 31 days, reset 2026-08-01 → 21.5d left,
// 9.5d elapsed. $10 used → projected = 10 × 31/9.5 ≈ $32.63.
assert.equal(
  dollarMonthlyProjectionNote(
    {
      label: "monthly",
      used_percent: 10,
      reset_at: "2026-08-01T00:00:00.000Z",
      bar_visible: true,
      is_unlimited: false,
      used_absolute: 10,
      limit_absolute: 100,
    },
    now,
  ),
  "Projected ~$32.63 by month-end vs $100.00 limit",
);
// Non-monthly labels, missing counters, and missing/past resets say nothing.
assert.equal(
  dollarMonthlyProjectionNote(
    {
      label: "weekly",
      used_percent: 10,
      reset_at: "2026-08-01T00:00:00.000Z",
      bar_visible: true,
      is_unlimited: false,
      used_absolute: 10,
      limit_absolute: 100,
    },
    now,
  ),
  null,
  "weekly label yields no monthly dollar projection",
);
assert.equal(
  dollarMonthlyProjectionNote(
    {
      label: "monthly",
      used_percent: 10,
      reset_at: "2026-08-01T00:00:00.000Z",
      bar_visible: true,
      is_unlimited: false,
    },
    now,
  ),
  null,
  "missing absolute counters yields null",
);
assert.equal(
  dollarMonthlyProjectionNote(
    {
      label: "monthly",
      used_percent: 10,
      reset_at: null,
      bar_visible: true,
      is_unlimited: false,
      used_absolute: 10,
      limit_absolute: 100,
    },
    now,
  ),
  null,
  "missing reset yields null",
);

// --- Recent-rate projection: extrapolate recent burn, not the period average ---

// Rate 0 (idle recently) → leftover at reset, not a stale "runs out".
assert.equal(
  formatProjectionNote(57, 43, 6.5 * DAY_MS, WEEK_MS, 0),
  "Projection: ~57% left at reset",
  "recent rate of 0 must project leftover at reset",
);
// A recent rate is extrapolated directly: 50% left at 10%/6h → 30h to run out.
assert.equal(
  formatProjectionNote(50, 50, 3 * DAY_MS, WEEK_MS, 10 / (6 * HOUR_MS)),
  "Projection: runs out in ~30h — 42h before reset",
  "recent rate drives the run-out projection when provided",
);
// Omitted rate → whole-period average, unchanged legacy behavior.
assert.equal(
  formatProjectionNote(50, 50, 3 * DAY_MS, WEEK_MS),
  formatProjectionNote(50, 50, 3 * DAY_MS, WEEK_MS, null),
  "omitting the rate falls back to the period average",
);

// recentProjectionRate picks the lookback window: min(cap, elapsed).
const t0 = now / 1000;
const weeklyWindowForRate = {
  label: "weekly",
  used_percent: 43,
  reset_at: resetAfter(6.5), // elapsed 12h → lookback capped at 6h
  bar_visible: true,
};
{
  const rate = recentProjectionRate(
    weeklyWindowForRate,
    [
      { t: t0 - 8 * 3600, burn: 10, reset: false }, // 8h ago: outside 6h cap
      { t: t0 - 5 * 3600, burn: 2, reset: false },
      { t: t0 - 3600, burn: 3, reset: false },
    ],
    now,
  );
  assert.ok(
    rate !== null && Math.abs(rate - 5 / RECENT_BURN_WINDOW_WEEKLY_MS) < 1e-15,
    `weekly rate must be 5% over the 6h cap, got ${rate}`,
  );
}
// Just reset (elapsed ≈ 0) → null: no dilution over time not yet running.
assert.equal(
  recentProjectionRate(
    { label: "weekly", used_percent: 0, reset_at: resetAfter(7), bar_visible: true },
    [{ t: t0 - 60, burn: 5, reset: false }],
    now,
  ),
  null,
  "a window that just reset returns null",
);
// 5h window with 4h left (elapsed 1h) → lookback is 1h, not the full cap.
{
  const rate = recentProjectionRate(
    { label: "5h", used_percent: 20, reset_at: resetAfter(4 / 24), bar_visible: true },
    [{ t: t0 - 1800, burn: 5, reset: false }],
    now,
  );
  assert.ok(
    rate !== null && Math.abs(rate - 5 / HOUR_MS) < 1e-15,
    `5h rate must use the elapsed 1h window, got ${rate}`,
  );
}
// Degenerate inputs → null (period-average fallback).
assert.equal(recentProjectionRate(weeklyWindowForRate, undefined, now), null);
assert.equal(
  recentProjectionRate(
    { label: "monthly", used_percent: 40, reset_at: resetAfter(3), bar_visible: true },
    [{ t: t0 - 60, burn: 5, reset: false }],
    now,
  ),
  null,
  "non-weekly/5h windows have no burn history",
);
assert.equal(
  recentProjectionRate(
    { label: "weekly", used_percent: 40, reset_at: null, bar_visible: true },
    [{ t: t0 - 60, burn: 5, reset: false }],
    now,
  ),
  null,
);

// Adapter-level: the recent rate flows into the projection note.
const idleRecentEven = weeklyEvenPace(
  { label: "wk", used_percent: 43, reset_at: resetAfter(6.5), bar_visible: true },
  now,
  0,
);
assert.ok(idleRecentEven);
assert.equal(
  idleRecentEven.projectionNote,
  "Projection: ~57% left at reset",
  "idle recent rate flips the adapter's projection to leftover",
);
const busyRecentEven = weeklyEvenPace(
  { label: "wk", used_percent: 50, reset_at: resetAfter(3), bar_visible: true },
  now,
  10 / (6 * HOUR_MS),
);
assert.ok(busyRecentEven);
assert.equal(
  busyRecentEven.projectionNote,
  "Projection: runs out in ~30h — 42h before reset",
);

console.log("weekly pace tests passed");
