import { describe, expect, it } from "vitest";
import { executeDifferential } from "./instrumenter-harness";

function generator(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    return state;
  };
}

const literals = [
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

function generatedExpression(seed: number): string {
  const next = generator(seed);
  let marker = 0;
  const expression = (depth: number): string => {
    const id = marker++;
    if (depth === 0) {
      if (next() % 13 === 0) return `fail("throw-${id}")`;
      return `mark("atom-${id}", ${literals[next() % literals.length]})`;
    }
    const kind = next() % 5;
    if (kind <= 2) {
      const operator = (["&&", "||", "??"] as const)[next() % 3];
      return `(${expression(depth - 1)} ${operator} ${expression(depth - 1)})`;
    }
    if (kind === 3)
      return `(${expression(depth - 1)} ? ${expression(depth - 1)} : ${expression(depth - 1)})`;
    return `!(${expression(depth - 1)})`;
  };
  return expression(3);
}

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
      const result = await executeDifferential(
        source,
        `app/generated-${seed.toString(16)}.ts`,
      );
      expect(
        result.instrumented,
        `seed ${seed.toString(16)}\n${expression}`,
      ).toStrictEqual(result.original);
    }
  });
});

