import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const provider = readFileSync(new URL("../src-tauri/src/providers/kimi.rs", import.meta.url), "utf8");
const settings = readFileSync(new URL("../src/settings-panel.tsx", import.meta.url), "utf8");

assert.equal(
  /GetSubscriptionStats|fetch_membership_total|subscription_balance/.test(provider),
  false,
  "Kimi Code OAuth must not call or parse the unauthorised web-membership Total API",
);
assert.equal(
  /Tracks 5-hour and 7-day Kimi Code plan quotas/.test(settings),
  true,
  "Kimi settings must describe the authenticated Code API quotas actually available",
);
assert.equal(
  /Tracks Total, 5-hour, and 7-day Kimi Code plan quotas/.test(settings),
  false,
  "Kimi settings must not claim unsupported Total usage tracking",
);

console.log("Kimi Code usage-only tests passed");
