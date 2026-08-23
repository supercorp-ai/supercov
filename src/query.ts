import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";
import { coverageSummaryForTests, isIndependencePair } from "./analyze.ts";
import type {
  CoverageRunIntegrity,
  McdcDecisionResult,
  McdcReport,
  McdcVector,
} from "./types.ts";
import { compareRunIntegrity, createRunIntegrity } from "./integrity.ts";
import { discoverCoverageProject } from "./project.ts";

interface StoredRun {
  id: string;
  reportPath: string;
  metadata?: {
    command?: string[];
    durationMs?: number;
    testExitCode?: number | null;
    integrity?: CoverageRunIntegrity;
  };
}

interface QueryOptions {
  run?: string;
  kind?: string;
  runner?: string;
  filter: "all" | "passed" | "failed";
  limit: number;
  offset: number;
  json: boolean;
  positional: string[];
}

function parseOptions(args: string[]): QueryOptions {
  const options: QueryOptions = {
    limit: 20,
    offset: 0,
    json: false,
    filter: "all",
    positional: [],
  };
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index]!;
    if (value === "--json") options.json = true;
    else if (value === "--run") options.run = args[++index];
    else if (value === "--kind") options.kind = args[++index]?.toLowerCase();
    else if (value === "--runner")
      options.runner = args[++index]?.toLowerCase();
    else if (value === "--filter") {
      const filter = args[++index]?.toLowerCase();
      if (filter !== "all" && filter !== "passed" && filter !== "failed")
        throw new Error("--filter must be all, passed, or failed");
      options.filter = filter;
    }
    else if (value === "--limit")
      options.limit = Math.max(1, Number(args[++index]) || 20);
    else if (value === "--offset")
      options.offset = Math.max(0, Number(args[++index]) || 0);
    else if (value.startsWith("--"))
      throw new Error(`Unknown option: ${value}`);
    else options.positional.push(value);
  }
  return options;
}

function filteredCoverage(
  report: McdcReport,
  options: QueryOptions,
): McdcReport {
  if (options.filter === "all") return report;
  const filtered = report.filters?.[options.filter];
  if (!filtered) {
    throw new Error(
      "This run does not contain outcome-filtered coverage. Create a new coverage run.",
    );
  }
  return filtered as McdcReport;
}

function readJson<T>(path: string): T | undefined {
  try {
    const contents = readFileSync(path);
    const text = path.endsWith(".gz")
      ? gunzipSync(contents).toString("utf8")
      : contents.toString("utf8");
    return JSON.parse(text) as T;
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
      const reportPath = resolve(canonical, entry.name, "report.json.gz");
      if (!existsSync(reportPath)) continue;
      runs.set(entry.name, {
        id: entry.name,
        reportPath,
        metadata: readJson(resolve(canonical, entry.name, "run.json")),
      });
    }
  }
  return [...runs.values()].sort((left, right) =>
    right.id.localeCompare(left.id),
  );
}

