import test from "node:test";
import assert from "node:assert/strict";
import ts from "typescript";
import { dirname, resolve } from "node:path";
import { analyzeDirectReturnSensitivity } from "../dist/mock-counts.js";

const sourceFile = "/checked-direct-return/src/value.ts";
const testFile = "/checked-direct-return/tests/core.test.ts";
const source = `export const select = ({ items }: { items: string[] | undefined }) => {
  if (!items) return false;
  if (items.length === 0) return '*';
  const first = items.map(item => opaque(item));
  if (first.includes('*')) return '*';
  return first.map(item => anotherOpaque(item));
};`;
const tests = `import { test } from 'node:test';
import assert from 'node:assert/strict';
import { select } from '../src/value.js';
test('ordinary return values', () => {
  assert.equal(select({ items: undefined }), false);
  assert.equal(select({ items: [] }), '*');
});`;

function check({
  production = source,
  suite = tests,
  index = 1,
  target = "condition",
} = {}) {
  const texts = new Map([
    [sourceFile, production],
    [testFile, suite],
  ]);
  const options = { noLib: true, noEmit: true, module: ts.ModuleKind.ESNext };
  const host = ts.createCompilerHost(options);
  host.fileExists = (name) => texts.has(name);
  host.readFile = (name) => texts.get(name);
  host.getSourceFile = (name) =>
    texts.has(name)
      ? ts.createSourceFile(name, texts.get(name), ts.ScriptTarget.Latest, true)
      : undefined;
  host.resolveModuleNames = (names, from) =>
    names.map((name) => {
      const path = resolve(dirname(from), name).replace(/\.js$/, ".ts");
      return texts.has(path)
        ? { resolvedFileName: path, extension: ts.Extension.Ts }
        : undefined;
    });
  const program = ts.createProgram([...texts.keys()], options, host);
  assert.equal(program.getSyntacticDiagnostics().length, 0);
  const checker = program.getTypeChecker();
  const raw = (node) => checker.getSymbolAtLocation(node)?.declarations?.[0];
  const declaration = (node) => {
    let symbol =
      ts.isIdentifier(node) && ts.isShorthandPropertyAssignment(node.parent)
        ? checker.getShorthandAssignmentValueSymbol(node.parent)
        : checker.getSymbolAtLocation(node);
    if (symbol?.flags & ts.SymbolFlags.Alias)
      symbol = checker.getAliasedSymbol(symbol);
    return symbol?.valueDeclaration ?? symbol?.declarations?.[0];
  };
  const imported = (node, member, module) => {
    const d = raw(node);
    return (
      d &&
      ts.isImportSpecifier(d) &&
      (d.propertyName ?? d.name).text === member &&
      d.parent.parent.parent.moduleSpecifier.text === module
    );
  };
  const nativeTest = (call) => imported(call.expression, "test", "node:test");
  const nativePredicate = (call) => {
    if (!ts.isPropertyAccessExpression(call.expression)) return;
    const d = raw(call.expression.expression);
    if (
      !d ||
      !ts.isImportClause(d) ||
      d.parent.moduleSpecifier.text !== "node:assert/strict"
    )
      return;
    return { equal: "node-same-value", deepEqual: "node-deep-strict-equality" }[
      call.expression.name.text
    ];
  };
  const sf = program.getSourceFile(testFile),
    prod = program.getSourceFile(sourceFile);
  const registration = sf.statements
    .filter(ts.isExpressionStatement)
    .map((s) => s.expression)
    .find(
      (n) =>
        ts.isCallExpression(n) &&
        nativeTest(n) &&
        n.arguments[0]?.text === "ordinary return values",
    );
  const fn = registration.arguments[1];
  const assertions = [];
  const nodes = [];
  const walk = (node, f) => {
    f(node);
    ts.forEachChild(node, (child) => walk(child, f));
  };
  walk(fn, (n) => {
    if (ts.isCallExpression(n) && nativePredicate(n)) assertions.push(n);
  });
  walk(prod, (n) => {
    if (
      target === "condition" &&
      ts.isBinaryExpression(n) &&
      n.operatorToken.kind === ts.SyntaxKind.EqualsEqualsEqualsToken
    )
      nodes.push(n);
    if (
      target === "literal" &&
      ts.isReturnStatement(n) &&
      n.expression?.kind === ts.SyntaxKind.FalseKeyword
    )
      nodes.push(n);
  });
  assert.equal(nodes.length, 1);
  const location = (node) => {
    const file = node.getSourceFile(),
      p = file.getLineAndCharacterOfPosition(node.getStart());
    return `${file.fileName.replace("/checked-direct-return/", "")}:${p.line + 1}:${p.character + 1}`;
  };
  return analyzeDirectReturnSensitivity(ts, fn, assertions[index], nodes[0], {
    declaration,
    location,
    nativeTest,
    nativePredicate,
    production: (node) => node.getSourceFile() === prod,
    nativeMock: () => false,
    globalConsole: () => false,
    site: location,
  });
}

