import assert from "node:assert/strict";
import fs from "node:fs";
import {
  checkboxChecked,
  checkboxDisabled,
  hasDisplayableWindows,
  formatBalanceWindow,
  formatDollarWindow,
} from "../src/provider-visibility.ts";

assert.equal(checkboxChecked({ eligible: true, hidden: false }), true);
assert.equal(checkboxChecked({ eligible: false, hidden: false }), false);
assert.equal(checkboxDisabled({ eligible: false }), true);
assert.equal(checkboxDisabled({ eligible: true }), false);

assert.equal(
  hasDisplayableWindows({ windows: [{ bar_visible: false }] }),
  false,
);
assert.equal(
  hasDisplayableWindows({ windows: [{ bar_visible: true }] }),
  true,
);
assert.equal(
  formatBalanceWindow({ label: "balance $12.40" }),
  "$12.40",
);
assert.equal(
  formatDollarWindow({ label: "total $1.0605 left" }),
  "$1.0605",
);
assert.equal(
  formatDollarWindow({ label: "total $1.0605" }),
  "$1.0605",
);
assert.equal(
  formatDollarWindow({ label: "this month $0.2985" }),
  "$0.2985",
);

const ui = fs.readFileSync("src/settings-panel.tsx", "utf8");
const css = fs.readFileSync("src/styles.css", "utf8");

assert.doesNotMatch(ui, /Show in overlay/, "old label text removed");
assert.match(ui, /role="switch"/, "uses switch role");
assert.match(ui, /class="visibility-switch"/, "switch class exists");
assert.match(ui, /aria-checked=\{!hidden\}/, "switch reflects shown state");
assert.match(ui, /Overlay/, "new label text present");
assert.match(css, /\.visibility-switch\s*\{/, "switch styles exist");
assert.match(css, /\.visibility-switch\[aria-checked="true"\]/, "ON state styled");
assert.match(css, /transition:\s*transform\s+150ms\s+ease-out/, "thumb animation defined");

const settingsSource = fs.readFileSync(new URL("../src/settings-panel.tsx", import.meta.url), "utf8");
const overlaySource = fs.readFileSync(new URL("../src/overlay.tsx", import.meta.url), "utf8");
const apiSource = fs.readFileSync(new URL("../src/api.ts", import.meta.url), "utf8");
const commandsSource = fs.readFileSync(
  new URL("../src-tauri/src/commands.rs", import.meta.url),
  "utf8",
);
assert.match(settingsSource, /saveOpenrouterManagementKey/);
assert.match(settingsSource, /Management API Key/);
assert.match(settingsSource, /Rebase account balance/);
assert.match(apiSource, /rebaseOpenrouterAccount/);
assert.match(overlaySource, /hasDisplayableWindows/);
assert.match(overlaySource, /formatDollarWindow/);

assert.match(
  commandsSource,
  /app\.emit\("provider-visibility-changed",\s*\(\)\)/,
  "backend broadcasts provider visibility changes across webviews",
);
assert.match(
  commandsSource,
  /if is_hidden\(id\) \{\s*continue;/,
  "get_usage collect omits hidden providers so hide is immediate",
);
assert.match(
  commandsSource,
  /do_refresh_provider\(&app,\s*&state,\s*&provider\)/,
  "unhide refreshes only the toggled provider, not every provider",
);
assert.match(
  commandsSource,
  /fn emit_provider_visibility_changed/,
  "visibility broadcast is factored so hide/show can emit immediately",
);
assert.match(
  commandsSource,
  /let has_snap = \{[\s\S]*?if has_snap \{[\s\S]*?emit_provider_visibility_changed/,
  "unhide with last-good snapshot emits before the network fetch",
);
// Full multi-provider refresh must not run inside set_provider_hidden
// (single-provider path only). Match the function body via surrounding markers.
const setHiddenStart = commandsSource.indexOf("pub async fn set_provider_hidden");
const setHiddenEnd = commandsSource.indexOf("pub async fn save_overlay_position", setHiddenStart);
assert.ok(setHiddenStart >= 0 && setHiddenEnd > setHiddenStart, "locate set_provider_hidden body");
const setHiddenFn = commandsSource.slice(setHiddenStart, setHiddenEnd);
assert.doesNotMatch(
  setHiddenFn,
  /do_refresh\(&app/,
  "unhide must not block on a full multi-provider refresh",
);
assert.match(
  overlaySource,
  /listen\("provider-visibility-changed",[\s\S]*?void pull\(\)/,
  "overlay immediately pulls usage after a provider visibility change",
);
const toggleHandler = settingsSource.match(
  /async function onToggleHidden[\s\S]*?\n  async function onRecheck/,
)?.[0] ?? "";
assert.match(
  toggleHandler,
  /dispatchEvent\(new CustomEvent\("ai-usage-refresh"\)\)/,
  "visibility toggle also notifies embedded Settings via same-webview DOM event",
);

console.log("provider visibility tests passed");
