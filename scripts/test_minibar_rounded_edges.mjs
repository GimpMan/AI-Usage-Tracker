import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
const match = css.match(/\.bar\s*\{([^}]*)\}/);
assert.ok(match, "missing .bar rule");

const shadow = match[1].match(/box-shadow\s*:\s*([^;]+);/)?.[1]?.trim() ?? "";
assert.equal(
  shadow,
  "inset 0 1px 0 rgba(255, 255, 255, 0.035)",
  ".bar must use only the approved inset highlight so rounded corners stay transparent",
);

console.log("minibar rounded-edge regression checks passed");
