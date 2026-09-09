import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import ts from "typescript";
const json = (file, data) => writeFileSync(file, JSON.stringify(data));
export function input(
  t,
  projectRoot = resolve(import.meta.dirname, "fixtures/basic"),
) {
  const path = mkdtempSync(resolve(tmpdir(), "supercov-ts-test-"));
  t.after(() => rmSync(path, { recursive: true, force: true }));
  mkdirSync(resolve(path, "cov"));
  const file = "src/core.ts",
    source = readFileSync(resolve(projectRoot, file), "utf8");
  const sf = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  const sites = [];
  const position = (offset) => {
    const p = sf.getLineAndCharacterOfPosition(offset);
    return { line: p.line + 1, column: p.character + 1 };
  };
  const add = (node, owner, kind, category, method) =>
    sites.push({
      id: `S${sites.length + 1}`,
      file,
      kind,
      category,
      classification: "contractual",
      start: position(node.getStart(sf)),
      end: position(node.getEnd()),
      pos: node.getStart(sf),
      endPos: node.getEnd(),
      text: node.getText(sf),
      fn: owner,
      owner,
      exported: true,
      ...(method ? { method } : {}),
    });
  const visit = (node, owner = "") => {
    if (ts.isFunctionDeclaration(node)) owner = node.name.text;
    if (ts.isReturnStatement(node)) add(node, owner, "effect", "return");
    if (ts.isIfStatement(node))
      add(node.expression, owner, "decision", "condition");
    if (
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      ["setTimeout", "clearTimeout"].includes(node.expression.text)
    )
      add(node, owner, "effect", "schedule", node.expression.text);
    if (
      ts.isCallExpression(node) &&
      node.expression.getText(sf) === "console.log"
    )
      add(node, owner, "effect", "log", "log");
    ts.forEachChild(node, (child) => visit(child, owner));
  };
  visit(sf);
  json(resolve(path, "inventory.json"), { sites });
  const testFile = "tests/core.test.ts",
    testSource = readFileSync(resolve(projectRoot, testFile), "utf8");
  const rows = [...testSource.matchAll(/test\('([^']+)'/g)].map(
    (match, index) => ({
      id: `T${index + 1}`,
      name: match[1],
      title: match[1],
      file: testFile,
      line: testSource.slice(0, match.index).split("\n").length,
      ok: true,
    }),
  );
  json(resolve(path, "cov/index.json"), rows);
  const testAst = ts.createSourceFile(
    testFile,
    testSource,
    ts.ScriptTarget.Latest,
    true,
  );
  for (const row of rows) {
    // Controlled witness fixtures: real-run ownership is exercised separately by
    // archive.integration.test.mjs, not implied by these synthetic phases.
    const phases = [];
    const nextLine = rows[rows.indexOf(row) + 1]?.line ?? Infinity;
    const visitAssertion = (node) => {
      const p = testAst.getLineAndCharacterOfPosition(node.getStart(testAst));
      if (
        ts.isCallExpression(node) &&
        node.expression.getText(testAst).startsWith("assert.") &&
        p.line + 1 >= row.line &&
        p.line + 1 < nextLine
      )
        phases.push({
          op: `node:assert/strict.${node.expression.name.text}`,
          source: `${testFile}:${p.line + 1}:${p.character + 1}`,
          status: "passed",
          fns: [],
          decs: [],
          stmts: 0,
        });
      ts.forEachChild(node, visitAssertion);
    };
    visitAssertion(testAst);
    json(resolve(path, `cov/${row.id}.phases.json`), phases);
    writeFileSync(
      resolve(path, `cov/${row.id}.lcov`),
      `SF:${file}\n` +
        source
          .split("\n")
          .map((_, i) => `DA:${i + 1},1`)
          .join("\n") +
        "\nend_of_record\n",
    );
    const condition = sites.find((s) => s.kind === "decision");
    json(resolve(path, `cov/${row.id}.outcomes.json`), {
      [`${file}:${condition.start.line}:${condition.start.column}#0`]:
        row.name === "enabled outcome"
          ? [1, 0]
          : row.name === "disabled outcome"
            ? [0, 1]
            : [0, 0],
    });
  }
  return {
    projectRoot,
    inputDirectory: path,
    get evidenceFiles() {
      return {
        "inventory.json": readFileSync(resolve(path, "inventory.json"), "utf8"),
        ...Object.fromEntries(
          readdirSync(resolve(path, "cov")).map((file) => [
            `cov/${file}`,
            readFileSync(resolve(path, "cov", file), "utf8"),
          ]),
        ),
      };
    },
    sites,
    rows,
  };
}
