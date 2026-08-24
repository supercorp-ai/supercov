import fc from "fast-check";
import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { executeDifferential } from "./instrumenter-harness.ts";

const atoms = [
  "false",
  "true",
  "0",
  "1",
  "-0",
  "null",
  "undefined",
  '""',
  '"value"',
  "NaN",
] as const;

const expressionAtDepth: fc.Memo<string> = fc.memo(
  (depth): fc.Arbitrary<string> => {
  const atom = fc
    .tuple(fc.string({ unit: fc.constantFrom(..."abcdef"), maxLength: 6 }), fc.constantFrom(...atoms))
    .map(([label, value]) => `mark(${JSON.stringify(label)}, ${value})`);
  const throwing = fc
    .string({ unit: fc.constantFrom(..."abcdef"), maxLength: 6 })
    .map((label) => `fail(${JSON.stringify(label)})`);
  if (depth <= 1) return fc.oneof({ weight: 8, arbitrary: atom }, { weight: 1, arbitrary: throwing });
  const nested: fc.Arbitrary<string> = expressionAtDepth(depth - 1);
  return fc.oneof(
    { depthSize: "small" },
    { weight: 5, arbitrary: atom },
    { weight: 1, arbitrary: throwing },
    fc
      .tuple(nested, fc.constantFrom("&&", "||", "??"), nested)
      .map(([left, operator, right]) => `(${left} ${operator} ${right})`),
    fc
      .tuple(nested, nested, nested)
      .map(([condition, yes, no]) => `(${condition} ? ${yes} : ${no})`),
    nested.map((value) => `!(${value})`),
  );
  },
);

describe("instrumenter property-based semantic equivalence", () => {
  it("preserves generated nested expressions, results, errors, and effects", { timeout: 30_000 }, async () => {
    await fc.assert(
      fc.asyncProperty(expressionAtDepth(5), async (expression) => {
        const source = `
          const effects = [];
          function mark(label, value) { effects.push("mark:" + label); return value; }
          function fail(label) { effects.push("fail:" + label); throw new RangeError(label); }
          function run() { return ${expression}; }
          function observe() { return effects; }
        `;
        const result = await executeDifferential(source, "app/property-expression.js");
        expect(result.instrumented).toStrictEqual(result.original);
      }),
      { numRuns: 500, seed: 0x5e71c0 },
    );
  });

  it("preserves nested control flow over generated input domains", { timeout: 30_000 }, async () => {
    const values = fc.record({
      first: fc.array(fc.integer({ min: -3, max: 3 }), { maxLength: 6 }),
      second: fc.array(fc.integer({ min: -3, max: 3 }), { maxLength: 6 }),
      throwAt: fc.option(fc.integer({ min: 0, max: 5 })),
      mode: fc.integer({ min: 0, max: 4 }),
    });
    await fc.assert(
      fc.asyncProperty(values, async (input) => {
        const source = `
          const effects = [];
          const input = ${JSON.stringify(input)};
          function run() {
            let total = 0;
            outer: for (const [group, values] of [["first", input.first], ["second", input.second]]) {
              effects.push("group:" + group);
              for (let index = 0; index < values.length; index += 1) {
                const value = values[index];
                try {
                  effects.push("value:" + value);
                  if (input.throwAt === index && group === "second") throw new RangeError("generated");
                  if ((value > 0 && input.mode % 2 === 0) || value === -2) total += value;
                  else if (value === 0 || input.mode > 3) continue;
                  else total -= value;
                  switch ((input.mode + index) % 4) {
                    case 0: total += 2; break;
                    case 1: total -= 1; break;
                    case 2: if (total > 8) break outer; break;
                    default: total += 0;
                  }
                } catch (error) {
                  effects.push("caught:" + error.name);
                  total += 7;
                } finally {
                  effects.push("finally:" + index);
                }
              }
            }
            return total;
          }
          function observe() { return effects; }
        `;
        const result = await executeDifferential(source, "app/property-control.js");
        expect(result.instrumented).toStrictEqual(result.original);
      }),
      { numRuns: 300, seed: 0xc07f10 },
    );
  });
});
