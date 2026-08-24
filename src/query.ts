import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { coverageSummaryForTests, isIndependencePair } from "./analyze.ts";
import { EVIDENCE_ARCHIVE_SCHEMA_VERSION } from "./evidenceArchive.ts";
import {
  analyzeCoverageArchiveCached,
  readCoverageQueryIndex,
} from "./queryCache.ts";
import type {
  CoverageRunIntegrity,
  CoverageLimitation,
  McdcDecisionResult,
  McdcReport,
  McdcVector,
} from "./types.ts";
import { compareRunIntegrity, createRunIntegrity } from "./integrity.ts";
import {
  evaluateCoverageWaivers,
  readCoverageWaivers,
  WAIVERS_FILE,
  type CoverageWaiverEvaluation,
} from "./waivers.ts";
import { discoverCoverageProject } from "./project.ts";
import {
  agentPagination,
  agentSuccessJson,
  SupercovError,
  type AgentJsonPagination,
} from "./agentJson.ts";

interface StoredRun {
  id: string;
  evidencePath: string;
  metadata?: {
    startedAt?: string;
    command?: string[];
    durationMs?: number;
    timings?: {
      initializationMs: number;
      workspacePreparationMs: number;
      adapterSetupMs: number;
      instrumentedBuildMs: number;
      testCommandMs: number;
      evidencePublicationMs: number;
    };
    testExitCode?: number | null;
    integrity?: CoverageRunIntegrity;
    instrumentedBuildCache?: { key: string; reused: boolean };
    rawEvidence?: {
      schemaVersion: number;
      format: string;
      file: string;
      files: number;
      uncompressedBytes: number;
      compressedBytes: number;
    };
  };
}

interface QueryOptions {
  command: string;
  run?: string;
  kind?: string;
  runner?: string;
  filter: "all" | "passed" | "failed";
  limit: number;
  offset: number;
  json: boolean;
  target: number;
  metric: "all" | "lines" | "statements" | "functions" | "branches" | "mcdc";
  group: "none" | "decision";
  sort: "location" | "missing";
  positional: string[];
}

function parseOptions(command: string, args: string[]): QueryOptions {
  const options: QueryOptions = {
    command: command === "runs" || command === "diff" || command === "help"
      ? command
      : `coverage.${command}`,
    limit: 20,
    offset: 0,
    json: false,
    filter: "all",
    target: 100,
    metric: "all",
    group: "none",
    sort: "location",
    positional: [],
  };
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index]!;
    if (value === "--json") options.json = true;
    else if (value === "--run") {
      const run = args[++index];
      if (!run)
        throw new SupercovError("INVALID_ARGUMENT", "--run requires a run ID");
      options.run = run;
    }
    else if (value === "--kind") {
      const kind = args[++index]?.toLowerCase();
      if (!kind)
        throw new SupercovError("INVALID_ARGUMENT", "--kind requires a test kind");
      options.kind = kind;
    }
    else if (value === "--runner") {
      const runner = args[++index]?.toLowerCase();
      if (!runner)
        throw new SupercovError("INVALID_ARGUMENT", "--runner requires a runner name");
      options.runner = runner;
    }
    else if (value === "--target") {
      const target = Number(args[++index]);
      if (!Number.isFinite(target) || target < 0 || target > 100)
        throw new SupercovError("INVALID_ARGUMENT", "--target must be between 0 and 100");
      options.target = target;
    }
    else if (value === "--metric") {
      const metric = args[++index]?.toLowerCase();
      if (!metric || !["all", "lines", "statements", "functions", "branches", "mcdc"].includes(metric))
        throw new SupercovError("INVALID_ARGUMENT", "--metric must be all, lines, statements, functions, branches, or mcdc");
      options.metric = metric as QueryOptions["metric"];
    }
    else if (value === "--filter") {
      const filter = args[++index]?.toLowerCase();
      if (filter !== "all" && filter !== "passed" && filter !== "failed")
        throw new SupercovError("INVALID_ARGUMENT", "--filter must be all, passed, or failed");
      options.filter = filter;
    }
    else if (value === "--limit") {
      const limit = Number(args[++index]);
      if (!Number.isSafeInteger(limit) || limit < 1)
        throw new SupercovError("INVALID_ARGUMENT", "--limit must be a positive integer");
      options.limit = limit;
    }
    else if (value === "--offset") {
      const offset = Number(args[++index]);
      if (!Number.isSafeInteger(offset) || offset < 0)
        throw new SupercovError("INVALID_ARGUMENT", "--offset must be a non-negative integer");
      options.offset = offset;
    }
    else if (value === "--group") {
      const group = args[++index]?.toLowerCase();
      if (group !== "decision")
        throw new SupercovError("INVALID_ARGUMENT", "--group must be decision");
      if (command !== "file")
        throw new SupercovError(
          "INVALID_ARGUMENT",
          "--group is only supported by: supercov runs <run-id> coverage file <source-file>",
        );
      options.group = "decision";
    }
    else if (value === "--sort") {
      const sort = args[++index]?.toLowerCase();
      if (sort !== "location" && sort !== "missing")
        throw new SupercovError("INVALID_ARGUMENT", "--sort must be location or missing");
      options.sort = sort;
    }
    else if (value.startsWith("--"))
      throw new SupercovError("INVALID_ARGUMENT", `Unknown option: ${value}`, {
        details: { option: value },
      });
    else options.positional.push(value);
  }
  if (options.sort !== "location" && options.group === "none")
    throw new SupercovError(
      "INVALID_ARGUMENT",
      "--sort requires --group decision",
    );
  if (options.group === "decision" && options.metric !== "all" && options.metric !== "mcdc")
    throw new SupercovError(
      "INVALID_ARGUMENT",
      "--group decision lists MC/DC decisions; omit --metric or use --metric mcdc",
    );
  return options;
}

function filteredCoverage(
  report: McdcReport,
  options: QueryOptions,
): McdcReport {
  if (options.filter === "all") return report;
  const filtered = report.filters?.[options.filter];
  if (!filtered) {
    throw new SupercovError(
      "FILTER_UNAVAILABLE",
      "This run does not contain outcome-filtered coverage. Create a new coverage run.",
    );
  }
  return filtered as McdcReport;
}

function readJson<T>(path: string): T | undefined {
  try {
    return JSON.parse(readFileSync(path, "utf8")) as T;
  } catch {
    return undefined;
  }
}

