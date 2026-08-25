#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { basename, relative, resolve } from "node:path";
import { inspect } from "node:util";
import { canonicalize } from "./rust-parity-normalize.mjs";

const repository = resolve(new URL("..", import.meta.url).pathname);
const binary =
  process.env.SUPERCOV_RUST_BINARY ?? resolve(repository, "target/debug/supercov");
const candidateRust =
  process.env.SUPERCOV_PARITY_CANDIDATE !== "typescript-reference-repeat";
const temporary = mkdtempSync(resolve(repository, ".rust-engine-parity-"));
const keepTemporary = process.env.SUPERCOV_PARITY_KEEP_TEMP === "1";
if (keepTemporary)
  console.error(`[rust-engine-parity] preserving diagnostics at ${temporary}`);
const builtInFixtures = [
  { name: "playwright", directory: "generic-playwright" },
  { name: "node-test", directory: "generic-node" },
  { name: "esbuild", directory: "generic-esbuild" },
  { name: "webpack", directory: "generic-webpack" },
  {
    name: "swc",
    directory: "generic-swc",
    environment: { SUPERCOV_SOURCE_ROOTS: "src" },
  },
  { name: "next", directory: "generic-next" },
];
const projectArgument = process.argv.indexOf("--project");
const externalProject =
  projectArgument >= 0 && process.argv[projectArgument + 1]
    ? resolve(process.argv[projectArgument + 1])
    : undefined;
const typeScriptRunArgument = process.argv.indexOf("--typescript-run");
const rustRunArgument = process.argv.indexOf("--rust-run");
const existingRuns =
  typeScriptRunArgument >= 0 || rustRunArgument >= 0
    ? {
        typescript: process.argv[typeScriptRunArgument + 1],
        rust: process.argv[rustRunArgument + 1],
      }
    : undefined;
const commandSeparator = process.argv.indexOf("--");
const externalCommand =
  commandSeparator >= 0 ? process.argv.slice(commandSeparator + 1) : ["npm", "test"];
if (projectArgument >= 0 && !externalProject)
  throw new Error("--project requires a project directory");
if (externalProject && externalCommand.length === 0)
  throw new Error("The project parity test requires a test command after --");
if (
  existingRuns &&
  (!externalProject || !existingRuns.typescript || !existingRuns.rust)
)
  throw new Error(
    "--typescript-run and --rust-run must be supplied together with --project",
  );
const fixtures = externalProject
  ? [
      {
        name: `project-${basename(externalProject)}`,
        project: externalProject,
        command: externalCommand,
      },
    ]
  : builtInFixtures;
function execute(cwd, args, rust, environment = {}) {
  const result = spawnSync(
    process.execPath,
    [resolve(repository, "bin/supercov.js"), ...args],
    {
      cwd,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
      env: {
        ...process.env,
        ...environment,
        ...(rust
          ? { SUPERCOV_ENGINE: "rust", SUPERCOV_RUST_BINARY: binary }
          : { SUPERCOV_ENGINE: "typescript" }),
      },
    },
  );
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(
      `${rust ? "Rust" : "TypeScript"} engine command failed (${result.status}):\n${result.stderr}\n${result.stdout}`,
    );
  return result.stdout;
}

function oneRun(project) {
  const runs = readdirSync(resolve(project, ".supercov/runs")).sort();
  if (runs.length !== 1)
    throw new Error(`Expected one run in ${project}, received ${runs}`);
  return runs[0];
}

function runIds(project) {
  try {
    return new Set(readdirSync(resolve(project, ".supercov/runs")));
  } catch {
    return new Set();
  }
}

function oneNewRun(project, previous) {
  const created = [...runIds(project)].filter((run) => !previous.has(run));
  if (created.length !== 1)
    throw new Error(
      `Expected one new run in ${project}, received ${created.join(", ") || "none"}`,
    );
  return created[0];
}

function query(project, run, resource, rust, environment) {
  return JSON.parse(
    execute(
      project,
      ["runs", run, "coverage", ...resource, "--json"],
      rust,
      environment,
    ),
  );
}

