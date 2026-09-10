import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  symlinkSync,
  writeFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

const cases = [
  {
    name: "direct source callback throws",
    source: "export function run() { throw new Error('boom'); }",
    target: "throw new Error('boom');",
    assertion: "assert.throws(operation)",
    rejected: true,
  },
  {
    name: "normal return is ignored",
    source: "export function run() { return 101; }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    rejected: false,
  },
  {
    name: "return prevents a later exception",
    source: "export function run() { return 101; throw new Error('later'); }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    rejected: true,
    diagnostic: {
      basis: "native-error-message",
      message: { kind: "string", value: "later" },
    },
  },
  ...[
    [
      "primitive-derived Error message",
      "Error(42)",
      "native-error-message",
      { kind: "string", value: "42" },
    ],
    [
      "default Error message",
      "new Error()",
      "native-error-message",
      { kind: "string", value: "" },
    ],
    [
      "own string message",
      "{ message: 'boom', toString: () => process.exit(0) }",
      "own-primitive-message",
      { kind: "string", value: "boom" },
    ],
    [
      "own numeric message",
      "{ message: 42 }",
      "own-primitive-message",
      { kind: "number", value: 42 },
    ],
    [
      "own boolean message",
      "{ message: false }",
      "own-primitive-message",
      { kind: "boolean", value: false },
    ],
    [
      "own null message",
      "{ message: null }",
      "own-primitive-message",
      { kind: "null" },
    ],
    [
      "own undefined message",
      "{ message: undefined }",
      "own-primitive-message",
      { kind: "undefined" },
    ],
    [
      "absent object message",
      "{ toString: () => process.exit(0) }",
      "absent-message",
      { kind: "undefined" },
    ],
    [
      "absent array message",
      "[() => process.exit(0)]",
      "absent-message",
      { kind: "undefined" },
    ],
    [
      "absent function message",
      "() => process.exit(0)",
      "absent-message",
      { kind: "undefined" },
    ],
    ...["undefined", "null", "42", "false", "'boom'"].map((value) => [
      `thrown primitive ${value}`,
      value,
      "primitive-thrown-value",
      { kind: "undefined" },
    ]),
  ].map(([name, value, basis, message]) => ({
    name: `safe native failure diagnostic: ${name}`,
    source: `export function run() { return 101; throw ${value}; }`,
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    rejected: true,
    diagnostic: { basis, message },
  })),
  {
    name: "throwing assertion does not coerce the caught value's message",
    source:
      "export function run() { throw { message: { toString: () => process.exit(0) } }; }",
    target: "throw { message: { toString: () => process.exit(0) } };",
    assertion: "assert.throws(operation)",
    rejected: true,
  },
  {
    name: "failure message coercion can exit successfully before assertion rejection",
    source:
      "export function run() { return 101; throw { message: { toString: () => process.exit(0) } }; }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    unsupported: /completion-diagnostic-message-coercion-unresolved/,
    nativeOmission: { status: 0, reachedAfterAssertion: false },
  },
  {
    name: "benign nonprimitive diagnostic also needs a coercion proof",
    source:
      "export function run() { return 101; throw { message: { toString: () => 'boom' } }; }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    unsupported: /completion-diagnostic-message-coercion-unresolved/,
    nativeOmission: { status: 1, reachedAfterAssertion: false },
  },
  {
    name: "diagnostic getter is not assumed to be a data property",
    source:
      "export function run() { return 101; throw { get message() { process.exit(0); } }; }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    unsupported: /unsupported-object-member/,
    nativeOmission: { status: 0, reachedAfterAssertion: false },
  },
  {
    name: "a caught inner exception is not required",
    source:
      "export function run() { try { throw new Error('ignored'); } catch {} }",
    target: "throw new Error('ignored');",
    assertion: "assert.doesNotThrow(operation)",
    rejected: false,
  },
  {
    name: "another exception in the same owner supplies the witness",
    source:
      "export function run() { try { throw new Error('ignored'); } catch {} throw new Error('observed'); }",
    target: "throw new Error('ignored');",
    assertion: "assert.throws(operation)",
    rejected: false,
  },
  {
    name: "catch rethrows the original exception",
    source:
      "export function run() { try { throw new Error('boom'); } catch (error) { throw error; } }",
    target: "throw new Error('boom');",
    assertion: "assert.throws(operation)",
    rejected: true,
  },
  {
    name: "finally return suppresses the exception",
    source:
      "export function run() { try { throw new Error('ignored'); } finally { return 101; } }",
    target: "throw new Error('ignored');",
    assertion: "assert.doesNotThrow(operation)",
    rejected: false,
  },
  {
    name: "finally throw replaces the exception",
    source:
      "export function run() { try { throw new Error('ignored'); } finally { throw new Error('observed'); } }",
    target: "throw new Error('ignored');",
    assertion: "assert.throws(operation)",
    rejected: false,
  },
  {
    name: "factory returns the invoked callback",
    source:
      "export function run() { return () => { throw new Error('boom'); }; }",
    target: "throw new Error('boom');",
    assertion: "assert.throws(operation())",
    rejected: true,
  },
  {
    name: "factory catches a different exception before returning the callback",
    source:
      "export function run() { try { throw new Error('ignored'); } catch {} return () => { throw new Error('observed'); }; }",
    target: "throw new Error('ignored');",
    assertion: "assert.throws(operation())",
    rejected: false,
  },
  {
    name: "inline callback forwards an exception",
    source: "export function run() { throw new Error('boom'); }",
    target: "throw new Error('boom');",
    assertion: "assert.throws(() => { operation(); })",
    rejected: true,
  },
  {
    name: "test callback catches and replaces the exception",
    source: "export function run() { throw new Error('ignored'); }",
    target: "throw new Error('ignored');",
    assertion:
      "assert.throws(() => { try { operation(); } catch {} throw new Error('observed'); })",
    rejected: false,
  },
  {
    name: "undefined is a thrown value, not the no-exception sentinel",
    source: "export function run() { throw undefined; }",
    target: "throw undefined;",
    assertion: "assert.throws(operation)",
    rejected: true,
  },
  {
    name: "const alias keeps the callback identity",
    source: "export function run() { throw Error('boom'); }",
    target: "throw Error('boom');",
    setup: "const alias = operation;",
    assertion: "assert.throws(alias)",
    rejected: true,
  },
  {
    name: "renamed native assertion import",
    source: "export function run() { throw Error('boom'); }",
    target: "throw Error('boom');",
    imports: "import { throws as expectException } from 'node:assert/strict';",
    assertion: "expectException(operation)",
    rejected: true,
  },
  {
    name: "argument evaluation inside callback can throw",
    source:
      "export function run() { throw Error('argument'); }\nexport function consume(value) { return value; }",
    target: "throw Error('argument');",
    extraImport: ", consume",
    assertion: "assert.throws(() => consume(operation()))",
    rejected: true,
  },
  {
    name: "omission preserves a single-statement if body",
    source:
      "export function run() { if (true) throw Error('boom'); else return 101; }",
    target: "throw Error('boom');",
    assertion: "assert.throws(operation)",
    rejected: true,
  },
  {
    name: "factory exception cannot borrow the outer assertion witness",
    source: "export function run() { throw Error('factory'); }",
    target: "throw Error('factory');",
    assertion: "assert.throws(operation())",
    nativeFail: true,
  },
  {
    name: "error matcher needs its own predicate model",
    source: "export function run() { throw Error('boom'); }",
    target: "throw Error('boom');",
    assertion: "assert.throws(operation, /boom/)",
    unsupported: /completion-assertion-or-target-shape/,
  },
  {
    name: "async callback has no synchronous exception proof",
    source: "export async function run() { return 101; }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    unsupported: /unsupported-call-target/,
  },
  {
    name: "earlier test is outside the prefix model",
    source: "export function run() { throw Error('boom'); }",
    target: "throw Error('boom');",
    before: "test('earlier', () => {});",
    assertion: "assert.throws(operation)",
    unsupported: /setup-or-earlier-test/,
  },
  {
    name: "module setup is not assumed pure",
    source:
      "export function run() { throw Error('boom'); }\nconst shared = {};",
    target: "throw Error('boom');",
    assertion: "assert.throws(operation)",
    unsupported: /production-initialization/,
  },
  {
    name: "finally cannot conceal an evaluator limitation",
    source:
      "export function run() { try { Math.abs(-1); } finally { return 101; } }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    unsupported: /unresolved-value-binding|unsupported/,
  },
  {
    name: "catch cannot swallow an evaluator limitation",
    source:
      "export function run() { try { Math.abs(-1); } catch {} throw Error('boom'); }",
    target: "throw Error('boom');",
    assertion: "assert.throws(operation)",
    unsupported: /unresolved-value-binding|unsupported/,
  },
  {
    name: "mutation can reveal an unsupported continuation",
    source: "export function run() { return 101; Math.abs(-1); }",
    target: "return 101;",
    assertion: "assert.doesNotThrow(operation)",
    unsupported: /unresolved-value-binding|unsupported/,
  },
];

