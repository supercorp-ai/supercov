import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const runtimeUrl = pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/runtime.mjs")).href;

// The environment a strong pass starts a process with
// (docs/design/mutation-drivers.md): which mutant is on, and how often it may
// be evaluated before its switch throws the timeout error.
function evaluations(environment) {
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
    const runtime = await import(${JSON.stringify(runtimeUrl)});
    const slot = runtime.strongSlot("src/a.js");
    let evaluated = 0;
    let error;
    try {
      for (; evaluated < 10; evaluated += 1) slot.h();
    } catch (thrown) {
      error = thrown;
    }
    console.log(JSON.stringify({ active: slot.a, evaluated, timeout: runtime.isMutantTimeout(error), code: error?.code }));
  `], { encoding: "utf8", env: { ...process.env, ...environment } });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout.trim());
}

test("a process runs the mutant its environment names, up to its hit limit", () => {
  assert.deepEqual(evaluations({ SUPERCOV_ACTIVE_MUTANT: "src/a.js#3", SUPERCOV_MUTANT_HIT_LIMIT: "4" }),
    { active: 3, evaluated: 4, timeout: true, code: "SUPERCOV_MUTANT_TIMEOUT" });
  // No limit given, or one that is no count: a million.
  for (const limit of [undefined, "0", "many"]) {
    const environment = { SUPERCOV_ACTIVE_MUTANT: "src/a.js#3" };
    if (limit !== undefined) environment.SUPERCOV_MUTANT_HIT_LIMIT = limit;
    assert.deepEqual(evaluations(environment), { active: 3, evaluated: 10, timeout: false });
  }
  // `-` and another file's mutant leave this file as written.
  assert.equal(evaluations({ SUPERCOV_ACTIVE_MUTANT: "-" }).active, -1);
  assert.equal(evaluations({ SUPERCOV_ACTIVE_MUTANT: "src/b.js#0" }).active, -1);
});

// A string built a segment at a time by `+` stays a rope; the probe of `+`
// against Stryker's `-` must not flatten it at every step (a path 100,000
// levels deep ran a test process out of memory).
test("the probe of + and - on strings says differs without working the other out", () => {
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
    const runtime = await import(${JSON.stringify(runtimeUrl)});
    runtime.enableRuntimeSnapshotEvidence();
    let path = "";
    const started = Date.now();
    for (let index = 0; index < 50000; index += 1)
      path = runtime.arithV2("p:1:1-1:5#arith", path, ".segment" + index, 0);
    const elapsed = Date.now() - started;
    const outcomes = [...new Set(runtime.coverageSnapshot().comparisons.flatMap((entry) => entry.outcomes))];
    const minus = runtime.arithV2("m:1:1-1:5#arith", "5", 2, 1);
    const numbers = runtime.arithV2("n:1:1-1:5#arith", 2, 2, 2);
    const later = runtime.coverageSnapshot().comparisons.map((entry) => [entry.id, entry.outcomes]);
    console.log(JSON.stringify({ length: path.length > 0, elapsed, outcomes, minus, numbers, later }));
  `], { encoding: "utf8", env: { ...process.env, SUPERCOV_RUN_ID: "run" } });
  assert.equal(result.status, 0, result.stderr);
  const seen = JSON.parse(result.stdout.trim());
  assert.deepEqual(seen.outcomes, ["differs"]);
  assert.ok(seen.elapsed < 5000, `${seen.elapsed} ms`);
  assert.equal(seen.minus, 3);
  assert.equal(seen.numbers, 4);
  // 2 * 2 and 2 / 2 differ; working them out still decides numbers.
  assert.deepEqual(Object.fromEntries(seen.later)["n:1:1-1:5#arith"], ["differs"]);
});