function materializeState(project, run, destination, selfHosting) {
  mkdirSync(destination, { recursive: true });
  for (const mode of ["evidence", "report"]) {
    const result = spawnSync(
      process.execPath,
      [
        resolve(repository, "scripts/rust-parity-state.mjs"),
        mode,
        project,
        run,
        destination,
        temporary,
        ...(selfHosting ? ["self-hosting"] : []),
      ],
      { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 },
    );
    if (result.error) throw result.error;
    if (result.status !== 0)
      throw new Error(
        `${mode} materialization failed for ${run} (${result.status}):\n${result.stderr}`,
      );
  }
  const savedContext = JSON.parse(
    readFileSync(resolve(destination, "context.json"), "utf8"),
  );
  const context = {
    run,
    project,
    projectName: basename(project),
    parityRoot: temporary,
    attempts: new Map(savedContext.attempts),
    unorderedEvidence: true,
  };
  return {
    manifest: readFileSync(resolve(destination, "manifest.json"), "utf8"),
    evidence: JSON.parse(
      readFileSync(resolve(destination, "evidence-signatures.json"), "utf8"),
    ),
    context,
    derivedContext: {
      ...context,
      omitTimestampCorrelation: true,
      omitEngineTransportTopology: true,
    },
    report: JSON.parse(
      readFileSync(resolve(destination, "report-state.json"), "utf8"),
    ),
  };
}

function requireEqual(actual, expected, message) {
  try {
    assert.deepStrictEqual(actual, expected);
  } catch (error) {
    throw new Error(
      `${message}\n${
        error instanceof Error ? error.message : inspect(error, { depth: 8 })
      }`,
    );
  }
}

function requireSignaturesEqual(
  actual,
  expected,
  message,
  { allowProbeV2Deduplication = false } = {},
) {
  const differences = [];
  let deduplicatedServerRecords = 0;
  for (const key of Object.keys(expected)) {
    const left = new Map();
    const right = new Map();
    for (const value of expected[key]) {
      const encoded = JSON.stringify(value);
      left.set(encoded, (left.get(encoded) ?? 0) + 1);
    }
    for (const value of actual[key]) {
      const encoded = JSON.stringify(value);
      right.set(encoded, (right.get(encoded) ?? 0) + 1);
    }
    let onlyExpected = 0;
    let onlyActual = 0;
    for (const [value, count] of left)
      onlyExpected += Math.max(0, count - (right.get(value) ?? 0));
    for (const [value, count] of right)
      onlyActual += Math.max(0, count - (left.get(value) ?? 0));
    const probeV2Transport =
      allowProbeV2Deduplication &&
      (key === "scopedServer" || key === "semanticScopedServer");
    const expectedSet = new Set(left.keys());
    const actualSet = new Set(right.keys());
    const isExactSetReduction =
      probeV2Transport &&
      actualSet.size === expectedSet.size &&
      [...actualSet].every((value) => expectedSet.has(value)) &&
      [...right].every(([value, count]) => count <= (left.get(value) ?? 0));
    if (isExactSetReduction) {
      if (key === "scopedServer")
        deduplicatedServerRecords = expected[key].length - actual[key].length;
      continue;
    }
    if (onlyExpected || onlyActual)
      differences.push(`${key}: reference-only=${onlyExpected}, Rust-only=${onlyActual}`);
  }
  if (differences.length > 0)
    throw new Error(`${message}\n${differences.join("\n")}`);
  return deduplicatedServerRecords;
}

function withoutEngineTransportTopology(value) {
  const copy = structuredClone(value);
  const normalize = transport => {
    if (!transport || typeof transport !== "object") return;
    delete transport.scopedServerRecords;
    delete transport.processes;
    delete transport.childLaunches;
  };
  normalize(copy);
  normalize(copy?.transport);
  normalize(copy?.data?.transport);
  return copy;
}

