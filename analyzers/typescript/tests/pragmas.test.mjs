import test from "node:test";
import assert from "node:assert/strict";
import ts from "typescript";
import { collectPragmas } from "../dist/pragmas.js";

const sites = [
  {
    id: "log",
    file: "src/core.ts",
    fn: "log",
    owner: "log",
    category: "log",
    text: "console.log(value)",
  },
  {
    id: "one",
    file: "src/core.ts",
    fn: "compute",
    owner: "compute",
    text: "return value;",
  },
  {
    id: "two",
    file: "src/core.ts",
    fn: "choose",
    owner: "choose",
    text: "return 1;",
  },
  {
    id: "three",
    file: "src/core.ts",
    fn: "choose",
    owner: "choose",
    text: "return 2;",
  },
];
function collect(source, phases, inventory = sites) {
  const sf = ts.createSourceFile(
    "tests/core.test.ts",
    source,
    ts.ScriptTarget.Latest,
    true,
  );
  const collector = collectPragmas(ts, [sf], (s) => s.fileName, inventory);
  const calls = [];
  const visit = (node) => {
    if (
      ts.isCallExpression(node) &&
      node.expression.getText(sf) === "assert.equal"
    ) {
      collector.register(node, "equal", "T", false);
      const pos = sf.getLineAndCharacterOfPosition(node.getStart(sf));
      calls.push({
        source: `${sf.fileName}:${pos.line + 1}:${pos.character + 1}`,
        op: "node:assert/strict.equal",
        status: "passed",
      });
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return collector.finish([
    { id: "runtime-T", testKey: "T", phases: phases ? phases(calls) : calls },
  ]);
}

test("checked selectors prefer the complete site but never guess between equal siblings", () => {
  const source =
    "// observes: src/core.ts#format typeof arg === 'object'; check value\nassert.equal(value, 'hello');";
  const condition = {
    id: "condition",
    file: "src/core.ts",
    owner: "format",
    kind: "decision",
    category: "condition",
    text: "typeof arg === 'object'",
  };
  const outer = {
    ...condition,
    id: "outer",
    kind: "effect",
    category: "return",
    text: "args.map(arg => { if (typeof arg === 'object') return inspect(arg); return arg; })",
  };
  assert.deepEqual(
    collect(source, undefined, [outer, condition])[0].candidateSites,
    ["condition"],
  );
  assert.equal(
    collect(source, undefined, [
      outer,
      condition,
      { ...condition, id: "sibling" },
    ])[0].issue,
    "ambiguous-target",
  );
});

test("pragma targets are unique, local and explicit; via is explanatory only", () => {
  const hint = collect(
    "// observes: src/core.ts#compute return value via result\nassert.equal(compute(), 4);",
  )[0];
  assert.deepEqual(hint.candidateSites, ["one"]);
  assert.deepEqual(hint.target, {
    file: "src/core.ts",
    function: "compute",
    snippet: "return value",
    via: "result",
  });
  assert.equal(hint.issue, undefined);
  assert.equal(hint.witness, "passed");
  assert.equal(hint.assertionSource, "tests/core.test.ts:2:1");
  assert.equal(
    collect(
      "// observes: src/core.ts#compute via result\nassert.equal(compute(), 4);",
    )[0].target.via,
    "result",
  );
  for (const [target, issue] of [
    ["src/core.ts", "invalid-syntax"],
    ["/src/core.ts#compute", "invalid-target-path"],
    ["../src/core.ts#compute", "invalid-target-path"],
    ["src/../src/core.ts#compute", "invalid-target-path"],
    ["src\\core.ts#compute", "invalid-target-path"],
    ["src/core.ts#missing", "target-not-in-inventory"],
    ["src/core.ts#compute nonexistent snippet", "target-not-in-inventory"],
    ["src/core.ts#choose", "ambiguous-target"],
  ])
    assert.equal(
      collect(`// observes: ${target}\nassert.equal(compute(), 4);`)[0].issue,
      issue,
      target,
    );
  assert.deepEqual(
    collect(
      "// observes: src/core.ts#choose return 2\nassert.equal(choose(), 2);",
    )[0].candidateSites,
    ["three"],
  );
});

test("comments cannot silently attach to an arbitrary assertion or hide invalid syntax", () => {
  assert.equal(
    collect(
      "// observes: src/core.ts#compute\nconst value = compute();\nassert.equal(value, 4);",
    )[0].issue,
    "unattached-assertion",
  );
  assert.equal(
    collect(
      "// observes: src/core.ts#compute\n(assert.equal(4,4), assert.equal(4,4));",
    )[0].issue,
    "ambiguous-assertion",
  );
  assert.equal(
    collect(
      "// observes: src/core.ts#compute\nif (true) { assert.equal(4, 4); }",
    )[0].issue,
    "unattached-assertion",
  );
  assert.equal(
    collect("assert.equal(4,4);\n// observes: src/core.ts#compute")[0].issue,
    "unattached-assertion",
  );
  assert.deepEqual(
    collect(
      "const text = '// observes: src/core.ts#compute'; assert.equal(4,4);",
    ),
    [],
  );
});

test("pragma witnesses use exact position, operation and every call outcome", () => {
  const source =
    "// observes: src/core.ts#compute\nassert.equal(compute(), 4);";
  for (const [change, reason] of [
    [() => undefined, "capture-unavailable"],
    [() => [], "call-not-recorded"],
    [
      (p) => [{ ...p[0], source: "tests/core.test.ts:2:9" }],
      "call-not-recorded",
    ],
    [(p) => [{ ...p[0], op: "node:assert/strict.ok" }], "call-not-recorded"],
    [(p) => [{ ...p[0], status: "failed" }], "call-failed"],
    [(p) => [{ ...p[0], status: "running" }], "call-incomplete"],
    [(p) => [...p, { ...p[0], status: "failed" }], "mixed-call-outcomes"],
  ]) {
    const hint = collect(source, change)[0];
    assert.equal(hint.witness, "unavailable", reason);
    assert.equal(hint.witnessIssue, reason);
  }
});

test("a missing-call recipe is explicit and does not reinterpret explanatory via text", () => {
  const hint = collect(
    "// observes: src/core.ts#log console.log; check missing call\nassert.equal(compute(), 4);",
  )[0];
  assert.equal(hint.check, "missing-call");
  assert.equal(hint.target.snippet, "console.log");
  assert.equal(hint.issue, undefined);
  assert.equal(
    collect(
      "// observes: src/core.ts#compute via missing call\nassert.equal(compute(), 4);",
    )[0].check,
    undefined,
  );
  assert.equal(
    collect(
      "// observes: src/core.ts#compute; check anything\nassert.equal(compute(), 4);",
    )[0].issue,
    "unsupported-check-recipe",
  );
});
