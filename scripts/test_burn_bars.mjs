import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  burnBarHeights,
  burnBarTitle,
  bucketSecsForLabel,
  burnedTodayPercent,
  hasBurnHistory,
  recentBurnRatePercentPerMs,
} from "../src/burn-bars.ts";

// ============================================================
// Behavioral — pure helper correctness
// ============================================================

// burnBarHeights scales against a FIXED reference (not the window's own max),
// so a given burn always renders at the same height regardless of what other
// buckets contain. Reference = 4× even-pace burn per bucket (100/60*4 ≈ 6.67).
const varied = [
  { t: 0, burn: 0, reset: false },
  { t: 1, burn: 2, reset: false },
  { t: 2, burn: 4, reset: false },
  { t: 3, burn: 8, reset: false },
  { t: 4, burn: 1, reset: false },
];
const variedHeights = burnBarHeights(varied);
assert.deepEqual(variedHeights, [0, 30, 60, 100, 15]);
assert.equal(variedHeights.length, varied.length, "one height per bucket");

// Stability contract: adding a much larger bucket elsewhere must NOT rescale
// existing buckets. This is the fix for the "bars jump to different sizes"
// flicker that relative-to-max normalization caused.
const withBiggerSpike = [
  { t: 0, burn: 0, reset: false },
  { t: 1, burn: 2, reset: false },
  { t: 2, burn: 4, reset: false },
  { t: 3, burn: 8, reset: false },
  { t: 4, burn: 1, reset: false },
  { t: 5, burn: 50, reset: false }, // huge new spike
];
const withBiggerHeights = burnBarHeights(withBiggerSpike);
// Buckets 0–4 keep exactly the same heights as in `varied`; only the new
// spike clamps to 100.
assert.deepEqual(
  withBiggerHeights.slice(0, 5),
  variedHeights,
  "existing bucket heights must be stable when a larger spike appears elsewhere",
);
assert.equal(withBiggerHeights[5], 100, "a large spike clamps to full height");

// All-zero input → all-zero heights (never NaN, never negative).
const allZero = [
  { t: 0, burn: 0, reset: false },
  { t: 1, burn: 0, reset: false },
  { t: 2, burn: 0, reset: false },
];
const zeroHeights = burnBarHeights(allZero);
assert.deepEqual(zeroHeights, [0, 0, 0]);
for (const h of zeroHeights) {
  assert.equal(Number.isFinite(h), true, "heights must be finite");
}

// Empty input → empty output.
assert.deepEqual(burnBarHeights([]), []);

// Reset-only buckets still produce a height series (the green marker is
// rendered by the component, not by height scaling).
const resetOnly = [
  { t: 0, burn: 0, reset: true },
  { t: 1, burn: 0, reset: false },
];
assert.deepEqual(burnBarHeights(resetOnly), [0, 0]);

// Single-bucket window with a positive burn still scales to 100.
const single = [{ t: 0, burn: 7.5, reset: false }];
assert.deepEqual(burnBarHeights(single), [100]);

// bucketSecsForLabel: weekly = 7d / 60 buckets, 5h = 5h / 60 buckets.
assert.equal(bucketSecsForLabel("weekly", true), (7 * 24 * 3600) / 60);
assert.equal(bucketSecsForLabel("5h", false), (5 * 3600) / 60);
assert.equal(bucketSecsForLabel("wk", true), (7 * 24 * 3600) / 60);

// hasBurnHistory: only true when at least one bucket carries a real signal.
// Fresh installs always have 60 buckets of zeros — those must be treated as
// empty so the popup doesn't show a hollow row.
assert.equal(hasBurnHistory(undefined), false, "undefined is empty");
assert.equal(hasBurnHistory([]), false, "[] is empty");
assert.equal(
  hasBurnHistory(Array.from({ length: 60 }, () => ({ t: 0, burn: 0, reset: false }))),
  false,
  "60 zero-burn buckets with no reset is empty",
);
assert.equal(
  hasBurnHistory([{ t: 0, burn: 0, reset: true }, { t: 1, burn: 0, reset: false }]),
  true,
  "any reset marker counts as history",
);
assert.equal(
  hasBurnHistory([{ t: 0, burn: 0.1, reset: false }]),
  true,
  "any positive burn counts as history",
);
assert.equal(
  hasBurnHistory([{ t: 0, burn: 1.5, reset: false }]),
  true,
  "meaningful burn shows real history",
);