function validateSelfHostingEvidence(actual, expected, message) {
  const actualResults = new Map(actual.results.map((entry) => [entry.key, entry]));
  const expectedResults = new Map(
    expected.results.map((entry) => [entry.key, entry]),
  );
  requireEqual(
    [...actualResults.keys()].sort(),
    [...expectedResults.keys()].sort(),
    `${message}: test-attempt identities differ`,
  );

  const divergent = new Set();
  for (const [key, reference] of expectedResults) {
    const candidate = actualResults.get(key);
    if (candidate.signature === reference.signature) continue;
    if (candidate.semanticSignature !== reference.semanticSignature)
      throw new Error(
        `${message}: ${key} differs outside the selected-engine implementation boundary`,
      );
    if (
      reference.implementationFiles.length === 0 &&
      candidate.implementationFiles.length === 0
    )
      throw new Error(`${message}: ${key} differs without engine implementation evidence`);
    divergent.add(reference.testId);
  }
  if (divergent.size === 0)
    throw new Error(
      `${message}: self-hosting did not exercise engine-specific implementations`,
    );

  requireSignaturesEqual(
    {
      scopedServer: actual.semanticScopedServer,
      backgroundServer: actual.backgroundServer,
    },
    {
      scopedServer: expected.semanticScopedServer,
      backgroundServer: expected.backgroundServer,
    },
    `${message}: evidence outside selected-engine implementation files differs`,
  );
  return divergent;
}

function copyFixture(fixture, destination) {
  const source = resolve(repository, "tests/fixtures", fixture.directory);
  cpSync(source, destination, {
    recursive: true,
    filter: (path) =>
      !relative(source, path)
        .split(/[\\/]/)
        .some((segment) =>
          [
            ".supercov",
            "supercov",
            "dist",
            ".next",
            "coverage",
            "playwright-report",
            "test-results",
          ].includes(segment),
        ),
  });
}

