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
import * as collectorRuntime from "../dist/runtime.js";
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
  registerProbeV2: "function(definition){return Object.assign(definition,{clock:{epoch:1,fast:true},hitEpochs:new Uint32Array(definition.pointIds.length),decisionEpochs:definition.decisions.map(function(meta){return meta.conditions.length<=6?new Uint32Array(2*3**meta.conditions.length):new Map();}),decisionCompleteEpochs:new Uint32Array(definition.decisions.length)});}",
  coverageHitV2: "function(){}",
  mcdcEndV2: "function(file,index,encoded,value){return value;}",
};
function compileInstrumentedRuntime(probeVersion, source = runtimeSource) {
  const transformed = instrumentMcdc(
    source,
    "benchmark/runtime.js",
    { probeVersion },
  ).code;
  const importMatch = transformed.match(
    /^import\s*\{([\s\S]*?)\}\s*from\s*["']virtual:supercov-runtime["'];?/,
  );
  if (!importMatch) throw new Error("runtime benchmark has no Supercov import");
  const runtimePrelude = [...importMatch[1].matchAll(/([\w$]+)\s+as\s+([\w$]+)/g)]
    .map(([, imported, local]) => `var ${local}=${implementations[imported]};`)
    .join("");
  return new Function(
    `${runtimePrelude}${transformed.slice(importMatch[0].length)}\nreturn run;`,
  )();
}
function compileCollectorRuntime(probeVersion, source = runtimeSource) {
  const transformed = instrumentMcdc(
    source,
    "benchmark/runtime.js",
    { probeVersion },
  ).code;
  const importMatch = transformed.match(
    /^import\s*\{([\s\S]*?)\}\s*from\s*["']virtual:supercov-runtime["'];?/,
  );
  if (!importMatch) throw new Error("runtime benchmark has no Supercov import");
  const bindings = [...importMatch[1].matchAll(/([\w$]+)\s+as\s+([\w$]+)/g)]
    .map(([, imported, local]) => [local, collectorRuntime[imported]]);
  if (bindings.some(([, implementation]) => typeof implementation !== "function"))
    throw new Error("collector runtime benchmark is missing an imported probe");
  return new Function(
    ...bindings.map(([local]) => local),
    `${transformed.slice(importMatch[0].length)}\nreturn run;`,
  )(...bindings.map(([, implementation]) => implementation));
}
const originalRun = new Function(`${runtimeSource}\nreturn run;`)();
const instrumentedRunV1 = compileInstrumentedRuntime(1);
const instrumentedRunV2 = compileInstrumentedRuntime(2);
const collectorRunV1 = compileCollectorRuntime(1);
const collectorRunV2 = compileCollectorRuntime(2);
originalRun(10_000);
instrumentedRunV1(10_000);
instrumentedRunV2(10_000);
collectorRuntime.resetCoverage();
collectorRunV1(10_000);
collectorRuntime.resetCoverage();
collectorRunV2(10_000);
collectorRuntime.resetCoverage();
const originalRuntime = median(timed(() => originalRun(250_000), 7));
const instrumentedRuntimeV1 = median(timed(() => instrumentedRunV1(250_000), 7));
const instrumentedRuntimeV2 = median(timed(() => instrumentedRunV2(250_000), 7));
const runtimeOverhead = instrumentedRuntimeV1 / Math.max(originalRuntime, 0.001);
const probeV2RuntimeOverhead = instrumentedRuntimeV2 / Math.max(originalRuntime, 0.001);
const collectorRuntimeV1 = median(timed(() => collectorRunV1(250_000), 7));
collectorRuntime.resetCoverage();
const collectorRuntimeV2 = median(timed(() => collectorRunV2(250_000), 7));
collectorRuntime.resetCoverage();
const collectorOverheadV1 = collectorRuntimeV1 / Math.max(originalRuntime, 0.001);
const collectorOverheadV2 = collectorRuntimeV2 / Math.max(originalRuntime, 0.001);
const benchmarkScope = {
  version: 1,
  runId: "benchmark",
  workerId: "worker-0",
  testId: "hot-loop",
  testKey: "hot-loop",
  retry: 0,
  attemptId: "attempt-0",
};
collectorRuntime.enableRuntimeSnapshotEvidence();
collectorRuntime.resetCoverage();
const scopedCollectorRuntimeV1 = median(
  timed(
    () => collectorRuntime.withCoverageCarrier(
      { version: 1, scope: benchmarkScope },
      () => collectorRunV1(250_000),
    ),
    7,
  ),
);
collectorRuntime.resetCoverage();
const scopedCollectorRuntimeV2 = median(
  timed(
    () => collectorRuntime.withCoverageCarrier(
      { version: 1, scope: benchmarkScope },
      () => collectorRunV2(250_000),
    ),
    7,
  ),
);
collectorRuntime.resetCoverage();
const scopedCollectorOverheadV1 = scopedCollectorRuntimeV1 / Math.max(originalRuntime, 0.001);
const scopedCollectorOverheadV2 = scopedCollectorRuntimeV2 / Math.max(originalRuntime, 0.001);

const realisticRuntimeSource = `
  const records = Array.from({ length: 2_000 }, (_, index) => ({
    name: " Customer " + index + " ",
    payload: JSON.stringify({ active: index % 3 !== 0, score: index % 101 }),
    tags: index % 11 === 0 ? ["priority", "batch"] : ["batch"],
  }));
  function run(rounds) {
    let digest = 2_166_136_261;
    for (let round = 0; round < rounds; round += 1) {
      for (const record of records) {
        const parsed = JSON.parse(record.payload);
        const normalized = record.name.trim().toLowerCase();
        if ((parsed.active && parsed.score >= 50) || record.tags.includes("priority")) {
          digest = Math.imul(digest ^ normalized.length ^ parsed.score, 16_777_619);
        } else {
          digest = Math.imul(digest ^ round, 16_777_619);
        }
      }
    }
    return digest >>> 0;
  }
`;
const realisticOriginal = new Function(`${realisticRuntimeSource}\nreturn run;`)();
const realisticCollectorV2 = compileCollectorRuntime(2, realisticRuntimeSource);
const realisticRounds = 100;
const realisticSamples = 15;
realisticOriginal(25);
collectorRuntime.resetCoverage();
collectorRuntime.withCoverageCarrier(
  { version: 1, scope: benchmarkScope },
  () => realisticCollectorV2(25),
);
const realisticOriginalDurations = [];
const realisticCollectorDurations = [];
const timeOne = (callback) => {
  const started = performance.now();
  callback();
  return performance.now() - started;
};
const runRealisticCollector = () =>
  collectorRuntime.withCoverageCarrier(
      { version: 1, scope: benchmarkScope },
      () => realisticCollectorV2(realisticRounds),
  );
for (let index = 0; index < realisticSamples; index += 1) {
  if (index % 2 === 0) {
    realisticOriginalDurations.push(
      timeOne(() => realisticOriginal(realisticRounds)),
    );
    realisticCollectorDurations.push(timeOne(runRealisticCollector));
  } else {
    realisticCollectorDurations.push(timeOne(runRealisticCollector));
    realisticOriginalDurations.push(
      timeOne(() => realisticOriginal(realisticRounds)),
    );
  }
}
const realisticOriginalMs = median(realisticOriginalDurations);
const realisticCollectorV2Ms = median(realisticCollectorDurations);
collectorRuntime.resetCoverage();
const realisticProbeV2Overhead =
  realisticCollectorV2Ms / Math.max(realisticOriginalMs, 0.001);

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
  rmSync(workspaceRoot, {
    recursive: true,
    force: true,
    maxRetries: 20,
    retryDelay: 25,
  });
}
const workspaceMedian = median(workspacePreparation);
const workspaceP95 = Math.max(...workspacePreparation);

console.log(`[benchmark] transform median=${transformMedian.toFixed(1)}ms p95=${transformP95.toFixed(1)}ms files=${budget.corpusFiles}`);
console.log(`[benchmark] output expansion=${expansion.toFixed(2)}x; runtime overhead v1=${runtimeOverhead.toFixed(2)}x v2=${probeV2RuntimeOverhead.toFixed(2)}x`);
console.log(`[benchmark] collector hot-loop overhead v1=${collectorOverheadV1.toFixed(2)}x v2=${collectorOverheadV2.toFixed(2)}x`);
console.log(`[benchmark] attributed collector original=${originalRuntime.toFixed(2)}ms v1=${scopedCollectorRuntimeV1.toFixed(2)}ms (${scopedCollectorOverheadV1.toFixed(2)}x) v2=${scopedCollectorRuntimeV2.toFixed(2)}ms (${scopedCollectorOverheadV2.toFixed(2)}x)`);
console.log(`[benchmark] realistic attributed steady-state v2 original=${realisticOriginalMs.toFixed(2)}ms instrumented=${realisticCollectorV2Ms.toFixed(2)}ms overhead=${realisticProbeV2Overhead.toFixed(2)}x samples=${realisticSamples}`);
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
if (realisticProbeV2Overhead > budget.realisticProbeV2OverheadRatioMax)
  failures.push(`realistic probe v2 overhead ${realisticProbeV2Overhead.toFixed(2)}x > ${budget.realisticProbeV2OverheadRatioMax}x`);
if (workspaceMedian > budget.workspaceMedianMsMax)
  failures.push(`workspace median ${workspaceMedian.toFixed(1)}ms > ${budget.workspaceMedianMsMax}ms`);
if (workspaceP95 > budget.workspaceP95MsMax)
  failures.push(`workspace p95 ${workspaceP95.toFixed(1)}ms > ${budget.workspaceP95MsMax}ms`);
if (failures.length) {
  for (const failure of failures) console.error(`[benchmark] FAIL ${failure}`);
  process.exitCode = 1;
}
