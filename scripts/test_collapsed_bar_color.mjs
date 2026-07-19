import assert from "node:assert/strict";
import { collapsedBarColorPercent, isWeeklyUnderRedLine, isWeeklyWindowUnderRedLine, isFiveHourWindowUnderRedLine, isFiveHourUnderRedLine } from "../src/bar-summary.ts";
import { resolveEvenPace } from "../src/weekly-pace.ts";

// Fixed timeline so pace math is deterministic. reset_at values are exact
// multiples of DAY_MS / HOUR_MS from NOW so daysLeft/hoursLeft are whole.
const NOW = Date.parse("2026-07-11T12:00:00Z");
const WEEK_RESET = "2026-07-18T12:00:00Z";   // +7 days  -> daysLeft 7, weekly target 100, subTarget = 100 - remaining/7
const FIVE_H_RESET = "2026-07-11T17:00:00Z"; // +5 hours -> hoursLeft 5, 5h target 100
const FIVE_H_RESET_AHEAD = "2026-07-11T15:00:00Z"; // +3 hours -> hoursLeft 3, 5h target 60 (ahead)

const win = (label, used_percent, extra = {}) => ({
  label,
  used_percent,
  reset_at: label.startsWith("5h") ? FIVE_H_RESET : WEEK_RESET,
  bar_visible: true,
  is_unlimited: false,
  ...extra,
});

const paceColor = (w, provider = "glm") =>
  resolveEvenPace(w, provider, NOW).gradientPercent;

const underRedLine = (w, provider = "glm") => {
  const p = resolveEvenPace(w, provider, NOW);
  return p.remainingPercent < p.subTargetRemainingPercent;
};

// Case 1: 5h healthy + weekly under red line -> 5h color. The weekly warning
// is surfaced by the pulsing red ring (isWeeklyUnderRedLine), not the fill.
{
  const five = win("5h", 0);        // remaining 100, on target -> green
  const weekly = win("weekly", 30); // remaining 70 < 90 (subTarget 100 - 70/7) -> under red line
  assert.equal(underRedLine(weekly), true, "case 1 precondition: weekly under red line");
  const got = collapsedBarColorPercent([five, weekly], "glm", NOW);
  assert.equal(got, paceColor(five), "case 1: 5h color wins; weekly handled by pulsing ring");
}

// Case 2: 5h healthy + weekly above red line -> 5h color.
{
  const five = win("5h", 0);        // remaining 100 -> green (gradientPercent 100)
  const weekly = win("weekly", 5);  // remaining 95 > 86.4 (subTarget 100 - 95/7) -> above red line
  assert.equal(underRedLine(weekly), false, "case 2 precondition: weekly above red line");
  const got = collapsedBarColorPercent([five, weekly], "glm", NOW);
  assert.equal(got, paceColor(five), "case 2: 5h color when weekly is fine");
}

// Case 3: 5h ahead (purple) + weekly above red line -> 5h purple (NOT masked).
{
  const five = win("5h", 0, { reset_at: FIVE_H_RESET_AHEAD }); // remaining 100 > target 60 -> purple
  const weekly = win("weekly", 5);                              // above red line
  const fiveCp = paceColor(five);
  assert.ok(fiveCp > 100, "case 3 precondition: 5h is ahead (purple)");
  const got = collapsedBarColorPercent([five, weekly], "glm", NOW);
  assert.equal(got, fiveCp, "case 3: 5h purple preserved when weekly is fine");
}

// Case 4: 5h ahead (purple) + weekly under red line -> 5h purple wins. The
// weekly warning is shown by the pulsing ring, not by recoloring the bar.
{
  const five = win("5h", 0, { reset_at: FIVE_H_RESET_AHEAD }); // purple
  const weekly = win("weekly", 30);                            // under red line
  const fiveCp = paceColor(five);
  assert.ok(fiveCp > 100, "case 4 precondition: 5h is ahead (purple)");
  assert.equal(underRedLine(weekly), true, "case 4 precondition: weekly under red line");
  const got = collapsedBarColorPercent([five, weekly], "glm", NOW);
  assert.equal(got, fiveCp, "case 4: 5h color wins; weekly handled by pulsing ring");
}

