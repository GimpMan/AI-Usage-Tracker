import assert from "node:assert/strict";
import fs from "node:fs";
import * as overlayWidthModule from "../src/overlay-width.ts";
import {
  EXPANDED_PANEL_MAX_WIDTH,
  EXPANDED_PANEL_MIN_WIDTH,
  expandedPanelWidth,
  overlayWindowWidth,
} from "../src/overlay-width.ts";

assert.equal(typeof overlayWidthModule.stableNaturalBarWidth, "function");
const { stableNaturalBarWidth } = overlayWidthModule;
assert.equal(stableNaturalBarWidth(187, false, null), 187);
assert.equal(stableNaturalBarWidth(320, true, 187), 187);
assert.equal(stableNaturalBarWidth(320, true, null), 320);

assert.equal(EXPANDED_PANEL_MIN_WIDTH, 320);
assert.equal(EXPANDED_PANEL_MAX_WIDTH, 420);
assert.equal(expandedPanelWidth(185), 320);
assert.equal(expandedPanelWidth(360), 360);
assert.equal(expandedPanelWidth(600), 420);
assert.equal(overlayWindowWidth(185, false), 185);
assert.equal(overlayWindowWidth(185, true), 320);
assert.equal(overlayWindowWidth(360, true), 360);
assert.equal(overlayWindowWidth(600, true), 600);

const overlaySource = fs.readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);
const overlayRs = fs.readFileSync(
  new URL("../src-tauri/src/overlay.rs", import.meta.url),
  "utf8",
);
const commandsRs = fs.readFileSync(
  new URL("../src-tauri/src/commands.rs", import.meta.url),
  "utf8",
);

assert.match(
  overlaySource,
  /const OVERLAY_PREALLOCATED_W_LOGICAL = 800/,
  "frontend prealloc width must stay in sync with overlay.rs",
);
assert.match(
  overlayRs,
  /OVERLAY_PREALLOCATED_W_LOGICAL: f64 = 800\.0/,
  "native prealloc width must fit a multi-provider minibar",
);
assert.match(
  overlayRs,
  /OVERLAY_PREALLOCATED_H_LOGICAL: f64 = 942\.0/,
  "native prealloc height must fit the tallest popup",
);
assert.match(
  overlayRs,
  /let \(w, h\) = \(OVERLAY_PREALLOCATED_W_LOGICAL, OVERLAY_PREALLOCATED_H_LOGICAL\)/,
  "open_overlay must size the window from the prealloc constants",
);
assert.match(
  commandsRs,
  /const MAX_BAR_W: f64 = 1600\.0/,
  "native width ceiling must allow future providers beyond six trackers",
);

