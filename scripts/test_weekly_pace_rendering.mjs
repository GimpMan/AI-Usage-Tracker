import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const overlaySource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);

// --- collapsed mini track must NOT render pace markers ---
const miniTrackStart = overlaySource.indexOf('<div class="bar-track-mini">');
const miniTrackEnd = overlaySource.indexOf(
  '<div class="bar-segment-text">',
  miniTrackStart,
);
const miniTrack = overlaySource.slice(miniTrackStart, miniTrackEnd);

assert.notEqual(miniTrackStart, -1, "collapsed mini track must exist");
assert.notEqual(miniTrackEnd, -1, "collapsed mini track boundary must exist");
assert.equal(
  miniTrack.includes('class="bar-pace-target"'),
  false,
  "collapsed mini track must not render a pace marker",
);

// --- expanded popup track MUST render pace markers via EvenPace ---
const popupTrackStart = overlaySource.indexOf('<div class="bar-track-full">');
const popupTrackEnd = overlaySource.indexOf(
  '<div class="popup-section-foot">',
  popupTrackStart,
);
const popupTrack = overlaySource.slice(popupTrackStart, popupTrackEnd);

assert.notEqual(popupTrackStart, -1, "expanded popup track must exist");
assert.notEqual(popupTrackEnd, -1, "expanded popup track boundary must exist");
assert.equal(
  popupTrack.includes('class="bar-pace-target"'),
  true,
  "expanded popup track must retain its pace marker",
);

// --- overlay routes all pace through resolveEvenPace / EvenPace ---
assert.equal(
  overlaySource.includes("resolveEvenPace"),
  true,
  "overlay must route windows through resolveEvenPace",
);
assert.equal(
  overlaySource.includes("evenPace"),
  true,
  "overlay must consume the EvenPace shape",
);
assert.equal(
  overlaySource.includes("evenPace?.targetRemainingPercent") ||
    overlaySource.includes("evenPace.targetRemainingPercent"),
  true,
  "overlay must read the blue target from EvenPace",
);
assert.equal(
  overlaySource.includes("evenPace.gapNote"),
  true,
  "overlay must render the pace gap % note on popup cards",
);
assert.equal(
  overlaySource.includes("evenPace.timeNote"),
  true,
  "overlay must render the pace time note on popup cards",
);
assert.equal(
  overlaySource.includes('class="weekly-pace-time"'),
  true,
  "overlay must use weekly-pace-time for the recovery/headroom line",
);

// --- pace notes are collapsed into a dropdown so they don't dominate the popup ---
assert.equal(
  overlaySource.includes("pace-notes-dropdown"),
  true,
  "overlay must wrap pace notes in a dropdown container",
);
assert.equal(
  overlaySource.includes("pace-notes-toggle"),
  true,
  "overlay must expose a pace-notes toggle button",
);
assert.equal(
  overlaySource.includes("pace-notes-body"),
  true,
  "pace notes body is rendered only when the dropdown is open",
);
assert.equal(
  overlaySource.includes("PaceNotesDropdown"),
  true,
  "overlay must define a PaceNotesDropdown component",
);
assert.equal(
  /\<PaceNotesDropdown\b/.test(overlaySource),
  true,
  "overlay must render <PaceNotesDropdown in the popup",
);
// Sanity: the toggle wraps everything in a real <button> with a closing tag.
assert.equal(
  /pace-notes-toggle[\s\S]*?\<\/button\>/.test(overlaySource),
  true,
  "pace notes toggle must be a real button element with proper closing tag",
);
// The pace notes block in the popup window section must be fully delegated
// to the dropdown — no raw pace-note divs should leak next to the bars.
assert.equal(
  /\{evenPace && \(\s*\<\s*PaceNotesDropdown[\s\S]*?\/>\s*\)\}/.test(
    overlaySource,
  ),
  true,
  "evenPace must render through <PaceNotesDropdown> only (no raw note divs)",
);

// --- collapsed provider tracks still include pace-relative color ---
assert.equal(
  overlaySource.includes("fillColor(sum.colorPercent)"),
  true,
  "collapsed provider tracks must include their highest visible pace gradient",
);

// --- the branched weekly/5h pace code is gone ---
assert.equal(
  overlaySource.includes("fiveHourPaceGradientPercent"),
  false,
  "overlay must no longer call fiveHourPaceGradientPercent directly",
);
assert.equal(
  overlaySource.includes("paceTargetLabel"),
  false,
  "overlay must no longer use the old paceTargetLabel variable",
);

// --- CSS for ticks/targets is unchanged ---
const stylesSource = readFileSync(
  new URL("../src/styles.css", import.meta.url),
  "utf8",
);
const tickStyleStart = stylesSource.indexOf(".bar-pace-tick {");
const tickStyleEnd = stylesSource.indexOf("}", tickStyleStart);
const tickStyle = stylesSource.slice(tickStyleStart, tickStyleEnd);
const targetStyleStart = stylesSource.indexOf(".bar-pace-target {");
const targetStyleEnd = stylesSource.indexOf("}", targetStyleStart);
const targetStyle = stylesSource.slice(targetStyleStart, targetStyleEnd);

assert.notEqual(tickStyleStart, -1, "tick styles must exist");
assert.notEqual(targetStyleStart, -1, "target styles must exist");
assert.equal(tickStyle.includes("top: -1px"), true);
assert.equal(tickStyle.includes("bottom: -1px"), true);
assert.equal(tickStyle.includes("width: 2px"), true);
assert.equal(tickStyle.includes("margin-left: -1px"), true);
assert.equal(tickStyle.includes("background: var(--fg-bright)"), true);
assert.equal(targetStyle.includes("background: #22d3ee"), true);
assert.equal(
  targetStyle.includes("0 0 6px 2px rgba(34, 211, 238, 0.9)"),
  true,
);
assert.equal(targetStyle.includes("width: 4px"), true);
assert.equal(targetStyle.includes("margin-left: -2px"), true);
assert.equal(targetStyle.includes("z-index: 2"), true);
assert.equal(tickStyle.includes("z-index: 1"), true);

// --- burn bars are tall enough to read the weekly/5h burn distribution ---
const burnBarsStart = stylesSource.indexOf(".burn-bars {");
const burnBarsEnd = stylesSource.indexOf("}", burnBarsStart);
assert.notEqual(burnBarsStart, -1, ".burn-bars rule must exist in styles.css");
const burnBarsDecl = stylesSource.slice(burnBarsStart, burnBarsEnd);
const burnBarsHeightMatch = burnBarsDecl.match(/height:\s*(\d+)px/);
assert.ok(
  burnBarsHeightMatch,
  ".burn-bars must declare a numeric pixel height",
);
const burnBarsHeight = Number(burnBarsHeightMatch[1]);
assert.ok(
  burnBarsHeight >= 32,
  `.burn-bars height must be at least 32px (got ${burnBarsHeight}px)`,
);

console.log("weekly pace rendering tests passed");
