import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");

function rule(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`));
  assert.ok(match, `missing ${selector} rule`);
  return match[1];
}

for (const selector of [".popup", ".settings-popup"]) {
  const block = rule(selector);
  const shadow = block.match(/box-shadow\s*:\s*([^;]+);/)?.[1] ?? "";
  assert.equal(
    shadow.trim(),
    "inset 0 1px 0 rgba(255, 255, 255, 0.025)",
    `${selector} must use only the approved inset highlight`,
  );
}

console.log("popup gap transparency regression checks passed");
