#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { instrumentMcdc } from "../src/instrumenter.ts";
import { executeDifferential } from "../tests/unit/instrumenter-harness.ts";
import { generatedExpression } from "../tests/support/generated-programs.ts";

const corpusPath = resolve("contracts/js-instrumenter-v1/cases.json");
const corpusText = readFileSync(corpusPath, "utf8");
const corpus = JSON.parse(corpusText);
const executionCorpus = JSON.parse(
  readFileSync(resolve("contracts/js-instrumenter-v1/execution-cases.json"), "utf8"),
);
const generatedCorpus = Array.from({ length: 160 }, (_, index) => {
  const seed = 0x5eed_0000 + index;
  const expression = generatedExpression(seed);
  return {
    file: `differential/generated-${seed.toString(16)}.js`,
    source: `
      const effects = [];
      function mark(label, value) { effects.push(label); return value; }
      function fail(label) { effects.push(label); throw new RangeError(label); }
      function run() {
        if (${expression}) return "truthy";
        return "falsy";
      }
      function observe() { return effects; }
    `,
  };
});
const coercedFunctionSources = [
  `const value = "" + function () { if (flag) return 1; };`,
  `const value = other < function () { if (flag) return 1; };`,
  `const value = (function () { if (flag) return 1; }).toString();`,
  `const value = lookup[function () { if (flag) return 1; }];`,
  `const value = { [function () { if (flag) return 1; }]: 1 };`,
  `class Keys { [function () { if (flag) return 1; }]() {} }`,
  `class Fields { [function () { if (flag) return 1; }] = 1; }`,
  `const value = { [function () { if (flag) return 1; }]() {} };`,
  `const value = lookup?.[function () { if (flag) return 1; }];`,
  `const value = (function () { if (flag) return 1; })?.toString();`,
  `const value = String(flag ? function () { if (flag) return 1; } : fallback);`,
  `const value = String(flag ? fallback : function () { if (flag) return 1; });`,
  `const value = String(function () { if (flag) return 1; } || flag);`,
  `const value = String(flag || function () { if (flag) return 1; });`,
  `const value = String((0, function () { if (flag) return 1; }));`,
  `const value = String(target = function () { if (flag) return 1; });`,
  `const value = String(function () { if (flag) return 1; } as unknown);`,
  `const value = String(<any>function () { if (flag) return 1; });`,
  `const value = String((function () { if (flag) return 1; })!);`,
];
const consumedFunctionSources = [
  `const value = [function () { if (flag) return 1; }];`,
  `const value = other === function () { if (flag) return 1; };`,
  `const value = (function () { if (flag) return 1; }).name;`,
  `const value = (function () { if (flag) return 1; })[key];`,
  `const value = String((function () { if (flag) return 1; }, other));`,
  `const value = String(function () { if (flag) return 1; } ? left : right);`,
  `const value = { plain: function () { if (flag) return 1; } };`,
  `const value = { [key]: function () { if (flag) return 1; } };`,
  `const value = (function () { if (flag) return 1; })?.[key];`,
];
const safetyCorpus = [
  ...coercedFunctionSources.map((source, index) => ({
    file: `safety/coerced-${index}.ts`,
    source,
  })),
  ...consumedFunctionSources.map((source, index) => ({
    file: `safety/consumed-${index}.ts`,
    source,
  })),
  {
    file: "safety/with.js",
    source: `with (scope) { if (left && right) value = 1; }`,
  },
  {
    file: "safety/eval.ts",
    source: `function decide(source) { if (source) return eval(source); }`,
  },
  {
    file: "safety/function-constructor.ts",
    source: `const decide = new Function("value", "if (value) return 1;");`,
  },
];
const allCases = [...corpus, ...executionCorpus, ...generatedCorpus, ...safetyCorpus];
const rust = spawnSync(
  "cargo",
  ["run", "--quiet", "-p", "supercov-engine", "--example", "js_manifest"],
  { input: JSON.stringify(allCases), encoding: "utf8", maxBuffer: 16 * 1024 * 1024 },
);
if (rust.status !== 0)
  throw new Error(`Rust JS candidate failed (${rust.status}):\n${rust.stderr}`);
const outputs = JSON.parse(rust.stdout);
if (outputs.length !== allCases.length)
  throw new Error(`Rust candidate returned ${outputs.length} outputs for ${allCases.length} cases`);

