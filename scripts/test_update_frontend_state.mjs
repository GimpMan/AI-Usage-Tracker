import assert from "node:assert/strict";
import fs from "node:fs";

const types = fs.readFileSync("src/types.ts", "utf8");
const api = fs.readFileSync("src/api.ts", "utf8");
const state = fs.readFileSync("src/update-state.ts", "utf8");
const overlay = fs.readFileSync("src/overlay.tsx", "utf8");

for (const phase of ["idle", "checking", "up_to_date", "available", "downloading", "installing", "error"])
  assert.match(types, new RegExp(`"${phase}"`), `missing UpdatePhase ${phase}`);
for (const field of ["current_version", "available_version", "notes", "published_at", "last_checked_at", "downloaded_bytes", "total_bytes", "error"])
  assert.match(types, new RegExp(field), `missing UpdateState.${field}`);
assert.match(types, /UpdateChannel/, "missing UpdateChannel type");
assert.match(types, /"stable"/, "missing UpdateChannel stable value");
assert.match(types, /"prerelease"/, "missing UpdateChannel prerelease value");
assert.match(types, /channel:\s*UpdateChannel/, "UpdateState must expose active channel");
assert.match(api, /invoke<UpdateState>\("get_update_state"\)/);
assert.match(api, /invoke<UpdateState>\("check_for_update", \{ manual \}\)/);
assert.match(api, /installUpdate\(\): Promise<void>/);
assert.match(api, /invoke<void>\("install_update"\)/);
assert.match(
  api,
  /setUpdateChannel\(\s*channel:\s*UpdateChannel\s*\):\s*Promise</,
  "missing setUpdateChannel(channel) API",
);
assert.match(
  api,
  /invoke(?:<[^>]*>)?\(\s*["']set_update_channel["']\s*,\s*\{\s*channel\s*\}\s*\)/,
  "setUpdateChannel must invoke set_update_channel with channel",
);
assert.match(state, /listen<UpdateState>\("update-state-changed"/);
assert.match(state, /return \(\) =>[\s\S]*unlisten/);
assert.match(state, /catch[\s\S]*console\.error/);
assert.match(state, /shouldApplyInitialUpdateState/);
assert.match(overlay, /UpdateStateProvider/);
assert.match(overlay, /phase === "available"/);
// The gear button shows an update badge (above) but no longer a hover tooltip
// with the version string; available_version is rendered by Settings → Updates
// (settings-panel.tsx), which is a separate source file.
assert.doesNotMatch(overlay, /available_version/);
assert.doesNotMatch(state + overlay, /from ["'][^"']*(toast|notification)|import[^;]*(toast|notification)/i);
console.log("update frontend state checks passed");
