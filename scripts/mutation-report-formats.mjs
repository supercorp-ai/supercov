#!/usr/bin/env node

// The mutation report formats other tools read, checked by those tools:
//
// - `mutants --format stryker` against the published mutation testing report
//   schema (mutation-testing-report-schema, with Ajv), and rendered by the
//   official mutation-testing-elements report in Chromium;
// - `mutants --format pit` read by PIT's own reader of `mutations.xml`, the
//   one PIT's report aggregator uses (needs `java` and `mvn`; skipped
//   without them unless SUPERCOV_REQUIRE_PIT_READER=1).
//
// The run is a small Mocha project of two languages' mutants (a JavaScript
// file it measured and a Python file listed beside it) with a partial strong
// pass written by hand, the way `mutants test` saves one, so every outcome
// is in the reports.

import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SCHEMA = "mutation-testing-report-schema@3.9.0";
const ELEMENTS = "mutation-testing-elements@3.9.0";
const AJV = "ajv@8.17.1";
const PIT = "1.17.1";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, "target/debug/supercov");
const work = mkdtempSync(resolve(tmpdir(), "supercov-mutation-formats-"));
const project = resolve(work, "project");
const tools = resolve(work, "tools");

const run = (command, args, options = {}) => execFileSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...options });

try {
  mkdirSync(resolve(project, "src"), { recursive: true });
  mkdirSync(resolve(project, "test"));
  symlinkSync(resolve(repository, "node_modules"), resolve(project, "node_modules"));
  writeFileSync(resolve(project, "package.json"), JSON.stringify({ name: "formats", type: "module" }));
  writeFileSync(resolve(project, "src/cart.js"), "export function shipping(sum) {\n  if (sum >= 50) {\n    return 0;\n  }\n  return 5;\n}\n\nexport function label(name) {\n  return name.trim().toUpperCase();\n}\n");
  writeFileSync(resolve(project, "src/tax.py"), "def tax(total):\n    return total * 2\n");
  writeFileSync(resolve(project, "test/cart.test.js"), 'import assert from "node:assert/strict";\nimport { shipping, label } from "../src/cart.js";\ndescribe("cart", () => {\n  it("ships big orders free", () => assert.equal(shipping(80), 0));\n  it("labels", () => assert.equal(label(" a "), "A"));\n});\n');
  run(binary, ["--", "npx", "mocha"], { cwd: project });
  const runId = readdirSync(resolve(project, ".supercov/runs"))[0];
  const directory = resolve(project, ".supercov/runs", runId);
  // The run's mutants listed, then the Python file's added to the list.
  run(binary, ["runs", "latest", "mutants", "--json", "--limit", "1"], { cwd: project });
  const listed = JSON.parse(readFileSync(resolve(directory, "mutants-listed.json"), "utf8"));
  listed.mutantSets = { javascript: listed.mutantSet, python: "mutmut@3.8.1" };
  listed.languages = { "src/cart.js": "javascript", "src/tax.py": "python" };
  listed.files["src/tax.py"] = [{ mutator: "ArithmeticOperator", replacement: "total / 2", start: [2, 12], end: [2, 21], original: "total * 2" }];
  writeFileSync(resolve(directory, "mutants-listed.json"), JSON.stringify(listed));
  const ids = JSON.parse(run(binary, ["runs", "latest", "mutants", "--json", "--limit", "100"], { cwd: project })).data.mutants.map(mutant => mutant.id);
  const cart = ids.filter(id => id.startsWith("src/cart.js"));
  const python = ids.find(id => id.startsWith("src/tax.py"));
  assert(cart.length >= 5 && python, ids.join("\n"));
  const verdicts = {
    [cart[0]]: { status: "killed", killedBy: "test/cart.test.js::cart > ships big orders free", tests: 1, durationMs: 5 },
    [cart[1]]: { status: "survived", tests: 2, durationMs: 9 },
    [cart[2]]: { status: "timeout", tests: 1, durationMs: 2000 },
    [cart[3]]: { status: "unviable", tests: 0, durationMs: 0, reason: "it does not compile" },
    [cart[4]]: { status: "no-coverage", tests: 0, durationMs: 0 },
    [python]: { status: "error", tests: 0, durationMs: 0, reason: "no strong mutation driver for python yet" },
  };
  writeFileSync(resolve(directory, "strong-mutation.json"), JSON.stringify({ schemaVersion: 1, run: runId, mutantSet: listed.mutantSet, mutants: verdicts }));
  run(binary, ["runs", "latest", "mutants", "--format", "stryker", "--output", "mutation.json"], { cwd: project });
  run(binary, ["runs", "latest", "mutants", "--format", "pit", "--output", "mutations.xml"], { cwd: project });
  const report = JSON.parse(readFileSync(resolve(project, "mutation.json"), "utf8"));
  const statuses = Object.values(report.files).flatMap(file => file.mutants.map(mutant => mutant.status));
  for (const status of ["Killed", "Survived", "Timeout", "CompileError", "NoCoverage", "Pending"]) assert(statuses.includes(status), `${status} in ${statuses}`);
  assert.deepEqual(Object.keys(report.files).sort(), ["src/cart.js", "src/tax.py"]);
  assert.equal(report.files["src/tax.py"].language, "python");

  // The published schema and the official report, pinned.
  mkdirSync(tools);
  writeFileSync(resolve(tools, "package.json"), "{}");
  run("npm", ["install", "--no-audit", "--no-fund", "--ignore-scripts", SCHEMA, ELEMENTS, AJV], { cwd: tools, stdio: "inherit" });
  const { default: Ajv } = await import(resolve(tools, "node_modules/ajv/dist/ajv.js"));
  const schema = JSON.parse(readFileSync(resolve(tools, "node_modules/mutation-testing-report-schema/dist/src/mutation-testing-report-schema.json"), "utf8"));
  const ajv = new Ajv({ allErrors: true, strict: false, validateFormats: false });
  const validate = ajv.compile(schema);
  assert(validate(report), `the report breaks ${SCHEMA}: ${JSON.stringify(validate.errors, null, 2)}`);
  console.log(`[mutation-formats] ${statuses.length} mutants of ${Object.keys(report.files).length} files valid against ${SCHEMA}`);

  const elements = resolve(tools, "node_modules/mutation-testing-elements/dist/mutation-test-elements.js");
  writeFileSync(resolve(work, "elements.html"), `<!doctype html><meta charset="utf-8"><body><mutation-test-report-app title-postfix="Supercov"></mutation-test-report-app><script src="file://${elements}"></script><script>document.querySelector("mutation-test-report-app").report = ${JSON.stringify(report).replace(/</g, "\\u003c")};</script>`);
  const { chromium } = await import("playwright");
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    const failures = [];
    page.on("pageerror", error => failures.push(String(error)));
    page.on("console", message => { if (message.type() === "error") failures.push(message.text()); });
    await page.goto(`file://${resolve(work, "elements.html")}`, { waitUntil: "load" });
    // The report's text, through its shadow roots, once it has drawn its
    // files and their mutants.
    const shown = () => page.locator("mutation-test-report-app").evaluate(app => {
      const all = [];
      const walk = root => { for (const element of root.querySelectorAll("*")) { if (element.shadowRoot) walk(element.shadowRoot); } all.push(root.textContent ?? ""); };
      walk(app.shadowRoot ?? app);
      return all.join(" ");
    });
    let text = "";
    for (let attempt = 0; attempt < 60 && !(text.includes("cart.js") && text.includes("tax.py")); attempt++) {
      await page.waitForTimeout(500);
      text = await shown();
    }
    assert(text.includes("cart.js") && text.includes("tax.py"), "the report lists both files");
    // The totals row: one killed, one timed out, of the files' mutants.
    const totals = await page.getByRole("row").filter({ hasText: "All files" }).first().innerText();
    assert.match(totals.replace(/\s+/g, " "), /All files .*\b1\b/, totals);
    // A file's view: its source with each mutant on it.
    await page.getByRole("link", { name: "cart.js" }).first().click();
    for (let attempt = 0; attempt < 60 && !text.includes("toUpperCase"); attempt++) {
      await page.waitForTimeout(500);
      text = await shown();
    }
    assert(text.includes("toUpperCase"), "the file's view shows its source");
    for (const expected of ["Killed", "Survived", "Timeout", "No coverage"]) assert(text.includes(expected), `${expected} in the rendered report`);
    if (process.env.SUPERCOV_ELEMENTS_SCREENSHOT) await page.screenshot({ path: process.env.SUPERCOV_ELEMENTS_SCREENSHOT, fullPage: true });
    assert.deepEqual(failures, [], "mutation-testing-elements renders the report without errors");
    console.log(`[mutation-formats] ${ELEMENTS} rendered both files`);
  } finally {
    await browser.close();
  }

  // PIT's reader of mutations.xml.
  const javaReady = spawnSync("java", ["-version"]).status === 0 && spawnSync("mvn", ["-v"]).status === 0;
  if (!javaReady) {
    assert.notEqual(process.env.SUPERCOV_REQUIRE_PIT_READER, "1", "java and mvn are required to read mutations.xml with PIT");
    console.log("[mutation-formats] no java and mvn here: PIT's reader skipped");
  } else {
    const reader = resolve(work, "pit");
    mkdirSync(resolve(reader, "src/main/java/org/pitest/aggregate"), { recursive: true });
    writeFileSync(resolve(reader, "pom.xml"), `<project xmlns="http://maven.apache.org/POM/4.0.0"><modelVersion>4.0.0</modelVersion><groupId>check</groupId><artifactId>pit-reader</artifactId><version>1</version><properties><maven.compiler.release>17</maven.compiler.release><project.build.sourceEncoding>UTF-8</project.build.sourceEncoding></properties><dependencies><dependency><groupId>org.pitest</groupId><artifactId>pitest-aggregator</artifactId><version>${PIT}</version></dependency></dependencies></project>`);
    // In PIT's own package: its reader is package-private.
    writeFileSync(resolve(reader, "src/main/java/org/pitest/aggregate/ReadMutations.java"), `package org.pitest.aggregate;

import java.io.File;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import org.pitest.mutationtest.MutationResult;

public class ReadMutations {
  public static void main(String[] args) throws Exception {
    var results = new MutationResultDataLoader(List.of(new File(args[0]))).loadData();
    Map<String, Integer> statuses = new TreeMap<>();
    for (MutationResult result : results) {
      statuses.merge(result.getStatus().name(), 1, Integer::sum);
      if (result.getDetails().getFilename().isEmpty() || result.getDetails().getLineNumber() < 1) throw new IllegalStateException("no place: " + result);
    }
    System.out.println(results.size() + " " + statuses);
  }
}
`);
    run("mvn", ["-q", "-B", "compile", "dependency:build-classpath", "-Dmdep.outputFile=cp.txt"], { cwd: reader, stdio: "inherit" });
    const classpath = `${resolve(reader, "target/classes")}:${readFileSync(resolve(reader, "cp.txt"), "utf8").trim()}`;
    const read = run("java", ["-cp", classpath, "org.pitest.aggregate.ReadMutations", resolve(project, "mutations.xml")]).trim();
    const written = (readFileSync(resolve(project, "mutations.xml"), "utf8").match(/<mutation /g) ?? []).length;
    assert.match(read, new RegExp(`^${written} \\{`), read);
    for (const status of ["KILLED", "SURVIVED", "TIMED_OUT", "NON_VIABLE", "NO_COVERAGE", "NOT_STARTED"]) assert(read.includes(`${status}=`), `${status} in ${read}`);
    console.log(`[mutation-formats] PIT ${PIT}'s reader read ${read}`);
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}
