import { describe, it } from "node:test";
import { expect } from "../support/expect.ts";
import { executeDifferential } from "./instrumenter-harness.ts";

const semanticCases: Array<{ name: string; source: string }> = [
  {
    name: "runtime helper-like user bindings never collide with injected bindings",
    source: `
      const __supercovMcdcBegin = "user:mcdc";
      const __supercovCoverageHit = "user:hit";
      const __supercovLoopBegin = "user:loop";
      const effects = [];
      function run() {
        effects.push(__supercovMcdcBegin, __supercovCoverageHit, __supercovLoopBegin);
        if (effects.length === 3 && __supercovMcdcBegin) return effects.join("|");
        return "wrong";
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "primitive thrown values and their preceding effects remain identical",
    source: `
      const effects = [];
      function run() {
        effects.push("before-throw");
        if (effects.length === 1 && true) throw "primitive failure";
        return "unreachable";
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "short-circuiting preserves getter count and evaluation order",
    source: `
      const effects = [];
      let reads = 0;
      const value = {
        get current() {
          effects.push("get:" + reads);
          reads += 1;
          return reads === 1 ? 0 : 4;
        },
      };
      function right(label) {
        effects.push("right:" + label);
        return label;
      }
      function run() {
        const first = value.current && right("first");
        const second = value.current || right("second");
        return { first, second, reads };
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "optional chains preserve computed access, arguments, and method this",
    source: `
      const effects = [];
      const target = {
        value: 6,
        method(argument) {
          effects.push(["method", this === target, argument]);
          return this.value + argument;
        },
      };
      function base(value) { effects.push(["base", value === null]); return value; }
      function key() { effects.push("key"); return "value"; }
      function argument() { effects.push("argument"); return 3; }
      function run() {
        const skippedProperty = base(null)?.[key()];
        const property = base(target)?.[key()];
        const called = target.method?.(argument());
        const skippedCall = ({ method: undefined }).method?.(argument());
        return { skippedProperty, property, called, skippedCall };
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "optional method calls preserve whole-chain short-circuit continuation",
    source: `
      const effects = [];
      const absent = { method: undefined };
      const present = {
        value: 4,
        method() {
          effects.push(this === present ? "receiver" : "wrong-receiver");
          return () => ({ value: this.value });
        },
      };
      function run() {
        const skipped = absent.method?.()().value;
        const called = present.method?.()().value;
        return { skipped, called };
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "direct function source coercion remains behaviorally identical",
    source: `
      const effects = [];
      function run() {
        const left = {} + function () { effects.push("left"); return 1; };
        const right = String(function () { effects.push("right"); return 1; });
        return {
          leftHasFunction: left.includes("function"),
          rightHasFunction: right.includes("function"),
        };
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "logical assignment evaluates a computed reference once",
    source: `
      const effects = [];
      let stored = 0;
      const target = new Proxy({}, {
        get(_object, key) { effects.push("get:" + String(key)); return stored; },
        set(_object, key, value) { effects.push("set:" + String(key) + ":" + value); stored = value; return true; },
      });
      function key() { effects.push("key"); return "value"; }
      function right(value) { effects.push("right:" + value); return value; }
      function run() {
        target[key()] ||= right(4);
        target[key()] &&= right(7);
        target[key()] ??= right(9);
        return stored;
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "parameter and destructuring defaults preserve nested evaluation order",
    source: `
      const effects = [];
      function fallback(label, value) { effects.push(label); return value; }
      function choose(
        first = fallback("parameter:first", { value: undefined }),
        { value = fallback("parameter:value", 2) } = first,
      ) {
        effects.push("body");
        const { nested = fallback("local:nested", 3) } = {};
        return value + nested;
      }
      function run() {
        const defaulted = choose();
        const supplied = choose({ value: 5 }, { value: 7 });
        return { defaulted, supplied };
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "try catch finally preserves completion values and ordering",
    source: `
      const effects = [];
      function choose(mode) {
        try {
          effects.push("try:" + mode);
          if (mode === "throw") throw new TypeError("boom");
          if (mode === "return") return "returned";
          return "normal";
        } catch (error) {
          effects.push("catch:" + error.name);
          return "caught";
        } finally {
          effects.push("finally:" + mode);
        }
      }
      function run() {
        return [choose("normal"), choose("return"), choose("throw")];
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "for-of preserves iterator closing on break and throw",
    source: `
      const effects = [];
      function iterable(label) {
        let value = 0;
        return {
          [Symbol.iterator]() { effects.push(label + ":iterator"); return this; },
          next() { effects.push(label + ":next:" + value); return { value: value++, done: value > 3 }; },
          return() { effects.push(label + ":return"); return { done: true }; },
        };
      }
      function run() {
        for (const value of iterable("break")) {
          effects.push("break:value:" + value);
          break;
        }
        try {
          for (const value of iterable("throw")) {
            effects.push("throw:value:" + value);
            throw new Error("stop");
          }
        } catch (error) {
          effects.push("caught:" + error.message);
        }
        return effects.length;
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "for-in and for-of preserve zero, continue, break, and destructuring defaults",
    source: `
      const effects = [];
      function fallback() { effects.push("fallback"); return 10; }
      function run() {
        let total = 0;
        for (const key in {}) total += key.length;
        for (const key in { a: 1, bb: 2 }) {
          if (key === "a") continue;
          total += key.length;
        }
        for (const [value = fallback()] of [[], [2], []]) {
          total += value;
          if (total > 20) break;
        }
        return total;
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "switch preserves fallthrough, default, and discriminant side effects",
    source: `
      const effects = [];
      function discriminant(value) { effects.push("discriminant:" + value); return value; }
      function choose(value) {
        switch (discriminant(value)) {
          case 1:
            effects.push("one");
          case 2:
            effects.push("two");
            break;
          default:
            effects.push("default");
        }
        return effects.length;
      }
      function run() { return [choose(1), choose(2), choose(3)]; }
      function observe() { return effects; }
    `,
  },
  {
    name: "coercion hooks remain single-evaluation decision operands",
    source: `
      const effects = [];
      function comparable(label, value) {
        return {
          [Symbol.toPrimitive](hint) {
            effects.push(label + ":" + hint);
            return value;
          },
        };
      }
      function run() {
        const left = comparable("left", 2);
        const right = comparable("right", 3);
        const outcome = left < 4 && right > 1 ? "yes" : "no";
        return outcome;
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "proxy get and apply traps retain their original order",
    source: `
      const effects = [];
      const callable = new Proxy(function (value) { effects.push("target:" + value); return value * 2; }, {
        apply(target, receiver, args) { effects.push("apply:" + args[0]); return Reflect.apply(target, receiver, args); },
      });
      const object = new Proxy({ callable }, {
        get(target, key, receiver) { effects.push("get:" + String(key)); return Reflect.get(target, key, receiver); },
      });
      function run() {
        return object.callable?.(5);
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "async functions preserve microtask and finally ordering",
    source: `
      const effects = [];
      async function step(label, value) {
        effects.push("start:" + label);
        await Promise.resolve();
        effects.push("end:" + label);
        return value;
      }
      async function run() {
        try {
          const left = await step("left", true);
          if (left && await step("right", false)) effects.push("branch");
          return effects.slice();
        } finally {
          effects.push("finally");
        }
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "generators preserve yield, return, and cleanup semantics",
    source: `
      const effects = [];
      function* values() {
        try {
          effects.push("start");
          yield 1;
          effects.push("middle");
          yield 2;
        } finally {
          effects.push("cleanup");
        }
      }
      function run() {
        const iterator = values();
        const first = iterator.next();
        const closed = iterator.return(9);
        return { first, closed };
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "labeled loops preserve continue and break targets",
    source: `
      const effects = [];
      function run() {
        outer: for (const left of [1, 2, 3]) {
          for (const right of [1, 2, 3]) {
            if (right === 1) continue;
            if (left === 2) continue outer;
            if (left === 3) break outer;
            effects.push(left + ":" + right);
          }
        }
        return effects.length;
      }
      function observe() { return effects; }
    `,
  },
  {
    name: "control decisions in parameter defaults own an inline scratch frame",
    source: `
      const effects = [];
      function choose(
        value = typeof process === "undefined" ? "browser" : "node",
      ) {
        effects.push(value);
        return value;
      }
      function run() {
        return [choose(), choose("provided")];
      }
      function observe() { return effects; }
    `,
  },
];

describe("instrumenter semantic equivalence", () => {
  for (const fixture of semanticCases) {
    it(fixture.name, async () => {
      const result = await executeDifferential(fixture.source);
      expect(result.instrumented, fixture.name).toStrictEqual(result.original);
    });
  }
});
