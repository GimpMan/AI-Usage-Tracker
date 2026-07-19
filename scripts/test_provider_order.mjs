import assert from "node:assert/strict";
import {
  normalizeProviderOrder,
  reorderProviderOrder,
} from "../src/provider-order.ts";

const providers = ["glm", "minimax", "codex", "grok"];

// A cold start has no persisted order. All visible providers must be made part
// of the reorder state before determining a drop index.
assert.deepEqual(normalizeProviderOrder([], providers), providers);
assert.deepEqual(
  reorderProviderOrder([], providers, "codex", "minimax", false),
  ["glm", "codex", "minimax", "grok"],
);

// The same must hold when only some providers have been persisted by earlier
// drags; an unpersisted drop target still has a valid position.
assert.deepEqual(
  reorderProviderOrder(["grok"], providers, "codex", "minimax", false),
  ["grok", "glm", "codex", "minimax"],
);

// Saved providers that are no longer present are removed, duplicates collapse,
// and newly visible providers append in their current display order.
assert.deepEqual(
  normalizeProviderOrder(["removed", "codex", "codex"], providers),
  ["codex", "glm", "minimax", "grok"],
);

console.log("provider order tests passed");
