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
