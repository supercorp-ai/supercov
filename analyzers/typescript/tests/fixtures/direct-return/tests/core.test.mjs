import { test } from "node:test";
import assert from "node:assert/strict";
import { select } from "../src/value.mjs";

test("direct results", () => {
  // observes: src/value.mjs#select return false; check value
  assert.equal(select({ items: undefined }), false);
  // observes: src/value.mjs#select items.length === 0; check value
  assert.equal(select({ items: [] }), "*");
  const value = select({ items: [] });
  const alias = value;
  // observes: src/value.mjs#select items.length === 0; check value
  assert.equal(alias, "*");
  // A different invocation reached this return; this selected call does not.
  // observes: src/value.mjs#select return false; check value
  assert.equal(select({ items: [] }), "*");
  // observes: src/value.mjs#select return false; check value
  assert.equal((select({ items: undefined }), false), false);
  if (false) {
    // observes: src/value.mjs#select return false; check value
    assert.equal(select({ items: undefined }), false);
  }
  // observes: src/value.mjs#select return false; check unknown
  assert.equal(1, 1);
});

test("later registration is not a first-test prefix", () => {
  // observes: src/value.mjs#select return false; check value
  assert.equal(select({ items: undefined }), false);
});
