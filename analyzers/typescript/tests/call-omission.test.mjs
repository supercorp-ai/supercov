import test from "node:test";
import assert from "node:assert/strict";
import ts from "typescript";
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import {
  analyzeFirstTestOmission,
  analyzeMockCounts,
} from "../dist/mock-counts.js";

const root = resolve(import.meta.dirname, "fixtures/call-omission");
const testFile = resolve(root, "tests/core.test.mjs");
const sourceFile = resolve(root, "src/logger.mjs");
const originalTest = readFileSync(testFile, "utf8");
const originalSource = readFileSync(sourceFile, "utf8");

function check(
  testText = originalTest,
  sourceText = originalSource,
  targetName = "log",
) {
  const texts = new Map([
    [testFile, testText],
    [sourceFile, sourceText],
  ]);
  const options = {
    noLib: true,
    allowJs: true,
    noEmit: true,
    module: ts.ModuleKind.ESNext,
  };
  const host = ts.createCompilerHost(options);
  host.fileExists = (p) => texts.has(p);
  host.readFile = (p) => texts.get(p);
  host.getSourceFile = (p) =>
    texts.has(p)
      ? ts.createSourceFile(p, texts.get(p), ts.ScriptTarget.Latest, true)
      : undefined;
  host.resolveModuleNames = (names, from) =>
    names.map((name) => {
      const file = resolve(dirname(from), name);
      return texts.has(file)
        ? { resolvedFileName: file, extension: ts.Extension.Mjs }
        : undefined;
    });
  const program = ts.createProgram([...texts.keys()], options, host);
  assert.equal(program.getSyntacticDiagnostics().length, 0);
  const checker = program.getTypeChecker();
  const declaration = (n) => {
    let s =
      ts.isIdentifier(n) && ts.isShorthandPropertyAssignment(n.parent)
        ? checker.getShorthandAssignmentValueSymbol(n.parent)
        : checker.getSymbolAtLocation(n);
    if (s?.flags & ts.SymbolFlags.Alias) s = checker.getAliasedSymbol(s);
    return s?.valueDeclaration ?? s?.declarations?.[0];
  };
  const location = (n) => {
    const sf = n.getSourceFile(),
      p = sf.getLineAndCharacterOfPosition(n.getStart());
    return `${sf.fileName.slice(root.length + 1)}:${p.line + 1}:${p.character + 1}`;
  };
  const sf = program.getSourceFile(testFile),
    production = program.getSourceFile(sourceFile);
  const registration = sf.statements.find(
    (s) =>
      ts.isExpressionStatement(s) &&
      ts.isCallExpression(s.expression) &&
      s.expression.arguments[0]?.text === "first logger count",
  ).expression;
  const fn = registration.arguments[1];
  let assertion;
  const findAssertion = (n) => {
    if (ts.isCallExpression(n) && n.expression.getText() === "assert.equal")
      assertion ??= n;
    ts.forEachChild(n, findAssertion);
  };
  findAssertion(fn.body);
  let target;
  const visit = (n) => {
    if (
      ts.isCallExpression(n) &&
      n.expression.getText() === `console.${targetName}`
    )
      target ??= n;
    ts.forEachChild(n, visit);
  };
  visit(production);
  const model = {
    declaration,
    location,
    production: (n) => n.getSourceFile() === production,
    site: (n) => location(n),
    globalConsole: (n) =>
      ts.isIdentifier(n) && n.text === "console" && !declaration(n),
    nativeMock: (n) => n.expression.getText() === "t.mock.method",
    nativePredicate: (n) =>
      n.expression.getText() === "assert.equal" ? "node-same-value" : undefined,
    nativeTest: (n) => n.expression.getText() === "test",
  };
  return {
    guided: analyzeFirstTestOmission(ts, fn, assertion, target, model),
    ordinary: analyzeMockCounts(ts, fn, model),
  };
}