test(
  "completion guidance follows actual invocation and abrupt completion through ordinary archives",
  {
    skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
  },
  async (t) => {
    const repository = resolve(import.meta.dirname, "../../..");
    for (const c of cases)
      await t.test(c.name, () => {
        const root = mkdtempSync(resolve(tmpdir(), "supercov-completion-"));
        t.after(() => rmSync(root, { recursive: true, force: true }));
        for (const dir of ["src", "tests", "node_modules"])
          mkdirSync(resolve(root, dir));
        symlinkSync(
          resolve(
            repository,
            "analyzers/typescript/node_modules",
            process.env.SUPERCOV_ASSERTED_TEST_COMPILER === "7.0.2"
              ? "typescript-native"
              : "typescript",
          ),
          resolve(root, "node_modules/typescript"),
        );
        writeFileSync(
          resolve(root, "package.json"),
          JSON.stringify({ type: "module" }),
        );
        const sourcePath = resolve(root, "src/core.mjs");
        writeFileSync(sourcePath, c.source);
        const suite = `import test from 'node:test';
${c.imports ?? "import assert from 'node:assert/strict';"}
import { run as operation${c.extraImport ?? ""} } from '../src/core.mjs';
${c.before ?? ""}
test('checked completion', () => {
  ${c.setup ?? ""}
  // observes: src/core.mjs#run ${c.target}; check completion
  ${c.assertion};
  console.log('AFTER_ASSERTION');
});
${c.nativeFail ? "test('passing companion', () => assert.ok(true));" : ""}`;
        writeFileSync(resolve(root, "tests/core.test.mjs"), suite);
        const env = { ...process.env, SUPERCOV_PACKAGE_ROOT: repository };
        for (const key of [
          "NODE_OPTIONS",
          "NODE_PATH",
          "NODE_TEST_CONTEXT",
          "NODE_V8_COVERAGE",
        ])
          delete env[key];
        const run = (command, args, expected = 0) => {
          const r = spawnSync(command, args, {
            cwd: root,
            env,
            encoding: "utf8",
            timeout: 60000,
            maxBuffer: 16 * 1024 * 1024,
          });
          assert.equal(r.error, undefined);
          assert.equal(r.status, expected, r.stderr + r.stdout);
          return r.stdout;
        };
        const args = [
          "--test",
          "--test-concurrency=1",
          "--test-reporter=tap",
          "tests/core.test.mjs",
        ];
        const checkAfterAssertion = (output, reached) =>
          assert.equal(output.includes("AFTER_ASSERTION"), reached, output);
        checkAfterAssertion(
          run(process.execPath, args, c.nativeFail ? 1 : 0),
          !c.nativeFail,
        );
        const binary = resolve(repository, "target/debug/supercov");
        run(binary, ["--", process.execPath, ...args], c.nativeFail ? 1 : 0);
        const [runId] = readdirSync(resolve(root, ".supercov/runs"));
        const query = (extra) =>
          JSON.parse(
            run(binary, ["runs", runId, "assertions", ...extra, "--json"]),
          ).data;
        const report = query(["--pragmas"]);
        assert.equal(report.pragmas.length, 1, JSON.stringify(report));
        const p = report.pragmas[0];
        const checkNativeOmission = ({ status, reachedAfterAssertion }) => {
          assert.equal(c.source.split(c.target).length, 2);
          writeFileSync(sourcePath, c.source.replace(c.target, ";"));
          checkAfterAssertion(
            run(process.execPath, args, status),
            reachedAfterAssertion,
          );
          // Comments must never be responsible for a native result.
          writeFileSync(
            resolve(root, "tests/core.test.mjs"),
            suite.replace(/^.*\/\/ observes:.*$/m, ""),
          );
          checkAfterAssertion(
            run(process.execPath, args, status),
            reachedAfterAssertion,
          );
        };
        if (c.nativeFail) {
          assert.notEqual(p.hint.witness, "passed");
          assert.notEqual(p.validation, "analyzer-supported");
          return;
        }
        assert.equal(p.hint.witness, "passed", JSON.stringify(p));
        if (c.unsupported) {
          assert.equal(p.validation, "unresolved", JSON.stringify(p));
          assert.match(
            p.hint.completionSensitivity?.reason ?? p.reason,
            c.unsupported,
          );
          if (c.nativeOmission) checkNativeOmission(c.nativeOmission);
          return;
        }
        assert.equal(p.validation, "analyzer-supported", JSON.stringify(p));
        assert.equal(p.reason, "modeled-completion-sensitivity");
        const e = p.hint.completionSensitivity;
        assert.equal(e.scope, "first-synchronous-test-prefix");
        assert.equal(e.change, "statement-omitted");
        assert.equal(e.original.outcome, "not-rejected");
        assert.equal(
          e.omitted.outcome,
          c.rejected ? "rejected" : "not-rejected",
        );
        assert.equal(e.original.targetEvaluations, 1);
        assert.equal(e.omitted.targetEvaluations, 1);
        assert.equal(e.original.diagnostic, undefined);
        assert.deepEqual(e.omitted.diagnostic, c.diagnostic);
        // A checked exact edit remains separate from automatic per-site credit.
        const ordinary = query([
          "--analysis",
          report.analysisId,
          "--limit",
          "100",
        ]);
        assert.equal(ordinary.assertionScore, null);
        const selected = ordinary.sites.find(
          (s) => s.site.id === p.hint.candidateSites[0],
        );
        assert.equal(selected.candidate.status, "unresolved");
        assert.equal(selected.candidate.reason.kind, "limit:operand-shape");
        checkNativeOmission({
          status: c.rejected ? 1 : 0,
          reachedAfterAssertion: !c.rejected,
        });
      });
  },
);
