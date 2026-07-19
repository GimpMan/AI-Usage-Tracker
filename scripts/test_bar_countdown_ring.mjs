import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const overlaySource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);

// --- Bar receives the countdown props ---
const barStart = overlaySource.indexOf("function Bar({");
assert.notEqual(barStart, -1, "Bar component must exist");
const barPropsEnd = overlaySource.indexOf("}) {", barStart);
assert.notEqual(barPropsEnd, -1, "Bar props block must close");
const barProps = overlaySource.slice(barStart, barPropsEnd);
assert.equal(barProps.includes("nowMs"), true, "Bar must receive nowMs");
assert.equal(
  barProps.includes("refreshIntervalSecs"),
  true,
  "Bar must receive refreshIntervalSecs",
);

// --- the countdown math happens inside the Bar body ---
const barBodyEnd = overlaySource.indexOf("// Expanded popup", barStart);
assert.notEqual(barBodyEnd, -1, "Bar body must end before the popup section");
const barBody = overlaySource.slice(barStart, barBodyEnd);
assert.equal(
  barBody.includes("refreshRemainingFraction("),
  true,
  "Bar must compute the ring fraction via refreshRemainingFraction",
);
// The gear button has no hover tooltip, so the Bar does not compute a
// countdown label here. refreshCountdownLabel still runs in the popup.

// --- the gear button renders the ring, update badge stays last ---
const gearStart = overlaySource.indexOf('class="bar-btn bar-btn-settings"');
assert.notEqual(gearStart, -1, "gear button must exist");
const gearEnd = overlaySource.indexOf("</button>", gearStart);
assert.notEqual(gearEnd, -1, "gear button must close");
const gearButton = overlaySource.slice(gearStart, gearEnd);
assert.equal(
  gearButton.includes("RefreshCountdownRing"),
  true,
  "gear button must render the countdown ring",
);
assert.ok(
  gearButton.indexOf("RefreshCountdownRing") < gearButton.indexOf("update-badge"),
  "update badge must stay the last child of the gear button",
);

// --- the <Bar render site passes the countdown props through ---
const barRenderStart = overlaySource.search(/<Bar\r?\n/);
assert.notEqual(barRenderStart, -1, "<Bar render site must exist");
const barRenderEnd = overlaySource.indexOf("/>", barRenderStart);
assert.notEqual(barRenderEnd, -1, "<Bar render site must close");
const barRender = overlaySource.slice(barRenderStart, barRenderEnd);
assert.equal(
  barRender.includes("nowMs={nowMs}"),
  true,
  "<Bar must receive the overlay nowMs tick",
);
assert.equal(
  barRender.includes("refreshIntervalSecs={"),
  true,
  "<Bar must receive a refresh interval",
);

console.log("bar countdown ring tests passed");
