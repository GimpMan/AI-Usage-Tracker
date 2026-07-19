import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const overlaySource = readFileSync(
  new URL("../src-tauri/src/overlay.rs", import.meta.url),
  "utf8",
);

const overlayBuilderStart = overlaySource.indexOf(
  'WebviewWindowBuilder::new(app, OVERLAY_LABEL',
);
assert.notEqual(overlayBuilderStart, -1, "overlay window builder must exist");

// Isolate the overlay builder chain (up to its `.build()?`) before asserting.
const overlayBuilderEnd = overlaySource.indexOf(".build()?", overlayBuilderStart);
assert.notEqual(overlayBuilderEnd, -1, "overlay builder must end with .build()?");
const overlayBuilder = overlaySource.slice(overlayBuilderStart, overlayBuilderEnd);

assert.equal(
  overlayBuilder.includes(".always_on_top(true)"),
  true,
  "the tracker overlay must stay above other windows",
);

console.log("overlay always-on-top test passed");
