import assert from "node:assert/strict";
import fs from "node:fs";

const readJson = (path) => JSON.parse(fs.readFileSync(path, "utf8"));
const packageJson = readJson("package.json");
const cargoToml = fs.readFileSync("src-tauri/Cargo.toml", "utf8");
const mainRs = fs.readFileSync("src-tauri/src/main.rs", "utf8");
const config = readJson("src-tauri/tauri.conf.json");
const capability = readJson("src-tauri/capabilities/default.json");
const gitignore = fs.readFileSync(".gitignore", "utf8");
const checksWorkflow = fs.readFileSync(".github/workflows/checks.yml", "utf8");
const releaseWorkflow = fs.readFileSync(".github/workflows/release.yml", "utf8");

assert(packageJson.dependencies?.["@tauri-apps/plugin-updater"], "missing JS updater plugin");
assert(packageJson.dependencies?.["@tauri-apps/plugin-process"], "missing JS process plugin");
assert(
  Object.keys(packageJson.scripts ?? {}).every((name) => !/release|package/i.test(name)),
  "local package scripts must not publish releases",
);
assert.match(releaseWorkflow, /tags:\s*\r?\n\s*- ["']v\*["']/, "release workflow must run on version tags only");
for (const workflow of [checksWorkflow, releaseWorkflow]) {
  assert.match(workflow, /actions\/checkout@v7/);
  assert.match(workflow, /actions\/setup-node@v7/);
}
assert.match(releaseWorkflow, /tauri-apps\/tauri-action@v1/, "release workflow must use the official Tauri action");
assert.match(releaseWorkflow, /TAURI_SIGNING_PRIVATE_KEY:\s*\$\{\{ secrets\.TAURI_SIGNING_PRIVATE_KEY \}\}/);
assert.match(releaseWorkflow, /TAURI_SIGNING_PRIVATE_KEY_PASSWORD:\s*\$\{\{ secrets\.TAURI_SIGNING_PRIVATE_KEY_PASSWORD \}\}/);
assert.match(releaseWorkflow, /generateReleaseNotes:\s*true/);
assert.match(releaseWorkflow, /args:\s*--bundles nsis/);
assert.match(cargoToml, /^tauri-plugin-updater\s*=/m, "missing Rust updater plugin");
assert.match(cargoToml, /^tauri-plugin-process\s*=/m, "missing Rust process plugin");

const manageIndex = mainRs.indexOf(".manage(AppState");
assert(manageIndex > 0, "managed state registration not found");
for (const init of [
  ".plugin(tauri_plugin_updater::Builder::new().build())",
  ".plugin(tauri_plugin_process::init())",
]) {
  const index = mainRs.indexOf(init);
  assert(index >= 0, `missing plugin registration: ${init}`);
  assert(index < manageIndex, `${init} must be registered before managed state`);
}

assert(capability.permissions.includes("updater:default"), "missing updater default permission");
assert(capability.permissions.includes("process:allow-restart"), "missing process restart permission");
assert.deepEqual(
  capability.permissions.filter((permission) => permission.startsWith("process:")),
  ["process:allow-restart"],
  "restart must be the only granted process permission",
);
assert.equal(config.bundle.targets, "nsis", "bundle target must be NSIS only");
assert.equal(config.bundle.createUpdaterArtifacts, true, "updater artifacts must be enabled");
assert(!config.identifier.endsWith(".app"), "bundle identifier must not use the reserved .app suffix");
assert.deepEqual(
  config.plugins?.updater?.endpoints,
  ["https://github.com/GimpMan/AI-Usage-Tracker/releases/latest/download/latest.json"],
  "updater endpoints must contain exactly the GitHub latest-release endpoint",
);
assert.equal(config.plugins?.updater?.windows?.installMode, "passive");

const pubkey = config.plugins?.updater?.pubkey;
assert.equal(typeof pubkey, "string", "updater public key must be a string");
assert(pubkey.trim().length > 0, "updater public key must not be empty");
assert(!/[\\/]/.test(pubkey), "updater public key must be content, not a path");

assert.match(gitignore, /(^|\n)\.tauri\//, "local .tauri directories must be ignored");
assert.match(gitignore, /(^|\r?\n)\*\.key(?:\r?\n|$)/, "private updater key files must be ignored");

console.log("updater configuration structural test passed");
