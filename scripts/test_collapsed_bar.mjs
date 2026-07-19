import assert from "node:assert/strict";
import { collapsedBarRemaining } from "../src/bar-summary.ts";

const window = (label, used_percent, bar_visible = true) => ({
  label,
  used_percent,
  reset_at: null,
  bar_visible,
});

assert.equal(
  collapsedBarRemaining([
    window("5h", 0),
    window("weekly", 39),
    window("monthly", 88, false),
  ]),
  100,
  "the 5h window must drive GLM's collapsed bar",
);

assert.equal(
  collapsedBarRemaining([window("weekly", 37)]),
  63,
  "weekly-only providers must still get a collapsed fill",
);

assert.equal(
  collapsedBarRemaining([window("5h", 140), window("weekly", -10)]),
  0,
  "remaining percentage must be clamped",
);

console.log("collapsed bar tests passed");
