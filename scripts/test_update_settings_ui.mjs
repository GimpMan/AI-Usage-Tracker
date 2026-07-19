import assert from "node:assert/strict";
import fs from "node:fs";
import { clampUpdateProgressPercent, shouldApplyInitialUpdateState } from "../src/update-state-logic.ts";

const ui = fs.readFileSync("src/settings-panel.tsx", "utf8");
const css = fs.readFileSync("src/styles.css", "utf8");

for (const text of ["Updates", "Installed version", "Last successful check", "Check for updates", "Available version", "Install update and restart", "Retry"])
  assert.ok(ui.includes(text), `missing Updates UI: ${text}`);
for (const phase of ["idle", "checking", "up_to_date", "available", "downloading", "installing", "error"])
  assert.ok(ui.includes(`"${phase}"`), `missing state rendering: ${phase}`);
assert.match(ui, /checkForUpdate\(true\)/, "manual checks must pass true");
assert.match(ui, /installUpdate\(\)/, "install must require explicit handler");
assert.match(ui, /disabled=\{busy/);
assert.match(ui, /<progress/);
assert.doesNotMatch(ui, /dangerouslySetInnerHTML|innerHTML/);
// Updates communicate inline (progress bar, retry), never via toasts. Scoped to
// UpdatesSection: the file also hosts the General section's quota-notification
// toggle, which legitimately mentions notifications.
const updatesSlice = ui.slice(ui.indexOf("function UpdatesSection"), ui.indexOf("function SettingsIcon"));
assert.ok(updatesSlice.length > 0, "UpdatesSection slice must be locatable");
assert.doesNotMatch(updatesSlice, /toast|notification/i);
assert.match(css, /update-badge/);
assert.match(css, /update-notes/);
assert.match(
  css,
  /\.update-notes\s*\{[^}]*scrollbar-color:\s*var\(--accent\)\s+transparent[^}]*scrollbar-width:\s*thin/,
  "release notes must define themed native scrollbar fallback",
);
assert.match(
  css,
  /\.update-notes::-webkit-scrollbar\s*\{[^}]*width:\s*6px[^}]*height:\s*6px/,
  "release notes must define a compact WebKit scrollbar",
);
assert.match(
  css,
  /\.update-notes::-webkit-scrollbar-thumb\s*\{[^}]*background:\s*rgba\(137,\s*180,\s*250,\s*0\.42\)[^}]*border-radius:\s*999px/,
  "release notes scrollbar thumb must use the theme accent",
);
assert.equal(shouldApplyInitialUpdateState(0, 0), true);
assert.equal(shouldApplyInitialUpdateState(0, 1), false, "an event newer than the snapshot request wins");
assert.equal(clampUpdateProgressPercent(150, 100), 100);
assert.equal(clampUpdateProgressPercent(-5, 100), 0);
assert.equal(clampUpdateProgressPercent(25, 0), 0);
assert.equal(clampUpdateProgressPercent(Number.NaN, 100), 0);

// Update channel selector (Settings > Updates)
assert.match(ui, /Update channel/, "missing accessible Update channel label");
assert.match(ui, /Stable releases/, "missing Stable releases choice");
assert.match(ui, /Prerelease builds/, "missing Prerelease builds choice");
assert.match(
  ui,
  /main releases only|stable releases only|published, non-draft|non-prerelease/i,
  "copy must clarify stable = main/stable releases only",
);
assert.match(
  ui,
  /prerelease builds only|prerelease only|prerelease-only/i,
  "copy must clarify prerelease = prerelease builds only",
);
assert.match(ui, /setUpdateChannel\s*\(/, "channel selector must call setUpdateChannel");
assert.match(
  ui,
  /disabled=\{[^}]*busy|channelBusy|channelChanging|changingChannel/,
  "channel selector must disable during update activity or channel changes",
);
assert.match(
  ui,
  /aria-label=["']Update channel["']|htmlFor=["'][^"']*update-channel|id=["']update-channel/,
  "Update channel control must be accessible (label/id/aria-label)",
);
assert.match(ui, /["']stable["']/, "selector must use stable channel value");
assert.match(ui, /["']prerelease["']/, "selector must use prerelease channel value");

console.log("update settings UI checks passed");
