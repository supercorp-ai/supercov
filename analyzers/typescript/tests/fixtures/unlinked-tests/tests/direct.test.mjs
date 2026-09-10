import { test } from "node:test";
import assert from "node:assert/strict";
import { independent, unchecked } from "../src/core.mjs";

test("independent direct assertion", () => {
  assert.equal(independent(), 9);
});
test("ordinary execution only", () => {
  unchecked();
});