try {
  for (const fixture of fixtures) {
    let projects;
    let runs;
    if (fixture.project) {
      projects = {
        typescript: fixture.project,
        rust: fixture.project,
      };
      if (existingRuns) {
        runs = existingRuns;
      } else {
        const beforeTypeScript = runIds(fixture.project);
        execute(
          fixture.project,
          ["--", ...fixture.command],
          false,
          fixture.environment,
        );
        const typescript = oneNewRun(fixture.project, beforeTypeScript);
        const beforeRust = runIds(fixture.project);
        execute(
          fixture.project,
          ["--", ...fixture.command],
          candidateRust,
          fixture.environment,
        );
        runs = {
          typescript,
          rust: oneNewRun(fixture.project, beforeRust),
        };
      }
    } else {
      const fixtureRoot = resolve(temporary, fixture.name);
      mkdirSync(fixtureRoot, { recursive: true });
      projects = {
        typescript: resolve(fixtureRoot, "typescript"),
        rust: resolve(fixtureRoot, "rust"),
      };
      for (const project of Object.values(projects))
        copyFixture(fixture, project);
      execute(
        projects.typescript,
        ["--", "npm", "test"],
        false,
        fixture.environment,
      );
      execute(
        projects.rust,
        ["--", "npm", "test"],
        candidateRust,
        fixture.environment,
      );
      runs = {
        typescript: oneRun(projects.typescript),
        rust: oneRun(projects.rust),
      };
    }
    const selfHosting =
      resolve(projects.typescript) === repository &&
      resolve(projects.rust) === repository;
    const states = {
      typescript: materializeState(
        projects.typescript,
        runs.typescript,
        resolve(temporary, `${fixture.name}-typescript-state`),
        selfHosting,
      ),
      rust: materializeState(
        projects.rust,
        runs.rust,
        resolve(temporary, `${fixture.name}-rust-state`),
        selfHosting,
      ),
    };

    requireEqual(
      JSON.parse(states.rust.manifest),
      JSON.parse(states.typescript.manifest),
      `${fixture.name}: Rust and TypeScript coverage obligations differ`,
    );
    let selectedEngineTests;
    if (selfHosting) {
      selectedEngineTests = validateSelfHostingEvidence(
        states.rust.evidence,
        states.typescript.evidence,
        `${fixture.name}: normalized raw evidence differs`,
      );
      requireEqual(
        states.rust.report.testOutcomes,
        states.typescript.report.testOutcomes,
        `${fixture.name}: test outcomes differ`,
      );
    } else {
      const deduplicatedServerRecords = requireSignaturesEqual(
        states.rust.evidence,
        states.typescript.evidence,
        `${fixture.name}: normalized raw evidence differs`,
        { allowProbeV2Deduplication: true },
      );
      requireEqual(
        withoutEngineTransportTopology(states.rust.report.transport),
        withoutEngineTransportTopology(states.typescript.report.transport),
        `${fixture.name}: evidence transport counts differ`,
      );
      if (deduplicatedServerRecords > 0)
        console.log(
          `[rust-engine-parity] ${fixture.name}: probe v2 removed ${deduplicatedServerRecords} duplicate server record(s) without removing a unique observation`,
        );
      requireEqual(
        Object.fromEntries(
          Object.entries(states.rust.report.digests).filter(([key]) => key !== "transport"),
        ),
        Object.fromEntries(
          Object.entries(states.typescript.report.digests).filter(([key]) => key !== "transport"),
        ),
        `${fixture.name}: complete analyzed coverage report differs`,
      );
    }

    if (fixture.name === "playwright") {
      requireEqual(
        states.typescript.report.playwrightContract,
        {
          flaky: {
            outcome: "flaky",
            attempts: [
              { retry: 0, status: "failed", expectedStatus: "passed" },
              { retry: 1, status: "passed", expectedStatus: "passed" },
            ],
          },
          skipped: { outcome: "skipped", hits: [] },
          expectedFailure: {
            outcome: "failed",
            attempts: [
              { retry: 0, status: "failed", expectedStatus: "failed" },
            ],
          },
          passedContainsFlakyTerminalAttempt: true,
          passedContainsExpectedFailure: false,
        },
        "playwright: retry, skipped, and expected-failure outcome contract regressed",
      );
    }
    const resources = [
      [],
      ["files"],
      ["gaps"],
      ["kinds"],
      ["runners"],
      ["scope"],
    ];
    const {
      firstFile,
      firstDecision,
      firstTest,
      firstCoveredLine,
    } = states.typescript.report.selectors;
    if (firstFile) resources.push(["file", firstFile]);
    if (firstDecision) resources.push(["decision", firstDecision]);
    if (firstTest) resources.push(["test", firstTest]);
    if (firstCoveredLine)
      resources.push(["covers", firstCoveredLine]);
    for (const resource of selfHosting ? [[]] : resources) {
      const typescript = withoutEngineTransportTopology(canonicalize(
        query(
          projects.typescript,
          runs.typescript,
          resource,
          false,
          fixture.environment,
        ),
        states.typescript.derivedContext,
      ));
      const rust = withoutEngineTransportTopology(canonicalize(
        query(
          projects.rust,
          runs.rust,
          resource,
          candidateRust,
          fixture.environment,
        ),
        states.rust.derivedContext,
      ));
      if (selfHosting) {
        const contract = (response) => ({
          schemaVersion: response.schemaVersion,
          ok: response.ok,
          command: response.command,
          measurement: response.data?.measurement,
          testOutcomes: response.data?.testOutcomes,
          tests: response.data?.tests,
          setups: response.data?.setups,
          sourceScope: response.data?.sourceScope,
        });
        requireEqual(
          contract(rust),
          contract(typescript),
          `${fixture.name}: self-hosted summary contract differs`,
        );
      } else {
        requireEqual(
          rust,
          typescript,
          `${fixture.name}: query mismatch for coverage ${
            resource.join(" ") || "summary"
          }`,
        );
      }
    }
    console.log(selfHosting
      ? `[rust-engine-parity] ${fixture.name}: exact obligations and non-engine evidence; ${selectedEngineTests.size} selected-engine test outcome(s) validated separately`
      : `[rust-engine-parity] ${fixture.name}: exact manifest, evidence, report, attribution, outcome, confidence, and query parity`);
  }
} finally {
  if (!keepTemporary)
    rmSync(temporary, {
      recursive: true,
      force: true,
      maxRetries: 20,
      retryDelay: 25,
    });
}