for (const [index, testCase] of allCases.entries()) {
  const referenceManifest = instrumentMcdc(testCase.source, testCase.file).manifest;
  const reference = referenceManifest.decisions;
  const candidate = outputs[index];
  if (candidate.complete !== false)
    throw new Error(`${testCase.file}: partial Rust slice claimed completeness`);
  if (JSON.stringify(candidate.decisions) !== JSON.stringify(reference))
    throw new Error(
      `${testCase.file}: Rust/TypeScript decision mismatch\nreference=${JSON.stringify(reference)}\ncandidate=${JSON.stringify(candidate.decisions)}`,
    );
  if (JSON.stringify(candidate.points) !== JSON.stringify(referenceManifest.points))
    throw new Error(
      `${testCase.file}: Rust/TypeScript point mismatch\nreference=${JSON.stringify(referenceManifest.points)}\ncandidate=${JSON.stringify(candidate.points)}`,
    );
  if (JSON.stringify(candidate.branches) !== JSON.stringify(referenceManifest.branches))
    throw new Error(
      `${testCase.file}: Rust/TypeScript complete branch-manifest mismatch\nreference=${JSON.stringify(referenceManifest.branches)}\ncandidate=${JSON.stringify(candidate.branches)}`,
    );
  const referenceLimitations = referenceManifest.limitations ?? [];
  if (JSON.stringify(candidate.coverageLimitations) !== JSON.stringify(referenceLimitations))
    throw new Error(
      `${testCase.file}: Rust/TypeScript limitation mismatch\nreference=${JSON.stringify(referenceLimitations)}\ncandidate=${JSON.stringify(candidate.coverageLimitations)}`,
    );
}

function normalize(value, seen = new WeakSet()) {
  if (value === undefined) return { $type: "undefined" };
  if (typeof value === "number") {
    if (Number.isNaN(value)) return { $type: "number", value: "NaN" };
    if (Object.is(value, -0)) return { $type: "number", value: "-0" };
    if (!Number.isFinite(value)) return { $type: "number", value: String(value) };
  }
  if (value === null || typeof value !== "object") return value;
  if (seen.has(value)) return { $type: "circular" };
  seen.add(value);
  if (Array.isArray(value)) return value.map((entry) => normalize(entry, seen));
  return Object.fromEntries(
    Object.entries(value).map(([key, entry]) => [key, normalize(entry, seen)]),
  );
}

function decodeVector(conditionCount, encoded, value) {
  const values = [];
  let remaining = encoded;
  for (let index = 0; index < conditionCount; index += 1) {
    const digit = remaining % 3;
    values.push(digit === 0 ? null : digit === 2);
    remaining = Math.floor(remaining / 3);
  }
  if (remaining !== 0) throw new Error(`aliased probe-v2 frame ${encoded}`);
  return { values, outcome: Boolean(value) };
}

