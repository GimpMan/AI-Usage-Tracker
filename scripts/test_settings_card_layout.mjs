import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../src/settings-panel.tsx", import.meta.url), "utf8");
const updatesCard = '<div class="provider-section updates-card">';
const updatesCardStart = source.indexOf(updatesCard);
const updatesSectionStart = source.indexOf("<UpdatesSection />", updatesCardStart);
const generalStart = source.indexOf('<h2>General</h2>');
const generalCardStart = source.lastIndexOf('<div class="provider-section">', generalStart);
const updatesCardBlock = source.slice(updatesCardStart, generalCardStart);

assert(updatesCardStart >= 0, "Updates must have a standalone provider card");
assert(updatesSectionStart > updatesCardStart, "Updates must render inside its card");
assert(generalCardStart > updatesSectionStart, "Updates card must come before General");
assert.match(
  updatesCardBlock,
  /^<div class="provider-section updates-card">\s*<UpdatesSection \/>\s*<\/div>\s*$/,
  "Updates card must be adjacent to General",
);

console.log("settings card layout tests passed");