assert.match(overlaySource, /overlayWindowWidth\(measuredBarW, expanded\)/);
assert.match(
  overlaySource,
  /function compactBarWidth\(barEl: HTMLElement\): number[\s\S]*?cloneNode\(true\)[\s\S]*?measuring-compact[\s\S]*?naturalBarWidth\(clone\)[\s\S]*?clone\.remove\(\)/,
);
assert.match(
  overlaySource,
  /const measuredBarW = compactBarWidth\(el\)/,
);
assert.match(
  overlaySource,
  /child\.classList\.contains\("bar-btn-settings"\)\s*\? 0\s*:\s*parseFloat\(childCs\.marginLeft \|\| "0"\) \|\| 0/,
);
assert.match(
  overlaySource,
  /const expanded =\s*activeLabelRef\.current !== null \|\| settingsOpenRef\.current/,
);
assert.match(
  overlaySource,
  /class={`bar compact width-fluid/,
);
assert.doesNotMatch(overlaySource, /barLayoutExpanded/);
assert.doesNotMatch(overlaySource, /requestAnimationFrame\(tick\)/);
assert.match(overlaySource, /el\.style\.width = `\$\{startW\}px`/);
assert.match(overlaySource, /el\.animate\([\s\S]*?width: `\$\{startW\}px`[\s\S]*?width: `\$\{w\}px`/);
assert.match(overlaySource, /await animation\.finished/);
assert.match(
  overlaySource,
  /style\.setProperty\("--expanded-content-width", `\$\{w\}px`\)/,
  "expanded card width must follow the live minibar target",
);

const ensureStart = overlaySource.indexOf("async function ensureNativeReserve(");
const applyOnceStart = overlaySource.indexOf("async function applyBarWidthOnce(");
const applyStart = overlaySource.indexOf("async function applyBarWidth()");
const showStart = overlaySource.indexOf("function showOverlay()");
assert.notEqual(ensureStart, -1, "ensureNativeReserve must exist");
assert.notEqual(applyOnceStart, -1, "applyBarWidthOnce must exist");
assert.notEqual(applyStart, -1, "applyBarWidth must exist");
assert.ok(
  ensureStart < applyOnceStart && applyOnceStart < applyStart && applyStart < showStart,
  "ensureNativeReserve must run before applyBarWidth so the HWND grows first",
);

const ensureSection = overlaySource.slice(ensureStart, applyOnceStart);
const applyBarWidthSection = overlaySource.slice(applyOnceStart, showStart);

assert.match(
  ensureSection,
  /await invoke<void>\("set_overlay_width"/,
  "native reserve must await set_overlay_width before the bar paints",
);
assert.match(
  applyBarWidthSection,
  /await ensureNativeReserve\(pass\)/,
  "bar width application must wait for the native HWND to fit content",
);
assert.doesNotMatch(
  applyBarWidthSection,
  /set_overlay_width/,
  "applyBarWidth must not call set_overlay_width directly (only via ensureNativeReserve)",
);
assert.match(
  ensureSection,
  /nativeReserveWidthRef\.current === reserve/,
  "provider removal must shrink the native reserve to the current minibar width",
);
assert.doesNotMatch(
  ensureSection,
  /reserve <= nativeReserveWidthRef\.current/,
  "the native reserve must not remain grow-only after providers are hidden",
);
assert.match(
  overlaySource,
  /const providerSignature = snaps\s*\.filter\(\(snap\) => barSegmentVisible\(snap, staleThreshold\)\)\s*\.map\(\(snap\) => snap\.provider\)/,
  "width remeasure must key off bar-visible trackers (stale rehydrate → live)",
);
assert.match(
  overlaySource,
  /}, \[providerSignature, activeLabel, settingsOpen\]\);/,
);
assert.doesNotMatch(
  overlaySource,
  /ro\?\.observe\(barEl\);[\s\S]*?}, \[providerSignature, activeLabel, settingsOpen\]\);/,
  "routine ResizeObserver reflows must not drive minibar width",
);
assert.match(
  overlaySource,
  /widthApplyQueuedRef\.current = true/,
  "concurrent applyBarWidth calls must coalesce so a short first pass cannot win",
);
assert.match(
  overlaySource,
  /if \(pass !== widthPassRef\.current\) return/,
  "stale width passes must abort after await",
);
assert.match(
  overlaySource,
  /if \(shownRef\.current\) return;\s*if \(!usageReady\) return/,
  "first show must wait for the initial get_usage attempt",
);
assert.match(
  overlaySource,
  /setUsageReady\(true\)/,
  "pull must mark usage ready so first-show sizing can run",
);

const styles = fs.readFileSync(
  new URL("../src/styles.css", import.meta.url),
  "utf8",
);

assert.match(styles, /\.popup\s*\{[^}]*max-width:\s*420px/);
assert.match(styles, /\.settings-popup\s*\{[^}]*max-width:\s*420px/);
assert.match(
  styles,
  /\.settings-popup\s*\{[^}]*width:\s*var\(--expanded-content-width, 100%\)[^}]*transition:\s*width 280ms/,
  "Settings must shrink smoothly with the minibar instead of being clipped",
);
assert.match(
  applyBarWidthSection,
  /if \(expanded && w > startW\) await setHitWidth\(w\)[\s\S]*?await animation\.finished[\s\S]*?await setHitWidth\(w\)/,
  "expanded hit-region shrink must wait for the width animation",
);
assert.match(
  overlaySource,
  /<div class="bar-spacer" aria-hidden="true" \/>/,
);
assert.match(styles, /\.bar-spacer\s*\{[^}]*display:\s*none/);
assert.doesNotMatch(
  styles,
  /\.widget-content\.is-width-expanded \.bar-spacer\s*\{[^}]*display:\s*block/,
);
assert.match(
  styles,
  /\.bar\.width-fluid \.bar-segment\s*\{[^}]*flex:\s*1 0 auto[^}]*min-width:\s*max-content/,
);
assert.match(
  styles,
  /\.bar\.width-fluid \.bar-track-mini\s*\{[^}]*width:\s*auto[^}]*min-width:\s*36px[^}]*flex:\s*1 1 36px/,
);
assert.doesNotMatch(
  styles,
  /\.bar\.provider-expanded \.bar-segment\.active\s*\{[^}]*flex:\s*1/,
);
assert.doesNotMatch(
  styles,
  /\.bar\.provider-expanded \.bar-track-mini\s*\{[^}]*flex:\s*1/,
);
assert.match(
  styles,
  /\.bar-btn-settings\s*\{[^}]*margin-left:\s*auto[^}]*margin-right:\s*-4px[^}]*flex:\s*0 0 auto/,
);
assert.match(
  styles,
  /\.bar\.measuring-compact \.bar-segment\s*\{[^}]*flex:\s*0 0 auto/,
);
assert.match(
  styles,
  /\.bar\.measuring-compact \.bar-track-mini\s*\{[^}]*width:\s*36px[^}]*flex:\s*0 0 36px/,
);
assert.match(styles, /\.bar\s*\{[^}]*align-self:\s*flex-end/);

console.log("overlay width tests passed");
