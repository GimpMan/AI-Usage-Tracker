import assert from "node:assert/strict";
import {
  normalizeRefreshIntervalSecs,
  refreshCountdownLabel,
  refreshRemainingFraction,
  refreshRingDash,
} from "../src/refresh-countdown.ts";

const now = Date.parse("2026-07-12T12:00:00.000Z");
const interval = 120; // 2 minutes

assert.equal(normalizeRefreshIntervalSecs(120), 120);
assert.equal(normalizeRefreshIntervalSecs("90"), 90);
assert.equal(normalizeRefreshIntervalSecs(0), null);
assert.equal(normalizeRefreshIntervalSecs(-1), null);

// Just refreshed → full ring.
assert.equal(
  refreshRemainingFraction(new Date(now).toISOString(), interval, now),
  1,
);

// Halfway through a 2m interval.
assert.equal(
  refreshRemainingFraction(new Date(now - 60_000).toISOString(), interval, now),
  0.5,
);

// Same age against a 60s setting → empty (linked to live setting).
assert.equal(
  refreshRemainingFraction(new Date(now - 60_000).toISOString(), 60, now),
  0,
);

// Due / overdue → empty.
assert.equal(
  refreshRemainingFraction(new Date(now - 120_000).toISOString(), interval, now),
  0,
);
assert.equal(
  refreshRemainingFraction(new Date(now - 200_000).toISOString(), interval, now),
  0,
);

// Staggered providers: two seconds offset should keep rings slightly apart.
const a = refreshRemainingFraction(new Date(now - 60_000).toISOString(), interval, now);
const b = refreshRemainingFraction(new Date(now - 62_000).toISOString(), interval, now);
assert.ok(b < a, "later-fetched provider should have less remaining");

assert.equal(refreshRingDash(0.5), 0.5);
assert.equal(refreshRingDash(1.2), 1);
assert.equal(refreshRingDash(-0.1), 0);

assert.match(
  refreshCountdownLabel(new Date(now - 30_000).toISOString(), interval, now),
  /Next refresh in 1m 30s|Next refresh in 1m 3\ds/,
);
assert.equal(
  refreshCountdownLabel(new Date(now - 150_000).toISOString(), interval, now),
  "Refresh due",
);

// Mid-cycle Settings change: label notes pending interval after next refresh.
assert.match(
  refreshCountdownLabel(
    new Date(now - 15_000).toISOString(),
    30, // current cycle still on 30s
    now,
    120, // Settings now 2m
  ),
  /Next refresh in 15s · new interval \(2m 00s\) after next refresh/,
);

console.log("refresh countdown tests passed");
