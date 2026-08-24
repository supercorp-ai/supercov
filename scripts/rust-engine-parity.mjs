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
import { readEvidenceArchive } from "../dist/evidenceArchive.js";
import { analyzeCoverageArchive } from "../dist/runAnalysis.js";

const repository = resolve(new URL("..", import.meta.url).pathname);
const binary =
  process.env.SUPERCOV_RUST_BINARY ?? resolve(repository, "target/debug/supercov");
const candidateRust =
  process.env.SUPERCOV_PARITY_CANDIDATE !== "typescript-reference-repeat";
const temporary = mkdtempSync(resolve(repository, ".rust-engine-parity-"));
const fixtures = [
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
const omittedDynamicKeys = new Set([
  "generatedAt",
  "integrity",
  "startedAtMs",
  "endedAtMs",
  "timestampMs",
]);

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

function parseLines(contents) {
  return contents
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function collectAttemptIdentities(value, identities) {
  if (Array.isArray(value)) {
    for (const entry of value) collectAttemptIdentities(entry, identities);
    return;
  }
  if (!value || typeof value !== "object") return;
  const scope = value.scope;
  if (
    scope &&
    typeof scope === "object" &&
    typeof scope.attemptId === "string" &&
    typeof scope.testId === "string"
  ) {
    identities.set(
      scope.attemptId,
      `<attempt:${scope.testId}:retry-${scope.retry ?? 0}>`,
    );
  }
  for (const entry of Object.values(value))
    collectAttemptIdentities(entry, identities);
}

function canonicalize(value, context, key) {
  if (key && omittedDynamicKeys.has(key)) return undefined;
  // A timestamp-only event is deliberately a weak attribution hint. Identical
  // reference-engine runs can place it on adjacent phases when their wall
  // clocks cross a phase boundary. Raw phase definitions and every explicit
  // phaseId are still compared exactly; only this derived hint is excluded.
  if (context.omitTimestampCorrelation && key === "phases") return undefined;
  if (
    context.omitTimestampCorrelation &&
    (key === "browserFallback" || key === "serverFallback")
  )
    return undefined;
  if (Array.isArray(value)) {
    const entries = value
      .map((entry) => canonicalize(entry, context))
      .filter((entry) => entry !== undefined);
    return key === "phases" || key === "explicitPhases"
      ? entries.sort((left, right) =>
          JSON.stringify(left).localeCompare(JSON.stringify(right)),
        )
      : entries;
  }
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value)
        .map(([entryKey, entry]) => [
          entryKey,
          canonicalize(entry, context, entryKey),
        ])
        .filter(([, entry]) => entry !== undefined),
    );
  if (typeof value !== "string") return value;
  if (key === "workerId")
    value = value
      .replace(/^pid-\d+-worker-(\d+)$/, "pid-<runtime>-worker-$1")
      .replace(/^node:test-\d+$/, "node:test-<runtime>");
  let result = value
    .replaceAll(
      `${context.project}/supercov/workspace/${basename(context.project)}`,
      "<project>/supercov/workspace/<workspace>",
    )
    .replaceAll(context.run, "<run-id>")
    .replaceAll(context.project, "<project>")
    .replaceAll(temporary, "<parity-root>");
  for (const [attempt, identity] of context.attempts)
    result = result.replaceAll(attempt, identity);
  return result;
}

function canonicalEvidence(archive, context) {
  const results = archive.files
    .filter((entry) => /(?:^|\/)mcdc\.json$/.test(entry.path))
    .map((entry) => JSON.parse(entry.contents));
  const scopedServer = archive.files
    .filter(
      (entry) =>
        entry.path.startsWith("server/") &&
        !entry.path.startsWith("server/background/") &&
        entry.path.endsWith(".jsonl"),
    )
    .flatMap((entry) => parseLines(entry.contents));
  const backgroundServer = archive.files
    .filter((entry) => /^server\/background\/.*\.jsonl$/.test(entry.path))
    .flatMap((entry) => parseLines(entry.contents));
  for (const value of [...results, ...scopedServer])
    collectAttemptIdentities(value, context.attempts);
  const canonicalRecords = (values) =>
    values
      .map((value) => canonicalize(value, context))
      .sort((left, right) =>
        JSON.stringify(left).localeCompare(JSON.stringify(right)),
      );
  return {
    results: canonicalRecords(results),
    scopedServer: canonicalRecords(scopedServer),
    backgroundServer: canonicalRecords(backgroundServer),
  };
}

