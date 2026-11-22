import test from "node:test";
import assert from "node:assert/strict";
import { label } from "../src/price.js";

test("large totals", () => {
  label(60);
});
