import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const bun = process.env.BUN_BINARY ?? "bun";
const available = spawnSync(bun, ["--version"], { encoding: "utf8" }).status === 0;

test("Bun proxied registration keeps each source file, including call/apply/bind", { skip: !available }, () => {
  const directory = mkdtempSync(join(tmpdir(), "supercov bun (ü)-"));
  try {
    const evidence = join(directory, "evidence");
    mkdirSync(evidence);
    const modes = ["direct", "call", "apply", "bind"];
    const files = modes.map(mode => `${mode}.test.ts`);
    for (const [index, mode] of modes.entries()) {
      const call = mode === "call" ? "test.call(undefined, title, body)" :
        mode === "apply" ? "test.apply(undefined, [title, body])" :
        mode === "bind" ? "test.bind(undefined)(title, body)" : "test(title, body)";
      writeFileSync(join(directory, files[index]),
        `import { test, expect } from "bun:test";\nconst title = "same title";\nconst body = () => expect(1).toBe(1);\n${call};\n`);
    }
    const env = Object.fromEntries(Object.entries(process.env).filter(([name]) =>
      !name.startsWith("SUPERCOV_") && !["NODE_OPTIONS", "BUN_OPTIONS"].includes(name)));
    const result = spawnSync(bun, ["test", "--parallel", "--preload", resolve(import.meta.dirname, "../../runtime/javascript/bunTest.mjs"), ...files], {
      cwd: directory, encoding: "utf8", env: { ...env, SUPERCOV_RUN_ID: "registration-location", SUPERCOV_EVIDENCE_DIR: evidence },
    });
    assert.equal(result.status, 0, result.stdout + result.stderr);
    const records = readdirSync(evidence).filter(name => name.startsWith("attempt-")).map(name =>
      JSON.parse(readFileSync(join(evidence, name, "mcdc.json"), "utf8")));
    assert.equal(records.length, modes.length);
    assert.deepEqual(records.map(record => record.testFile).sort(), [...files].sort());
    assert.equal(new Set(records.map(record => record.testId)).size, modes.length,
      "same-title registrations in different files must not share an identity");
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