// burnBarTitle: reset buckets show the marker text.
assert.equal(
  burnBarTitle({ t: 0, burn: 0, reset: true }),
  "Window reset here",
);

// burnBarTitle: regular buckets include "% burned" and a localized date.
const title = burnBarTitle({ t: 1752460800, burn: 2.1, reset: false });
assert.equal(title.includes("% burned"), true, "title must include % burned");
assert.equal(title.includes("2.1% burned"), true, "title must format burn %");
// Locale-agnostic date check: prefix before " · " must be non-empty and
// contain either a month abbreviation or a day number.
const datePart = title.split(" · ")[0] ?? "";
assert.notEqual(datePart.length, 0, "title must include a date prefix");
assert.equal(
  /[A-Za-z]|\d/.test(datePart),
  true,
  "title date prefix must include letters or digits",
);

// ============================================================
// Behavioral — burnedTodayPercent ("used today" summary)
// ============================================================

// Fixed local "now" at 15:00; midnight is local-midnight of the same day.
const usedTodayNow = new Date(2026, 6, 18, 15, 0, 0);
const usedTodayMidnight =
  new Date(2026, 6, 18, 0, 0, 0).getTime() / 1000;

// Burn before midnight is ignored; today's buckets sum.
assert.equal(
  burnedTodayPercent(
    [
      { t: usedTodayMidnight - 3600, burn: 50, reset: false },
      { t: usedTodayMidnight + 3600, burn: 10, reset: false },
      { t: usedTodayMidnight + 7200, burn: 5, reset: false },
    ],
    usedTodayNow,
  ),
  15,
  "only buckets since local midnight count toward used-today",
);

// A reset marker zeroes the running sum: burn recorded before it belonged to
// the previous window, so only post-reset burn counts.
assert.equal(
  burnedTodayPercent(
    [
      { t: usedTodayMidnight + 3600, burn: 10, reset: false },
      { t: usedTodayMidnight + 7200, burn: 0, reset: true },
      { t: usedTodayMidnight + 10800, burn: 3, reset: false },
    ],
    usedTodayNow,
  ),
  3,
  "a reset earlier today restarts the used-today sum",
);

// No buckets inside today → null (nothing to say yet).
assert.equal(
  burnedTodayPercent(
    [{ t: usedTodayMidnight - 3600, burn: 10, reset: false }],
    usedTodayNow,
  ),
  null,
  "no buckets today returns null",
);
assert.equal(
  burnedTodayPercent(undefined, usedTodayNow),
  null,
  "undefined history returns null",
);
assert.equal(
  burnedTodayPercent([], usedTodayNow),
  null,
  "empty history returns null",
);

// All-zero buckets today → 0 (an honest "nothing burned today").
assert.equal(
  burnedTodayPercent(
    [{ t: usedTodayMidnight + 3600, burn: 0, reset: false }],
    usedTodayNow,
  ),
  0,
  "zero-burn buckets today sum to 0",
);

// ============================================================
// Behavioral — recentBurnRatePercentPerMs (projection's recent rate)
// ============================================================

const recentNow = Date.parse("2026-07-18T15:00:00.000Z");
const recentHourMs = 60 * 60 * 1000;

// No history at all → null (caller falls back to the period average).
assert.equal(
  recentBurnRatePercentPerMs(undefined, recentHourMs, recentNow),
  null,
  "undefined history returns null",
);
assert.equal(
  recentBurnRatePercentPerMs([], recentHourMs, recentNow),
  null,
  "empty history returns null",
);
assert.equal(
  recentBurnRatePercentPerMs(
    [{ t: recentNow / 1000 - 60, burn: 0, reset: false }],
    recentHourMs,
    recentNow,
  ),
  null,
  "all-zero buckets carry no signal → null (period-average fallback)",
);

