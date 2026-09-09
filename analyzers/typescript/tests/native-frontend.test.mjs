import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import {
  readFileSync,
  writeFileSync,
  mkdtempSync,
  mkdirSync,
  rmSync,
  symlinkSync,
} from "node:fs";
import { tmpdir } from "node:os";
import ts from "typescript";
import { input } from "./fixture-input.mjs";
import { analyze, analyzeWithFrontend } from "../dist/analyze.js";
import { nativeFrontend } from "../dist/native-frontend.js";

const require = createRequire(new URL("../package.json", import.meta.url));
const entry = require.resolve("typescript-native");
assert.equal(
  require(entry).version,
  "7.0.2",
  "native calibration must use the pinned compiler",
);
const enabled = true;

test(
  "native 7.0.2 parser/checker preserves the controlled source-to-witness facts",
  { skip: !enabled },
  (t) => {
    const setup = input(t);
    const expected = analyze(setup);
    const frontend = nativeFrontend(entry, setup.projectRoot);
    t.after(() => frontend.close());
    const actual = analyzeWithFrontend(setup, frontend);
    assert.deepEqual(actual.facts, expected.facts);
    assert.deepEqual(actual.pragmas, expected.pragmas);
    assert.equal(actual.diagnostics.compilerVersion, "7.0.2");
    assert.equal(actual.diagnostics.compilerFrontend, "typescript-native-7");
    const phaseFile = resolve(setup.inputDirectory, "cov/T1.phases.json");
    const original = readFileSync(phaseFile, "utf8");
    writeFileSync(phaseFile, "[]");
    const missing = analyzeWithFrontend(setup, frontend);
    assert.equal(missing.facts.tests[0].observations.length, 0);
    assert.equal(
      missing.facts.tests[0].witnessIssues[0].kind,
      "call-not-recorded",
    );
    writeFileSync(phaseFile, original);
    assert.deepEqual(
      analyzeWithFrontend(setup, frontend).facts,
      expected.facts,
    );
    frontend.close();
    frontend.close();
    assert.throws(() => frontend.openProgram(setup), /closed/);
  },
);

function project(t) {
  const root = mkdtempSync(resolve(tmpdir(), "supercov native ü-"));
  mkdirSync(resolve(root, "src"));
  const frontend = nativeFrontend(entry, root);
  t.after(() => {
    frontend.close();
    rmSync(root, { recursive: true, force: true });
  });
  return { root, frontend };
}

test(
  "native parsing preserves UTF-16 offsets, CRLF, trivia and multiple files",
  { skip: !enabled },
  (t) => {
    const { frontend } = project(t);
    for (const [file, text] of [
      [
        "src/a.ts",
        "// 🦊 café\r\nexport async function f(value: number) { return value + 1; }\r\n",
      ],
      [
        "src/b.ts",
        "export function g() { for (let i=0; i<2; i++) { if (i) return 'yes'; } return 'no'; }",
      ],
    ]) {
      const old = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
      const next = frontend.parseSource(file, text);
      function tree(api, source) {
        const result = [];
        const visit = (node) => {
          // TS7 renamed only this terminal token; keep every node and offset.
          const name = api.SyntaxKind[node.kind];
          result.push([
            name === "EndOfFile" ? "EndOfFileToken" : name,
            node.getStart(source),
            node.getEnd(),
            node.getText(source),
          ]);
          api.forEachChild(node, visit);
        };
        visit(source);
        return result;
      }
      assert.deepEqual(tree(frontend.syntax, next), tree(ts, old));
      for (let offset = 0; offset <= text.length; offset++)
        assert.deepEqual(
          next.getLineAndCharacterOfPosition(offset),
          old.getLineAndCharacterOfPosition(offset),
        );
    }
  },
);

test(
  "native symbols resolve import aliases, shorthand values, and signature parameters",
  { skip: !enabled },
  (t) => {
    const { root, frontend } = project(t);
    writeFileSync(
      resolve(root, "tsconfig.json"),
      JSON.stringify({
        compilerOptions: { module: "NodeNext", moduleResolution: "NodeNext" },
        include: ["src/**/*.ts"],
      }),
    );
    writeFileSync(
      resolve(root, "src/a.ts"),
      "export function compute(value: number) { return value + 1; }",
    );
    writeFileSync(
      resolve(root, "src/b.ts"),
      "import { compute as alias } from './a.js'; const box = { alias }; alias(3);",
    );
    const program = frontend.openProgram({ projectRoot: root });
    const source = program.files.find((f) => f.fileName.endsWith("/src/b.ts"));
    const nodes = [];
    const visit = (node) => {
      nodes.push(node);
      frontend.syntax.forEachChild(node, visit);
    };
    visit(source);
    const shorthand = nodes.find(frontend.syntax.isShorthandPropertyAssignment);
    const symbol = program.checker.getShorthandAssignmentValueSymbol(shorthand);
    const declaration =
      program.checker.getAliasedSymbol(symbol).valueDeclaration;
    assert.equal(declaration.name.text, "compute");
    assert.ok(declaration.getSourceFile().fileName.endsWith("/src/a.ts"));
    const call = nodes.find(frontend.syntax.isCallExpression);
    assert.equal(
      program.checker.getResolvedSignature(call).getParameters()[0].name,
      "value",
    );
    assert.ok(
      program.resolveModule("./a.js", source.fileName).endsWith("/src/a.ts"),
    );
    assert.equal(
      program.resolveModule("./missing.js", source.fileName),
      undefined,
    );
    assert.ok(
      [...frontend.limitations].some((s) => s.includes("./missing.js")),
    );
  },
);

test(
  "native frontend preserves explicit ambient types through an extended config",
  { skip: !enabled },
  (t) => {
    const { root, frontend } = project(t);
    mkdirSync(resolve(root, "node_modules/@types"), { recursive: true });
    symlinkSync(
      resolve(import.meta.dirname, "../node_modules/@types/node"),
      resolve(root, "node_modules/@types/node"),
      "junction",
    );
    writeFileSync(
      resolve(root, "base.json"),
      JSON.stringify({
        compilerOptions: { types: ["node"], target: "ES2022" },
      }),
    );
    writeFileSync(
      resolve(root, "tsconfig.json"),
      JSON.stringify({ extends: "./base.json", include: ["src/**/*.ts"] }),
    );
    writeFileSync(
      resolve(root, "src/a.ts"),
      "export const timer = setTimeout(() => {}, 1);",
    );
    assert.ok(
      frontend
        .openProgram({ projectRoot: root })
        .files.some((f) =>
          f.fileName.replaceAll("\\\\", "/").includes("@types/node"),
        ),
    );
  },
);

test("invalid native configuration fails closed and the compiler can be closed afterward", (t) => {
  const { root, frontend } = project(t);
  writeFileSync(
    resolve(root, "tsconfig.json"),
    '{ "compilerOptions": { "unknownCompilerFlag": true }, "files": ["src/a.ts"] }',
  );
  writeFileSync(resolve(root, "src/a.ts"), "export const value = 1;");
  assert.throws(
    () => frontend.openProgram({ projectRoot: root }),
    /configuration is invalid|unknownCompilerFlag/,
  );
  frontend.close();
  assert.throws(() => frontend.openProgram({ projectRoot: root }), /closed/);
});
