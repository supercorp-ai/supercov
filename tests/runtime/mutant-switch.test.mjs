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

// A wasm module as the Rust strong runtime exports its switch: a buffer for
// the mutant's file at 64, and an activation that keeps what it is given
// (the file's length and the index) in globals the test reads; its index
// is 42 until it is switched.
function strongWasm() {
  const name = (text) => [text.length, ...new TextEncoder().encode(text)];
  const section = (id, bytes) => [id, bytes.length, ...bytes];
  const exported = (text, kind, index) => [...name(text), kind, index];
  const exports = [
    exported("memory", 2, 0),
    exported("__supercov_strong_buffer_k", 0, 0),
    exported("__supercov_strong_activate_k", 0, 1),
    exported("index", 3, 0),
    exported("length", 3, 1),
  ];
  const buffer = [0x00, 0x41, 0xc0, 0x00, 0x0b];
  const activate = [0x00, 0x20, 0x01, 0x24, 0x00, 0x20, 0x00, 0x24, 0x01, 0x0b];
  return [
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...section(1, [2, 0x60, 0, 1, 0x7f, 0x60, 2, 0x7f, 0x7e, 0]),
    ...section(3, [2, 0, 1]),
    ...section(5, [1, 0, 1]),
    ...section(6, [2, 0x7e, 1, 0x42, 42, 0x0b, 0x7f, 1, 0x41, 0, 0x0b]),
    ...section(7, [exports.length, ...exports.flat()]),
    ...section(10, [2, buffer.length, ...buffer, activate.length, ...activate]),
  ];
}

// What each instance was switched to: its file and index, as the runtime
// handed them over.
function wasmSwitches(script, environment) {
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
    const runtime = await import(${JSON.stringify(runtimeUrl)});
    const bytes = new Uint8Array(${JSON.stringify(strongWasm())});
    const seen = (instance) => {
      const { memory, index, length } = instance.exports;
      const file = new TextDecoder().decode(new Uint8Array(memory.buffer, 64, length.value));
      return { file, index: index.value === 2n ** 64n - 1n ? -1 : Number(index.value) };
    };
    ${script}
  `], { encoding: "utf8", env: { ...process.env, ...environment } });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout.trim());
}

test("each wasm instance runs the active Rust mutant, by its file in the crate's workspace", () => {
  const roots = JSON.stringify(["crate/"]);
  // Created with the process's mutant on, both ways an instance is made.
  assert.deepEqual(wasmSwitches(`
    const made = new WebAssembly.Instance(new WebAssembly.Module(bytes));
    const { instance } = await WebAssembly.instantiate(bytes);
    console.log(JSON.stringify([seen(made), seen(instance)]));
  `, { SUPERCOV_WASM_STRONG: roots, SUPERCOV_ACTIVE_MUTANT: "crate/src/lib.rs#7" }),
  [{ file: "src/lib.rs", index: 7 }, { file: "src/lib.rs", index: 7 }]);
  // Switched as the runtime switches: another mutant, one of no crate's, none.
  assert.deepEqual(wasmSwitches(`
    const made = new WebAssembly.Instance(new WebAssembly.Module(bytes));
    const seenAfter = [];
    runtime.setActiveMutant("crate/src/b.rs", 2);
    seenAfter.push(seen(made));
    runtime.setActiveMutant("web/a.js", 1);
    seenAfter.push(seen(made));
    runtime.setActiveMutant("crate/src/b.rs", 3);
    runtime.clearActiveMutant();
    seenAfter.push(seen(made));
    console.log(JSON.stringify(seenAfter));
  `, { SUPERCOV_WASM_STRONG: roots }),
  [{ file: "src/b.rs", index: 2 }, { file: "", index: -1 }, { file: "", index: -1 }]);
  // A crate at the project's root keeps its path; without the variable no
  // instance is switched.
  assert.deepEqual(wasmSwitches(`
    const made = new WebAssembly.Instance(new WebAssembly.Module(bytes));
    console.log(JSON.stringify(seen(made)));
  `, { SUPERCOV_WASM_STRONG: JSON.stringify([""]), SUPERCOV_ACTIVE_MUTANT: "src/lib.rs#0" }),
  { file: "src/lib.rs", index: 0 });
  assert.deepEqual(wasmSwitches(`
    const made = new WebAssembly.Instance(new WebAssembly.Module(bytes));
    console.log(JSON.stringify(seen(made)));
  `, { SUPERCOV_ACTIVE_MUTANT: "src/lib.rs#0" }),
  { file: "", index: 42 });
});
