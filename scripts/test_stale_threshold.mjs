import assert from "node:assert/strict";
import { isStaleSnapshot, staleThresholdMs } from "../src/stale-snapshot.ts";

// Floor stays at 2 minutes for short intervals (startup rehydrate hide).
assert.equal(staleThresholdMs(30), 120_000);
assert.equal(staleThresholdMs(60), 150_000); // 60s + 90s grace
assert.equal(staleThresholdMs(90), 180_000);

// Longer refresh intervals must clear the old fixed 120s cliff.
assert.equal(staleThresholdMs(120), 210_000); // 2m + grace
assert.equal(staleThresholdMs(180), 270_000);
assert.equal(staleThresholdMs(300), 390_000);

// A snapshot aged just under one full 2m cycle must stay visible.
const now = Date.parse("2026-07-12T12:00:00.000Z");
const ageAlmost2m = {
  provider: "Codex",
  level: null,
  windows: [{ label: "5h", used_percent: 10, reset_at: null, bar_visible: true, is_unlimited: false }],
  unavailable_reason: null,
  fetched_at: new Date(now - 119_000).toISOString(),
};
assert.equal(
  isStaleSnapshot(ageAlmost2m, staleThresholdMs(120), now),
  false,
  "2m interval: 119s-old snapshot must not hide",
);

// Same age would have been hidden under the old fixed 120s threshold — prove
// the interval-aware threshold is what saves it past 120s.
assert.equal(
  isStaleSnapshot(
    { ...ageAlmost2m, fetched_at: new Date(now - 125_000).toISOString() },
    staleThresholdMs(120),
    now,
  ),
  false,
  "2m interval: 125s-old snapshot stays until interval+grace",
);

// Still hide true startup rehydrate (hours old).
assert.equal(
  isStaleSnapshot(
    { ...ageAlmost2m, fetched_at: new Date(now - 3_600_000).toISOString() },
    staleThresholdMs(120),
    now,
  ),
  true,
);

// Invalid / NaN fetched_at is always stale.
assert.equal(
  isStaleSnapshot({ ...ageAlmost2m, fetched_at: "not-a-date" }, staleThresholdMs(60), now),
  true,
);

console.log("stale threshold tests passed");
