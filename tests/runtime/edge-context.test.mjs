import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

const register = pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/register.mjs")).href;

function run(source, env = {}) {
  const result = spawnSync(process.execPath, [`--import=${register}`, "--input-type=commonjs", "--eval", source], {
    encoding: "utf8",
    timeout: 15000,
    env: { ...process.env, ...env },
  });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return result.stdout.trim();
}

const inContext = (options) => `
  const vm = require("node:vm");
  const context = vm.createContext({}, ${options});
  process.stdout.write(vm.runInContext(
    "typeof globalThis.__SUPERCOV_DIRECT_RUNTIME__ + ' ' + Object.keys(globalThis).join(',')", context));
`;

test("Next's edge context reads the process's runtime", () => {
  // A middleware.ts and a route with `runtime = "edge"` run in a context whose
  // global is its own: instrumented code found no runtime there and failed on
  // "Cannot read properties of undefined (reading 'mcdcBegin')".
  const seen = run(inContext('{ name: "Edge Runtime" }'), { SUPERCOV_DIRECT_INSTRUMENTATION: "1" });
  assert.equal(seen, "object");
});

test("the edge context's runtime is the one the process has, and can be replaced", () => {
  const seen = run(`
    const vm = require("node:vm");
    const context = vm.createContext({}, { name: "Edge Runtime" });
    const same = vm.runInContext("globalThis.__SUPERCOV_DIRECT_RUNTIME__", context) === globalThis.__SUPERCOV_DIRECT_RUNTIME__;
    vm.runInContext("globalThis.__SUPERCOV_DIRECT_RUNTIME__ = 7", context);
    process.stdout.write(same + " " + vm.runInContext("globalThis.__SUPERCOV_DIRECT_RUNTIME__", context));
  `, { SUPERCOV_DIRECT_INSTRUMENTATION: "1" });
  assert.equal(seen, "true 7");
});

test("any other context is left as it was made", () => {
  // Jest and jsdom make contexts too, and load the runtime their own way.
  assert.equal(run(inContext("{}"), { SUPERCOV_DIRECT_INSTRUMENTATION: "1" }), "undefined");
  assert.equal(run(inContext('{ name: "sandbox" }'), { SUPERCOV_DIRECT_INSTRUMENTATION: "1" }), "undefined");
});