function selectRun(
  root: string,
  selector?: string,
  currentIntegrity?: CoverageRunIntegrity,
): {
  run: StoredRun;
  report: McdcReport;
} {
  const runs = discoverRuns(root);
  if (runs.length === 0)
    throw new Error("No local coverage runs. Run supercov first.");
  const selected =
    !selector || selector === "latest"
      ? runs[0]
      : (runs.find((run) => run.id === selector) ??
        runs.find((run) => run.id.startsWith(selector)));
  if (!selected) throw new Error(`Coverage run not found: ${selector}`);
  const report = readJson<McdcReport>(selected.reportPath);
  if (!report) throw new Error(`Cannot read ${selected.reportPath}`);
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
    if (comparison.stale) {
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

function output(value: unknown, options: QueryOptions, text: string): void {
  console.log(options.json ? JSON.stringify(value, null, 2) : text);
}

function pct(value: number): string {
  return `${value.toFixed(2)}%`;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'\\''`)}'`;
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
    throw new Error(`No tests match ${filter}`);
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

interface FileGap {
  file: string;
  uncoveredLines: number;
  uncoveredStatements: number;
  uncoveredFunctions: number;
  missingBranches: number;
  missingMcdcConditions: number;
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

type GapDimension = keyof FileGap["coveredByOtherTests"];

function fileGaps(report: McdcReport, selected?: Set<string>): FileGap[] {
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
  for (const gap of files.values()) {
    gap.score =
      gap.uncoveredLines +
      gap.uncoveredFunctions * 2 +
      gap.missingBranches * 2 +
      gap.missingMcdcConditions * 3;
  }
  return [...files.values()].sort(
    (left, right) =>
      right.score - left.score || left.file.localeCompare(right.file),
  );
}

function findFile(report: McdcReport, selector: string): string {
  const files = [...new Set(report.lines.map((line) => line.file))];
  if (files.includes(selector)) return selector;
  const matches = files.filter((file) => file.includes(selector));
  if (matches.length === 1) return matches[0]!;
  if (matches.length === 0)
    throw new Error(`Source file not found: ${selector}`);
  throw new Error(`Ambiguous file selector: ${matches.join(", ")}`);
}

function locationSelector(selector: string): { file: string; line: number } {
  const match = /^(.*):(\d+)(?::\d+)?$/.exec(selector);
  if (!match) throw new Error("Expected <source-file>:<line>");
  return { file: match[1]!, line: Number(match[2]) };
}

function vectorText(values: Array<boolean | null>, outcome: boolean): string {
  return `${values.map((value) => (value === null ? "-" : value ? "T" : "F")).join("")} -> ${outcome ? "T" : "F"}`;
}

function help(): void {
  console.log(`Agent-oriented local coverage queries:
  supercov runs [--limit N] [--json]
  supercov runs <run-id> coverage [--filter all|passed|failed] [--kind e2e] [--runner playwright] [--json]
  supercov runs <run-id> coverage kinds [--json]
  supercov runs <run-id> coverage runners [--json]
  supercov runs <run-id> coverage files [--filter all|passed|failed] [--limit N] [--offset N] [--json]
  supercov runs <run-id> coverage gaps [--filter all|passed|failed] [--kind e2e] [--limit N] [--offset N] [--json]
  supercov runs <run-id> coverage file <source-file> [--kind e2e] [--limit N] [--offset N] [--json]
  supercov runs <run-id> coverage decision <id|source-file:line> [--kind e2e] [--json]
  supercov runs <run-id> coverage covers <source-file:line> [--kind e2e] [--json]
  supercov runs <run-id> coverage test <id|name-fragment> [--kind e2e] [--limit N] [--json]
  supercov diff <older-run> <newer-run> [--limit N] [--json]
  supercov clean [--keep N] [--dry-run]

Use "latest" as <run-id> to query the newest local run.

Create a run with:
  supercov -- <test command>`);
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
  if (!runId || runId.startsWith("-") || args[1] !== "coverage") {
    return { command, args };
  }

  const childToken = args[2];
  const hasChild = Boolean(childToken && !childToken.startsWith("-"));
  const child = hasChild ? childToken! : "summary";
  const childArgs = args.slice(hasChild ? 3 : 2);
  const coverageCommands = new Set([
    "summary",
    "kinds",
    "runners",
    "files",
    "gaps",
    "file",
    "decision",
    "covers",
    "test",
  ]);
  if (!coverageCommands.has(child)) {
    throw new Error(
      `Unknown coverage query: ${child}. Try supercov help.`,
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
  const options = parseOptions(resolved.args);
  if (command === "help") return help();
  const currentIntegrity = currentProjectIntegrity(root);

  if (command === "runs") {
    const availableRuns = discoverRuns(root);
    const runs = page(availableRuns, options).map((run) => {
      const storedReport = readJson<McdcReport>(run.reportPath);
      const report = storedReport
        ? filteredCoverage(storedReport, options)
        : undefined;
      return {
        id: run.id,
        generatedAt: report?.generatedAt,
        lines: report?.summary.lines.percentage,
        branches: report?.summary.branches.percentage,
        mcdc: report?.summary.conditionCoveragePct,
        command: run.metadata?.command,
        durationMs: run.metadata?.durationMs,
        testExitCode: run.metadata?.testExitCode,
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
      runs,
      options,
      runs
        .map(
          (run) =>
            `${run.id}  lines ${pct(run.lines ?? 0)}  branches ${pct(run.branches ?? 0)}  MC/DC ${pct(run.mcdc ?? 0)}${run.stale ? `  STALE (${run.reasons.join(", ")})` : ""}`,
        )
        .join("\n") +
        `\n${pageLabel(availableRuns.length, runs.length, options)}` +
        (runsNext ? `\nnext page: ${runsNext}` : ""),
    );
  }

  if (command === "diff") {
    const [olderSelector, newerSelector] = options.positional;
    if (!olderSelector || !newerSelector)
      throw new Error("Usage: supercov diff <older-run> <newer-run>");
    const olderSelected = selectRun(root, olderSelector, currentIntegrity);
    const newerSelected = selectRun(root, newerSelector, currentIntegrity);
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
    );
  }

  const selectedRun = selectRun(root, options.run, currentIntegrity);
  const run = selectedRun.run;
  const report = filteredCoverage(selectedRun.report, options);
  const selectedTestSet = selectedTestIds(report, options);
  if (command === "summary") {
    const summary = selectedTestSet
      ? coverageSummaryForTests(report, selectedTestSet)
      : report.summary;
    const gaps = fileGaps(report, selectedTestSet).filter(
      (gap) => gap.score > 0,
    );
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
    const result = {
      run: run.id,
      filter: options.filter,
      ...(filterLabel(options) ? { filter: filterLabel(options) } : {}),
      generatedAt: report.generatedAt,
      valid: run.metadata?.testExitCode === 0,
      stale: report.integrity?.stale ?? false,
      staleReasons: report.integrity?.staleReasons ?? [],
      structurallyComplete: summary.coverageComplete,
      complete:
        options.filter === "passed" &&
        run.metadata?.testExitCode === 0 &&
        !report.integrity?.stale &&
        summary.coverageComplete,
      coverage: summary,
      coverageByKind: report.coverageByKind,
      coverageByRunner: report.coverageByRunner,
      attribution: attribution(report, selectedTestSet),
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
      tests: testCount,
      setups: setupCount,
      testOutcomes,
    };
    return output(
      result,
      options,
      `run ${run.id}${filterLabel(options) ? ` (${filterLabel(options)})` : ""}${run.metadata?.testExitCode !== 0 ? ` [INVALID: test exit ${run.metadata?.testExitCode ?? "unknown"}]` : ""}${report.integrity?.stale ? ` [STALE: ${(report.integrity.staleReasons ?? []).join(", ")}]` : ""}\nlines ${pct(summary.lines.percentage)} (${summary.lines.covered}/${summary.lines.total})\nbranches ${pct(summary.branches.percentage)} (${summary.branches.covered}/${summary.branches.total})\nMC/DC ${pct(summary.conditionCoveragePct)} (${summary.coveredConditions}/${summary.conditions})${!selectedTestSet ? `\nconfidence: ${report.lines.filter((line) => line.confidence?.level === "asserted").length} asserted lines, ${report.lines.filter((line) => line.confidence?.level === "action").length} action-linked, ${report.lines.filter((line) => line.confidence?.level === "executed").length} execution-only; ${report.decisions.reduce((total, decision) => total + decision.conditions.filter((condition) => condition.assertionCovered).length, 0)} assertion-linked MC/DC conditions` : ""}\n${testCount} test(s)${setupCount ? ` + ${setupCount} setup scope(s)` : ""}; outcomes ${Object.entries(testOutcomes).filter(([, count]) => count > 0).map(([outcome, count]) => `${outcome}=${count}`).join(", ") || "none"}; ${gaps.length} file(s) have remaining obligations${(report.limitations?.length ?? 0) ? `; ${report.limitations!.length} completeness blocker(s)` : ""}`,
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
        total: dimension.length,
        offset: options.offset,
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
    );
  }

  if (command === "files" || command === "gaps") {
    const files = fileGaps(report, selectedTestSet);
    const all =
      command === "gaps" ? files.filter((gap) => gap.score > 0) : files;
    const selectedFiles = page(all, options);
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
        ...(filterLabel(options) ? { filter: filterLabel(options) } : {}),
        total: all.length,
        offset: options.offset,
        [command]: selectedFiles,
      },
      options,
      selectedFiles
        .map(
          (gap) => {
            const status =
              gap.score === 0
                ? "complete"
                : `missing: lines ${gap.uncoveredLines}  stmts ${gap.uncoveredStatements}  funcs ${gap.uncoveredFunctions}  branches ${gap.missingBranches}  MC/DC ${gap.missingMcdcConditions}`;
            return `${gap.file}  ${status}${selectedTestSet ? `  [covered elsewhere: ${Object.values(gap.coveredByOtherTests).reduce((sum, value) => sum + value, 0)}; nowhere: ${Object.values(gap.uncoveredEverywhere).reduce((sum, value) => sum + value, 0)}]` : ""}`;
          },
        )
        .join("\n") +
        `\nshowing ${pageStart}-${pageEnd} of ${all.length}` +
        (nextCommand ? `\nnext page: ${nextCommand}` : ""),
    );
  }

  if (command === "file") {
    const selector = options.positional.join(" ");
    if (!selector)
      throw new Error(
        "Usage: supercov runs <run-id> coverage file <source-file>",
      );
    const file = findFile(report, selector);
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
    ].sort(
      (left, right) =>
        left.line - right.line || left.kind.localeCompare(right.kind),
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
    const filePageTotal = Math.max(obligations.length, allFileTests.length);
    const filePageReturned = Math.max(selected.length, tests.length);
    const nextFileOffset = options.offset + filePageReturned;
    const nextFileCommand =
      filePageReturned > 0 && nextFileOffset < filePageTotal
        ? `${coverageCommand(run.id, options, "file")} ${shellQuote(file)} --offset ${nextFileOffset}${options.limit !== 20 ? ` --limit ${options.limit}` : ""}`
        : undefined;
    const result = {
      run: run.id,
      ...(filterLabel(options) ? { filter: filterLabel(options) } : {}),
      file,
      counts: {
        uncoveredLines: uncoveredLines.length,
        uncoveredStatements: statements.length,
        uncoveredFunctions: functions.length,
        missingBranches: branches.length,
        missingMcdcConditions: mcdc.length,
      },
      tests,
      totalTests: allFileTests.length,
      totalObligations: obligations.length,
      offset: options.offset,
      obligations: selected,
    };
    return output(
      result,
      options,
      `${file}\nlines ${uncoveredLines.length}, statements ${statements.length}, functions ${functions.length}, branches ${branches.length}, MC/DC ${mcdc.length}\ncovered by ${allFileTests.length} test(s)\n${selected
        .map((item) =>
          item.kind === "line"
            ? `line ${item.line}: ${item.otherCoverage.coveredElsewhere ? `covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}` : "uncovered everywhere"}`
            : item.kind === "statement"
              ? `statement ${item.line}:${item.column}: ${item.source}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`
            : item.kind === "function"
              ? `function ${item.line}:${item.column}: ${item.source}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`
              : item.kind === "branch"
                ? `branch ${item.line}:${item.column}: missing ${item.missing}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`
                : `MC/DC ${item.line}:${item.column} [${item.id}]: ${item.missingCondition}${item.otherCoverage.coveredElsewhere ? ` [covered only by ${item.otherCoverage.kinds.join(", ")}/${item.otherCoverage.runners.join(", ")}]` : ""}`,
        )
        .join(
          "\n",
        )}\n${pageLabel(filePageTotal, filePageReturned, options)} obligations/tests${nextFileCommand ? `\nnext page: ${nextFileCommand}` : ""}`,
    );
  }

  if (command === "decision") {
    const selector = options.positional[0];
    if (!selector)
      throw new Error(
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
      throw new Error(`Decision not found: ${selector}`);
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
          total: matches.length,
          offset: options.offset,
          decisions: matchingDecisions,
        },
        options,
        `${matchingDecisions.map((decision) => `${decision.id}  ${decision.file}:${decision.line}:${decision.column}  ${decision.source}`).join("\n")}\n${pageLabel(matches.length, matchingDecisions.length, options)} matching decisions${matchesNext ? `\nnext page: ${matchesNext}` : ""}`,
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
      const vectorObservations = page(decision.vectorObservations, options);
      return {
        ...decision,
        vectors: vectorObservations.map((observation) => observation.vector),
        vectorObservations,
        conditions: page(decision.conditions, options),
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
      ...(filterLabel(options) ? { filter: filterLabel(options) } : {}),
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
                  `C${condition.index + 1} ${condition.covered ? "covered" : "MISSING"}${condition.assertionCovered ? " + asserted" : ""}: ${condition.source}`,
              )
              .join(
                "\n",
              )}\nconfidence ${decision.confidence?.level ?? "unknown"}; asserted MC/DC ${decision.conditions.filter((condition) => condition.assertionCovered).length}/${decision.conditions.length}\nvectors:\n${decision.vectorObservations.map((observation) => `  ${vectorText(observation.vector.values, observation.vector.outcome)}  tests=${observation.tests.length} confidence=${observation.confidence?.level ?? "unknown"}`).join("\n") || "  none"}`,
        )
        .join("\n\n") +
        `\n${pageLabel(totalDecisionEvidence, returnedDecisionEvidence, options)} conditions/vectors/tests per decision` +
        (decisionNext ? `\nnext page: ${decisionNext}` : ""),
    );
  }

  if (command === "covers") {
    const selector = options.positional[0];
    if (!selector)
      throw new Error(
        "Usage: supercov runs <run-id> coverage covers <source-file:line>",
      );
    const location = locationSelector(selector);
    const line = report.lines.find(
      (candidate) =>
        candidate.file === location.file && candidate.line === location.line,
    );
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
      ...(filterLabel(options) ? { filter: filterLabel(options) } : {}),
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
    );
  }

  if (command === "test") {
    const selector = options.positional.join(" ").toLowerCase();
    if (!selector)
      throw new Error(
        "Usage: supercov runs <run-id> coverage test <id|name-fragment>",
      );
    const matches = report.tests.filter(
      (test) =>
        (!selectedTestSet || selectedTestSet.has(test.id)) &&
        (test.id === selector || test.name.toLowerCase().includes(selector)),
    );
    if (matches.length === 0) throw new Error(`Test not found: ${selector}`);
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
          total: matches.length,
          offset: options.offset,
          tests: matchingTests,
        },
        options,
        `${matchingTests.map((test) => `${test.name} [${test.id}] — ${test.outcome}`).join("\n")}\n${pageLabel(matches.length, matchingTests.length, options)} matching tests${matchesNext ? `\nnext page: ${matchesNext}` : ""}`,
      );
    }
    const test = matches[0]!;
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
      decisions: page(test.decisions, options),
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
        ...(filterLabel(options) ? { filter: filterLabel(options) } : {}),
        tests: [selected],
      },
      options,
      `${selected.name}\noutcome ${selected.outcome}${selected.attempts.length ? `; ${selected.attempts.map((attempt) => `retry ${attempt.retry}=${attempt.status}`).join(", ")}` : ""}\n${selected.totals.lines} lines, ${selected.totals.hits} hits, ${selected.totals.decisions} decisions, ${selected.totals.phases} phases\n${selected.lines.map((line) => `line: ${line.file}:${line.line}`).join("\n")}${selected.lines.length && selected.phases.length ? "\n" : ""}${selected.phases.map((phase) => `${phase.kind}: ${phase.operation}${phase.source ? ` at ${phase.source}` : ""}`).join("\n")}\n${pageLabel(testTotal, testReturned, options)} per evidence category${testNext ? `\nnext page: ${testNext}` : ""}`,
    );
  }

  throw new Error(
    `Unknown coverage query: ${command}. Try supercov help.`,
  );
}

export const coverageQueryCommands = new Set(["help", "runs", "diff"]);