// Burn inside the window sums and divides by the window; older burn is out.
{
  const rate = recentBurnRatePercentPerMs(
    [
      { t: recentNow / 1000 - 7200, burn: 10, reset: false }, // 2h ago: outside
      { t: recentNow / 1000 - 3000, burn: 4, reset: false }, // 50m ago
      { t: recentNow / 1000 - 600, burn: 2, reset: false }, // 10m ago
    ],
    recentHourMs,
    recentNow,
  );
  assert.ok(rate !== null && Math.abs(rate - 6 / recentHourMs) < 1e-12,
    `rate must be 6% per hour over the window, got ${rate}`);
}

// A reset marker alone is history (not null) with zero burn → rate 0, so an
// idle stretch reads as "left at reset" instead of a stale "runs out".
assert.equal(
  recentBurnRatePercentPerMs(
    [{ t: recentNow / 1000 - 600, burn: 0, reset: true }],
    recentHourMs,
    recentNow,
  ),
  0,
  "reset marker with no burn returns 0, not null",
);

// Non-positive window is degenerate → null.
assert.equal(
  recentBurnRatePercentPerMs(
    [{ t: recentNow / 1000 - 600, burn: 5, reset: false }],
    0,
    recentNow,
  ),
  null,
  "zero-length window returns null",
);

// ============================================================
// Structural — wiring lives where the spec says it must
// ============================================================

const apiSource = readFileSync(
  new URL("../src/api.ts", import.meta.url),
  "utf8",
);
const typesSource = readFileSync(
  new URL("../src/types.ts", import.meta.url),
  "utf8",
);
const overlaySource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);
const stylesSource = readFileSync(
  new URL("../src/styles.css", import.meta.url),
  "utf8",
);

// src/api.ts: getBurnHistory wrapper wired to the "get_burn_history" command.
assert.equal(
  apiSource.includes('"get_burn_history"'),
  true,
  'src/api.ts must invoke the "get_burn_history" command',
);
assert.equal(
  apiSource.includes("getBurnHistory"),
  true,
  "src/api.ts must export getBurnHistory",
);
assert.equal(
  apiSource.includes("ProviderBurnHistory"),
  true,
  "src/api.ts must import ProviderBurnHistory",
);

// src/types.ts: contract types for the wire payload.
assert.equal(
  typesSource.includes("export interface BurnBucket"),
  true,
  "src/types.ts must export BurnBucket",
);
assert.equal(
  typesSource.includes("export interface WindowBurn"),
  true,
  "src/types.ts must export WindowBurn",
);
assert.equal(
  typesSource.includes("export interface ProviderBurnHistory"),
  true,
  "src/types.ts must export ProviderBurnHistory",
);