async function executeRustCandidate(testCase, candidate) {
  const vectors = [];
  const hits = [];
  const registrations = [];
  const runtime = candidate.runtime;
  if (!runtime?.coverageHit || !runtime?.mcdcBegin || !runtime?.mcdcCondition || !runtime?.mcdcEnd || !runtime?.registerProbeV2 || !runtime?.mcdcEndV2 || !runtime?.coverageHitV2 || !runtime?.probeFileV2 || !runtime?.selectionBegin || !runtime?.selectionRight || !runtime?.selectionEnd || !runtime?.withRequestPhase || !runtime?.optionalSelect || !runtime?.optionalCallBegin || !runtime?.optionalCallReached || !runtime?.optionalCallContinued || !runtime?.optionalCallEnd || !runtime?.defaultSelected || !runtime?.defaultEntered || !runtime?.tryBegin || !runtime?.tryCatch || !runtime?.tryEnd || !runtime?.loopBegin || !runtime?.loopEntered || !runtime?.loopEnd)
    throw new Error(`${testCase.file}: missing Rust candidate runtime bindings`);
  const coverageHit = (id) => hits.push(id);
  const registerProbeV2 = (definition) => {
    registrations.push(definition);
    return {
      ...definition,
      clock: { epoch: 1, fast: false },
      hitEpochs: new Uint32Array(definition.pointIds.length),
      decisionEpochs: definition.decisions.map((meta) =>
        meta.conditions.length <= 6
          ? new Uint32Array(2 * 3 ** meta.conditions.length)
          : new Map(),
      ),
      decisionCompleteEpochs: new Uint32Array(definition.decisions.length),
    };
  };
  const coverageHitV2 = (file, index) => coverageHit(file.pointIds[index]);
  const begin = (id, meta) => {
    if (id !== meta.id) throw new Error(`mismatched decision registration ${id}/${meta.id}`);
    return { meta, values: Array.from({ length: meta.conditions.length }, () => null) };
  };
  const condition = (frame, index, value) => {
    frame.values[index] = Boolean(value);
    return value;
  };
  const end = (frame, value) => {
    vectors.push({ values: frame.values, outcome: Boolean(value) });
    return value;
  };
  const selectionBegin = (shortId, rightId) => ({ shortId, rightId, evaluatedRight: false });
  const selectionRight = (frame, value, inferredName) => {
    frame.evaluatedRight = true;
    if (inferredName && typeof value === "function" && value.name === "")
      Object.defineProperty(value, "name", { value: inferredName, configurable: true });
    return value;
  };
  const selectionEnd = (frame, value) => {
    coverageHit(frame.evaluatedRight ? frame.rightId : frame.shortId);
    return value;
  };
  const withRequestPhase = (handler) => handler;
  const optionalSelect = (shortId, continuedId, value) => {
    coverageHit(value === null || value === undefined ? shortId : continuedId);
    return value;
  };
  const optionalCallBegin = (shortId, continuedId) => ({
    shortId,
    continuedId,
    reached: false,
    continued: false,
  });
  const optionalCallReached = (frame, value) => {
    frame.reached = true;
    return value;
  };
  const optionalCallContinued = (frame) => {
    frame.continued = true;
    return [];
  };
  const optionalCallEnd = (frame, value) => {
    if (frame.reached) coverageHit(frame.continued ? frame.continuedId : frame.shortId);
    return value;
  };
  const pendingDefaults = new Map();
  const defaultSelected = (defaultId, value, inferredName) => {
    pendingDefaults.set(defaultId, (pendingDefaults.get(defaultId) ?? 0) + 1);
    if (inferredName && typeof value === "function" && value.name === "")
      Object.defineProperty(value, "name", { value: inferredName, configurable: true });
    return value;
  };
  const defaultEntered = (defaultId, providedId) => {
    const pending = pendingDefaults.get(defaultId) ?? 0;
    if (pending > 0) {
      if (pending === 1) pendingDefaults.delete(defaultId);
      else pendingDefaults.set(defaultId, pending - 1);
      coverageHit(defaultId);
    } else coverageHit(providedId);
  };
  const tryBegin = (successId, catchId) => ({ successId, catchId, caught: false });
  const tryCatch = (frame, value) => {
    frame.caught = true;
    return value;
  };
  const tryEnd = (frame) => coverageHit(frame.caught ? frame.catchId : frame.successId);
  const loopBegin = (zeroId, enteredId) => ({ zeroId, enteredId, entered: false });
  const loopEntered = (frame) => {
    frame.entered = true;
  };
  const loopEnd = (frame) => coverageHit(frame.entered ? frame.enteredId : frame.zeroId);
  const recorder = (file, decisionIndex, encoded, value) => {
    if (file !== testCase.file) throw new Error(`unexpected probe file ${file}`);
    const decision = candidate.decisions[decisionIndex];
    if (!decision) throw new Error(`unexpected decision index ${decisionIndex}`);
    vectors.push(decodeVector(decision.conditions.length, encoded, value));
    return value;
  };
  // This evaluates only the checked-in, self-contained differential corpus.
  // eslint-disable-next-line no-new-func
  const executable = candidate.code.replace(
    /^import\s*\{[\s\S]*?\}\s*from\s*["']virtual:supercov-runtime["'];?\s*/,
    "",
  );
  if (executable === candidate.code)
    throw new Error(`${testCase.file}: Rust candidate is missing its production runtime import`);
  const factory = new Function(
    runtime.coverageHit,
    runtime.mcdcBegin,
    runtime.mcdcCondition,
    runtime.mcdcEnd,
    runtime.registerProbeV2,
    runtime.mcdcEndV2,
    runtime.coverageHitV2,
    runtime.selectionBegin,
    runtime.selectionRight,
    runtime.selectionEnd,
    runtime.withRequestPhase,
    runtime.optionalSelect,
    runtime.optionalCallBegin,
    runtime.optionalCallReached,
    runtime.optionalCallContinued,
    runtime.optionalCallEnd,
    runtime.defaultSelected,
    runtime.defaultEntered,
    runtime.tryBegin,
    runtime.tryCatch,
    runtime.tryEnd,
    runtime.loopBegin,
    runtime.loopEntered,
    runtime.loopEnd,
    `"use strict";\n${executable}\nreturn { run, observe: typeof observe === "function" ? observe : undefined };`,
  );
  const program = factory(
    coverageHit,
    begin,
    condition,
    end,
    registerProbeV2,
    recorder,
    coverageHitV2,
    selectionBegin,
    selectionRight,
    selectionEnd,
    withRequestPhase,
    optionalSelect,
    optionalCallBegin,
    optionalCallReached,
    optionalCallContinued,
    optionalCallEnd,
    defaultSelected,
    defaultEntered,
    tryBegin,
    tryCatch,
    tryEnd,
    loopBegin,
    loopEntered,
    loopEnd,
  );
  try {
    const value = await program.run();
    return {
      outcome: {
        status: "returned",
        value: normalize(value),
        effects: normalize(program.observe?.() ?? []),
      },
      hits,
      vectors,
      registrations,
    };
  } catch (error) {
    return {
      outcome: {
        status: "threw",
        error: { name: String(error?.name ?? typeof error), message: String(error?.message ?? error) },
        effects: normalize(program.observe?.() ?? []),
      },
      hits,
      vectors,
      registrations,
    };
  }
}

function vectorSet(vectors) {
  return [...new Set(vectors.map((vector) => JSON.stringify(vector)))].sort();
}

for (const [offset, testCase] of executionCorpus.entries()) {
  const candidate = outputs[corpus.length + offset];
  const reference = await executeDifferential(testCase.source, testCase.file, {
    probeVersion: 2,
  });
  if (JSON.stringify(reference.original) !== JSON.stringify(reference.instrumented))
    throw new Error(`${testCase.file}: TypeScript reference changed program behavior`);
  const rustExecution = await executeRustCandidate(testCase, candidate);
  if (JSON.stringify(rustExecution.registrations) !== JSON.stringify(reference.evidence.registrations))
    throw new Error(
      `${testCase.file}: Rust/TypeScript probe registration mismatch\nreference=${JSON.stringify(reference.evidence.registrations)}\ncandidate=${JSON.stringify(rustExecution.registrations)}`,
    );
  if (JSON.stringify(rustExecution.outcome) !== JSON.stringify(reference.original))
    throw new Error(
      `${testCase.file}: Rust candidate changed program behavior\noriginal=${JSON.stringify(reference.original)}\ncandidate=${JSON.stringify(rustExecution.outcome)}`,
    );
  if (JSON.stringify(vectorSet(rustExecution.vectors)) !== JSON.stringify(vectorSet(reference.evidence.vectors)))
    throw new Error(
      `${testCase.file}: Rust/TypeScript probe-v2 vectors differ\nreference=${JSON.stringify(vectorSet(reference.evidence.vectors))}\ncandidate=${JSON.stringify(vectorSet(rustExecution.vectors))}`,
    );
  const supportedHitIds = new Set([
    ...candidate.points.map((point) => point.id),
    ...candidate.branches.flatMap((branch) => branch.alternatives.map((alternative) => alternative.id)),
  ]);
  const referenceHits = [...new Set(reference.evidence.hits.filter((id) => supportedHitIds.has(id)))].sort();
  const candidateHits = [...new Set(rustExecution.hits)].sort();
  if (JSON.stringify(candidateHits) !== JSON.stringify(referenceHits))
    throw new Error(
      `${testCase.file}: Rust/TypeScript point hits differ\nreference=${JSON.stringify(referenceHits)}\ncandidate=${JSON.stringify(candidateHits)}`,
    );
}

for (const [offset, testCase] of generatedCorpus.entries()) {
  const candidate = outputs[corpus.length + executionCorpus.length + offset];
  const reference = await executeDifferential(testCase.source, testCase.file, {
    probeVersion: 2,
  });
  if (JSON.stringify(reference.original) !== JSON.stringify(reference.instrumented))
    throw new Error(`${testCase.file}: TypeScript reference changed generated-program behavior`);
  const rustExecution = await executeRustCandidate(testCase, candidate);
  if (JSON.stringify(rustExecution.registrations) !== JSON.stringify(reference.evidence.registrations))
    throw new Error(`${testCase.file}: Rust/TypeScript generated-program registration mismatch`);
  if (JSON.stringify(rustExecution.outcome) !== JSON.stringify(reference.original))
    throw new Error(
      `${testCase.file}: Rust candidate changed generated-program behavior\noriginal=${JSON.stringify(reference.original)}\ncandidate=${JSON.stringify(rustExecution.outcome)}`,
    );
  if (JSON.stringify(vectorSet(rustExecution.vectors)) !== JSON.stringify(vectorSet(reference.evidence.vectors)))
    throw new Error(`${testCase.file}: Rust/TypeScript generated-program vectors differ`);
  const referenceHits = [...new Set(reference.evidence.hits)].sort();
  const candidateHits = [...new Set(rustExecution.hits)].sort();
  if (JSON.stringify(candidateHits) !== JSON.stringify(referenceHits))
    throw new Error(`${testCase.file}: Rust/TypeScript generated-program hits differ`);
}

console.log(
  `[rust-js-differential] ${allCases.length} oxc/Babel decisions, points, complete branch manifests, and safety limitations match; ${executionCorpus.length} behavior/effect/vector/hit cases and ${generatedCorpus.length} generated behavior cases match`,
);
