import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const overlaySource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);
const weeklyPaceSource = readFileSync(
  new URL("../src/weekly-pace.ts", import.meta.url),
  "utf8",
);

// --- weekly-pace.ts exposes the projection formatter + EvenPace field ---
assert.equal(
  weeklyPaceSource.includes("export function formatProjectionNote"),
  true,
  "weekly-pace.ts must export formatProjectionNote",
);
assert.equal(
  weeklyPaceSource.includes("projectionNote: string"),
  true,
  "EvenPace must declare projectionNote",
);

// --- PaceNotesDropdown body renders the projection after the time note ---
const paceLinesStart = overlaySource.indexOf("function PaceNotesDropdown");
assert.notEqual(paceLinesStart, -1, "PaceNotesDropdown component must exist");
const paceLinesEnd = overlaySource.indexOf("function BurnBars", paceLinesStart);
assert.notEqual(paceLinesEnd, -1, "PaceNotesDropdown body must close");
const paceLines = overlaySource.slice(paceLinesStart, paceLinesEnd);

assert.equal(
  paceLines.includes("{gapNote"),
  true,
  "pace-notes body must render the gap note",
);
assert.equal(
  paceLines.includes("{timeNote"),
  true,
  "pace-notes body must render the time note",
);
assert.equal(
  paceLines.includes("{projectionNote"),
  true,
  "pace-notes body must render the projection note",
);
assert.equal(
  paceLines.includes('class="weekly-pace-note"'),
  true,
  "projection note reuses the weekly-pace-note style",
);
// The projection line renders after the time note (fourth line).
assert.ok(
  paceLines.indexOf("{projectionNote") > paceLines.indexOf("{timeNote"),
  "projection note must render after the time note",
);

console.log("projection rendering tests passed");
