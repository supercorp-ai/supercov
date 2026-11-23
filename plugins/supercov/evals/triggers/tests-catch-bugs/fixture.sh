#!/usr/bin/env bash
# Builds the fixture project in the empty workspace before Claude starts.
# Trigger cases only read it, so Supercov is not installed. Every line of
# discount() runs, but no test tries the boundary at 100 or a discount
# without a code over 100.
set -euo pipefail
cat > package.json <<'JSON'
{ "name": "shop", "private": true, "type": "module", "scripts": { "test": "node --test" } }
JSON
mkdir -p src test
cat > src/price.js <<'JS'
export function discount(amount, code) {
  if (code === "VIP") return amount * 0.8;
  if (code === "HALF" && amount > 100) return amount / 2;
  return amount;
}
JS
cat > test/price.test.js <<'JS'
import { test } from "node:test";
import assert from "node:assert/strict";
import { discount } from "../src/price.js";

test("VIP takes 20% off", () => {
  assert.equal(discount(100, "VIP"), 80);
});

test("HALF halves a large amount", () => {
  assert.equal(discount(200, "HALF"), 100);
});

test("no code keeps the amount", () => {
  assert.equal(discount(50), 50);
});
JS