function discoverRuns(root: string): StoredRun[] {
  const runs = new Map<string, StoredRun>();
  const canonical = resolve(root, ".supercov/runs");
  if (existsSync(canonical)) {
    for (const entry of readdirSync(canonical, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const evidencePath = resolve(canonical, entry.name, "evidence.raw.gz");
      const metadata = readJson<StoredRun["metadata"]>(
        resolve(canonical, entry.name, "run.json"),
      );
      if (
        !existsSync(evidencePath) ||
        !metadata ||
        metadata.rawEvidence?.schemaVersion !== EVIDENCE_ARCHIVE_SCHEMA_VERSION
      ) continue;
      runs.set(entry.name, {
        id: entry.name,
        evidencePath,
        metadata,
      });
    }
  }
  return [...runs.values()].sort((left, right) =>
    right.id.localeCompare(left.id),
  );
}

function storedRunAnalysisOptions(run: StoredRun) {
  return {
    runId: run.id,
    testExitCode: run.metadata?.testExitCode,
    integrity: run.metadata?.integrity,
    generatedAt: run.metadata?.startedAt,
  };
}

function analyzeStoredRun(run: StoredRun): McdcReport {
  return analyzeCoverageArchiveCached(
    run.evidencePath,
    storedRunAnalysisOptions(run),
  );
}

function readStoredRunIndex(run: StoredRun): McdcReport | undefined {
  return readCoverageQueryIndex(
    run.evidencePath,
    storedRunAnalysisOptions(run),
  );
}

function selectRun(
  root: string,
  selector?: string,
  currentIntegrity?: CoverageRunIntegrity,
  quiet = false,
): {
  run: StoredRun;
  report: McdcReport;
} {
  const runs = discoverRuns(root);
  if (runs.length === 0)
    throw new SupercovError("NO_RUNS", "No local coverage runs. Run supercov first.");
  const selected =
    !selector || selector === "latest"
      ? runs[0]
      : (runs.find((run) => run.id === selector) ??
        runs.find((run) => run.id.startsWith(selector)));
  if (!selected)
    throw new SupercovError("RUN_NOT_FOUND", `Coverage run not found: ${selector}`, {
      details: { selector },
    });
  const report = analyzeStoredRun(selected);
  if (currentIntegrity) {
    const comparison = compareRunIntegrity(
      selected.metadata?.integrity ?? report.integrity,
      currentIntegrity,
    );
    report.integrity = {
      ...(selected.metadata?.integrity ?? report.integrity ?? currentIntegrity),
      stale: comparison.stale,
      staleReasons: comparison.reasons,
    };
    if (comparison.stale && !quiet) {
      console.error(
        `[supercov] stale run ${selected.id}: ${comparison.reasons.join(", ")}`,
      );
    }
  }
  return { run: selected, report };
}

function currentProjectIntegrity(root: string): CoverageRunIntegrity | undefined {
  try {
    return createRunIntegrity(
      root,
      discoverCoverageProject(root),
      fileURLToPath(new URL(".", import.meta.url)),
    );
  } catch {
    return undefined;
  }
}

function page<T>(values: T[], options: QueryOptions): T[] {
  return values.slice(options.offset, options.offset + options.limit);
}

function output(
  value: unknown,
  options: QueryOptions,
  text: string,
  pagination?: AgentJsonPagination,
): void {
  process.stdout.write(
    options.json ? agentSuccessJson(options.command, value, pagination) : `${text}\n`,
  );
}

function queryPagination(
  total: number,
  returned: number,
  options: QueryOptions,
): AgentJsonPagination {
  return agentPagination(options.offset, options.limit, returned, total);
}

function pct(value: number): string {
  return `${value.toFixed(2)}%`;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'\\''`)}'`;
}

type MinimizeMetric = Exclude<QueryOptions["metric"], "all">;

interface CoverageObligation {
  id: string;
  metric: MinimizeMetric;
  /** Any one option fully contained in the selected set satisfies this obligation. */
  options: string[][];
}

export interface MinimumTestSetResult {
  optimal: true;
  target: number;
  metric: QueryOptions["metric"];
  selected: string[];
  expanded: string[];
  summary: McdcReport["summary"];
  exploredStates: number;
}

function optionKey(option: string[]): string {
  return [...new Set(option)].sort().join("\0");
}

function minimumTestObligations(
  report: McdcReport,
  candidates: Set<string>,
): {
  obligations: CoverageObligation[];
  expand(selected: Set<string>): Set<string>;
} {
  const tests = new Map(report.tests.map((test) => [test.id, test]));
  const testsByFile = new Map<string, string[]>();
  for (const test of report.tests) {
    if (test.role !== "test" || !test.file || !candidates.has(test.id)) continue;
    const ids = testsByFile.get(test.file) ?? [];
    ids.push(test.id);
    testsByFile.set(test.file, ids);
  }
  const evidenceChoices = (ids: string[]): string[][] => {
    const options: string[][] = [];
    for (const id of ids) {
      const test = tests.get(id);
      if (!test) continue;
      if (test.role === "background") options.push([]);
      else if (test.role === "setup" && test.file)
        options.push(...(testsByFile.get(test.file) ?? []).map((candidate) => [candidate]));
      else if (candidates.has(id)) options.push([id]);
    }
    return [...new Map(options.map((option) => [optionKey(option), option])).values()];
  };
  const obligations: CoverageObligation[] = [];
  const uniqueLines = new Map<string, McdcReport["lines"][number]>();
  for (const line of report.lines) uniqueLines.set(`${line.file}:${line.line}`, line);
  for (const [id, line] of uniqueLines)
    obligations.push({ id: `line:${id}`, metric: "lines", options: evidenceChoices(line.tests) });
  for (const point of report.points)
    obligations.push({
      id: `${point.meta.kind}:${point.meta.id}`,
      metric: point.meta.kind === "statement" ? "statements" : "functions",
      options: evidenceChoices(point.tests),
    });
  for (const branch of report.branches)
    for (const alternative of branch.alternatives)
      obligations.push({
        id: `branch:${branch.meta.id}:${alternative.id}`,
        metric: "branches",
        options: evidenceChoices(alternative.tests),
      });
  for (const decision of report.decisions) {
    for (let condition = 0; condition < decision.meta.conditions.length; condition += 1) {
      const options: string[][] = [];
      for (let left = 0; left < decision.vectorObservations.length; left += 1) {
        for (let right = left + 1; right < decision.vectorObservations.length; right += 1) {
          const first = decision.vectorObservations[left]!;
          const second = decision.vectorObservations[right]!;
          if (!isIndependencePair(first.vector, second.vector, condition)) continue;
          for (const firstChoice of evidenceChoices(first.tests))
            for (const secondChoice of evidenceChoices(second.tests))
              options.push([...new Set([...firstChoice, ...secondChoice])].sort());
        }
      }
      obligations.push({
        id: `mcdc:${decision.meta.id}:${condition}`,
        metric: "mcdc",
        options: [...new Map(options.map((option) => [optionKey(option), option])).values()],
      });
    }
  }
  return {
    obligations,
    expand(selected) {
      const expanded = new Set(selected);
      for (const test of report.tests) {
        if (test.role === "background") expanded.add(test.id);
        else if (
          test.role === "setup" &&
          test.file &&
          (testsByFile.get(test.file) ?? []).some((id) => selected.has(id))
        ) expanded.add(test.id);
      }
      return expanded;
    },
  };
}

function obligationSatisfied(obligation: CoverageObligation, selected: Set<string>): boolean {
  return obligation.options.some((option) => option.every((test) => selected.has(test)));
}

/** Exact branch-and-bound solver; MC/DC obligations retain their witness-pair structure. */
export function minimumTestSet(
  report: McdcReport,
  target = 100,
  metric: QueryOptions["metric"] = "all",
  maxStates = 5_000,
): MinimumTestSetResult {
  const unattributed = report.tests.filter(
    (test) =>
      test.role === "background" &&
      (test.hits.length > 0 || test.decisions.some((decision) => decision.vectors.length > 0)),
  );
  if (unattributed.length > 0) {
    throw new SupercovError(
      "UNATTRIBUTED_EVIDENCE",
      "Cannot minimize exactly: this coverage view contains background/unattributed evidence. Use a runner with exact test attribution or select a fully attributed coverage view.",
    );
  }
  const candidateTests = report.tests
    .filter((test) => test.role === "test")
    .map((test) => test.id)
    .sort();
  const candidates = new Set(candidateTests);
  const model = minimumTestObligations(report, candidates);
  const metrics: MinimizeMetric[] = metric === "all"
    ? ["lines", "statements", "functions", "branches", "mcdc"]
    : [metric];
  const obligations = model.obligations.filter((obligation) => metrics.includes(obligation.metric));
  const totals = new Map<MinimizeMetric, number>();
  for (const obligation of obligations)
    totals.set(obligation.metric, (totals.get(obligation.metric) ?? 0) + 1);
  const skipLimits = new Map<MinimizeMetric, number>();
  for (const selectedMetric of metrics) {
    const total = totals.get(selectedMetric) ?? 0;
    const required = Math.ceil((total * target) / 100);
    skipLimits.set(selectedMetric, total - required);
  }
  let best = new Set(candidateTests);
  const fullExpanded = model.expand(best);
  const fullSummary = coverageSummaryForTests(report, fullExpanded);
  const percentage = (selectedMetric: MinimizeMetric, summary: McdcReport["summary"]): number =>
    selectedMetric === "mcdc" ? summary.conditionCoveragePct : summary[selectedMetric].percentage;
  const impossible = metrics.find((selectedMetric) => percentage(selectedMetric, fullSummary) + 1e-9 < target);
  if (impossible)
    throw new SupercovError(
      "TARGET_UNREACHABLE",
      `The full selected test view reaches only ${percentage(impossible, fullSummary).toFixed(2)}% ${impossible}; target ${target}% is impossible`,
      { details: { metric: impossible, target, reachable: percentage(impossible, fullSummary) } },
    );

  let exploredStates = 0;
  const seen = new Set<string>();
  const search = (
    selected: Set<string>,
    skipped: Set<string>,
    skippedByMetric: Map<MinimizeMetric, number>,
  ): void => {
    exploredStates += 1;
    if (exploredStates > maxStates) {
      throw new SupercovError(
        "MINIMIZATION_COMPLEXITY_LIMIT",
        `Exact minimization exceeded its ${maxStates.toLocaleString()}-state safety budget. Narrow the test view with --kind or --runner, or request a different target.`,
        {
          details: {
            candidateTests: candidateTests.length,
            obligations: obligations.length,
            exploredStates,
            maxStates,
            target,
            metric,
          },
        },
      );
    }
    if (selected.size >= best.size) return;
    const stateKey = `${[...selected].sort().join(",")}|${[...skipped].sort().join(",")}`;
    if (seen.has(stateKey)) return;
    seen.add(stateKey);
    const unmet = obligations.filter(
      (obligation) => !skipped.has(obligation.id) && !obligationSatisfied(obligation, selected),
    );
    if (unmet.length === 0) {
      best = new Set(selected);
      return;
    }
    const obligation = unmet.sort((left, right) => {
      const feasible = (value: CoverageObligation) =>
        value.options.filter((option) => option.some((test) => !selected.has(test))).length;
      return feasible(left) - feasible(right) || left.id.localeCompare(right.id);
    })[0]!;
    const additions = [...new Map(
      obligation.options
        .map((option) => option.filter((test) => !selected.has(test)))
        .filter((option) => option.length > 0)
        .map((option) => [optionKey(option), option]),
    ).values()].sort((left, right) => left.length - right.length || optionKey(left).localeCompare(optionKey(right)));
    for (const addition of additions) {
      if (selected.size + addition.length >= best.size) continue;
      search(new Set([...selected, ...addition]), skipped, skippedByMetric);
    }
    const skippedCount = skippedByMetric.get(obligation.metric) ?? 0;
    if (skippedCount < (skipLimits.get(obligation.metric) ?? 0)) {
      const nextSkipped = new Set(skipped);
      nextSkipped.add(obligation.id);
      const nextCounts = new Map(skippedByMetric);
      nextCounts.set(obligation.metric, skippedCount + 1);
      search(selected, nextSkipped, nextCounts);
    }
  };
  search(new Set(), new Set(), new Map());
  const expanded = model.expand(best);
  return {
    optimal: true,
    target,
    metric,
    selected: [...best].sort(),
    expanded: [...expanded].sort(),
    summary: coverageSummaryForTests(report, expanded),
    exploredStates,
  };
}

function coverageCommand(
  runId: string,
  options: QueryOptions,
  child: string,
): string {
  return [
    "npx supercov runs",
    shellQuote(runId),
    "coverage",
    child,
    options.filter !== "all" ? `--filter ${options.filter}` : undefined,
    options.kind ? `--kind ${shellQuote(options.kind)}` : undefined,
    options.runner ? `--runner ${shellQuote(options.runner)}` : undefined,
    options.metric !== "all" ? `--metric ${options.metric}` : undefined,
  ]
    .filter(Boolean)
    .join(" ");
}

function pageLabel(total: number, returned: number, options: QueryOptions): string {
  const start = total === 0 || returned === 0 ? 0 : options.offset + 1;
  const end = Math.min(options.offset + returned, total);
  return `showing ${start}-${end} of ${total}`;
}

function nextPageCommand(
  base: string,
  total: number,
  returned: number,
  options: QueryOptions,
): string | undefined {
  const nextOffset = options.offset + returned;
  if (returned === 0 || nextOffset >= total) return undefined;
  return `${base} --offset ${nextOffset}${options.limit !== 20 ? ` --limit ${options.limit}` : ""}`;
}

function selectedTestIds(
  report: McdcReport,
  options: QueryOptions,
): Set<string> | undefined {
  if (!options.kind && !options.runner) return undefined;
  const selected = report.tests.filter(
    (test) =>
      (!options.kind || test.provenance.kind === options.kind) &&
      (!options.runner || test.provenance.runner === options.runner),
  );
  if (selected.length === 0) {
    const filter = [
      options.kind ? `kind=${options.kind}` : undefined,
      options.runner ? `runner=${options.runner}` : undefined,
    ]
      .filter(Boolean)
      .join(", ");
    throw new SupercovError("TEST_FILTER_EMPTY", `No tests match ${filter}`, {
      details: { kind: options.kind, runner: options.runner },
    });
  }
  return new Set(selected.map((test) => test.id));
}

function includesSelectedTest(
  tests: string[],
  selected?: Set<string>,
): boolean {
  return selected ? tests.some((test) => selected.has(test)) : tests.length > 0;
}

function otherCoverage(
  report: McdcReport,
  testIds: string[],
  selected?: Set<string>,
): {
  coveredElsewhere: boolean;
  kinds: string[];
  runners: string[];
  tests: Array<{ id: string; name: string }>;
} {
  const tests = selected
    ? testIds
        .filter((id) => !selected.has(id))
        .map((id) => report.tests.find((test) => test.id === id))
        .filter((test): test is McdcReport["tests"][number] => Boolean(test))
    : [];
  return {
    coveredElsewhere: tests.length > 0,
    kinds: [...new Set(tests.map((test) => test.provenance.kind))].sort(),
    runners: [...new Set(tests.map((test) => test.provenance.runner))].sort(),
    tests: tests.map((test) => ({ id: test.id, name: test.name })),
  };
}

function filterDecision(
  decision: McdcDecisionResult,
  selected?: Set<string>,
): McdcDecisionResult {
  if (!selected) return decision;
  const vectorObservations = decision.vectorObservations
    .map((observation) => ({
      ...observation,
      tests: observation.tests.filter((test) => selected.has(test)),
    }))
    .filter((observation) => observation.tests.length > 0);
  const vectors = vectorObservations.map((observation) => observation.vector);
  const conditions = decision.meta.conditions.map((source, index) => {
    let witness: [McdcVector, McdcVector] | undefined;
    for (let left = 0; left < vectors.length && !witness; left += 1) {
      for (let right = left + 1; right < vectors.length; right += 1) {
        const first = vectors[left]!;
        const second = vectors[right]!;
        if (isIndependencePair(first, second, index)) {
          witness = [first, second];
          break;
        }
      }
    }
    const witnessTests = witness?.map(
      (vector) =>
        vectorObservations.find((observation) => observation.vector === vector)
          ?.tests ?? [],
    ) as [string[], string[]] | undefined;
    return {
      index,
      source,
      covered: Boolean(witness),
      ...(witness ? { witness } : {}),
      ...(witnessTests ? { witnessTests } : {}),
    };
  });
  return {
    ...decision,
    executed: vectors.length > 0,
    covered: conditions.every((condition) => condition.covered),
    vectors,
    vectorObservations,
    conditions,
    tests: decision.tests.filter((test) => selected.has(test)),
  };
}

function filterLabel(options: QueryOptions): string {
  return [
    options.filter !== "all" ? `${options.filter} attempts only` : undefined,
    options.kind ? `kind ${options.kind}` : undefined,
    options.runner ? `runner ${options.runner}` : undefined,
  ]
    .filter(Boolean)
    .join(", ");
}

function queryFilters(options: QueryOptions): {
  outcome: QueryOptions["filter"];
  kind: string | null;
  runner: string | null;
} {
  return {
    outcome: options.filter,
    kind: options.kind ?? null,
    runner: options.runner ?? null,
  };
}

function attribution(
  report: McdcReport,
  selected?: Set<string>,
): Record<string, number> {
  const phases = selected
    ? report.phases.filter((phase) => selected.has(phase.test))
    : report.phases;
  return {
    browserExplicit: phases.reduce(
      (sum, phase) => sum + phase.explicitBrowserEvents,
      0,
    ),
    browserFallback: phases.reduce(
      (sum, phase) => sum + phase.inferredBrowserEvents,
      0,
    ),
    serverExplicit: phases.reduce(
      (sum, phase) => sum + phase.explicitServerEvents,
      0,
    ),
    serverFallback: phases.reduce(
      (sum, phase) => sum + phase.inferredServerEvents,
      0,
    ),
  };
}

function coverageDiagnostics(
  report: McdcReport,
  selected?: Set<string>,
): Array<{
  code: "REMOTE_SERVER_EVIDENCE_MISSING" | "CORRUPT_EVIDENCE_RECORDS";
  severity: "warning" | "error";
  message: string;
}> {
  const observed = attribution(report, selected);
  const diagnostics: ReturnType<typeof coverageDiagnostics> = [];
  if ((report.transport?.corruptRecords ?? 0) > 0) {
    diagnostics.push({
      code: "CORRUPT_EVIDENCE_RECORDS",
      severity: "error",
      message:
        `${report.transport!.corruptRecords} malformed evidence record(s) in ` +
        `${report.transport!.corruptFiles} file(s) were excluded; coverage is incomplete.`,
    });
  }
  if (
    (report.transport?.remoteLaunches ?? 0) > 0 &&
    (report.transport?.scopedServerRecords ?? 0) === 0 &&
    observed.serverExplicit === 0 &&
    observed.serverFallback === 0
  ) {
    diagnostics.push({
      code: "REMOTE_SERVER_EVIDENCE_MISSING",
      severity: "warning",
      message:
        "Remote launches were supervised, but no server evidence returned. Coverage may describe only browser/test processes; inspect how the application server is launched.",
    });
  }
  return diagnostics;
}

interface FileGap {
  file: string;
  uncoveredLines: number;
  uncoveredStatements: number;
  uncoveredFunctions: number;
  missingBranches: number;
  missingMcdcConditions: number;
  measurementLimitations: number;
  limitationKinds: CoverageLimitation["kind"][];
  coveredByOtherTests: {
    lines: number;
    statements: number;
    functions: number;
    branches: number;
    mcdcConditions: number;
  };
  uncoveredEverywhere: {
    lines: number;
    statements: number;
    functions: number;
    branches: number;
    mcdcConditions: number;
  };
  score: number;
}

function gapMetricValue(gap: FileGap, metric: QueryOptions["metric"]): number {
  if (metric === "all") return gap.score;
  if (metric === "lines") return gap.uncoveredLines;
  if (metric === "statements") return gap.uncoveredStatements;
  if (metric === "functions") return gap.uncoveredFunctions;
  if (metric === "branches") return gap.missingBranches;
  return gap.missingMcdcConditions;
}

function obligationMatchesMetric(
  obligation: { kind: "line" | "statement" | "function" | "branch" | "mcdc" },
  metric: QueryOptions["metric"],
): boolean {
  if (metric === "all") return true;
  const kindByMetric: Record<Exclude<QueryOptions["metric"], "all">, typeof obligation.kind> = {
    lines: "line",
    statements: "statement",
    functions: "function",
    branches: "branch",
    mcdc: "mcdc",
  };
  return obligation.kind === kindByMetric[metric];
}

type GapDimension = keyof FileGap["coveredByOtherTests"];

export function fileGaps(report: McdcReport, selected?: Set<string>): FileGap[] {
  const files = new Map<string, FileGap>();
  const get = (file: string): FileGap => {
    const existing = files.get(file);
    if (existing) return existing;
    const created: FileGap = {
      file,
      uncoveredLines: 0,
      uncoveredStatements: 0,
      uncoveredFunctions: 0,
      missingBranches: 0,
      missingMcdcConditions: 0,
      measurementLimitations: 0,
      limitationKinds: [],
      coveredByOtherTests: {
        lines: 0,
        statements: 0,
        functions: 0,
        branches: 0,
        mcdcConditions: 0,
      },
      uncoveredEverywhere: {
        lines: 0,
        statements: 0,
        functions: 0,
        branches: 0,
        mcdcConditions: 0,
      },
      score: 0,
    };
    files.set(file, created);
    return created;
  };
  const classify = (
    gap: FileGap,
    dimension: GapDimension,
    coveredOverall: boolean,
  ): void => {
    if (selected && coveredOverall) gap.coveredByOtherTests[dimension] += 1;
    else gap.uncoveredEverywhere[dimension] += 1;
  };
  for (const line of report.lines) {
    const gap = get(line.file);
    if (!includesSelectedTest(line.tests, selected)) {
      gap.uncoveredLines += 1;
      classify(gap, "lines", line.covered);
    }
  }
  for (const point of report.points) {
    const gap = get(point.meta.file);
    if (!includesSelectedTest(point.tests, selected)) {
      if (point.meta.kind === "function") gap.uncoveredFunctions += 1;
      else gap.uncoveredStatements += 1;
      classify(
        gap,
        point.meta.kind === "function" ? "functions" : "statements",
        point.covered,
      );
    }
  }
  for (const branch of report.branches) {
    const gap = get(branch.meta.file);
    for (const alternative of branch.alternatives) {
      if (!includesSelectedTest(alternative.tests, selected)) {
        gap.missingBranches += 1;
        classify(gap, "branches", alternative.covered);
      }
    }
  }
  for (const decision of report.decisions) {
    const gap = get(decision.meta.file);
    const filteredConditions = filterDecision(decision, selected).conditions;
    for (const condition of filteredConditions) {
      if (!condition.covered) {
        gap.missingMcdcConditions += 1;
        classify(
          gap,
          "mcdcConditions",
          decision.conditions[condition.index]?.covered ?? false,
        );
      }
    }
  }
  for (const limitation of report.limitations ?? []) {
    const gap = get(limitation.file);
    gap.measurementLimitations += 1;
    if (!gap.limitationKinds.includes(limitation.kind))
      gap.limitationKinds.push(limitation.kind);
  }
  for (const gap of files.values()) {
    gap.limitationKinds.sort();
    gap.score =
      gap.uncoveredLines +
      gap.uncoveredFunctions * 2 +
      gap.missingBranches * 2 +
      gap.missingMcdcConditions * 3 +
      gap.measurementLimitations * 3;
  }
  return [...files.values()].sort(
    (left, right) =>
      right.score - left.score || left.file.localeCompare(right.file),
  );
}

function findFile(report: McdcReport, selector: string): string {
  const files = [
    ...new Set([
      ...report.lines.map((line) => line.file),
      ...(report.limitations ?? []).map((limitation) => limitation.file),
    ]),
  ];
  if (files.includes(selector)) return selector;
  const matches = files.filter((file) => file.includes(selector));
  if (matches.length === 1) return matches[0]!;
  if (matches.length === 0)
    throw new SupercovError("SOURCE_NOT_FOUND", `Source file not found: ${selector}`, {
      details: { selector },
    });
  throw new SupercovError("AMBIGUOUS_SELECTOR", `Ambiguous file selector: ${matches.join(", ")}`, {
    details: { selector, matches },
  });
}

export interface CoverageMeasurementStatus {
  complete: boolean;
  limitations: number;
  evidenceCorruptions: number;
  blocking: number;
  files: number;
  byKind: Record<CoverageLimitation["kind"], number>;
}

export function coverageMeasurement(
  report: Pick<McdcReport, "limitations" | "transport">,
): CoverageMeasurementStatus {
  const limitations = report.limitations ?? [];
  const evidenceCorruptions = report.transport?.corruptRecords ?? 0;
  const byKind: CoverageMeasurementStatus["byKind"] = {
    "dynamic-code": 0,
    "semantic-safety": 0,
    "source-scope": 0,
  };
  for (const limitation of limitations) byKind[limitation.kind] += 1;
  return {
    complete: limitations.length === 0 && evidenceCorruptions === 0,
    limitations: limitations.length,
    evidenceCorruptions,
    // Every current limitation removes source from the measured denominator.
    blocking: limitations.length + evidenceCorruptions,
    files:
      new Set(limitations.map((limitation) => limitation.file)).size +
      (report.transport?.corruptFiles ?? 0),
    byKind,
  };
}

function locationSelector(selector: string): { file: string; line: number } {
  const match = /^(.*):(\d+)(?::\d+)?$/.exec(selector);
  if (!match)
    throw new SupercovError("INVALID_ARGUMENT", "Expected <source-file>:<line>", {
      details: { selector },
    });
  return { file: match[1]!, line: Number(match[2]) };
}

function vectorText(values: Array<boolean | null>, outcome: boolean): string {
  return `${values.map((value) => (value === null ? "-" : value ? "T" : "F")).join("")} -> ${outcome ? "T" : "F"}`;
}

const helpText = `Agent-oriented local coverage queries:
  supercov runs [--limit N] [--json]
  supercov runs <run-id> coverage [--filter all|passed|failed] [--kind e2e] [--runner playwright] [--json]
  supercov runs <run-id> coverage kinds [--json]
  supercov runs <run-id> coverage runners [--json]
  supercov runs <run-id> coverage scope [--limit N] [--offset N] [--json]
  supercov runs <run-id> coverage files [--metric all|lines|statements|functions|branches|mcdc] [--filter all|passed|failed] [--limit N] [--offset N] [--json]
  supercov runs <run-id> coverage gaps [--metric all|lines|statements|functions|branches|mcdc] [--filter all|passed|failed] [--kind e2e] [--limit N] [--offset N] [--json]
  supercov runs <run-id> coverage file <source-file> [--metric all|lines|statements|functions|branches|mcdc] [--group decision] [--sort location|missing] [--kind e2e] [--limit N] [--offset N] [--json]
  supercov runs <run-id> coverage decision <id|source-file:line> [--kind e2e] [--json]
  supercov runs <run-id> coverage covers <source-file:line> [--kind e2e] [--json]
  supercov runs <run-id> coverage test <id|name-fragment> [--kind e2e] [--limit N] [--json]
  supercov runs <run-id> coverage minimize [--target 0..100] [--metric all|lines|statements|functions|branches|mcdc] [--filter all|passed|failed] [--limit N] [--offset N] [--json]
  supercov diff <older-run> <newer-run> [--limit N] [--json]
  supercov merge <run-id> <run-id> [...]
  supercov prune [--keep N] [--dry-run]
  supercov clean [--keep N] [--dry-run]

Use "latest" as <run-id> to query the newest local run.

Reviewed MC/DC waivers (optional ${WAIVERS_FILE} at the project root):
  {"version":1,"waivers":[{"file":"src/x.ts","decision":"<id or source>","line":12,"condition":"<source or C2>","reason":"..."}]}
  Waived conditions stay uncovered in every raw total and are reported separately.

Create a run with:
  supercov -- <test command>`;

function help(options: QueryOptions): void {
  if (options.json) {
    return output(
      {
        usage: "supercov -- <test command>",
        runSelector: "Use latest as <run-id> to query the newest local run.",
        queryCommands: [
          "runs",
          "runs <run-id> coverage",
          "runs <run-id> coverage kinds",
          "runs <run-id> coverage runners",
          "runs <run-id> coverage scope",
          "runs <run-id> coverage files",
          "runs <run-id> coverage gaps",
          "runs <run-id> coverage file <source-file>",
          "runs <run-id> coverage decision <id|source-file:line>",
          "runs <run-id> coverage covers <source-file:line>",
          "runs <run-id> coverage test <id|name-fragment>",
          "runs <run-id> coverage minimize",
          "diff <older-run> <newer-run>",
        ],
      },
      options,
      helpText,
    );
  }
  console.log(helpText);
}

export interface CoverageQueryInvocation {
  command: string;
  args: string[];
}

/** Resolve the instance-first coverage resource syntax. */
export function resolveCoverageQueryInvocation(
  command: string,
  args: string[],
): CoverageQueryInvocation {
  if (command !== "runs") return { command, args };

  const runId = args[0];
  if (!runId || runId.startsWith("-")) {
    return { command, args };
  }
  if (args[1] !== "coverage") {
    // A positional run ID must never silently degrade to the run listing:
    // an agent that omits "coverage" would otherwise get unrelated output.
    throw new SupercovError(
      "UNKNOWN_COMMAND",
      args[1] === undefined || args[1].startsWith("-")
        ? `Missing coverage query after run ${runId}. Expected: supercov runs <run-id> coverage [<query>]. Try supercov help.`
        : `Unknown runs query: ${args[1]}. Expected: supercov runs <run-id> coverage [<query>]. Try supercov help.`,
      { details: { run: runId, command: args[1] ?? null } },
    );
  }

  const childToken = args[2];
  const hasChild = Boolean(childToken && !childToken.startsWith("-"));
  const child = hasChild ? childToken! : "summary";
  const childArgs = args.slice(hasChild ? 3 : 2);
  const coverageCommands = new Set([
    "summary",
    "kinds",
    "runners",
    "scope",
    "files",
    "gaps",
    "file",
    "decision",
    "covers",
    "test",
    "minimize",
  ]);
  if (!coverageCommands.has(child)) {
    throw new SupercovError(
      "UNKNOWN_COMMAND",
      `Unknown coverage query: ${child}. Try supercov help.`,
      { details: { command: child } },
    );
  }

  return {
    command: child,
    args: ["--run", runId, ...childArgs],
  };
}

export async function runQueryCommand(
  command: string,
  args: string[],
  root = process.cwd(),
): Promise<void> {
  const resolved = resolveCoverageQueryInvocation(command, args);
  command = resolved.command;
  const options = parseOptions(command, resolved.args);
  if (command === "help") return help(options);
  const currentIntegrity = currentProjectIntegrity(root);
  const waiverSource = readCoverageWaivers(root);

  if (command === "runs") {
    const availableRuns = discoverRuns(root);
    const runs = page(availableRuns, options).map((run) => {
      const cached = readStoredRunIndex(run);
      const report = cached ? filteredCoverage(cached, options) : undefined;
      return {
        id: run.id,
        generatedAt: report?.generatedAt ?? run.metadata?.startedAt,
        coverageIndexed: Boolean(report),
        lines: report?.summary.lines.percentage ?? null,
        branches: report?.summary.branches.percentage ?? null,
        mcdc: report?.summary.conditionCoveragePct ?? null,
        command: run.metadata?.command,
        durationMs: run.metadata?.durationMs,
        timings: run.metadata?.timings,
        testExitCode: run.metadata?.testExitCode,
        buildReused: run.metadata?.instrumentedBuildCache?.reused,
        rawEvidence: run.metadata?.rawEvidence,
        ...(currentIntegrity
          ? compareRunIntegrity(run.metadata?.integrity, currentIntegrity)
          : { stale: undefined, reasons: [] }),
      };
    });
    const runsBase = `npx supercov runs${options.filter !== "all" ? ` --filter ${options.filter}` : ""}`;
    const runsNext = nextPageCommand(
      runsBase,
      availableRuns.length,
      runs.length,
      options,
    );
    return output(
      { filters: queryFilters(options), runs },
      options,
      runs
        .map(
          (run) =>
            `${run.id}  ${run.coverageIndexed ? `lines ${pct(run.lines!)}  branches ${pct(run.branches!)}  MC/DC ${pct(run.mcdc!)}` : "coverage not indexed"}${run.stale ? `  STALE (${run.reasons.join(", ")})` : ""}`,
        )
        .join("\n") +
        `\n${pageLabel(availableRuns.length, runs.length, options)}` +
        (runsNext ? `\nnext page: ${runsNext}` : ""),
      queryPagination(availableRuns.length, runs.length, options),
    );
  }

  if (command === "diff") {
    const [olderSelector, newerSelector] = options.positional;
    if (!olderSelector || !newerSelector)
      throw new SupercovError("INVALID_ARGUMENT", "Usage: supercov diff <older-run> <newer-run>");
    const olderSelected = selectRun(root, olderSelector, currentIntegrity, options.json);
    const newerSelected = selectRun(root, newerSelector, currentIntegrity, options.json);
    const older = {
      ...olderSelected,
      report: filteredCoverage(olderSelected.report, options),
    };
    const newer = {
      ...newerSelected,
      report: filteredCoverage(newerSelected.report, options),
    };
    const key = (file: string, line: number): string => `${file}:${line}`;
    const oldLines = new Set(
      older.report.lines
        .filter((line) => line.covered)
        .map((line) => key(line.file, line.line)),
    );
    const newLines = new Set(
      newer.report.lines
        .filter((line) => line.covered)
        .map((line) => key(line.file, line.line)),
    );
    const branchKeys = (report: McdcReport): Map<string, string> =>
      new Map(
        report.branches.flatMap((branch) =>
          branch.alternatives
            .filter((alternative) => alternative.covered)
            .map((alternative) => [
              `${branch.meta.id}:${alternative.id}`,
              `${branch.meta.file}:${branch.meta.line} ${alternative.label}`,
            ]),
        ),
      );
    const mcdcKeys = (report: McdcReport): Map<string, string> =>
      new Map(
        report.decisions.flatMap((decision) =>
          decision.conditions
            .filter((condition) => condition.covered)
            .map((condition) => [
              `${decision.meta.id}:c${condition.index}`,
              `${decision.meta.file}:${decision.meta.line} C${condition.index + 1} ${condition.source}`,
            ]),
        ),
      );
    const oldBranches = branchKeys(older.report);
    const newBranches = branchKeys(newer.report);
    const oldMcdc = mcdcKeys(older.report);
    const newMcdc = mcdcKeys(newer.report);
    const gainedLines = [...newLines]
      .filter((line) => !oldLines.has(line))
      .sort();
    const lostLines = [...oldLines]
      .filter((line) => !newLines.has(line))
      .sort();
    const gainedBranches = [...newBranches]
      .filter(([id]) => !oldBranches.has(id))
      .map(([, label]) => label)
      .sort();
    const lostBranches = [...oldBranches]
      .filter(([id]) => !newBranches.has(id))
      .map(([, label]) => label)
      .sort();
    const gainedMcdc = [...newMcdc]
      .filter(([id]) => !oldMcdc.has(id))
      .map(([, label]) => label)
      .sort();
    const lostMcdc = [...oldMcdc]
      .filter(([id]) => !newMcdc.has(id))
      .map(([, label]) => label)
      .sort();
    const result = {
      filters: queryFilters(options),
      older: older.run.id,
      newer: newer.run.id,
      delta: {
        lines: Number(
          (
            newer.report.summary.lines.percentage -
            older.report.summary.lines.percentage
          ).toFixed(2),
        ),
        branches: Number(
          (
            newer.report.summary.branches.percentage -
            older.report.summary.branches.percentage
          ).toFixed(2),
        ),
        mcdc: Number(
          (
            newer.report.summary.conditionCoveragePct -
            older.report.summary.conditionCoveragePct
          ).toFixed(2),
        ),
      },
      gained: {
        lineCount: gainedLines.length,
        branchCount: gainedBranches.length,
        mcdcCount: gainedMcdc.length,
        lines: page(gainedLines, options),
        branches: page(gainedBranches, options),
        mcdc: page(gainedMcdc, options),
      },
      lost: {
        lineCount: lostLines.length,
        branchCount: lostBranches.length,
        mcdcCount: lostMcdc.length,
        lines: page(lostLines, options),
        branches: page(lostBranches, options),
        mcdc: page(lostMcdc, options),
      },
    };
    const diffTotal = Math.max(
      gainedLines.length,
      gainedBranches.length,
      gainedMcdc.length,
      lostLines.length,
      lostBranches.length,
      lostMcdc.length,
    );
    const diffReturned = Math.max(
      result.gained.lines.length,
      result.gained.branches.length,
      result.gained.mcdc.length,
      result.lost.lines.length,
      result.lost.branches.length,
      result.lost.mcdc.length,
    );
    const diffBase = [
      "npx supercov diff",
      shellQuote(older.run.id),
      shellQuote(newer.run.id),
      options.filter !== "all" ? `--filter ${options.filter}` : undefined,
    ]
      .filter(Boolean)
      .join(" ");
    const diffNext = nextPageCommand(
      diffBase,
      diffTotal,
      diffReturned,
      options,
    );
    return output(
      result,
      options,
      `${older.run.id} -> ${newer.run.id}\nlines ${result.delta.lines >= 0 ? "+" : ""}${result.delta.lines}pp, branches ${result.delta.branches >= 0 ? "+" : ""}${result.delta.branches}pp, MC/DC ${result.delta.mcdc >= 0 ? "+" : ""}${result.delta.mcdc}pp\ngained: ${gainedLines.length} lines, ${gainedBranches.length} branches, ${gainedMcdc.length} MC/DC conditions\nlost: ${lostLines.length} lines, ${lostBranches.length} branches, ${lostMcdc.length} MC/DC conditions\n${result.gained.lines.map((line) => `+ line ${line}`).join("\n")}${result.gained.branches.length ? `\n${result.gained.branches.map((item) => `+ branch ${item}`).join("\n")}` : ""}${result.gained.mcdc.length ? `\n${result.gained.mcdc.map((item) => `+ MC/DC ${item}`).join("\n")}` : ""}\n${pageLabel(diffTotal, diffReturned, options)} per category${diffNext ? `\nnext page: ${diffNext}` : ""}`,
      queryPagination(diffTotal, diffReturned, options),
    );
  }

  const selectedRun = selectRun(root, options.run, currentIntegrity, options.json);
  const run = selectedRun.run;
  const report = filteredCoverage(selectedRun.report, options);
  const selectedTestSet = selectedTestIds(report, options);
  const waiverEvaluation: CoverageWaiverEvaluation | undefined = waiverSource
    ? evaluateCoverageWaivers(report.decisions, waiverSource)
    : undefined;
  if (command === "summary") {
    const summary = selectedTestSet
      ? coverageSummaryForTests(report, selectedTestSet)
      : report.summary;
    const gaps = fileGaps(report, selectedTestSet).filter(
      (gap) => gap.score > 0,
    );
    const measurement = coverageMeasurement(report);
    const selectedTests = report.tests.filter(
      (test) => !selectedTestSet || selectedTestSet.has(test.id),
    );
    const testCount = selectedTests.filter(
      (test) => (test.role ?? "test") === "test",
    ).length;
    const setupCount = selectedTests.filter(
      (test) => test.role === "setup",
    ).length;
    const testOutcomes = Object.fromEntries(
      ["passed", "failed", "flaky", "skipped", "timedOut", "interrupted", "unknown"].map(
        (outcome) => [
          outcome,
          selectedTests.filter(
            (test) => test.role === "test" && test.outcome === outcome,
          ).length,
        ],
      ),
    );
    const diagnostics = coverageDiagnostics(report, selectedTestSet);
    const result = {
      run: run.id,
      filters: queryFilters(options),
      generatedAt: report.generatedAt,
      valid: run.metadata?.testExitCode === 0,
      stale: report.integrity?.stale ?? false,
      staleReasons: report.integrity?.staleReasons ?? [],
      structurallyComplete: summary.coverageComplete && measurement.complete,
      complete:
        options.filter === "passed" &&
        run.metadata?.testExitCode === 0 &&
        !report.integrity?.stale &&
        summary.coverageComplete &&
        measurement.complete,
      coverage: summary,
      measurement,
      ...(waiverEvaluation
        ? {
            waivers: {
              file: WAIVERS_FILE,
              entries: waiverEvaluation.waivers.length,
              applied: waiverEvaluation.applied.length,
              contradicted: waiverEvaluation.contradicted.map((match) => ({
                file: match.file,
                line: match.line,
                condition: match.conditionSource,
                reason: match.waiver.reason,
              })),
              unmatched: waiverEvaluation.unmatched,
              mcdcExcludingWaived: {
                covered: report.summary.coveredConditions,
                total:
                  report.summary.conditions -
                  waiverEvaluation.applied.length,
                percentage:
                  report.summary.conditions -
                    waiverEvaluation.applied.length >
                  0
                    ? (report.summary.coveredConditions /
                        (report.summary.conditions -
                          waiverEvaluation.applied.length)) *
                      100
                    : 100,
              },
            },
          }
        : {}),
      coverageByKind: report.coverageByKind,
      coverageByRunner: report.coverageByRunner,
      attribution: attribution(report, selectedTestSet),
      transport: report.transport,
      diagnostics,
      ...(!selectedTestSet
        ? {
            confidence: {
              lines: Object.fromEntries(
                ["unexecuted", "executed", "action", "asserted"].map(
                  (level) => [
                    level,
                    report.lines.filter(
                      (line) => line.confidence?.level === level,
                    ).length,
                  ],
                ),
              ),
              assertionCoveredMcdcConditions: report.decisions.reduce(
                (total, decision) =>
                  total +
                  decision.conditions.filter(
                    (condition) => condition.assertionCovered,
                  ).length,
                0,
              ),
            },
          }
        : {}),
      filesWithGaps: gaps.length,
      filesWithCoverageGaps: gaps.filter(
        (gap) =>
          gap.uncoveredLines > 0 ||
          gap.uncoveredStatements > 0 ||
          gap.uncoveredFunctions > 0 ||
          gap.missingBranches > 0 ||
          gap.missingMcdcConditions > 0,
      ).length,
      filesWithMeasurementLimitations: measurement.files,
      tests: testCount,
      setups: setupCount,
      testOutcomes,
      sourceScope: report.scope
        ? {
            mode: report.scope.mode,
            roots: report.scope.roots,
            included: report.scope.entries.filter((entry) => entry.status === "included").length,
            excluded: report.scope.entries.filter((entry) => entry.status === "excluded").length,
            ambiguous: report.scope.entries.filter((entry) => entry.status === "ambiguous").length,
          }
        : undefined,
    };
    return output(
      result,
      options,
      `run ${run.id}${filterLabel(options) ? ` (${filterLabel(options)})` : ""}${run.metadata?.testExitCode !== 0 ? ` [INVALID: test exit ${run.metadata?.testExitCode ?? "unknown"}]` : ""}${report.integrity?.stale ? ` [STALE: ${(report.integrity.staleReasons ?? []).join(", ")}]` : ""}\nlines ${pct(summary.lines.percentage)} (${summary.lines.covered}/${summary.lines.total})\nbranches ${pct(summary.branches.percentage)} (${summary.branches.covered}/${summary.branches.total})\nMC/DC ${pct(summary.conditionCoveragePct)} (${summary.coveredConditions}/${summary.conditions})\nmeasurement: ${measurement.complete ? "complete" : `incomplete — ${measurement.blocking} blocking limitation(s) in ${measurement.files} file(s)`}${waiverEvaluation ? `\nwaivers: ${waiverEvaluation.applied.length} applied, ${waiverEvaluation.contradicted.length} contradicted, ${waiverEvaluation.unmatched.length} unmatched; MC/DC excluding waived ${pct(report.summary.conditions - waiverEvaluation.applied.length > 0 ? (report.summary.coveredConditions / (report.summary.conditions - waiverEvaluation.applied.length)) * 100 : 100)} (${report.summary.coveredConditions}/${report.summary.conditions - waiverEvaluation.applied.length})${waiverEvaluation.contradicted.map((match) => `\n  contradicted (condition is covered): ${match.file}:${match.line} ${match.conditionSource}`).join("")}${waiverEvaluation.unmatched.map((waiver) => `\n  unmatched (no such condition): ${waiver.file}${waiver.line !== undefined ? `:${waiver.line}` : ""} ${waiver.condition}`).join("")}` : ""}${diagnostics.length ? `\ndiagnostic: ${diagnostics.map((item) => `${item.code}: ${item.message}`).join("; ")}` : ""}${!selectedTestSet ? `\nconfidence: ${report.lines.filter((line) => line.confidence?.level === "asserted").length} asserted lines, ${report.lines.filter((line) => line.confidence?.level === "action").length} action-linked, ${report.lines.filter((line) => line.confidence?.level === "executed").length} execution-only; ${report.decisions.reduce((total, decision) => total + decision.conditions.filter((condition) => condition.assertionCovered).length, 0)} assertion-linked MC/DC conditions` : ""}\n${testCount} test(s)${setupCount ? ` + ${setupCount} setup scope(s)` : ""}; outcomes ${Object.entries(testOutcomes).filter(([, count]) => count > 0).map(([outcome, count]) => `${outcome}=${count}`).join(", ") || "none"}; ${gaps.length} file(s) have unresolved coverage or measurement gaps`,
    );
  }

  if (command === "minimize") {
    const solverReport = selectedTestSet
      ? {
          ...report,
          tests: report.tests.filter((test) => selectedTestSet.has(test.id)),
        }
      : report;
    const minimized = minimumTestSet(solverReport, options.target, options.metric);
    const selectedDetails = minimized.selected.map((id) => {
      const test = report.tests.find((candidate) => candidate.id === id)!;
      return {
        id,
        name: test.name,
        file: test.file,
        runner: test.provenance.runner,
        kind: test.provenance.kind,
      };
    });
    const selectedPage = page(selectedDetails, options);
    const base = `${coverageCommand(run.id, options, "minimize")} --target ${options.target}`;
    const next = nextPageCommand(base, selectedDetails.length, selectedPage.length, options);
    return output(
      {
        run: run.id,
        filters: queryFilters(options),
        ...minimized,
        selectedCount: selectedDetails.length,
        totalCandidateTests: solverReport.tests.filter((test) => test.role === "test").length,
        tests: selectedPage,
      },
      options,
      `exact minimum ${selectedDetails.length}/${solverReport.tests.filter((test) => test.role === "test").length} test(s) for ${options.target}% ${options.metric === "all" ? "coverage across all measured metrics" : options.metric}; explored ${minimized.exploredStates} state(s)\nlines ${pct(minimized.summary.lines.percentage)}, statements ${pct(minimized.summary.statements.percentage)}, functions ${pct(minimized.summary.functions.percentage)}, branches ${pct(minimized.summary.branches.percentage)}, MC/DC ${pct(minimized.summary.conditionCoveragePct)}\n${selectedPage.map((test) => `${test.id}  ${test.kind}/${test.runner}  ${test.file ?? "unknown"}  ${test.name}`).join("\n")}\n${pageLabel(selectedDetails.length, selectedPage.length, options)}${next ? `\nnext page: ${next}` : ""}`,
      queryPagination(selectedDetails.length, selectedPage.length, options),
    );
  }

  if (command === "scope") {
    if (!report.scope)
      throw new SupercovError("SCOPE_UNAVAILABLE", "This run does not contain a source-scope inventory.");
    const limitationsByFile = new Map<string, CoverageLimitation[]>();
    for (const limitation of report.limitations ?? []) {
      const existing = limitationsByFile.get(limitation.file) ?? [];
      existing.push(limitation);
      limitationsByFile.set(limitation.file, existing);
    }
    const ordered = report.scope.entries.map((entry) => {
      const limitations = limitationsByFile.get(entry.file) ?? [];
      return {
        ...entry,
        measurementLimitations: limitations.length,
        limitationKinds: [...new Set(limitations.map((item) => item.kind))].sort(),
      };
    }).sort((left, right) => {
      const rank = { ambiguous: 0, included: 1, excluded: 2 } as const;
      return rank[left.status] - rank[right.status] || left.file.localeCompare(right.file);
    });
    const selectedEntries = page(ordered, options);
    const base = coverageCommand(run.id, options, "scope");
    const next = nextPageCommand(base, ordered.length, selectedEntries.length, options);
    const counts = {
      included: ordered.filter((entry) => entry.status === "included").length,
      excluded: ordered.filter((entry) => entry.status === "excluded").length,
      ambiguous: ordered.filter((entry) => entry.status === "ambiguous").length,
    };
    return output(
      {
        run: run.id,
        filters: queryFilters(options),
        mode: report.scope.mode,
        roots: report.scope.roots,
        counts,
        measurement: coverageMeasurement(report),
        entries: selectedEntries,
      },
      options,
      `mode ${report.scope.mode}; roots ${report.scope.roots.join(", ") || "none"}; included ${counts.included}, excluded ${counts.excluded}, ambiguous ${counts.ambiguous}; measurement ${coverageMeasurement(report).complete ? "complete" : `${coverageMeasurement(report).blocking} blocking limitation(s)`}\n${selectedEntries.map((entry) => `${entry.status.toUpperCase()}  ${entry.file}  ${entry.reason}${entry.measurementLimitations ? `  [measurement limitations: ${entry.measurementLimitations} ${entry.limitationKinds.join(", ")}]` : ""}${entry.packageRoot ? `  [package ${entry.packageRoot}]` : ""}`).join("\n")}\n${pageLabel(ordered.length, selectedEntries.length, options)}${next ? `\nnext page: ${next}` : ""}`,
      queryPagination(ordered.length, selectedEntries.length, options),
    );
  }

  if (command === "kinds" || command === "runners") {
    const dimension: Array<{
      kind?: string;
      runner?: string;
      tests: number;
      setups: number;
      summary: McdcReport["summary"];
    }> =
      command === "kinds" ? report.coverageByKind : report.coverageByRunner;
    const selectedDimension = page(dimension, options);
    const dimensionNext = nextPageCommand(
      coverageCommand(run.id, options, command),
      dimension.length,
      selectedDimension.length,
      options,
    );
    return output(
      {
        run: run.id,
        filters: queryFilters(options),
        [command]: selectedDimension,
      },
      options,
      selectedDimension
        .map((entry) => {
          const name = entry.kind ?? entry.runner ?? "unknown";
          return `${name}  ${entry.tests} test(s)${entry.setups ? ` + ${entry.setups} setup scope(s)` : ""}  lines ${pct(entry.summary.lines.percentage)}  branches ${pct(entry.summary.branches.percentage)}  MC/DC ${pct(entry.summary.conditionCoveragePct)}`;
        })
        .join("\n") +
        `\n${pageLabel(dimension.length, selectedDimension.length, options)}` +
        (dimensionNext ? `\nnext page: ${dimensionNext}` : ""),
      queryPagination(dimension.length, selectedDimension.length, options),
    );
  }

  if (command === "files" || command === "gaps") {
    const files = fileGaps(report, selectedTestSet);
    const all = files
      .filter((gap) =>
        command === "files" ||
        gapMetricValue(gap, options.metric) > 0 ||
        gap.measurementLimitations > 0,
      )
      .sort((left, right) =>
        gapMetricValue(right, options.metric) - gapMetricValue(left, options.metric) ||
        right.measurementLimitations - left.measurementLimitations ||
        left.file.localeCompare(right.file),
      );
    const selectedFiles = page(all, options).map((gap) => ({
      ...gap,
      ...(waiverEvaluation
        ? {
            waivedMcdcConditions:
              waiverEvaluation.appliedByFile.get(gap.file) ?? 0,
          }
        : {}),
    }));
    const pageStart = all.length === 0 ? 0 : options.offset + 1;
    const pageEnd = Math.min(
      options.offset + selectedFiles.length,
      all.length,
    );
    const nextOffset = options.offset + selectedFiles.length;
    const nextCommand =
      nextOffset < all.length
        ? `${coverageCommand(run.id, options, command)} --offset ${nextOffset}${options.limit !== 20 ? ` --limit ${options.limit}` : ""}`
        : undefined;
    return output(
      {
        run: run.id,
        filters: queryFilters(options),
        metric: options.metric,
        [command]: selectedFiles,
      },
      options,
      selectedFiles
        .map(
          (gap) => {
            const missing =
              gap.uncoveredLines +
              gap.uncoveredStatements +
              gap.uncoveredFunctions +
              gap.missingBranches +
              gap.missingMcdcConditions;
            const status = missing === 0
              ? "coverage complete"
              : `missing: lines ${gap.uncoveredLines}  stmts ${gap.uncoveredStatements}  funcs ${gap.uncoveredFunctions}  branches ${gap.missingBranches}  MC/DC ${gap.missingMcdcConditions}${(gap as { waivedMcdcConditions?: number }).waivedMcdcConditions ? ` (${(gap as { waivedMcdcConditions?: number }).waivedMcdcConditions} waived)` : ""}`;
            const limitations = gap.measurementLimitations
              ? `  measurement limitations ${gap.measurementLimitations} (${gap.limitationKinds.join(", ")})`
              : "";
            return `${gap.file}  ${status}${limitations}${selectedTestSet ? `  [covered elsewhere: ${Object.values(gap.coveredByOtherTests).reduce((sum, value) => sum + value, 0)}; nowhere: ${Object.values(gap.uncoveredEverywhere).reduce((sum, value) => sum + value, 0)}]` : ""}`;
          },
        )
        .join("\n") +
        `\nshowing ${pageStart}-${pageEnd} of ${all.length}` +
        (nextCommand ? `\nnext page: ${nextCommand}` : ""),
      queryPagination(all.length, selectedFiles.length, options),
    );
  }

  if (command === "file") {
    const selector = options.positional.join(" ");
    if (!selector)
      throw new SupercovError(
        "INVALID_ARGUMENT",
        "Usage: supercov runs <run-id> coverage file <source-file>",
      );
    const file = findFile(report, selector);
    if (options.group === "decision") {
      const decisionRows = report.decisions
        .filter((decision) => decision.meta.file === file)
        .map((decision) => {
          const filtered = filterDecision(decision, selectedTestSet);
          const waived = waiverEvaluation?.waivedByDecision.get(
            decision.meta.id,
          );
          const missing = filtered.conditions.filter(
            (condition) => !condition.covered,
          );
          const waivedMissing = missing.filter((condition) =>
            waived?.has(condition.index),
          );
          return {
            id: decision.meta.id,
            line: decision.meta.line,
            column: decision.meta.column,
            kind: decision.meta.kind,
            conditions: filtered.conditions.length,
            missingConditions: missing.length,
            waivedConditions: waivedMissing.length,
            source: decision.meta.source.replace(/\s+/g, " ").trim(),
          };
        });
      const withMissing = decisionRows.filter(
        (row) => row.missingConditions > 0,
      );
      const ordered = [...withMissing].sort((left, right) =>
        options.sort === "missing"
          ? right.missingConditions -
              right.waivedConditions -
              (left.missingConditions - left.waivedConditions) ||
            right.missingConditions - left.missingConditions ||
            left.line - right.line ||
            left.column - right.column
          : left.line - right.line ||
            left.column - right.column ||
            left.id.localeCompare(right.id),
      );
      const rows = page(ordered, options);
      const totals = {
        decisions: decisionRows.length,
        decisionsWithMissingConditions: withMissing.length,
        conditions: decisionRows.reduce((sum, row) => sum + row.conditions, 0),
        missingConditions: decisionRows.reduce(
          (sum, row) => sum + row.missingConditions,
          0,
        ),
        waivedConditions: decisionRows.reduce(
          (sum, row) => sum + row.waivedConditions,
          0,
        ),
      };
      const groupedBase = `${coverageCommand(run.id, options, "file")} ${shellQuote(file)} --group decision${options.sort !== "location" ? ` --sort ${options.sort}` : ""}`;
      const groupedNext = nextPageCommand(
        groupedBase,
        ordered.length,
        rows.length,
        options,
      );
      const snippet = (source: string): string =>
        source.length > 96 ? `${source.slice(0, 95)}…` : source;
      return output(
        {
          run: run.id,
          filters: queryFilters(options),
          file,
          group: "decision" as const,
          sort: options.sort,
          totals,
          decisions: rows,
        },
        options,
        `${file}  MC/DC by decision\ndecisions ${totals.decisions}, with missing conditions ${totals.decisionsWithMissingConditions}; conditions missing ${totals.missingConditions}/${totals.conditions}${totals.waivedConditions ? `, waived ${totals.waivedConditions}` : ""}\n${rows
          .map(
            (row) =>
              `${row.line}:${row.column}  [${row.id}]  missing ${row.missingConditions}/${row.conditions}${row.waivedConditions ? ` (${row.waivedConditions} waived)` : ""}  ${snippet(row.source)}`,
          )
          .join(
            "\n",
          )}\n${pageLabel(ordered.length, rows.length, options)} decisions with missing conditions${groupedNext ? `\nnext page: ${groupedNext}` : ""}`,
        queryPagination(ordered.length, rows.length, options),
      );
    }
    const uncoveredLines = report.lines
      .filter(
        (line) =>
          line.file === file &&
          !includesSelectedTest(line.tests, selectedTestSet),
      )
      .map((line) => ({
        kind: "line" as const,
        line: line.line,
        otherCoverage: otherCoverage(report, line.tests, selectedTestSet),
      }));
    const functions = report.points
      .filter(
        (point) =>
          point.meta.file === file &&
          point.meta.kind === "function" &&
          !includesSelectedTest(point.tests, selectedTestSet),
      )
      .map((point) => ({
        kind: "function" as const,
        line: point.meta.line,
        column: point.meta.column,
        source: point.meta.label ?? point.meta.source,
        otherCoverage: otherCoverage(report, point.tests, selectedTestSet),
      }));
    const statements = report.points
      .filter(
        (point) =>
          point.meta.file === file &&
          point.meta.kind === "statement" &&
          !includesSelectedTest(point.tests, selectedTestSet),
      )
      .map((point) => ({
        kind: "statement" as const,
        line: point.meta.line,
        column: point.meta.column,
        source: point.meta.label ?? point.meta.source,
        otherCoverage: otherCoverage(report, point.tests, selectedTestSet),
      }));
    const branches = report.branches
      .filter((branch) => branch.meta.file === file)
      .flatMap((branch) =>
        branch.alternatives
          .filter(
            (alternative) =>
              !includesSelectedTest(alternative.tests, selectedTestSet),
          )
          .map((alternative) => ({
            kind: "branch" as const,
            line: branch.meta.line,
            column: branch.meta.column,
            source: branch.meta.source,
            missing: alternative.label,
            otherCoverage: otherCoverage(
              report,
              alternative.tests,
              selectedTestSet,
            ),
          })),
      );
    const mcdc = report.decisions
      .filter((decision) => decision.meta.file === file)
      .flatMap((decision) =>
        filterDecision(decision, selectedTestSet)
          .conditions.filter((condition) => !condition.covered)
          .map((condition) => ({
            kind: "mcdc" as const,
            id: decision.meta.id,
            line: decision.meta.line,
            column: decision.meta.column,
            decision: decision.meta.source,
            missingCondition: condition.source,
            ...(waiverEvaluation?.waivedByDecision
              .get(decision.meta.id)
              ?.has(condition.index)
              ? {
                  waived: true,
                  waiverReason: waiverEvaluation.waivedByDecision
                    .get(decision.meta.id)!
                    .get(condition.index)!.reason,
                }
              : {}),
            observedVectors: filterDecision(
              decision,
              selectedTestSet,
            ).vectorObservations.map((observation) =>
              vectorText(observation.vector.values, observation.vector.outcome),
            ),
            otherCoverage: otherCoverage(
              report,
              (decision.conditions[condition.index]?.witnessTests ?? []).flat(),
              selectedTestSet,
            ),
          })),
      );
    const obligations = [
      ...uncoveredLines,
      ...statements,
      ...functions,
      ...branches,
      ...mcdc,
    ].filter((obligation) => obligationMatchesMetric(obligation, options.metric)).sort(
      (left, right) =>
        left.line - right.line || left.kind.localeCompare(right.kind),
    );
    const allFileLimitations = (report.limitations ?? [])
      .filter((limitation) => limitation.file === file)
      .map((limitation) => ({
        ...limitation,
        blocking: true as const,
        effect: "outside-measured-denominator" as const,
      }))
      .sort(
        (left, right) =>
          left.line - right.line ||
          left.column - right.column ||
          left.id.localeCompare(right.id),
      );
    const allFileTests = report.tests
      .filter(
        (test) =>
          (!selectedTestSet || selectedTestSet.has(test.id)) &&
          test.lines.some((line) => line.file === file),
      )
      .map((test) => ({
        id: test.id,
        name: test.name,
        provenance: test.provenance,
      }));
    const tests = page(allFileTests, options);
    const selected = page(obligations, options);
    const limitations = page(allFileLimitations, options);
    const filePageTotal = Math.max(
      obligations.length,
      allFileTests.length,
      allFileLimitations.length,
    );
    const filePageReturned = Math.max(
      selected.length,
      tests.length,
      limitations.length,
    );
    const nextFileOffset = options.offset + filePageReturned;
    const nextFileCommand =
      filePageReturned > 0 && nextFileOffset < filePageTotal
        ? `${coverageCommand(run.id, options, "file")} ${shellQuote(file)} --offset ${nextFileOffset}${options.limit !== 20 ? ` --limit ${options.limit}` : ""}`
        : undefined;
    const result = {
      run: run.id,
      filters: queryFilters(options),
      file,
      metric: options.metric,
      counts: {
        uncoveredLines: uncoveredLines.length,
        uncoveredStatements: statements.length,
        uncoveredFunctions: functions.length,
        missingBranches: branches.length,
        missingMcdcConditions: mcdc.length,
        waivedMcdcConditions: mcdc.filter((item) => item.waived).length,
        measurementLimitations: allFileLimitations.length,
      },
      tests,
      totalTests: allFileTests.length,
      totalObligations: obligations.length,
      obligations: selected,
      totalLimitations: allFileLimitations.length,
      limitations,
    };
    return output(
      result,
      options,
      `${file}\nlines ${uncoveredLines.length}, statements ${statements.length}, functions ${functions.length}, branches ${branches.length}, MC/DC ${mcdc.length}, measurement limitations ${allFileLimitations.length}\ncovered by ${allFileTests.length} test(s)\n${selected
        .map((item) =>
          item.kind === "line"
            ? `line ${item.line}: ${item.otherCoverage.coveredElsewhere ? `covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}` : "uncovered everywhere"}`
            : item.kind === "statement"
              ? `statement ${item.line}:${item.column}: ${item.source}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`
            : item.kind === "function"
              ? `function ${item.line}:${item.column}: ${item.source}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`
              : item.kind === "branch"
                ? `branch ${item.line}:${item.column}: missing ${item.missing}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`
                : `MC/DC ${item.line}:${item.column} [${item.id}]: ${item.missingCondition}${item.waived ? " [waived]" : ""}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`,
        )
        .join(
          "\n",
        )}${selected.length && limitations.length ? "\n" : ""}${limitations.map((limitation) => `LIMITATION ${limitation.kind} ${limitation.line}:${limitation.column} [${limitation.id}]\n  ${limitation.reason}\n  source: ${limitation.source}\n  effect: outside measured denominator`).join("\n")}\n${pageLabel(filePageTotal, filePageReturned, options)} obligations/tests/limitations per category${nextFileCommand ? `\nnext page: ${nextFileCommand}` : ""}`,
      queryPagination(filePageTotal, filePageReturned, options),
    );
  }

  if (command === "decision") {
    const selector = options.positional[0];
    if (!selector)
      throw new SupercovError(
        "INVALID_ARGUMENT",
        "Usage: supercov runs <run-id> coverage decision <id|source-file:line>",
      );
    let matches = report.decisions.filter(
      (decision) => decision.meta.id === selector,
    );
    if (matches.length === 0 && /:\d+(?::\d+)?$/.test(selector)) {
      const location = locationSelector(selector);
      matches = report.decisions.filter(
        (decision) =>
          decision.meta.file === location.file &&
          decision.meta.line === location.line,
      );
    }
    if (matches.length === 0)
      throw new SupercovError("DECISION_NOT_FOUND", `Decision not found: ${selector}`, {
        details: { selector },
      });
    if (matches.length > 1) {
      const matchingDecisions = page(matches, options).map((decision) => ({
        id: decision.meta.id,
        file: decision.meta.file,
        line: decision.meta.line,
        column: decision.meta.column,
        source: decision.meta.source,
      }));
      const matchesNext = nextPageCommand(
        `${coverageCommand(run.id, options, "decision")} ${shellQuote(selector)}`,
        matches.length,
        matchingDecisions.length,
        options,
      );
      return output(
        {
          run: run.id,
          filters: queryFilters(options),
          decisions: matchingDecisions,
        },
        options,
        `${matchingDecisions.map((decision) => `${decision.id}  ${decision.file}:${decision.line}:${decision.column}  ${decision.source}`).join("\n")}\n${pageLabel(matches.length, matchingDecisions.length, options)} matching decisions${matchesNext ? `\nnext page: ${matchesNext}` : ""}`,
        queryPagination(matches.length, matchingDecisions.length, options),
      );
    }
    matches = matches.map((decision) =>
      filterDecision(decision, selectedTestSet),
    );
    const totalDecisionEvidence = Math.max(
      0,
      ...matches.map((decision) =>
        Math.max(
          decision.vectorObservations.length,
          decision.conditions.length,
          decision.tests.length,
        ),
      ),
    );
    matches = matches.map((decision) => {
      const totals = {
        conditions: decision.conditions.length,
        vectorObservations: decision.vectorObservations.length,
        tests: decision.tests.length,
      };
      const waived = waiverEvaluation?.waivedByDecision.get(decision.meta.id);
      const vectorObservations = page(decision.vectorObservations, options);
      return {
        ...decision,
        totals,
        vectors: vectorObservations.map((observation) => observation.vector),
        vectorObservations,
        conditions: page(decision.conditions, options).map((condition) =>
          !condition.covered && waived?.has(condition.index)
            ? {
                ...condition,
                waived: true,
                waiverReason: waived.get(condition.index)!.reason,
              }
            : condition,
        ),
        tests: page(decision.tests, options),
      };
    });
    const returnedDecisionEvidence = Math.max(
      0,
      ...matches.map((decision) =>
        Math.max(
          decision.vectorObservations.length,
          decision.conditions.length,
          decision.tests.length,
        ),
      ),
    );
    const decisionNext = nextPageCommand(
      `${coverageCommand(run.id, options, "decision")} ${shellQuote(selector)}`,
      totalDecisionEvidence,
      returnedDecisionEvidence,
      options,
    );
    const result = {
      run: run.id,
      filters: queryFilters(options),
      paginationAppliesTo:
        "conditions, vectorObservations, and tests independently within each decision",
      decisions: matches,
    };
    return output(
      result,
      options,
      matches
        .map(
          (decision) =>
            `${decision.meta.id}  ${decision.meta.file}:${decision.meta.line}:${decision.meta.column}\n${decision.meta.source}\n${decision.conditions
              .map(
                (condition) =>
                  `C${condition.index + 1} ${condition.covered ? "covered" : (condition as { waived?: boolean }).waived ? "MISSING (waived)" : "MISSING"}${condition.assertionCovered ? " + asserted" : ""}: ${condition.source}${(condition as { waiverReason?: string }).waiverReason ? `\n   waived: ${(condition as { waiverReason?: string }).waiverReason}` : ""}`,
              )
              .join(
                "\n",
              )}\nconfidence ${decision.confidence?.level ?? "unknown"}; asserted MC/DC ${decision.conditions.filter((condition) => condition.assertionCovered).length}/${decision.conditions.length}\nvectors:\n${decision.vectorObservations.map((observation) => `  ${vectorText(observation.vector.values, observation.vector.outcome)}  tests=${observation.tests.length} confidence=${observation.confidence?.level ?? "unknown"}`).join("\n") || "  none"}`,
        )
        .join("\n\n") +
        `\n${pageLabel(totalDecisionEvidence, returnedDecisionEvidence, options)} conditions/vectors/tests per decision` +
        (decisionNext ? `\nnext page: ${decisionNext}` : ""),
      queryPagination(totalDecisionEvidence, returnedDecisionEvidence, options),
    );
  }

  if (command === "covers") {
    const selector = options.positional[0];
    if (!selector)
      throw new SupercovError(
        "INVALID_ARGUMENT",
        "Usage: supercov runs <run-id> coverage covers <source-file:line>",
      );
    const location = locationSelector(selector);
    const line = report.lines.find(
      (candidate) =>
        candidate.file === location.file && candidate.line === location.line,
    );
    if (!line) {
      // "Uncovered" would be a false claim here: nothing is measured on this
      // exact line. Report what does anchor at it instead of a misleading no.
      const anchored = [
        ...report.decisions
          .filter(
            (decision) =>
              decision.meta.file === location.file &&
              decision.meta.line === location.line,
          )
          .map((decision) => ({
            kind: "decision" as const,
            id: decision.meta.id,
            column: decision.meta.column,
            covered: decision.covered,
            coveredConditions: decision.conditions.filter(
              (condition) => condition.covered,
            ).length,
            conditions: decision.conditions.length,
          })),
        ...report.branches
          .filter(
            (branch) =>
              branch.meta.file === location.file &&
              branch.meta.line === location.line,
          )
          .map((branch) => ({
            kind: "branch" as const,
            id: branch.meta.id,
            column: branch.meta.column,
            covered: branch.alternatives.every(
              (alternative) => alternative.covered,
            ),
          })),
        ...report.points
          .filter(
            (point) =>
              point.meta.file === location.file &&
              point.meta.line === location.line,
          )
          .map((point) => ({
            kind: point.meta.kind,
            id: point.meta.id,
            column: point.meta.column,
            covered: includesSelectedTest(point.tests, selectedTestSet),
          })),
      ].sort((left, right) => left.column - right.column);
      const anchoredPage = page(anchored, options);
      return output(
        {
          run: run.id,
          filters: queryFilters(options),
          location,
          lineObligation: false,
          anchored: anchoredPage,
          totalAnchored: anchored.length,
        },
        options,
        `${location.file}:${location.line} has no line obligation${anchored.length === 0 ? "; nothing is measured at this exact line" : `; ${anchored.length} obligation(s) anchor here`}\n${anchoredPage
          .map(
            (obligation) =>
              `${obligation.kind} ${location.line}:${obligation.column} [${obligation.id}] ${obligation.covered ? "covered" : "not fully covered"}${"conditions" in obligation ? ` (${obligation.coveredConditions}/${obligation.conditions} conditions)` : ""}`,
          )
          .join(
            "\n",
          )}${anchoredPage.length ? "\n" : ""}${pageLabel(anchored.length, anchoredPage.length, options)} anchored obligations`,
        queryPagination(anchored.length, anchoredPage.length, options),
      );
    }
    const allTests = (line?.tests ?? [])
      .filter((id) => !selectedTestSet || selectedTestSet.has(id))
      .map((id) => {
        const test = report.tests.find((candidate) => candidate.id === id);
        return {
          id,
          name: test?.name ?? id,
          provenance: test?.provenance,
        };
      });
    const allPhases = (line?.phases ?? [])
      .map((id) => report.phases.find((candidate) => candidate.id === id))
      .filter(
        (phase) =>
          Boolean(phase) &&
          (!selectedTestSet || selectedTestSet.has(phase!.test)),
      )
      .map((phase) => ({
        id: phase!.id,
        kind: phase!.kind,
        operation: phase!.operation,
        source: phase!.source,
        test: phase!.test,
        status: phase!.status,
        causedByPhaseId: phase!.causedByPhaseId,
      }));
    const tests = page(allTests, options);
    const phases = page(allPhases, options);
    const coversTotal = Math.max(allTests.length, allPhases.length);
    const coversReturned = Math.max(tests.length, phases.length);
    const coversNext = nextPageCommand(
      `${coverageCommand(run.id, options, "covers")} ${shellQuote(selector)}`,
      coversTotal,
      coversReturned,
      options,
    );
    const result = {
      run: run.id,
      filters: queryFilters(options),
      location,
      covered: includesSelectedTest(line?.tests ?? [], selectedTestSet),
      confidence: line?.confidence,
      totalTests: allTests.length,
      totalPhases: allPhases.length,
      tests,
      phases,
    };
    return output(
      result,
      options,
      `${location.file}:${location.line} ${result.covered ? "covered" : "uncovered"}; confidence ${result.confidence?.level ?? "unknown"}${result.confidence?.e2e ? "; E2E-covered" : ""}\n${tests.map((test) => `test: ${test.name} [${test.id}] (${test.provenance?.kind ?? "unknown"}/${test.provenance?.runner ?? "unknown"})`).join("\n") || "no covering tests"}\n${phases.map((phase) => `phase: ${phase.operation}${phase.status ? ` (${phase.status})` : ""}${phase.source ? ` at ${phase.source}` : ""}`).join("\n")}\n${pageLabel(coversTotal, coversReturned, options)} tests/phases${coversNext ? `\nnext page: ${coversNext}` : ""}`,
      queryPagination(coversTotal, coversReturned, options),
    );
  }

  if (command === "test") {
    const selector = options.positional.join(" ").toLowerCase();
    if (!selector)
      throw new SupercovError(
        "INVALID_ARGUMENT",
        "Usage: supercov runs <run-id> coverage test <id|name-fragment>",
      );
    const matches = report.tests.filter(
      (test) =>
        (!selectedTestSet || selectedTestSet.has(test.id)) &&
        (test.id === selector || test.name.toLowerCase().includes(selector)),
    );
    if (matches.length === 0)
      throw new SupercovError("TEST_NOT_FOUND", `Test not found: ${selector}`, {
        details: { selector },
      });
    const testBase = `${coverageCommand(run.id, options, "test")} ${shellQuote(options.positional.join(" "))}`;
    if (matches.length > 1) {
      const matchingTests = page(matches, options).map((test) => ({
        id: test.id,
        name: test.name,
        outcome: test.outcome,
        provenance: test.provenance,
      }));
      const matchesNext = nextPageCommand(
        testBase,
        matches.length,
        matchingTests.length,
        options,
      );
      return output(
        {
          run: run.id,
          filters: queryFilters(options),
          tests: matchingTests,
        },
        options,
        `${matchingTests.map((test) => `${test.name} [${test.id}] — ${test.outcome}`).join("\n")}\n${pageLabel(matches.length, matchingTests.length, options)} matching tests${matchesNext ? `\nnext page: ${matchesNext}` : ""}`,
        queryPagination(matches.length, matchingTests.length, options),
      );
    }
    const test = matches[0]!;
    const pointById = new Map(
      report.points.map((point) => [point.meta.id, point.meta]),
    );
    const branchAlternativeById = new Map(
      report.branches.flatMap((branch) =>
        branch.alternatives.map((alternative) => [
          alternative.id,
          {
            ...branch.meta,
            id: alternative.id,
            alternative: alternative.label,
          },
        ] as const),
      ),
    );
    const allHitDetails = test.hits.map((id) => {
      const point = pointById.get(id);
      if (point)
        return {
          id: point.id,
          obligation: point.kind,
          file: point.file,
          line: point.line,
          column: point.column,
          label: point.label,
        };
      const branch = branchAlternativeById.get(id);
      if (branch)
        return {
          id: branch.id,
          obligation: "branch" as const,
          branchKind: branch.kind,
          file: branch.file,
          line: branch.line,
          column: branch.column,
          alternative: branch.alternative,
        };
      return { id, obligation: "unknown" as const };
    });
    const decisionById = new Map(
      report.decisions.map((decision) => [decision.meta.id, decision.meta]),
    );
    const allPhases = report.phases
      .filter((phase) => phase.test === test.id)
      .map((phase) => ({
          id: phase.id,
          kind: phase.kind,
          operation: phase.operation,
          source: phase.source,
          status: phase.status,
          causedByPhaseId: phase.causedByPhaseId,
          lines: phase.lines.length,
          decisions: phase.decisions.reduce(
            (sum, decision) => sum + decision.vectors.length,
            0,
          ),
        }));
    const testTotal = Math.max(
      test.lines.length,
      test.hits.length,
      test.decisions.length,
      allPhases.length,
    );
    const selected = {
      ...test,
      lines: page(test.lines, options),
      hits: page(test.hits, options),
      hitDetails: page(allHitDetails, options),
      decisions: page(test.decisions, options).map((decision) => ({
        ...decision,
        meta: decisionById.get(decision.id),
      })),
      phases: page(allPhases, options),
      totals: {
        lines: test.lines.length,
        hits: test.hits.length,
        decisions: test.decisions.length,
        phases: allPhases.length,
      },
    };
    const testReturned = Math.max(
      selected.lines.length,
      selected.hits.length,
      selected.decisions.length,
      selected.phases.length,
    );
    const testNext = nextPageCommand(
      testBase,
      testTotal,
      testReturned,
      options,
    );
    return output(
      {
        run: run.id,
        filters: queryFilters(options),
        paginationAppliesTo:
          "lines, hits/hitDetails, decisions, and phases independently within the test",
        tests: [selected],
      },
      options,
      `${selected.name}\noutcome ${selected.outcome}${selected.attempts.length ? `; ${selected.attempts.map((attempt) => `retry ${attempt.retry}=${attempt.status}`).join(", ")}` : ""}\n${selected.totals.lines} lines, ${selected.totals.hits} hits, ${selected.totals.decisions} decisions, ${selected.totals.phases} phases\n${selected.lines.map((line) => `line: ${line.file}:${line.line}`).join("\n")}${selected.lines.length && selected.phases.length ? "\n" : ""}${selected.phases.map((phase) => `${phase.kind}: ${phase.operation}${phase.source ? ` at ${phase.source}` : ""}`).join("\n")}\n${pageLabel(testTotal, testReturned, options)} per evidence category${testNext ? `\nnext page: ${testNext}` : ""}`,
      queryPagination(testTotal, testReturned, options),
    );
  }

  throw new SupercovError(
    "UNKNOWN_COMMAND",
    `Unknown coverage query: ${command}. Try supercov help.`,
    { details: { command } },
  );
}

export const coverageQueryCommands = new Set(["help", "runs", "diff"]);
