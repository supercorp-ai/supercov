import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { instrumentNodeAssertionPhases } from "../../src/nodeAssertionInstrumenter.ts";
import {
  takeNodeAssertionPhases,
  withCoverageCarrier,
  withNodeAssertionPhase,
} from "../../src/runtime.ts";
import type { CoverageExecutionScope } from "../../src/types.ts";

describe("native node:assert attribution", () => {
  it("wraps default, named, strict, and CommonJS assertion calls before arguments", () => {
    const fixtures = [
      `import assert from "node:assert/strict"; assert.equal(value(), 1);`,
      `import { deepStrictEqual as same } from "node:assert"; same(value(), {});`,
      `const assert = require("assert").strict; assert.ok(value());`,
      `const { throws: fails } = require("node:assert/strict"); fails(() => value());`,
    ];
    for (const [index, fixture] of fixtures.entries()) {
      const transformed = instrumentNodeAssertionPhases(
        fixture,
        `tests/example-${index}.test.js`,
      );
      expect(transformed.assertions).toBe(1);
      expect(transformed.code).toContain("withNodeAssertionPhase");
      expect(transformed.code).toMatch(/\(\) =>/);
    }
  });

  it("does not claim assertion attribution for an unrelated assert-shaped API", () => {
    const source = `const assert = localFactory(); assert.equal(value(), 1);`;
    const transformed = instrumentNodeAssertionPhases(
      source,
      "tests/unrelated.test.js",
    );
    expect(transformed.assertions).toBe(0);
    expect(transformed.code).toBe(source);
  });

  it("wraps imported expect matchers only in native node:test files", () => {
    const source = `
      import { test } from "node:test";
      import { expect as verify } from "../support/expect.js";
      test("example", () => verify(value()).not.toEqual(2));
    `;
    const transformed = instrumentNodeAssertionPhases(
      source,
      "tests/example.test.js",
    );
    expect(transformed.assertions).toBe(1);
    expect(transformed.code).toContain("expect.not.toEqual");

    const unrelated = instrumentNodeAssertionPhases(
      source.replace("node:test", "vitest"),
      "tests/example.test.js",
    );
    expect(unrelated.assertions).toBe(0);
  });

  it("records passed and failed asynchronous assertion phases per exact attempt", async () => {
    const scope: CoverageExecutionScope = {
      version: 1,
      runId: "assertion-run",
      workerId: "worker",
      testId: "test",
      testKey: "key",
      retry: 0,
      attemptId: "attempt",
    };
    await withCoverageCarrier({ version: 1, scope }, async () => {
      await withNodeAssertionPhase("node:assert.rejects", "test.ts:1:1", async () => {
        await Promise.resolve();
      });
      try {
        withNodeAssertionPhase("node:assert.equal", "test.ts:2:1", () => {
          throw new Error("mismatch");
        });
      } catch {
        // The runner records the failing assertion without changing its error.
      }
    });
    const phases = takeNodeAssertionPhases(scope);
    expect(phases.map((phase) => phase.status)).toEqual(["passed", "failed"]);
    expect(phases.map((phase) => phase.operation)).toEqual([
      "node:assert.rejects",
      "node:assert.equal",
    ]);
    expect(phases[1]?.error).toBe("mismatch");
  });
});
