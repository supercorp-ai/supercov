import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { generatedExpression } from "../support/generated-programs.ts";
import { executeDifferential } from "./instrumenter-harness.ts";

describe("instrumenter generated differential corpus", () => {
  it("preserves 160 deterministic nested evaluation programs", async () => {
    for (let index = 0; index < 160; index += 1) {
      const seed = 0x5eed_0000 + index;
      const expression = generatedExpression(seed);
      const source = `
        const effects = [];
        function mark(label, value) { effects.push(label); return value; }
        function fail(label) { effects.push(label); throw new RangeError(label); }
        function run() {
          const value = ${expression};
          return value;
        }
        function observe() { return effects; }
      `;
      const v1 = await executeDifferential(
        source,
        `app/generated-${seed.toString(16)}.ts`,
      );
      const v2 = await executeDifferential(
        source,
        `app/generated-${seed.toString(16)}.ts`,
        { probeVersion: 2 },
      );
      expect(
        v1.instrumented,
        `seed ${seed.toString(16)}\n${expression}`,
      ).toStrictEqual(v1.original);
      expect(v2.instrumented).toStrictEqual(v2.original);
      expect(v2.evidence.manifest).toStrictEqual(v1.evidence.manifest);
      expect(v2.evidence.vectors).toStrictEqual(v1.evidence.vectors);
      expect(v2.evidence.hits).toStrictEqual(v1.evidence.hits);
    }
  });
});
