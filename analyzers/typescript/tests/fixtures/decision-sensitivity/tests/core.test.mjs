import { test } from "node:test";
import assert from "node:assert/strict";
import { identical, distinct, masked } from "../src/core.mjs";

test("identical true branch", () => {
  assert.equal(identical(true), 7);
});

test("identical false branch", () => {
  assert.equal(identical(false), 7);
});

test("distinct true branch", () => {
  assert.equal(distinct(true), 1);
});

test("distinct false branch", () => {
  assert.equal(distinct(false), 2);
});

test("masked true branch", () => {
  assert.notEqual(masked(true), 0);
});

test("masked false branch", () => {
  assert.notEqual(masked(false), 0);
});
