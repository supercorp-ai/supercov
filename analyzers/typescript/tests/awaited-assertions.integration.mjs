import test from "node:test";
import assert from "node:assert/strict";
import {
  cpSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "../../..");
test(
  "awaited native assertions preserve execution and exact source witnesses",
  {
    skip: process.env.SUPERCOV_ASSERTED_INTEGRATION !== "1",
  },
  () => {
    const root = mkdtempSync(resolve(tmpdir(), "supercov-awaited-assertions-"));
    try {
      cpSync(
        resolve(import.meta.dirname, "fixtures/awaited-assertions"),
        root,
        { recursive: true },
      );
      const env = { ...process.env, SUPERCOV_PACKAGE_ROOT: repository };
      delete env.NODE_TEST_CONTEXT;
      const exec = (command, args) => {
        const result = spawnSync(command, args, {
          cwd: root,
          env,
          encoding: "utf8",
          timeout: 60000,
          maxBuffer: 16 * 1024 * 1024,
        });
        assert.equal(result.status, 0, result.stderr || result.stdout);
        return result.stdout;
      };
      const suite = ["--test", "--test-reporter=tap", "tests/core.test.mjs"];
      const traces = (output) =>
        [...output.matchAll(/^# TRACE (.+)$/gm)].map((match) =>
          JSON.parse(match[1]),
        );
      const native = traces(exec(process.execPath, suite));
      assert.equal(native.length, 8);
      const binary = resolve(repository, "target/debug/supercov");
      assert.deepEqual(
        traces(exec(binary, ["--", process.execPath, ...suite])),
        native,
      );
      const [run] = readdirSync(resolve(root, ".supercov/runs"));
      const lines = readFileSync(
        resolve(root, "tests/core.test.mjs"),
        "utf8",
      ).split("\n");
      const expected = new Map();
      let title;
      for (const [index, line] of lines.entries()) {
        title = /^test\('([^']+)'/.exec(line)?.[1] ?? title;
        if (!expected.has(title) && title) expected.set(title, []);
        const marker = /\/\/ witness:(passed|failed)(?::(\d+))?/.exec(line);
        if (!marker) continue;
        const column = line.search(/(?:assert(?:\.|\[|\()|equal\()/) + 1;
        for (let n = 0; n < Number(marker[2] ?? 1); n++)
          expected
            .get(title)
            .push([`tests/core.test.mjs:${index + 1}:${column}`, marker[1]]);
      }
      for (const [name, witnesses] of expected) {
        const detail = JSON.parse(
          exec(binary, ["runs", run, "test", name, "--json"]),
        ).data;
        assert.equal(detail.tests.length, 1, name);
        const phases = detail.tests[0].phases.filter(
          (p) => p.kind === "assertion",
        );
        assert.deepEqual(
          phases.map((p) => [p.source, p.status]).sort(),
          witnesses.sort(),
          name,
        );
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);
