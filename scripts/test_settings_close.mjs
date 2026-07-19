import assert from "node:assert/strict";
import { reduceSettingsClose } from "../src/settings-close.ts";

let phase = "open";

let effect = reduceSettingsClose(phase, "close-requested");
assert.deepEqual(effect, {
  phase: "closing",
  startAnimation: true,
  unmount: false,
});
phase = effect.phase;

// DOM blur and Tauri focus-loss can arrive for the same click. The second
// notification must not restart the animation or schedule another close.
effect = reduceSettingsClose(phase, "close-requested");
assert.deepEqual(effect, {
  phase: "closing",
  startAnimation: false,
  unmount: false,
});
phase = effect.phase;

effect = reduceSettingsClose(phase, "animation-finished");
assert.deepEqual(effect, {
  phase: "closed",
  startAnimation: false,
  unmount: true,
});
phase = effect.phase;

// A stale timer callback after unmount must be harmless.
assert.deepEqual(reduceSettingsClose(phase, "animation-finished"), {
  phase: "closed",
  startAnimation: false,
  unmount: false,
});

// A fresh open starts a new lifecycle.
assert.deepEqual(reduceSettingsClose(phase, "opened"), {
  phase: "open",
  startAnimation: false,
  unmount: false,
});

console.log("settings close lifecycle tests passed");