// Case 5: OpenRouter keeps aggregate (ahead wins) even though a non-openrouter
// provider with the same windows would pick the first visible window.
{
  // daily behind (red), monthly ahead (purple) -> aggregate picks purple (ahead).
  const daily = win("daily", 80, { reset_at: "2026-07-12T00:00:00Z" }); // +12h
  const monthly = win("monthly", 0, { reset_at: "2026-07-31T12:00:00Z" }); // +20d
  const monthlyCp = paceColor(monthly, "openrouter");
  assert.ok(monthlyCp > 100, "case 5 precondition: monthly is ahead (purple)");
  const got = collapsedBarColorPercent([daily, monthly], "openrouter", NOW);
  assert.equal(got, monthlyCp, "case 5: OpenRouter aggregate keeps ahead/purple");
}

// Case 6: weekly-only provider under red line -> weekly color (no 5h window,
// so the weekly window is the primary and drives the fill).
{
  const weekly = win("weekly", 30); // under red line, no 5h window present
  assert.equal(underRedLine(weekly), true, "case 6 precondition: weekly under red line");
  const got = collapsedBarColorPercent([weekly], "glm", NOW);
  assert.equal(got, paceColor(weekly), "case 6: weekly-only (no 5h) uses weekly as primary");
}

// Case 7: non-paced window on a non-openrouter provider -> raw remaining fallback.
{
  // "daily" on a non-openrouter provider has no pace (resolveEvenPace -> null),
  // so the fallback returns raw 100 - used_percent.
  const daily = win("daily", 40);
  const got = collapsedBarColorPercent([daily], "glm", NOW);
  assert.equal(got, 60, "case 7: raw-remaining fallback when no pace applies");
}

// Case 8: unlimited primary -> green (100); empty visible -> 100.
{
  const unlimited = win("5h", 0, { is_unlimited: true });
  assert.equal(collapsedBarColorPercent([unlimited], "glm", NOW), 100, "case 8a: unlimited primary -> 100");
  assert.equal(collapsedBarColorPercent([], "glm", NOW), 100, "case 8b: no visible windows -> 100");
}

// Case 9: unlimited weekly (e.g. MiniMax status 3) must NOT trigger the red-line
// override, even when its used_percent would place it under the red tick. The 5h
// window's color wins instead.
{
  const five = win("5h", 0);                                   // green (gradientPercent 100)
  const weeklyUnlimited = win("weekly", 30, { is_unlimited: true }); // remaining 70 < 90 (subTarget 100 - 70/7)
  assert.equal(underRedLine(weeklyUnlimited), true, "case 9 precondition: weekly would-be under red line");
  const got = collapsedBarColorPercent([five, weeklyUnlimited], "minimax", NOW);
  assert.equal(got, paceColor(five, "minimax"), "case 9: unlimited weekly must not override 5h color");
  assert.notEqual(got, paceColor(weeklyUnlimited, "minimax"), "case 9: must not use the unlimited weekly color");
}

console.log("collapsed bar color tests passed");