// src/overlay.tsx: imports burnBarHeights from ./burn-bars.ts and renders
// the <BurnBars> component inside the popup. overlay.tsx is CRLF, so anchor
// with \r?\n in case the next lines are LF.
assert.equal(
  /from\s+["']\.\/burn-bars\.ts["']/.test(overlaySource),
  true,
  "src/overlay.tsx must import helpers from ./burn-bars.ts",
);
assert.equal(
  /from\s+["']\.\/burn-bars\.ts["'][\s\S]*?\bburnBarHeights\b/.test(
    overlaySource,
  ),
  true,
  "src/overlay.tsx must import burnBarHeights from ./burn-bars.ts",
);
assert.equal(
  /<BurnBars\b/.test(overlaySource),
  true,
  "src/overlay.tsx must render <BurnBars in the popup",
);
// Both Popup sites (hidden measure-layer clone + real popup) must receive
// the burnLookup prop so the offscreen clone never inflates the window with
// bars the real popup will not show, and vice versa.
const popupSiteMatches = overlaySource.match(/<Popup\b/g) ?? [];
assert.ok(
  popupSiteMatches.length >= 2,
  `overlay.tsx must mount <Popup at least twice (measure clone + real), found ${popupSiteMatches.length}`,
);
assert.equal(
  (overlaySource.match(/\bburnLookup=\{burnLookup\}/g) ?? []).length >= 2,
  true,
  "both Popup sites must receive burnLookup",
);
// Render guard: BurnBars appears AFTER popup-section-foot and BEFORE the pace
// notes dropdown (which is the only pace-notes container left in the popup).
const burnBarsIdx = overlaySource.search(/<BurnBars\b/);
const popupSectionFootIdx = overlaySource.lastIndexOf(
  'class="popup-section-foot"',
  burnBarsIdx,
);
// PaceNotesDropdown is rendered as a JSX value, not a string `class=`.
const paceDropdownIdx = overlaySource.indexOf("<PaceNotesDropdown", burnBarsIdx);
assert.notEqual(burnBarsIdx, -1, "<BurnBars must be rendered");
assert.notEqual(popupSectionFootIdx, -1, "popup-section-foot must exist");
assert.notEqual(paceDropdownIdx, -1, "<PaceNotesDropdown must be rendered");
assert.ok(
  popupSectionFootIdx < burnBarsIdx,
  "<BurnBars must render after popup-section-foot",
);
assert.ok(
  burnBarsIdx < paceDropdownIdx,
  "<BurnBars must render before the pace-notes dropdown",
);
// pull() must call getBurnHistory without letting its failure fail get_usage.
assert.equal(
  overlaySource.includes("getBurnHistory()"),
  true,
  "pull() must call getBurnHistory()",
);
assert.equal(
  /getBurnHistory\(\)\.catch\(/.test(overlaySource),
  true,
  "getBurnHistory must be wrapped in .catch so it cannot fail get_usage",
);
// burnLookup must be memoized (useMemo) so per-render churn doesn't rebuild it.
assert.equal(
  /useMemo\(\s*\(\)\s*=>\s*\{[\s\S]*?new Map<string, Map<string, BurnBucket\[\]>>/.test(
    overlaySource,
  ),
  true,
  "burnLookup must be memoized with useMemo",
);
// burn bars limited to weekly + 5h windows with bar_visible.
assert.equal(
  overlaySource.includes("isWeeklyWindow(w.label)") &&
    overlaySource.includes("isFiveHourWindow(w.label)"),
  true,
  "burn bars must be gated on weekly + 5h window labels",
);
// Burn-bar block is fully suppressed when the backend has no real history yet
// so the popup doesn't reserve vertical space for an empty track.
assert.equal(
  overlaySource.includes("hasBurnHistory("),
  true,
  "overlay must guard the burn-bar block with hasBurnHistory",
);

// src/styles.css: rules for the bars, reset marker, and caption.
assert.equal(
  stylesSource.includes(".burn-bars {") &&
    /display:\s*flex;/.test(stylesSource),
  true,
  "styles.css must define .burn-bars as a flex row",
);
assert.equal(
  stylesSource.includes(".burn-bar-reset {") &&
    stylesSource.includes(".burn-bar-reset::after {") &&
    stylesSource.includes("var(--fg-bright)"),
  true,
  "styles.css must define .burn-bar-reset as a thin white marker line",
);
assert.equal(
  stylesSource.includes(".burn-bars-caption {"),
  true,
  "styles.css must define .burn-bars-caption",
);

// "Used today" summary: the dropdown's collapsed row reads "More details" and
// the expanded body carries the usedTodayNote line for weekly windows.
assert.equal(
  overlaySource.includes(">More details</span>"),
  true,
  'pace-notes toggle must read "More details"',
);
assert.equal(
  overlaySource.includes("burnedTodayPercent("),
  true,
  "overlay must compute used-today via burnedTodayPercent",
);
assert.equal(
  overlaySource.includes("usedTodayNote={"),
  true,
  "PaceNotesDropdown must receive usedTodayNote",
);
assert.equal(
  overlaySource.includes("% of weekly quota burned today"),
  true,
  "used-today line must read '% of weekly quota burned today'",
);

console.log("burn bars tests passed");