test("guided omission checks a specified callback edit, without relaxing ordinary shared-state analysis", () => {
  for (const source of [
    originalSource,
    originalSource
      .replaceAll("identity", "preserveArgs")
      .replaceAll("prefix", "another prefix"),
  ]) {
    const { guided, ordinary } = check(originalTest, source);
    assert.equal(guided.status, "source-checked", guided.reason);
    assert.equal(guided.outcome, "rejected");
    assert.equal(guided.originalCount, 1);
    assert.equal(guided.omittedCount, 0);
    assert.match(ordinary.limitation, /shared-module-object-history/);
    assert.equal(ordinary.checks.size, 0);
  }
});

test("a selected slice can hide the missing call; local non-rejection is not whole-suite survival", () => {
  const source = originalTest
    .replace(
      "logger.error('oops');",
      "logger.error('oops'); console.log('other'); const selected = log.mock.calls.slice(0, 1);",
    )
    .replace(
      "assert.equal(log.mock.callCount(), 1);",
      "assert.equal(selected.length, 1);",
    );
  const result = check(source).guided;
  assert.equal(result.status, "source-checked", result.reason);
  assert.equal(result.outcome, "not-rejected");
  assert.equal(result.omittedCount, 1);
});

test("guidance cannot excuse setup, earlier callers, effects, wrong histories or unsupported predicates", () => {
  const variants = [
    [
      "earlier test",
      originalTest.replace(
        "test('first logger count'",
        "test('earlier', () => {}); test('first logger count'",
      ),
    ],
    [
      "top-level setup",
      originalTest.replace(
        "test('first logger count'",
        "getLogger().info = () => {}; test('first logger count'",
      ),
    ],
    ["unrelated import", "import './unrelated.mjs';\n" + originalTest],
    [
      "hook",
      originalTest.replace("const log =", "t.before(() => {}); const log ="),
    ],
    ["async callback", originalTest.replace("(t) => {", "async (t) => {")],
    [
      "test parameter initializer",
      originalTest.replace(
        "(t) => {",
        "(t, setup = (getLogger().info = () => console.log('replacement'))) => {",
      ),
    ],
    [
      "aliased mutation",
      originalTest.replace(
        "logger.info('hello', { a: 1 });",
        "const alias = logger; alias.info = () => {}; logger.info('hello', { a: 1 });",
      ),
    ],
    [
      "reset",
      originalTest.replace(
        "logger.error('oops');",
        "logger.error('oops'); log.mock.resetCalls();",
      ),
    ],
    [
      "constant operand",
      originalTest.replace(
        "assert.equal(log.mock.callCount(), 1);",
        "assert.equal(1, 1);",
      ),
    ],
    [
      "self comparison",
      originalTest.replace(
        "assert.equal(log.mock.callCount(), 1);",
        "assert.equal(log.mock.callCount(), log.mock.callCount());",
      ),
    ],
    [
      "caught assertion",
      originalTest.replace(
        "assert.equal(log.mock.callCount(), 1);",
        "try { assert.equal(log.mock.callCount(), 1); } catch {}",
      ),
    ],
    [
      "captured mutable state",
      originalTest,
      originalSource.replace(
        "const identity =",
        "let extra = 0; const identity =",
      ),
    ],
    [
      "native method mutation",
      originalTest,
      originalSource.replace("info: stdout()", "info: Array.prototype.pop"),
    ],
    [
      "module init effect",
      originalTest,
      originalSource + "\nconsole.log('setup');\n",
    ],
    [
      "runtime production import",
      originalTest,
      "import './side-effect.mjs';\n" + originalSource,
    ],
    [
      "callback default effects",
      originalTest,
      originalSource.replace(
        "(...args) => console.log",
        "(args = console.error()) => console.log",
      ),
    ],
  ];
  for (const [name, testText, sourceText] of variants) {
    const result = check(testText, sourceText).guided;
    assert.equal(
      result.status,
      "unresolved",
      `${name}: ${JSON.stringify(result)}`,
    );
    assert.equal(result.outcome, undefined, name);
  }
  const wrong = check(originalTest, originalSource, "error").guided;
  assert.equal(wrong.reason, "omission-target-not-in-selected-history");
});