// ============================================================
// isWeeklyUnderRedLine — drives the pulsing red ring on the mini bar.
// ============================================================
{
  // Under red line -> true.
  const five = win("5h", 0);
  const weekly = win("weekly", 30); // remaining 70 < 90 (subTarget 100 - 70/7)
  assert.equal(underRedLine(weekly), true, "isWeeklyUnderRedLine precondition: weekly under red line");
  assert.equal(isWeeklyUnderRedLine([five, weekly], "glm", NOW), true, "weekly under red line -> true");

  // Above red line -> false.
  const weeklyOk = win("weekly", 5); // remaining 95 > 86.4 (subTarget 100 - 95/7)
  assert.equal(isWeeklyUnderRedLine([five, weeklyOk], "glm", NOW), false, "weekly above red line -> false");

  // OpenRouter never pulses, even with a weekly window under the red line.
  assert.equal(isWeeklyUnderRedLine([five, weekly], "openrouter", NOW), false, "openrouter excluded from pulse");

  // Unlimited weekly (MiniMax status 3) never pulses.
  const weeklyUnlimited = win("weekly", 30, { is_unlimited: true });
  assert.equal(isWeeklyUnderRedLine([five, weeklyUnlimited], "minimax", NOW), false, "unlimited weekly must not pulse");

  // No weekly window -> false.
  assert.equal(isWeeklyUnderRedLine([five], "glm", NOW), false, "no weekly window -> false");

  // Hidden weekly window (bar_visible false) is ignored.
  const hidden = win("weekly", 30, { bar_visible: false });
  assert.equal(isWeeklyUnderRedLine([five, hidden], "glm", NOW), false, "hidden weekly window ignored");
}

console.log("isWeeklyUnderRedLine tests passed");

// ============================================================
// Depleted + near reset: 0% left must pulse even when daysLeft < 1
// (sub-target clamps to 0 and the raw pace comparison misses it).
// ============================================================
{
  // Weekly fully depleted, reset under 1 day away. Live Z.ai shape.
  const weeklyDepletedNearReset = win("weekly", 100, {
    reset_at: "2026-07-11T16:00:00Z", // +4h -> daysLeft < 1; depleted -> quota 0, subTarget = target
  });
  assert.equal(
    isWeeklyWindowUnderRedLine(weeklyDepletedNearReset, "glm", NOW),
    true,
    "depleted weekly near reset must pulse (0% left is always critical)",
  );
  assert.equal(
    isWeeklyUnderRedLine([win("5h", 0), weeklyDepletedNearReset], "glm", NOW),
    true,
    "mini bar pulses when weekly is depleted near reset",
  );
  // Depleted weekly with plenty of time left still pulses (remaining 0 < subTarget).
  const weeklyDepletedFarReset = win("weekly", 100); // +7d -> depleted: subTarget = target = 100
  assert.equal(
    isWeeklyWindowUnderRedLine(weeklyDepletedFarReset, "glm", NOW),
    true,
    "depleted weekly with time left pulses (existing path)",
  );
  // Depleted 5h near reset (< 1h) — same clamp bug on the hourly sub-target.
  const fiveDepletedNearReset = win("5h", 100, {
    reset_at: "2026-07-11T12:30:00Z", // +30m -> hoursLeft < 1; depleted -> quota 0, subTarget = target
  });
  assert.equal(
    isFiveHourWindowUnderRedLine(fiveDepletedNearReset, "glm", NOW),
    true,
    "depleted 5h near reset must pulse (0% left is always critical)",
  );
  assert.equal(
    isFiveHourUnderRedLine([fiveDepletedNearReset], "glm", NOW),
    true,
    "mini bar pulses when 5h is depleted near reset",
  );
  // Unlimited depleted must still NOT pulse (depletion is meaningless for unlimited).
  const weeklyUnlimitedDepleted = win("weekly", 100, { is_unlimited: true });
  assert.equal(
    isWeeklyWindowUnderRedLine(weeklyUnlimitedDepleted, "minimax", NOW),
    false,
    "unlimited depleted weekly must not pulse",
  );
  // OpenRouter depleted must NOT pulse (openrouter excluded from pulse entirely).
  assert.equal(
    isWeeklyWindowUnderRedLine(weeklyDepletedNearReset, "openrouter", NOW),
    false,
    "openrouter depleted must not pulse",
  );
}

console.log("depleted near-reset pulse tests passed");

