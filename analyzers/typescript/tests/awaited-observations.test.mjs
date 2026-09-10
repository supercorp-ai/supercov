import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import ts from "typescript";
import { awaitedObservationSource } from "../dist/awaited-observations.js";
import { collectPragmas } from "../dist/pragmas.js";

const root = resolve(import.meta.dirname, "fixtures/awaited-capture");
const testFile = resolve(root, "tests/core.test.mjs");
const helperFile = resolve(root, "tests/process.mjs");
const originalTest = readFileSync(testFile, "utf8"),
  originalHelper = readFileSync(helperFile, "utf8");
const relativeFile = (sf) =>
  sf.fileName.slice(root.length + 1).replaceAll("\\", "/");
function analyzeCaller(
  testText = originalTest,
  helperText = originalHelper,
  title = "awaited child capture",
) {
  const texts = new Map([
    [testFile, testText],
    [helperFile, helperText],
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
  host.resolveModuleNames = (names, containing) =>
    names.map((name) => {
      const file = resolve(dirname(containing), name);
      return texts.has(file)
        ? { resolvedFileName: file, extension: ts.Extension.Mjs }
        : undefined;
    });
  const program = ts.createProgram([...texts.keys()], options, host),
    checker = program.getTypeChecker();
  assert.equal(program.getSyntacticDiagnostics().length, 0);
  const sf = program.getSourceFile(testFile);
  const raw = (n) => checker.getSymbolAtLocation(n)?.declarations?.[0];
  const decl = (n) => {
    let symbol =
      ts.isIdentifier(n) && ts.isShorthandPropertyAssignment(n.parent)
        ? checker.getShorthandAssignmentValueSymbol(n.parent)
        : checker.getSymbolAtLocation(n);
    if (symbol?.flags & ts.SymbolFlags.Alias)
      symbol = checker.getAliasedSymbol(symbol);
    return symbol?.valueDeclaration ?? symbol?.declarations?.[0];
  };
  let selected;
  const visit = (n) => {
    if (ts.isCallExpression(n) && n.arguments[0]?.text === title)
      selected = n.arguments.at(-1);
    ts.forEachChild(n, visit);
  };
  visit(sf);
  assert.ok(selected);
  const calls = [];
  const collect = (n) => {
    if (
      ts.isCallExpression(n) &&
      ts.isPropertyAccessExpression(n.expression) &&
      n.expression.name.text === "settled"
    )
      calls.push(n);
    ts.forEachChild(n, collect);
  };
  collect(selected);
  const call = calls[0];
  assert.ok(call);
  const model = awaitedObservationSource(
    { syntax: ts, declaration: decl, rawDeclaration: raw, relativeFile },
    selected,
    call,
  );
  return { model, sf, call };
}

test("native capture/poll source recognition uses bindings, not helper or receiver names", () => {
  for (const [name, testText, helperText, title] of [
    ["first caller", originalTest, originalHelper],
    [
      "second caller",
      originalTest,
      originalHelper,
      "second caller of the same helper",
    ],
    [
      "renamed receiver",
      originalTest.replaceAll("processHandle", "server"),
      originalHelper,
    ],
    [
      "renamed factory",
      originalTest.replace("openWorker as", "newName as"),
      originalHelper.replace("function openWorker", "function newName"),
    ],
    [
      "renamed buffers",
      originalTest,
      originalHelper
        .replaceAll("captured", "stdoutText")
        .replaceAll("diagnostic", "stderrText"),
    ],
    [
      "different timeout and regex",
      originalTest,
      originalHelper
        .replace("+ 5000", "+ 1234")
        .replace("pause(5)", "pause(17)")
        .replace("/Listening on port/", "/Server ready/i"),
    ],
    [
      "one stream",
      originalTest,
      originalHelper.replace(".test(captured + diagnostic)", ".test(captured)"),
    ],
    [
      "block returned method",
      originalTest,
      originalHelper.replace(
        "settled: () => eventually(() => /Listening on port/.test(captured + diagnostic), 'startup')",
        "settled: () => { return eventually(() => /Listening on port/.test(captured + diagnostic), 'startup'); }",
      ),
    ],
    [
      "renamed native test import",
      originalTest
        .replace("{ test }", "{ test as it }")
        .replaceAll("test('", "it('"),
      originalHelper,
    ],
  ]) {
    const { model } = analyzeCaller(testText, helperText, title);
    assert.equal(model?.model, "node-child-capture-poll-v1", name);
    assert.ok(model.factorySource.startsWith("tests/process.mjs:"));
    assert.ok(model.predicateSource.startsWith("tests/process.mjs:"));
    assert.equal(model.captures[0].stream, "stdout");
    assert.equal(model.captures.length, name === "one stream" ? 1 : 2);
  }
});

test("unsupported caller and helper shapes do not become source-supported observations", () => {
  const helperChanges = [
    [
      "different native spawn",
      (s) => s.replace("spawn as launch", "exec as launch"),
    ],
    ["non-pipe capture", (s) => s.replace("stdio: 'pipe'", "stdio: 'inherit'")],
    [
      "computed stdio option",
      (s) => s.replace("stdio: 'pipe'", "['stdio']: 'pipe'"),
    ],
    [
      "overwritten buffer",
      (s) => s.replace("captured += data", "captured = data"),
    ],
    [
      "transformed buffer",
      (s) => s.replace("captured += data", "captured += data.toUpperCase()"),
    ],
    [
      "transformed predicate",
      (s) =>
        s.replace(
          ".test(captured + diagnostic)",
          ".test(captured.replace('bad', 'Listening on port') + diagnostic)",
        ),
    ],
    [
      "constant predicate",
      (s) =>
        s.replace(".test(captured + diagnostic)", ".test('Listening on port')"),
    ],
    [
      "stateful regex",
      (s) => s.replace("/Listening on port/", "/Listening on port/g"),
    ],
    [
      "early loop return",
      (s) => s.replace("const until", "return; const until"),
    ],
    ["loop break", (s) => s.replace("await pause(5)", "break")],
    [
      "replaced predicate",
      (s) => s.replace("while (!check())", "while (false)"),
    ],
    [
      "non-native delay",
      (s) => s.replace("setTimeout as pause", "setImmediate as pause"),
    ],
    ["shadowed Date", (s) => "const Date = { now: () => 1 };\n" + s],
    [
      "discarded polling result",
      (s) =>
        s.replace(
          "settled: () => eventually",
          "settled: () => void eventually",
        ),
    ],
    [
      "swallowed polling failure",
      (s) => s.replace("'startup')", "'startup').catch(() => {})"),
    ],
    [
      "method override",
      (s) =>
        s.replace(
          "    text: () => captured,",
          "    text: () => captured, ['set' + 'tled']: async () => {},",
        ),
    ],
    [
      "escaped buffer closure",
      (s) =>
        s.replace(
          "context.after(() => worker.kill());",
          "context.after(() => worker.kill()); unknown(() => captured);",
        ),
    ],
    [
      "decoder changed",
      (s) =>
        s.replace(
          "context.after(() => worker.kill());",
          "worker.stdout.setEncoding('hex'); context.after(() => worker.kill());",
        ),
    ],
    ["wrong stream child", (s) => s.replace("worker.stderr", "other.stderr")],
    [
      "escaped child",
      (s) =>
        s.replace(
          "context.after(() => worker.kill());",
          "unknown(worker); context.after(() => worker.kill());",
        ),
    ],
    [
      "computed child stream escape",
      (s) =>
        s.replace(
          "context.after(() => worker.kill());",
          "unknown(worker['stdout']); context.after(() => worker.kill());",
        ),
    ],
    [
      "optional stream listener",
      (s) => s.replace(".on('data'", ".on?.('data'"),
    ],
    [
      "factory default parameter",
      (s) =>
        s.replace("openWorker(context)", "openWorker(context = unknown())"),
    ],
    [
      "factory async",
      (s) => s.replace("export function", "export async function"),
    ],
  ];
  for (const [name, change] of helperChanges) {
    assert.notEqual(change(originalHelper), originalHelper, name);
    assert.equal(
      analyzeCaller(originalTest, change(originalHelper)).model,
      undefined,
      name,
    );
  }
  for (const [name, change] of [
    [
      "misleading test import",
      (s) => s.replace("{ test }", "{ describe as test }"),
    ],
    [
      "mutable receiver",
      (s) => s.replace("const processHandle", "let processHandle"),
    ],
    [
      "detached call",
      (s) =>
        s.replace("await processHandle.settled()", "processHandle.settled()"),
    ],
    [
      "caught failure",
      (s) =>
        s.replace(
          "await processHandle.settled();",
          "try { await processHandle.settled(); } catch {}",
        ),
    ],
    [
      "repeated call",
      (s) =>
        s.replace(
          "await processHandle.settled();",
          "await processHandle.settled(); await processHandle.settled();",
        ),
    ],
    [
      "receiver escape",
      (s) =>
        s.replace(
          "assert.match(processHandle.text()",
          "unknown(processHandle); assert.match(processHandle.text()",
        ),
    ],
    [
      "receiver override",
      (s) =>
        s.replace(
          "await processHandle.settled();",
          "processHandle.settled = async () => {}; await processHandle.settled();",
        ),
    ],
    [
      "optional call",
      (s) => s.replace("processHandle.settled()", "processHandle.settled?.()"),
    ],
    [
      "extra launch",
      (s) =>
        s.replace(
          "assert.match(processHandle.text()",
          "createChild(t); assert.match(processHandle.text()",
        ),
    ],
  ])
    assert.equal(analyzeCaller(change(originalTest)).model, undefined, name);
});

test("a source-supported awaited pragma cannot borrow an explicit assertion phase", () => {
  const { sf, call, model } = analyzeCaller();
  const collector = collectPragmas(ts, [sf], relativeFile, [
    {
      id: "target",
      file: "src/cli.mjs",
      fn: "start",
      owner: "start",
      text: "console.log('Listening on port 1234')",
    },
  ]);
  assert.equal(collector.hasHint(call), true);
  collector.register(call, "settled", "first", false, model);
  const source = `tests/core.test.mjs:${sf.getLineAndCharacterOfPosition(call.getStart()).line + 1}:9`;
  for (const phases of [
    undefined,
    [],
    [{ source, op: "node:assert/strict.settled", status: "passed" }],
  ]) {
    const [hint] = collector.finish([{ testKey: "first", id: "T", phases }]);
    assert.equal(hint.issue, undefined);
    assert.equal(hint.awaitedObservation.model, "node-child-capture-poll-v1");
    assert.equal(hint.witness, "unavailable");
    assert.equal(hint.witnessIssue, "observation-capture-unavailable");
  }
});
