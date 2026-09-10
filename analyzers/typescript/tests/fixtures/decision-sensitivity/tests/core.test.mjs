import { test } from "node:test";
import assert from "node:assert/strict";
import {
  identical,
  distinct,
  masked,
  transformed,
  effectful,
  zero,
  typed,
  lone,
} from "../src/core.mjs";
import { mutableChoice } from "../src/mutable.mjs";

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

test("transformed true branch", () => {
  assert.equal(Math.abs(transformed(true)), 1);
});

test("transformed false branch", () => {
  assert.equal(Math.abs(transformed(false)), 1);
});

test("effectful true branch", () => {
  const events = [];
  assert.equal(effectful(true, events), 7);
  assert.deepEqual(events, ["left"]);
});

test("effectful false branch", () => {
  const events = [];
  assert.equal(effectful(false, events), 7);
  assert.deepEqual(events, ["right"]);
});

test("negative zero", () => {
  assert.equal(zero(true), -0);
});
test("positive zero", () => {
  assert.equal(zero(false), 0);
});
test("string one", () => {
  assert.equal(typed(true), "1");
});
test("number one", () => {
  assert.equal(typed(false), 1);
});
test("lone surrogate true", () => {
  assert.equal(lone(true), "\ud800");
});
test("lone surrogate false", () => {
  assert.equal(lone(false), "\ud800");
});
test("mutable function true", () => {
  assert.equal(mutableChoice(true), 1);
});
test("mutable function false", () => {
  assert.equal(mutableChoice(false), 2);
});
