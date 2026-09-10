import { test } from "node:test";
import assert from "node:assert/strict";
import { aliased, witnessless } from "../src/core.mjs";

const ignore = () => {};
for (const enabled of [true, false]) {
  const runCase = enabled ? test : ignore;
  runCase(`conditional registration ${enabled}`, () => {
    // observes: src/core.mjs#aliased return 7; check value
    assert.equal(aliased(), 7);
  });
}

const register = test;
register("alias without assertions", () => {
  witnessless();
});
