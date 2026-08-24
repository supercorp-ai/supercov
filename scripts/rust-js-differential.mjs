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
const allCases = [...corpus, ...executionCorpus, ...generatedCorpus];
const rust = spawnSync(
  "cargo",
  ["run", "--quiet", "-p", "supercov-engine", "--example", "js_manifest"],
  { input: JSON.stringify(allCases), encoding: "utf8" },
);
if (rust.status !== 0)
  throw new Error(`Rust JS candidate failed (${rust.status}):\n${rust.stderr}`);
const outputs = JSON.parse(rust.stdout);
if (outputs.length !== allCases.length)
  throw new Error(`Rust candidate returned ${outputs.length} outputs for ${allCases.length} cases`);

for (const [index, testCase] of allCases.entries()) {
  const reference = instrumentMcdc(testCase.source, testCase.file).manifest.decisions
    .filter((decision) => decision.kind === "if");
  const candidate = outputs[index];
  if (candidate.complete !== false)
    throw new Error(`${testCase.file}: partial Rust slice claimed completeness`);
  if (JSON.stringify(candidate.decisions) !== JSON.stringify(reference))
    throw new Error(
      `${testCase.file}: Rust/TypeScript decision mismatch\nreference=${JSON.stringify(reference)}\ncandidate=${JSON.stringify(candidate.decisions)}`,
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
  const runtimeName = candidate.runtime?.mcdcEndV2;
  if (!runtimeName) throw new Error(`${testCase.file}: missing Rust candidate runtime binding`);
  const recorder = (file, decisionIndex, encoded, value) => {
    if (file !== testCase.file) throw new Error(`unexpected probe file ${file}`);
    const decision = candidate.decisions[decisionIndex];
    if (!decision) throw new Error(`unexpected decision index ${decisionIndex}`);
    vectors.push(decodeVector(decision.conditions.length, encoded, value));
    return value;
  };
  // This evaluates only the checked-in, self-contained differential corpus.
  // eslint-disable-next-line no-new-func
  const factory = new Function(
    runtimeName,
    `"use strict";\n${candidate.code}\nreturn { run, observe: typeof observe === "function" ? observe : undefined };`,
  );
  const program = factory(recorder);
  try {
    const value = await program.run();
    return {
      outcome: {
        status: "returned",
        value: normalize(value),
        effects: normalize(program.observe?.() ?? []),
      },
      vectors,
    };
  } catch (error) {
    return {
      outcome: {
        status: "threw",
        error: { name: String(error?.name ?? typeof error), message: String(error?.message ?? error) },
        effects: normalize(program.observe?.() ?? []),
      },
      vectors,
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
  if (JSON.stringify(rustExecution.outcome) !== JSON.stringify(reference.original))
    throw new Error(
      `${testCase.file}: Rust candidate changed program behavior\noriginal=${JSON.stringify(reference.original)}\ncandidate=${JSON.stringify(rustExecution.outcome)}`,
    );
  if (JSON.stringify(vectorSet(rustExecution.vectors)) !== JSON.stringify(vectorSet(reference.evidence.vectors)))
    throw new Error(
      `${testCase.file}: Rust/TypeScript probe-v2 vectors differ\nreference=${JSON.stringify(vectorSet(reference.evidence.vectors))}\ncandidate=${JSON.stringify(vectorSet(rustExecution.vectors))}`,
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
  if (JSON.stringify(rustExecution.outcome) !== JSON.stringify(reference.original))
    throw new Error(
      `${testCase.file}: Rust candidate changed generated-program behavior\noriginal=${JSON.stringify(reference.original)}\ncandidate=${JSON.stringify(rustExecution.outcome)}`,
    );
}

console.log(
  `[rust-js-differential] ${allCases.length} oxc/Babel if-decision manifests match; ${executionCorpus.length} behavior/effect/vector cases and ${generatedCorpus.length} generated behavior cases match`,
);
