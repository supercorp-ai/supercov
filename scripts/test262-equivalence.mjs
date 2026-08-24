#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, relative, resolve } from "node:path";
import { parse as parseYaml } from "yaml";
import { instrumentMcdc } from "../dist/instrumenter.js";

function option(name, fallback) {
  const inline = process.argv.slice(2).find((value) => value.startsWith(`${name}=`));
  if (inline) return inline.slice(name.length + 1);
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}

const test262Root = resolve(
  option("--test262", process.env.TEST262_DIR ?? ".cache/test262"),
);
const shardText = option("--shard", "1/1");
const shardMatch = /^(\d+)\/(\d+)$/.exec(shardText);
if (!shardMatch) throw new Error("--shard must have the form INDEX/TOTAL");
const shardIndex = Number(shardMatch[1]);
const shardTotal = Number(shardMatch[2]);
if (shardIndex < 1 || shardIndex > shardTotal)
  throw new Error("--shard index must be between 1 and the total");
const limitText = option("--limit", "");
const limit = limitText ? Number(limitText) : undefined;
if (limit !== undefined && (!Number.isSafeInteger(limit) || limit < 1))
  throw new Error("--limit must be a positive integer");
const keepTemporary = process.argv.includes("--keep-temp");
const pathPattern = option("--pattern", "");

if (!existsSync(resolve(test262Root, "test")) || !existsSync(resolve(test262Root, "harness"))) {
  throw new Error(
    `Test262 corpus not found at ${test262Root}. Clone tc39/test262 there, set TEST262_DIR, or pass --test262 <path>.`,
  );
}

function walk(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    return entry.isDirectory() ? walk(path) : entry.isFile() && entry.name.endsWith(".js") ? [path] : [];
  });
}

function metadata(source) {
  const match = /\/\*---([\s\S]*?)---\*\//.exec(source);
  if (!match) return {};
  return parseYaml(match[1].replace(/\r\n?/g, "\n")) ?? {};
}

const sourceReflectionTests = new Map([
  [
    "built-ins/RegExp/prototype/exec/S15.10.6.2_A1_T9.js",
    "coerces a Function to its implementation-defined source string and matches that source with a RegExp",
  ],
  [
    "staging/sm/async-functions/toString.js",
    "asserts exact Function.prototype.toString source text",
  ],
  [
    "staging/sm/generators/runtime.js",
    "asserts exact generator Function.prototype.toString source text",
  ],
]);

function exclusionReason(path, source) {
  if (path.includes("_FIXTURE")) return "fixture support file";
  const relativePath = relative(resolve(test262Root, "test"), path).replaceAll(
    "\\",
    "/",
  );
  // Supercov instruments Vite application modules, where Annex B's sloppy
  // block-level function extensions do not apply. Source reflection is an
  // inherent exception for every source-to-source coverage transformer:
  // Function#toString observes the transformed source by definition.
  if (relativePath.startsWith("annexB/"))
    return "Annex B sloppy-script extension outside Supercov's application-module target";
  if (relativePath.startsWith("built-ins/Function/prototype/toString/"))
    return "asserts Function.prototype.toString source text";
  if (sourceReflectionTests.has(relativePath))
    return sourceReflectionTests.get(relativePath);
  const data = metadata(source);
  const flags = new Set(Array.isArray(data.flags) ? data.flags : []);
  if (flags.has("module")) return "module scenario unsupported by the selected Test262 host harness";
  if (flags.has("async")) return "asynchronous Test262 harness scenario";
  if (flags.has("raw")) return "raw Test262 harness scenario";
  if (data.negative?.phase === "parse" || data.negative?.phase === "resolution")
    return "parse/resolution-negative test does not execute source";
  return undefined;
}

