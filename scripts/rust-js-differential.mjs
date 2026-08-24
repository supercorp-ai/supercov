#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { instrumentMcdc } from "../src/instrumenter.ts";

const corpusPath = resolve("contracts/js-instrumenter-v1/cases.json");
const corpusText = readFileSync(corpusPath, "utf8");
const corpus = JSON.parse(corpusText);
const rust = spawnSync(
  "cargo",
  ["run", "--quiet", "-p", "supercov-engine", "--example", "js_manifest"],
  { input: corpusText, encoding: "utf8" },
);
if (rust.status !== 0)
  throw new Error(`Rust JS candidate failed (${rust.status}):\n${rust.stderr}`);
const outputs = JSON.parse(rust.stdout);
if (outputs.length !== corpus.length)
  throw new Error(`Rust candidate returned ${outputs.length} outputs for ${corpus.length} cases`);

for (const [index, testCase] of corpus.entries()) {
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

console.log(`[rust-js-differential] ${corpus.length} oxc/Babel if-decision manifests match exactly`);
