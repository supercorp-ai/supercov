#!/usr/bin/env node

import { performance } from "node:perf_hooks";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { instrumentMcdc } from "../dist/instrumenter.js";
import { prepareCachedWorkspace } from "../dist/workspace.js";

const budget = JSON.parse(readFileSync(resolve("benchmarks/budget.json"), "utf8"));

function median(values) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.floor(sorted.length / 2)];
}

function timed(callback, iterations) {
  const durations = [];
  for (let index = 0; index < iterations; index += 1) {
    const started = performance.now();
    callback();
    durations.push(performance.now() - started);
  }
  return durations;
}

const corpus = Array.from({ length: budget.corpusFiles }, (_, index) => `
  export function decision${index}(input, fallback = ${index % 7}) {
    let total = 0;
    try {
      for (const value of input?.values ?? []) {
        if ((value > ${index % 5} && input.enabled) || input.force) total += value;
        else if (value === 0 || fallback > 3) continue;
        else total -= fallback;
      }
    } catch (error) {
      total = error?.code === "EXPECTED" ? fallback : -1;
    }
    return total > 0 && input.valid ? total : fallback;
  }
`).join("\n");

instrumentMcdc(corpus, "benchmark/warmup.js");
const transforms = timed(() => instrumentMcdc(corpus, "benchmark/corpus.js"), 7);
const transformed = instrumentMcdc(corpus, "benchmark/size.js").code;
const transformMedian = median(transforms);
const transformP95 = Math.max(...transforms);
const expansion = Buffer.byteLength(transformed) / Buffer.byteLength(corpus);

const runtimeSource = `
  function decide(a, b, c) { return (a && b) || c ? 1 : 0; }
  function run(iterations) {
    let total = 0;
    for (let index = 0; index < iterations; index += 1)
      total += decide(index % 2, index % 3, index % 5);
    return total;
  }
`;
const runtimeTransformed = instrumentMcdc(runtimeSource, "benchmark/runtime.js").code;
const importMatch = runtimeTransformed.match(
  /^import\s*\{([\s\S]*?)\}\s*from\s*["']virtual:supercov-runtime["'];?/,
);
if (!importMatch) throw new Error("runtime benchmark has no Supercov import");
const implementations = {
  mcdcBegin: "function(){return {};}",
  mcdcCondition: "function(frame,index,value){return value;}",
  mcdcEnd: "function(frame,value){return value;}",
  coverageHit: "function(){}",
  selectionBegin: "function(){return {};}",
  selectionRight: "function(frame,value){return value;}",
  selectionEnd: "function(frame,value){return value;}",
  optionalSelect: "function(a,b,value){return value;}",
  defaultSelected: "function(id,value){return value;}",
  defaultEntered: "function(){}",
  tryBegin: "function(){return {};}",
  tryCatch: "function(frame,value){return value;}",
  tryEnd: "function(){}",
  loopBegin: "function(){return {};}",
  loopEntered: "function(){}",
  loopEnd: "function(){}",
};
const runtimePrelude = [...importMatch[1].matchAll(/([\w$]+)\s+as\s+([\w$]+)/g)]
  .map(([, imported, local]) => `var ${local}=${implementations[imported]};`)
  .join("");
const originalRun = new Function(`${runtimeSource}\nreturn run;`)();
const instrumentedRun = new Function(
  `${runtimePrelude}${runtimeTransformed.slice(importMatch[0].length)}\nreturn run;`,
)();
originalRun(10_000);
instrumentedRun(10_000);
const originalRuntime = median(timed(() => originalRun(250_000), 7));
const instrumentedRuntime = median(timed(() => instrumentedRun(250_000), 7));
const runtimeOverhead = instrumentedRuntime / Math.max(originalRuntime, 0.001);

const workspaceRoot = mkdtempSync(resolve(tmpdir(), "supercov-benchmark-"));
let workspacePreparation;
try {
  mkdirSync(resolve(workspaceRoot, "src"));
  writeFileSync(
    resolve(workspaceRoot, "package.json"),
    '{"name":"supercov-workspace-benchmark","private":true}\n',
  );
  for (let index = 0; index < budget.workspaceFiles; index += 1)
    writeFileSync(
      resolve(workspaceRoot, "src", `${index}.js`),
      `export const value${index} = ${index};\n`,
    );
  workspacePreparation = timed(
    () => prepareCachedWorkspace(workspaceRoot),
    7,
  );
} finally {
  rmSync(workspaceRoot, { recursive: true, force: true });
}
const workspaceMedian = median(workspacePreparation);
const workspaceP95 = Math.max(...workspacePreparation);

console.log(`[benchmark] transform median=${transformMedian.toFixed(1)}ms p95=${transformP95.toFixed(1)}ms files=${budget.corpusFiles}`);
console.log(`[benchmark] output expansion=${expansion.toFixed(2)}x; runtime overhead=${runtimeOverhead.toFixed(2)}x`);
console.log(`[benchmark] workspace median=${workspaceMedian.toFixed(1)}ms p95=${workspaceP95.toFixed(1)}ms files=${budget.workspaceFiles}`);

const failures = [];
if (transformMedian > budget.transformMedianMsMax)
  failures.push(`transform median ${transformMedian.toFixed(1)}ms > ${budget.transformMedianMsMax}ms`);
if (transformP95 > budget.transformP95MsMax)
  failures.push(`transform p95 ${transformP95.toFixed(1)}ms > ${budget.transformP95MsMax}ms`);
if (expansion > budget.outputExpansionRatioMax)
  failures.push(`output expansion ${expansion.toFixed(2)}x > ${budget.outputExpansionRatioMax}x`);
if (runtimeOverhead > budget.runtimeOverheadRatioMax)
  failures.push(`runtime overhead ${runtimeOverhead.toFixed(2)}x > ${budget.runtimeOverheadRatioMax}x`);
if (workspaceMedian > budget.workspaceMedianMsMax)
  failures.push(`workspace median ${workspaceMedian.toFixed(1)}ms > ${budget.workspaceMedianMsMax}ms`);
if (workspaceP95 > budget.workspaceP95MsMax)
  failures.push(`workspace p95 ${workspaceP95.toFixed(1)}ms > ${budget.workspaceP95MsMax}ms`);
if (failures.length) {
  for (const failure of failures) console.error(`[benchmark] FAIL ${failure}`);
  process.exitCode = 1;
}