test("direct returns reuse value evaluation without requiring a mock history", () => {
  const result = check();
  assert.equal(result.status, "source-checked", result.reason);
  assert.equal(result.original.actual.value, "*");
  assert.equal(result.original.targetEvaluations, 1);
  assert.deepEqual(
    result.variants.map((v) => [v.change, v.status, v.check?.outcome]),
    [
      ["condition-true", "source-checked", "not-rejected"],
      ["condition-false", "source-checked", "rejected"],
      ["condition-inverted", "source-checked", "rejected"],
    ],
  );
  assert.deepEqual(result.variants[1].check.actual, {
    kind: "array",
    properties: [],
  });
  const first = check({ index: 0, target: "literal" });
  assert.equal(first.status, "source-checked", first.reason);
  assert.equal(first.variants[0].change, "boolean-literal-inverted");
  assert.equal(first.variants[0].check.outcome, "rejected");
});

test("immutable local aliases retain the source call; transformed operands do not", () => {
  const alias = check({
    suite: tests.replace(
      "assert.equal(select({ items: [] }), '*');",
      "const value = select({ items: [] }); const alias = value; assert.equal(alias, '*');",
    ),
  });
  assert.equal(alias.status, "source-checked", alias.reason);
  for (const replacement of [
    "assert.equal((select({ items: [] }), '*'), '*');",
    "const value = select({ items: [] }); assert.equal(value === '*' ? '*' : '*', '*');",
  ])
    assert.equal(
      check({
        suite: tests.replace(
          "assert.equal(select({ items: [] }), '*');",
          replacement,
        ),
      }).status,
      "unresolved",
    );
});

test("a target reached by an earlier call cannot supply the selected call's path", () => {
  const result = check({ target: "literal" });
  assert.equal(result.status, "unresolved");
  assert.match(result.reason, /target-not-evaluated-by-selected-call/);
});

test("unsupported earlier effects and setup remain limits, not implied purity", () => {
  const controls = [
    { production: source.replace("if (!items)", "opaque(); if (!items)") },
    { production: "const state = {};\n" + source },
    { production: "import './setup.js';\n" + source },
    { suite: tests + "\nArray.prototype.includes = () => true;" },
    { suite: tests + "\nimport './setup.js';" },
    {
      suite: tests.replace(
        "test('ordinary",
        "test('earlier', () => {}); test('ordinary",
      ),
    },
    {
      suite: tests.replace(
        "assert.equal(select({ items: [] }), '*');",
        "opaque(); assert.equal(select({ items: [] }), '*');",
      ),
    },
    { suite: tests.replace("{ items: [] }", "{ get items() { return []; } }") },
    { suite: tests.replace("{ items: [] }", "{}") },
    { suite: tests.replace("{ items: [] }", "{ items: ['value'] }") },
    {
      suite: tests.replace(
        "assert.equal(select({ items: undefined }), false);",
        "assert.equal(select({ items: undefined }), true);",
      ),
    },
  ];
  for (const control of controls)
    assert.equal(check(control).status, "unresolved", JSON.stringify(control));
});