function runtimePrelude(transformed) {
  const match = transformed.match(
    /import\s*\{([\s\S]*?)\}\s*from\s*["']virtual:supercov-runtime["'];?/,
  );
  if (!match) return { prelude: "", code: transformed };
  const aliases = Object.fromEntries(
    [...match[1].matchAll(/([\w$]+)\s+as\s+([\w$]+)/g)].map(([, imported, local]) => [
      imported,
      local,
    ]),
  );
  const functions = {
    mcdcBegin: "function(){return {};}",
    mcdcCondition: "function(frame,index,value){return value;}",
    mcdcEnd: "function(frame,value){return value;}",
    coverageHit: "function(){}",
    registerProbeV2: "function(definition){return Object.assign(definition,{clock:{epoch:1,fast:true},hitEpochs:new Uint32Array(definition.pointIds.length),decisionEpochs:definition.decisions.map(function(meta){return new Uint32Array(2*3**Math.min(meta.conditions.length,6));})});}",
    coverageHitV2: "function(){}",
    mcdcEndV2: "function(file,index,encoded,value){return value;}",
    selectionBegin: "function(){return {};}",
    selectionRight: "function(frame,value,name){if(name&&typeof value==='function'&&value.name==='')Object.defineProperty(value,'name',{value:name,configurable:true});return value;}",
    selectionEnd: "function(frame,value){return value;}",
    optionalSelect: "function(shortId,continuedId,value){return value;}",
    optionalCallBegin: "function(shortId,continuedId){return {shortId:shortId,continuedId:continuedId,reached:false,continued:false};}",
    optionalCallReached: "function(frame,value){frame.reached=true;return value;}",
    optionalCallContinued: "function(frame){frame.continued=true;return {[Symbol.iterator]:function(){return {next:function(){return {done:true};}};}};}",
    optionalCallEnd: "function(frame,value){return value;}",
    defaultSelected: "function(id,value,name){if(name&&typeof value==='function'&&value.name==='')Object.defineProperty(value,'name',{value:name,configurable:true});return value;}",
    defaultEntered: "function(){}",
    tryBegin: "function(){return {};}",
    tryCatch: "function(frame,value){return value;}",
    tryEnd: "function(){}",
    loopBegin: "function(){return {};}",
    loopEntered: "function(){}",
    loopEnd: "function(){}",
    withRequestPhase: "function(handler){return handler;}",
  };
  const declarations = Object.entries(aliases).map(([imported, local]) => {
    const implementation = functions[imported];
    if (!implementation) throw new Error(`unknown runtime helper ${imported}`);
    // Module imports are lexical bindings and never create global object
    // properties. `let` is the closest script-host equivalent for Test262.
    return `let ${local}=${implementation};`;
  });
  // Replace the import in place. Babel emits it after directive prologues, and
  // prepending the bindings would turn `"use strict"` into an ordinary string
  // expression and invalidate the equivalence test itself.
  const index = match.index ?? 0;
  return {
    prelude: "",
    code: `${transformed.slice(0, index)}${declarations.join("")}\n${transformed.slice(index + match[0].length)}`,
  };
}

function runHarness(input) {
  const binary = resolve(temporaryHarnessRunnerRoot, "bin/run.js");
  const result = spawnSync(
    process.execPath,
    [
      binary,
      `--test262-dir=${test262Root}`,
      `--includes-dir=${temporaryHarnessRoot}`,
      "--reporter=simple",
      `--threads=${Math.max(1, Number(process.env.TEST262_THREADS ?? 4))}`,
      input,
    ],
    {
      encoding: "utf8",
      maxBuffer: 1024 * 1024 * 256,
      env: {
        ...process.env,
        NODE_PATH: [resolve("node_modules"), process.env.NODE_PATH]
          .filter(Boolean)
          .join(delimiter),
      },
    },
  );
  if (result.error) throw result.error;
  const records = [...result.stdout.matchAll(/^(PASS|FAIL)\s+(.+)$/gm)].map(
    ([, status, file]) => ({ pass: status === "PASS", file }),
  );
  if (records.length === 0) {
    throw new Error(
      `Test262 harness failed (${result.status ?? "signal"}):\n${result.stderr}\n${result.stdout}`,
    );
  }
  return records;
}

function resultKey(record) {
  const normalized = String(record.file).replaceAll("\\", "/");
  for (const marker of ["/original/test/", "/instrumented/test/"]) {
    const index = normalized.indexOf(marker);
    if (index >= 0) return normalized.slice(index + marker.length);
  }
  return normalized;
}

const corpusRoot = resolve(test262Root, "test");
let candidates = walk(corpusRoot)
  .sort()
  .filter((path) => !pathPattern || relative(corpusRoot, path).includes(pathPattern))
  .filter((_, index) => index % shardTotal === shardIndex - 1)
  .map((path) => ({ path, source: readFileSync(path, "utf8") }));
const exclusions = candidates
  .map(({ path, source }) => ({
    file: relative(corpusRoot, path).replaceAll("\\", "/"),
    reason: exclusionReason(path, source),
  }))
  .filter((entry) => entry.reason);
let selected = candidates.filter(
  ({ path, source }) => !exclusionReason(path, source),
);
if (limit !== undefined) selected = selected.slice(0, limit);
if (selected.length === 0) throw new Error("No eligible Test262 tests were selected");

const temporary = mkdtempSync(resolve(tmpdir(), "supercov-test262-"));
const originalRoot = resolve(temporary, "original/test");
const instrumentedRoot = resolve(temporary, "instrumented/test");
const temporaryHarnessRoot = resolve(temporary, "harness");
const temporaryHarnessRunnerRoot = resolve(temporary, "test262-harness");

try {
  cpSync(resolve("node_modules/test262-harness"), temporaryHarnessRunnerRoot, {
    recursive: true,
  });
  const agentPoolPath = resolve(temporaryHarnessRunnerRoot, "lib/agent-pool.js");
  const agentPoolSource = readFileSync(agentPoolPath, "utf8");
  const redundantCompile = "      test.compiled = agent.compile(test.contents);";
  if (!agentPoolSource.includes(redundantCompile))
    throw new Error("Installed Test262 harness no longer matches the validated runner patch");
  writeFileSync(
    agentPoolPath,
    agentPoolSource.replace(
      redundantCompile,
      "      // Supercov: skip reporter-only recompilation after execution.",
    ),
  );
  const sourceHarnessRoot = resolve(test262Root, "harness");
  for (const source of walk(sourceHarnessRoot)) {
    const destination = resolve(temporaryHarnessRoot, relative(sourceHarnessRoot, source));
    mkdirSync(dirname(destination), { recursive: true });
    if (relative(sourceHarnessRoot, source) === "assert.js") {
      // eshost's host-runtime insertion regex has pathological backtracking on
      // long comment/directive prologues. This semantics-free statement ends
      // the prologue immediately after Test262's optional strict directive.
      writeFileSync(destination, `0;\n${readFileSync(source, "utf8")}`);
    } else copyFileSync(source, destination);
  }
  for (const test of selected) {
    const destination = resolve(originalRoot, relative(corpusRoot, test.path));
    mkdirSync(dirname(destination), { recursive: true });
    writeFileSync(destination, test.source);
  }

  const originalResults = runHarness(resolve(originalRoot, "**/*.js"));
  const baselinePasses = new Map();
  for (const record of originalResults.filter((item) => item.pass)) {
    const key = resultKey(record);
    baselinePasses.set(key, (baselinePasses.get(key) ?? 0) + 1);
  }
  const baselineFiles = new Set(baselinePasses.keys());
  const transformationFailures = [];

  for (const test of selected) {
    const relativePath = relative(corpusRoot, test.path).replaceAll("\\", "/");
    if (!baselineFiles.has(relativePath)) continue;
    try {
      const transformed = instrumentMcdc(
        test.source,
        `test262/${relativePath}`,
        {
          probeVersion:
            process.env.SUPERCOV_PROBE_VERSION === "2" ? 2 : 1,
        },
      ).code;
      const runtime = runtimePrelude(transformed);
      const destination = resolve(instrumentedRoot, relativePath);
      mkdirSync(dirname(destination), { recursive: true });
      const copyright =
        test.source
          .split(/\r|\n|\u2028|\u2029/)
          .find((line) => line.includes("Copyright")) ??
        "// Copyright Supercov Test262 equivalence run";
      // Babel may render a transformed legacy fixture onto one very long
      // line. test262-stream's historical copyright regex has nested greedy
      // quantifiers, so retain the original short copyright line up front.
      writeFileSync(
        destination,
        `${copyright}\n${runtime.prelude}${runtime.code}`,
      );
    } catch (error) {
      transformationFailures.push({
        file: relativePath,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  const instrumentedResults = baselineFiles.size
    ? runHarness(resolve(instrumentedRoot, "**/*.js"))
    : [];
  const instrumentedPasses = new Map();
  for (const record of instrumentedResults.filter((item) => item.pass)) {
    const key = resultKey(record);
    instrumentedPasses.set(key, (instrumentedPasses.get(key) ?? 0) + 1);
  }
  const semanticFailures = [];
  for (const [key, expectedPasses] of baselinePasses) {
    const actualPasses = instrumentedPasses.get(key) ?? 0;
    if (actualPasses < expectedPasses) {
      semanticFailures.push({
        test: key,
        error: `${actualPasses}/${expectedPasses} baseline-passing scenario(s) still pass after instrumentation`,
      });
    }
  }

  const baselinePassingScenarios = [...baselinePasses.values()].reduce(
    (total, count) => total + count,
    0,
  );
  const baselineFailed = originalResults.filter((record) => !record.pass).length;
  const revisionResult = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: test262Root,
    encoding: "utf8",
  });
  const revision =
    revisionResult.status === 0 ? revisionResult.stdout.trim() : "unknown";
  console.log(`[test262] revision=${revision} shard=${shardText}`);
  console.log(
    `[test262] selected files=${selected.length}; baseline passing scenarios=${baselinePassingScenarios}; baseline unsupported/failed scenarios=${baselineFailed}`,
  );
  console.log(`[test262] explicitly excluded files in shard=${exclusions.length}`);
  const excludedByReason = new Map();
  for (const exclusion of exclusions)
    excludedByReason.set(
      exclusion.reason,
      (excludedByReason.get(exclusion.reason) ?? 0) + 1,
    );
  for (const [reason, count] of [...excludedByReason].sort(([left], [right]) =>
    String(left).localeCompare(String(right)),
  ))
    console.log(`[test262] excluded ${count}: ${reason}`);
  console.log(
    `[test262] transform failures=${transformationFailures.length}; semantic-equivalence failures=${semanticFailures.length}`,
  );
  for (const failure of [...transformationFailures, ...semanticFailures].slice(0, 100))
    console.error(`[test262] FAIL ${failure.file ?? failure.test}: ${failure.error}`);

  const minimum = Number(
    option("--minimum", String(Math.max(1, Math.floor(selected.length * 0.5)))),
  );
  if (baselinePassingScenarios < minimum)
    throw new Error(`Only ${baselinePassingScenarios} baseline scenarios passed; required ${minimum}`);
  if (transformationFailures.length || semanticFailures.length) process.exitCode = 1;
} finally {
  if (keepTemporary) console.log(`[test262] retained ${temporary}`);
  else rmSync(temporary, { recursive: true, force: true });
}