function runState(project, run) {
  const directory = resolve(project, ".supercov/runs", run);
  const metadata = JSON.parse(
    readFileSync(resolve(directory, "run.json"), "utf8"),
  );
  const archivePath = resolve(directory, "evidence.raw.gz");
  const archive = readEvidenceArchive(archivePath);
  const context = { run, project, attempts: new Map() };
  const evidence = canonicalEvidence(archive, context);
  const derivedContext = {
    ...context,
    omitTimestampCorrelation: true,
  };
  const report = canonicalize(
    analyzeCoverageArchive(archivePath, {
      runId: run,
      testExitCode: metadata.testExitCode,
      integrity: metadata.integrity,
      generatedAt: metadata.startedAt,
    }),
    derivedContext,
  );
  return {
    archive,
    context,
    derivedContext,
    evidence,
    metadata,
    report,
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
    const fixtureRoot = resolve(temporary, fixture.name);
    mkdirSync(fixtureRoot, { recursive: true });
    const projects = {
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
    const runs = {
      typescript: oneRun(projects.typescript),
      rust: oneRun(projects.rust),
    };
    const states = {
      typescript: runState(projects.typescript, runs.typescript),
      rust: runState(projects.rust, runs.rust),
    };

    const manifests = Object.fromEntries(
      Object.entries(states).map(([engine, state]) => [
        engine,
        state.archive.files.find((entry) => entry.path === "manifest.json")
          ?.contents,
      ]),
    );
    if (!manifests.typescript || manifests.typescript !== manifests.rust)
      throw new Error(
        `${fixture.name}: Rust and TypeScript evidence archives contain different manifests`,
      );
    requireEqual(
      states.rust.evidence,
      states.typescript.evidence,
      `${fixture.name}: normalized raw evidence differs`,
    );
    requireEqual(
      states.rust.report,
      states.typescript.report,
      `${fixture.name}: complete analyzed coverage report differs`,
    );

    const referenceReport = states.typescript.report;
    const resources = [
      [],
      ["files"],
      ["gaps"],
      ["kinds"],
      ["runners"],
      ["scope"],
    ];
    const firstFile = referenceReport.lines[0]?.file;
    const firstDecision = referenceReport.decisions[0]?.meta.id;
    const firstTest = referenceReport.tests.find(
      (test) => test.role === "test",
    )?.id;
    const firstCoveredLine = referenceReport.lines.find(
      (line) => line.covered,
    );
    if (firstFile) resources.push(["file", firstFile]);
    if (firstDecision) resources.push(["decision", firstDecision]);
    if (firstTest) resources.push(["test", firstTest]);
    if (firstCoveredLine)
      resources.push([
        "covers",
        `${firstCoveredLine.file}:${firstCoveredLine.line}`,
      ]);
    for (const resource of resources) {
      const typescript = canonicalize(
        query(
          projects.typescript,
          runs.typescript,
          resource,
          false,
          fixture.environment,
        ),
        states.typescript.derivedContext,
      );
      const rust = canonicalize(
        query(
          projects.rust,
          runs.rust,
          resource,
          candidateRust,
          fixture.environment,
        ),
        states.rust.derivedContext,
      );
      requireEqual(
        rust,
        typescript,
        `${fixture.name}: query mismatch for coverage ${
          resource.join(" ") || "summary"
        }`,
      );
    }
    console.log(
      `[rust-engine-parity] ${fixture.name}: exact manifest, evidence, report, attribution, outcome, confidence, and query parity`,
    );
  }
} finally {
  rmSync(temporary, {
    recursive: true,
    force: true,
    maxRetries: 20,
    retryDelay: 25,
  });
}
