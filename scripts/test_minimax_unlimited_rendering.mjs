import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const providerSource = readFileSync(
  new URL("../src-tauri/src/providers/minimax.rs", import.meta.url),
  "utf8",
);
const typeSource = readFileSync(new URL("../src/types.ts", import.meta.url), "utf8");
const overlaySource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);

assert.equal(
  providerSource.includes("current_weekly_status: Option<i32>"),
  true,
  "MiniMax responses must parse the live weekly-status field",
);
assert.equal(
  providerSource.includes("is_unlimited: remains.current_weekly_status == Some(3)"),
  true,
  "MiniMax weekly status 3 must map to an unlimited weekly window",
);
assert.equal(
  typeSource.includes("is_unlimited: boolean"),
  true,
  "frontend usage windows must receive the unlimited flag",
);
assert.equal(
  overlaySource.includes('w.is_unlimited ? "∞"'),
  true,
  "the compact bar must display infinity for unlimited windows",
);
assert.equal(
  overlaySource.includes('w.is_unlimited ? "∞ Unlimited"'),
  true,
  "the expanded weekly section must display an unlimited state",
);

console.log("MiniMax unlimited rendering tests passed");
