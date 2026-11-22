#!/usr/bin/env bash
# Builds the fixture project in the empty workspace: a main branch, and a
# branch that adds the HALF code with one test. Every changed line runs, but
# some of its mutants survive: `amount > 100` made `>=` (no test tries 100),
# and `code === "HALF"` made `true` or `&&` made `||` (no test passes an
# amount over 100 without the code). Runs outside the sandbox, before Claude starts, so it may use the
# network to install Supercov locally; the run itself then needs no network.
set -euo pipefail
git() { command git -c user.name=Eval -c user.email=eval@example.com -c commit.gpgsign=false "$@"; }
git init --quiet --initial-branch=main
cat > package.json <<'JSON'
{ "name": "shop", "private": true, "type": "module", "scripts": { "test": "node --test" } }
JSON
printf 'node_modules/\n' > .gitignore
mkdir -p src test
cat > src/price.js <<'JS'
export function discount(amount, code) {
  if (code === "VIP") return amount * 0.8;
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

test("no code keeps the amount", () => {
  assert.equal(discount(50), 50);
});
JS
git add -A
git commit --quiet -m "Discount for VIP customers"
git checkout --quiet -b half-price
cat > src/price.js <<'JS'
export function discount(amount, code) {
  if (code === "VIP") return amount * 0.8;
  if (code === "HALF" && amount > 100) return amount / 2;
  return amount;
}
JS
cat >> test/price.test.js <<'JS'

test("HALF halves a large amount", () => {
  assert.equal(discount(200, "HALF"), 100);
});
JS
git add -A
git commit --quiet -m "HALF halves amounts over 100"
npm install --no-save --no-audit --no-fund --silent supercov@latest
