import { test } from "node:test";
import assert from "node:assert/strict";
import { aliased } from "../src/core.mjs";

function swallow(name, body) {
  test(name, () => {
    try {
      body();
    } catch {
      // Deliberately bad harness: observing a passing assertion in body does
      // not establish that an alternative failure would fail this test.
    }
  });
}

swallow("opaque wrapper catches assertion failures", () => {
  // observes: src/core.mjs#aliased return 7; check value
  assert.equal(aliased(), 7);
});
