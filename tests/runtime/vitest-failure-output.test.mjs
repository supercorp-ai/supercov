import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

const map = (sources) => Buffer.from(JSON.stringify({
  version: 3, names: [], sources, sourcesContent: ["authored\n"], mappings: "AAAA",
})).toString("base64");

// A copy as a run lays it out: a rewritten file with its map, the text it was
// rewritten from beside the runtime, and Vitest's own code where it is found.
function copy(t) {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-vitest-output-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const write = (file, contents) => {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), contents);
  };
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), resolve(root, ".supercov/node_modules"), { recursive: true });
  write("lib/crypto.ts", `instrumented\n//# sourceMappingURL=data:application/json;base64,${map(["/project/lib/crypto.ts"])}\n`);
  write("lib/plain.ts", "untouched\n");
  write(".supercov/node_modules/.authored/lib/crypto.ts", "authored\n");
  write(".supercov/node_modules/authored-sources.json", JSON.stringify(["lib/crypto.ts"]));
  const read = "import { readFileSync } from 'node:fs';\nexport const read = (file) => readFileSync(file, 'utf8');\n";
  write("node_modules/vitest/dist/chunks/print.mjs", read);
  write("node_modules/vite/dist/node/load.mjs", read);
  write("main.mjs", `
    import Reporter, { supercovSourceMaps } from "./.supercov/node_modules/vitestReporter.mjs";
    import { read as vitest } from "./node_modules/vitest/dist/chunks/print.mjs";
    import { read as vite } from "./node_modules/vite/dist/node/load.mjs";
    const [file, plain] = process.argv.slice(2);
    const plugin = supercovSourceMaps();
    const loaded = await plugin.load(file);
    new Reporter().onInit({ projects: [] });
    process.stdout.write(JSON.stringify({
      loaded, untouched: await plugin.load(plain), query: await plugin.load(file + "?raw"),
      vitest: vitest(file), vite: vite(file), other: vitest(plain),
    }));
  `);
  return root;
}

function run(root) {
  const result = spawnSync(process.execPath, [resolve(root, "main.mjs"), resolve(root, "lib/crypto.ts"), resolve(root, "lib/plain.ts")], {
    cwd: root, encoding: "utf8", timeout: 15000,
  });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return JSON.parse(result.stdout);
}

test("Vite is handed a rewritten file with a map that names the file itself", t => {
  // The map on disk names the project's file by absolute path. Vitest prints
  // a frame relative to its root, the copy: `../../../../lib/crypto.ts:3:11`,
  // and a file outside its module graph gets no code under the frame.
  const seen = run(copy(t));
  assert.equal(seen.loaded.code, "instrumented\n");
  assert.deepEqual(seen.loaded.map.sources, ["crypto.ts"]);
  assert.deepEqual(seen.loaded.map.sourcesContent, ["authored\n"]);
  // A file Supercov did not rewrite, and a request that is not the file, are Vite's to load.
  assert.equal(seen.untouched, null);
  assert.equal(seen.query, null);
});

test("what Vitest itself reads of a rewritten file is the text it was rewritten from", t => {
  // It reads the file to print the code under a frame, and Vitest 4 to look
  // for a source map in it: positions that were already the author's were
  // mapped a second time. What Vite reads is what runs.
  const root = copy(t);
  const seen = run(root);
  assert.equal(seen.vitest, "authored\n");
  assert.match(seen.vite, /^instrumented\n/);
  assert.equal(seen.other, "untouched\n");
  assert.match(readFileSync(resolve(root, "lib/crypto.ts"), "utf8"), /^instrumented\n/);
});