// ============================================================
// isWeeklyWindowUnderRedLine — drives the pulsing red ring on the
// weekly window's row inside the popup card.
// ============================================================
{
  const five = win("5h", 0);
  const weeklyUnder = win("weekly", 30); // remaining 70 < 90 (subTarget 100 - 70/7)
  const weeklyAbove = win("weekly", 5);  // remaining 95 > 86.4 (subTarget 100 - 95/7)

  assert.equal(isWeeklyWindowUnderRedLine(weeklyUnder, "glm", NOW), true, "weekly window under red line -> true");
  assert.equal(isWeeklyWindowUnderRedLine(weeklyAbove, "glm", NOW), false, "weekly window above red line -> false");
  assert.equal(isWeeklyWindowUnderRedLine(five, "glm", NOW), false, "non-weekly (5h) window -> false");
  assert.equal(isWeeklyWindowUnderRedLine(weeklyUnder, "openrouter", NOW), false, "openrouter window excluded");
  assert.equal(
    isWeeklyWindowUnderRedLine(win("weekly", 30, { is_unlimited: true }), "minimax", NOW),
    false,
    "unlimited weekly window excluded",
  );
}

console.log("isWeeklyWindowUnderRedLine tests passed");

// ============================================================
// isFiveHourWindowUnderRedLine — drives the pulsing red ring on the
// 5h window's row inside the popup card. 5h subTarget with FIVE_H_RESET
// (+5h) is target 100 minus one hour of the available %/hour (remaining/5).
// ============================================================
{
  const fiveUnder = win("5h", 30); // remaining 70 < 86 (100 - 70/5) -> under 5h red line
  const fiveAbove = win("5h", 5);  // remaining 95 > 81 (100 - 95/5) -> above
  const weekly = win("weekly", 30);

  assert.equal(underRedLine(fiveUnder), true, "5h precondition: under its red line");
  assert.equal(isFiveHourWindowUnderRedLine(fiveUnder, "glm", NOW), true, "5h under red line -> true");
  assert.equal(isFiveHourWindowUnderRedLine(fiveAbove, "glm", NOW), false, "5h above red line -> false");
  assert.equal(isFiveHourWindowUnderRedLine(weekly, "glm", NOW), false, "non-5h (weekly) window -> false");
  assert.equal(
    isFiveHourWindowUnderRedLine(win("5h", 30, { is_unlimited: true }), "glm", NOW),
    false,
    "unlimited 5h window excluded",
  );
}

console.log("isFiveHourWindowUnderRedLine tests passed");

// ============================================================
// isFiveHourUnderRedLine — aggregate that drives the pulsing red
// ring on the mini bar when any visible 5h window is under its red
// line. Mirrors isWeeklyUnderRedLine's semantics.
// ============================================================
{
  const five = win("5h", 0);        // remaining 100 > 80 -> above red line
  const fiveUnder = win("5h", 30);  // remaining 70 < 86 -> under red line
  const weeklyUnder = win("weekly", 30); // under weekly red line (no 5h here)

  // 5h under red line -> true.
  assert.equal(isFiveHourUnderRedLine([fiveUnder, win("weekly", 5)], "glm", NOW), true, "5h under red line -> true");

  // 5h above red line -> false.
  assert.equal(isFiveHourUnderRedLine([five, win("weekly", 5)], "glm", NOW), false, "5h above red line -> false");

  // OpenRouter never pulses, even with a 5h window under the red line.
  assert.equal(isFiveHourUnderRedLine([fiveUnder], "openrouter", NOW), false, "openrouter excluded from 5h pulse");

  // Unlimited 5h window never pulses.
  assert.equal(isFiveHourUnderRedLine([win("5h", 30, { is_unlimited: true })], "glm", NOW), false, "unlimited 5h must not pulse");

  // No 5h window -> false (weekly under red line alone does not trigger 5h aggregate).
  assert.equal(isFiveHourUnderRedLine([weeklyUnder], "glm", NOW), false, "no 5h window -> false");

  // Hidden 5h window (bar_visible false) is ignored.
  const hidden = win("5h", 30, { bar_visible: false });
  assert.equal(isFiveHourUnderRedLine([five, hidden], "glm", NOW), false, "hidden 5h window ignored");
}

console.log("isFiveHourUnderRedLine tests passed");
