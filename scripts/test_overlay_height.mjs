import assert from "node:assert/strict";
import fs from "node:fs";
import {
  SETTINGS_PANEL_FALLBACK_HEIGHT,
  settingsPanelMaxHeight,
} from "../src/overlay-height.ts";

assert.equal(SETTINGS_PANEL_FALLBACK_HEIGHT, 640);
assert.equal(settingsPanelMaxHeight(1040, 1, 24), 994);
assert.equal(settingsPanelMaxHeight(1040, 1.5, 24), 647);
assert.equal(settingsPanelMaxHeight(1040, 1, 24, 942), 896);
assert.equal(settingsPanelMaxHeight(1040, 1.5, 24, 900), 554);
assert.equal(settingsPanelMaxHeight(300, 1, 24), 320);
assert.equal(settingsPanelMaxHeight(1040, 0, 24), 640);
assert.equal(settingsPanelMaxHeight(Number.NaN, 1, 24), 640);

const overlaySource = fs.readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);
const styles = fs.readFileSync(
  new URL("../src/styles.css", import.meta.url),
  "utf8",
);

assert.match(overlaySource, /currentMonitor\(\)/);
assert.match(overlaySource, /monitorFromPoint\(/);
assert.match(overlaySource, /currentWindow\.outerPosition\(\)/);
assert.match(overlaySource, /currentWindow\.outerSize\(\)/);
assert.match(overlaySource, /settingsPanelMaxHeight\(/);
assert.match(overlaySource, /settingsPanelMaxHeight\([\s\S]{0,220}size\.height/);
assert.match(overlaySource, /class="settings-scroll-content"/);
assert.match(overlaySource, /can-scroll-up/);
assert.match(overlaySource, /can-scroll-down/);
assert.match(overlaySource, /settings-scroll-position/);
assert.match(overlaySource, /scrollPosition\.size/);
assert.match(overlaySource, /scrollPosition\.offset/);
assert.match(overlaySource, /onScroll=\{updateScrollCues\}/);
assert.match(overlaySource, /el\.scrollTop = 0;/);
assert.match(overlaySource, /setHitHeight\(contentStripH\(visiblePopupH\)\)/);
assert.match(overlaySource, /setHitHeight\(contentStripH\(h\)\)/);
assert.match(overlaySource, /ro\?\.observe\(el\);[\s\S]{0,300}onResize\(\);/);

assert.match(styles, /\.settings-scroll-content\s*\{[^}]*overflow-y:\s*auto/);
assert.match(styles, /\.settings-popup\s*\{[^}]*\n\s+height:\s*var\(--settings-max-height/);
assert.match(styles, /\.settings-popup\s*\{[^}]*min-height:\s*0/);
assert.match(styles, /\.settings-popup\s*\{[^}]*flex:\s*0 0 var\(--settings-max-height/);
assert.match(styles, /scrollbar-width:\s*none/);
assert.match(styles, /::-webkit-scrollbar\s*\{[^}]*display:\s*none/);
assert.match(styles, /\.settings-scroll-cue\s*\{/);
assert.match(styles, /\.can-scroll-down \.settings-scroll-cue-bottom/);
assert.match(styles, /\.settings-scroll-position\s*\{[^}]*right:\s*2px[^}]*width:\s*3px/);
assert.match(styles, /\.settings-scroll-position\.visible\s*\{[^}]*opacity:\s*1/);

console.log("overlay height tests passed");
