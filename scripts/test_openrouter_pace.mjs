import assert from "node:assert/strict";
import {
  openrouterEvenPace,
  resolveEvenPace,
} from "../src/weekly-pace.ts";

const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;
const closeTo = (actual, expected) =>
  assert.ok(Math.abs(actual - expected) < 0.05, `${actual} != ${expected}`);

// --- daily ---
// now = midday; daily resets at next 00:00 UTC = 12h away.
const dailyNow = Date.parse("2026-07-11T12:00:00.000Z");
const dailyReset = "2026-07-12T00:00:00.000Z";
const daily = openrouterEvenPace(
  { label: "daily", used_percent: 40, reset_at: dailyReset, bar_visible: true },
  dailyNow,
);
assert.ok(daily, "daily window should produce pace");
closeTo(daily.remainingPercent, 60);
// 12h left of 24h → 50% even-pace target.
closeTo(daily.targetRemainingPercent, 50);
// sub-target = blue target minus one hour of the available %/hour (60% / 12h).
closeTo(daily.subTargetRemainingPercent, 50 - 60 / 12);
assert.equal(daily.tickPercentages.length, 5);
closeTo(daily.tickPercentages[0], (4 / 24) * 100);
closeTo(daily.tickPercentages[4], (20 / 24) * 100);
assert.equal(daily.targetLabel, "daily");
assert.equal(daily.subTargetKind, "hourly");
// 60% left, 12h left → 5.0%/hour; 60 vs target 50 → 10% ahead.
assert.equal(daily.note, "~5.0%/hour available until reset");
assert.equal(daily.gapNote, "10.0% ahead of even pace");
// 10% of a 24h period = 2.4h → 2h 24m ahead of even pace
assert.equal(daily.timeNote, "~2h 24m ahead of even pace");
// 40% used in the first 12h → burns another 40% → 20% left at reset.
assert.equal(daily.projectionNote, "Projection: ~20% left at reset");

// 90% used in 12h → the last 10% runs out 1h 20m in, 10h 40m before reset.
const dailyRunsOut = openrouterEvenPace(
  { label: "daily", used_percent: 90, reset_at: dailyReset, bar_visible: true },
  dailyNow,
);
assert.ok(dailyRunsOut);
assert.equal(
  dailyRunsOut.projectionNote,
  "Projection: runs out in ~1h 20m — 10h 40m before reset",
);

// Period just started (reset exactly 24h away) → no projection line.
const dailyJustReset = openrouterEvenPace(
  { label: "daily", used_percent: 0, reset_at: dailyReset, bar_visible: true },
  Date.parse("2026-07-11T00:00:00.000Z"),
);
assert.ok(dailyJustReset);
assert.equal(dailyJustReset.projectionNote, "");

// --- monthly ---
// July has 31 days; reset is 2026-08-01T00:00:00Z.
const monthlyNow = Date.parse("2026-07-11T12:00:00.000Z");
const monthlyReset = "2026-08-01T00:00:00.000Z";
const monthly = openrouterEvenPace(
  { label: "monthly", used_percent: 30, reset_at: monthlyReset, bar_visible: true },
  monthlyNow,
);
assert.ok(monthly);
closeTo(monthly.remainingPercent, 70);
const msLeft = Date.parse(monthlyReset) - monthlyNow;
closeTo(monthly.targetRemainingPercent, (msLeft / (31 * DAY_MS)) * 100);
// sub-target = blue target minus one day of the available %/day (70% / 20.5d).
closeTo(
  monthly.subTargetRemainingPercent,
  monthly.targetRemainingPercent - 70 / (msLeft / DAY_MS),
);
assert.equal(monthly.tickPercentages.length, 5);
closeTo(monthly.tickPercentages[0], (1 / 6) * 100);
closeTo(monthly.tickPercentages[4], (5 / 6) * 100);
assert.equal(monthly.targetLabel, "monthly");
assert.equal(monthly.subTargetKind, "daily");
assert.ok(
  monthly.note.startsWith("~") && monthly.note.includes("%/day"),
  "monthly note should quote %/day",
);
assert.ok(
  typeof monthly.timeNote === "string" && monthly.timeNote.length > 0,
  "monthly should expose a time note",
);
// 30% used in the elapsed 10.5d of July → ~11% left at reset.
assert.equal(monthly.projectionNote, "Projection: ~11% left at reset");

// 95% used in 10.5d → the last 5% runs out ~13h 16m in, 19d 22h before reset.
const monthlyRunsOut = openrouterEvenPace(
  { label: "monthly", used_percent: 95, reset_at: monthlyReset, bar_visible: true },
  monthlyNow,
);
assert.ok(monthlyRunsOut);
assert.equal(
  monthlyRunsOut.projectionNote,
  "Projection: runs out in ~13h 16m — 19d 22h before reset",
);

// --- null cases ---
const pastWindow = {
  label: "daily",
  used_percent: 40,
  reset_at: "2026-07-10T00:00:00Z",
  bar_visible: true,
};
assert.equal(
  openrouterEvenPace(
    { label: "weekly", used_percent: 40, reset_at: dailyReset, bar_visible: true },
    dailyNow,
  ),
  null,
  "weekly is not handled here",
);
assert.equal(
  openrouterEvenPace(
    { label: "balance $12.00", used_percent: 40, reset_at: dailyReset, bar_visible: true },
    dailyNow,
  ),
  null,
  "balance is not paced",
);
assert.equal(
  openrouterEvenPace(
    { label: "daily", used_percent: 40, reset_at: null, bar_visible: true },
    dailyNow,
  ),
  null,
  "missing reset_at → null",
);
assert.equal(openrouterEvenPace(pastWindow, dailyNow), null, "past reset → null");
assert.equal(
  openrouterEvenPace(
    { label: "daily", used_percent: NaN, reset_at: dailyReset, bar_visible: true },
    dailyNow,
  ),
  null,
  "non-finite used_percent → null",
);
assert.equal(
  openrouterEvenPace(
    { label: "daily", used_percent: 40, reset_at: "garbage", bar_visible: true },
    dailyNow,
  ),
  null,
  "unparseable reset_at → null",
);

// --- resolveEvenPace routing ---
const win = (label) => ({
  label,
  used_percent: 40,
  reset_at: dailyReset,
  bar_visible: true,
});
assert.ok(resolveEvenPace(win("5h"), "GLM", dailyNow), "5h is label-driven");
assert.ok(resolveEvenPace(win("weekly"), "GLM", dailyNow), "weekly is label-driven");
assert.ok(
  resolveEvenPace(win("daily"), "OpenRouter", dailyNow),
  "daily for OpenRouter is paced",
);
assert.ok(
  resolveEvenPace(win("monthly"), "OpenRouter", dailyNow),
  "monthly for OpenRouter is paced",
);
assert.ok(
  resolveEvenPace(win("monthly"), "Grok", dailyNow),
  "monthly for Grok is paced",
);
assert.equal(
  resolveEvenPace(win("monthly"), "GLM", dailyNow),
  null,
  "GLM monthly must NOT be paced",
);
assert.equal(
  resolveEvenPace(win("daily"), "GLM", dailyNow),
  null,
  "daily for non-OpenRouter is not paced",
);

console.log("openrouter pace tests passed");
