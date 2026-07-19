import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const overlaySource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);
const stylesSource = readFileSync(
  new URL("../src/styles.css", import.meta.url),
  "utf8",
);

// --- overlay listens for the backend reset event ---
assert.equal(
  overlaySource.includes('"quota-window-reset"'),
  true,
  "overlay must listen for the quota-window-reset event",
);
assert.equal(
  overlaySource.includes("resetFlash"),
  true,
  "overlay must track reset-flash state",
);

// --- ... and passes the flashing set down to the collapsed bar ---
const barRenderStart = overlaySource.search(/<Bar\r?\n/);
assert.notEqual(barRenderStart, -1, "<Bar render site must exist");
const barRenderEnd = overlaySource.indexOf("/>", barRenderStart);
assert.notEqual(barRenderEnd, -1, "<Bar render site must close");
const barRender = overlaySource.slice(barRenderStart, barRenderEnd);
assert.equal(
  barRender.includes("resetFlash={resetFlash}"),
  true,
  "<Bar must receive the resetFlash set",
);

// --- bar segment carries the reset-flash class while active ---
const segmentStart = overlaySource.indexOf("bar-segment ${isActive");
assert.notEqual(segmentStart, -1, "bar segment className must exist");
const segmentEnd = overlaySource.indexOf("`", segmentStart);
assert.notEqual(segmentEnd, -1, "bar segment className must close");
const segmentClass = overlaySource.slice(segmentStart, segmentEnd);
assert.equal(
  segmentClass.includes('resetFlash.has(id) ? "reset-flash"'),
  true,
  "bar segment must gain the reset-flash class for flashing providers",
);

// --- styles define the green flash animation ---
assert.equal(
  stylesSource.includes("@keyframes reset-flash-pulse"),
  true,
  "styles.css must define the reset-flash-pulse keyframes",
);
assert.equal(
  stylesSource.includes(".bar-segment.reset-flash"),
  true,
  "styles.css must style .bar-segment.reset-flash",
);
assert.equal(
  stylesSource.includes("reset-flash-pulse 2.8s ease-out 1"),
  true,
  "the flash must be a one-shot 2.8s ease-out animation",
);

console.log("reset flash tests passed");
