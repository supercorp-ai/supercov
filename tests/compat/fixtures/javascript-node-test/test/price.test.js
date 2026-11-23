import test from "node:test";
import assert from "node:assert/strict";
import { price } from "../src/price.js";

test("members save on large orders", () => {
  assert.equal(price(200, true, false), 180);
});

test("coupons take five off", () => {
  assert.equal(price(20, false, true), 15);
});
