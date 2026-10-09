import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

const inline = (map) => `\n//# sourceMappingURL=data:application/json;base64,${Buffer.from(JSON.stringify(map)).toString("base64")}\n`;
const mapOf = (text) => JSON.parse(Buffer.from(text.split("base64,").pop().trim(), "base64").toString("utf8"));

// A project with a copy in it, as a run lays it out: a rewritten file whose
// map names the project's own, the text it was rewritten from beside the
// runtime, and each reader's code where that reader is found.
function project(t) {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-failure-output-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const copy = resolve(root, ".supercov/workspaces/workspace/app");
  const write = (file, contents) => {
    mkdirSync(dirname(resolve(copy, file)), { recursive: true });
    writeFileSync(resolve(copy, file), contents);
  };
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), resolve(copy, ".supercov/node_modules"), { recursive: true });
  // Two lines Supercov added, then the author's two, moved down and in.
  const own = {
    version: 3, names: [], sources: [resolve(root, "lib/crypto.ts")],
    sourcesContent: ["authored one\nauthored two\n"], mappings: ";;EAAA;EACA",
  };
  write("lib/crypto.ts", `probe\nprobe\n  instrumented one\n  instrumented two${inline(own)}`);
  write("lib/plain.ts", "untouched\n");
  write(".supercov/node_modules/.authored/lib/crypto.ts", "authored one\nauthored two\n");
  write(".supercov/node_modules/authored-sources.json", JSON.stringify(["lib/crypto.ts"]));
  const reader = `
    const fs = require("node:fs");
    const path = require("node:path");
    module.exports = {
      read: (file) => fs.readFileSync(file, "utf8"),
      relative: (from, to) => path.relative(from, to),
    };
  `;
  for (const place of ["vitest/dist/chunks/print.cjs", "vite/dist/node/load.cjs", "jest-message-util/build/index.cjs", "jest-runner/build/runTest.cjs"])
    write(`node_modules/${place}`, reader);
  write("main.cjs", `
    const fs = require("node:fs");
    const [runner, file] = process.argv.slice(2);
    process.__SUPERCOV_FAILURE_OUTPUT__(runner);
    const from = (place) => require("./node_modules/" + place);
    (async () => {
      process.stdout.write(JSON.stringify({
        vitest: from("vitest/dist/chunks/print.cjs").read(file),
        vite: from("vite/dist/node/load.cjs").read(file),
        loaded: await fs.promises.readFile(file, "utf8"),
        jest: from("jest-message-util/build/index.cjs").read(file),
        printed: from("jest-message-util/build/index.cjs").relative(process.cwd(), ${JSON.stringify(resolve(root, "lib/crypto.ts"))}),
        elsewhere: from("jest-runner/build/runTest.cjs").relative(process.cwd(), ${JSON.stringify(resolve(root, "lib/crypto.ts"))}),
        outside: from("jest-message-util/build/index.cjs").relative(process.cwd(), ${JSON.stringify(resolve(root, "lib/other.ts"))}),
        map: process.argv[4] ? from("jest-runner/build/runTest.cjs").read(process.argv[4]) : null,
      }));
    })();
  `);
  return { root, copy, write };
}

function run(copy, runner, map) {
  const result = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(copy, ".supercov/node_modules/register.mjs")).href}`,
    resolve(copy, "main.cjs"), runner, resolve(copy, "lib/crypto.ts"), ...(map ? [map] : []),
  ], { cwd: copy, encoding: "utf8", timeout: 15000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return JSON.parse(result.stdout);
}

test("what Vitest itself reads of a rewritten file is the text it was rewritten from", t => {
  // It reads the file to print the code under a frame, and Vitest 4 to look
  // for a source map in it: positions that were already the author's were
  // mapped a second time, `:12:35` to `:12:0`.
  const { copy } = project(t);
  const seen = run(copy, "vitest");
  assert.equal(seen.vitest, "authored one\nauthored two\n");
  assert.match(readFileSync(resolve(copy, "lib/crypto.ts"), "utf8"), /^probe\n/);
});

test("Vite loads a rewritten file with a map that names the file itself", t => {
  // The map on disk names the project's file. Vitest prints a frame relative
  // to its root, the copy: `../../../../lib/crypto.ts:3:11`, and a file
  // outside its module graph gets no code under the frame. Read as Vite reads
  // it, for a root project and for a workspace's, the map names the copy's.
  const { copy } = project(t);
  const seen = run(copy, "vitest");
  for (const text of [seen.loaded, seen.vite]) {
    assert.match(text, /^probe\nprobe\n {2}instrumented one\n/);
    assert.deepEqual(mapOf(text).sources, ["crypto.ts"]);
    assert.deepEqual(mapOf(text).sourcesContent, ["authored one\nauthored two\n"]);
  }
});

test("Jest prints a rewritten file's frame as the project's path, and its code as written", t => {
  // `at decrypt (../../../../lib/crypto.ts:10:11)`, and under a test file's
  // frame the line Supercov wrapped. Jest's coverage keeps the project's
  // paths, so what is loaded is read as it is.
  const { copy } = project(t);
  const seen = run(copy, "jest");
  assert.equal(seen.printed, "lib/crypto.ts");
  assert.equal(seen.jest, "authored one\nauthored two\n");
  assert.equal(seen.elsewhere, "../../../../lib/crypto.ts");
  assert.equal(seen.outside, "../../../../lib/other.ts");
  assert.deepEqual(mapOf(seen.loaded).sources, mapOf(readFileSync(resolve(copy, "lib/crypto.ts"), "utf8")).sources);
});

test("a map from a transformer that read none is taken through the rewritten file's own", t => {
  // ts-jest compiles the text it is given. Its map led to the rewritten
  // file, and a frame read `lib/crypto.ts:51:10` for line 10.
  const { root, copy, write } = project(t);
  const rewritten = resolve(copy, "lib/crypto.ts");
  const transformed = (content) => JSON.stringify({
    version: 3, names: [], sources: [rewritten], sourcesContent: [content],
    // Output lines 0-3 are the rewritten file's lines 0-3, column 2.
    mappings: "AAAE;AACA;AACA;AACA",
  });
  write("cache/crypto.map", transformed(readFileSync(rewritten, "utf8")));
  const through = JSON.parse(run(copy, "jest", resolve(copy, "cache/crypto.map")).map);
  assert.deepEqual(through.sources, [resolve(root, "lib/crypto.ts")]);
  assert.deepEqual(through.sourcesContent, ["authored one\nauthored two\n"]);
  // Supercov's two lines map nothing; the author's are lines 0 and 1.
  assert.equal(through.mappings, "A;A;AAAA;AACA");
  // One that carries the author's text already went through it.
  write("cache/crypto.map", transformed("authored one\nauthored two\n"));
  const kept = run(copy, "jest", resolve(copy, "cache/crypto.map")).map;
  assert.equal(kept, transformed("authored one\nauthored two\n"));
});
