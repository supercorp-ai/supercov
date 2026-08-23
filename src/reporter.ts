import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
} from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { gzipSync } from "node:zlib";
import type { FullConfig, Reporter } from "@playwright/test/reporter";
import { createMcdcReport } from "./analyze.ts";
import { atomicWriteFileSync } from "./atomic.ts";
import {
  backgroundEvidenceDirectory,
  serverRunEvidenceDirectory,
} from "./transport.ts";
import type {
  CoverageManifest,
  McdcCoverageView,
  McdcRawTestResult,
  McdcReport,
  McdcVector,
  CoverageServerRecord,
  CoverageRunIntegrity,
} from "./types.ts";

function findFiles(root: string, name: string): string[] {
  if (!existsSync(root)) return [];
  const found: string[] = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = resolve(root, entry.name);
    if (entry.isDirectory()) found.push(...findFiles(path, name));
    else if (entry.name === name) found.push(path);
  }
  return found;
}

function readBackgroundEvidence(
  runId: string,
  serverEvidenceRoot?: string,
): McdcRawTestResult | undefined {
  const directory = backgroundEvidenceDirectory(runId, serverEvidenceRoot);
  if (!existsSync(directory)) return undefined;
  const records = readdirSync(directory, { withFileTypes: true }).flatMap(
    (entry) => {
      if (!entry.isFile() || !entry.name.endsWith(".jsonl")) return [];
      return readFileSync(resolve(directory, entry.name), "utf8")
        .split("\n")
        .filter(Boolean)
        .flatMap((line) => {
          try {
            return [JSON.parse(line) as CoverageServerRecord];
          } catch {
            return [];
          }
        });
    },
  );
  if (records.length === 0) return undefined;
  return {
    testId: `background:${runId}`,
    test: "Background / unattributed",
    title: "Background / unattributed",
    status: "unknown",
    provenance: {
      runner: "background",
      kind: "background",
      source: "explicit",
    },
    role: "background",
    browser: [],
    server: records,
  };
}

function formatVector(vector: McdcVector): string {
  return `${vector.values.map((value) => (value === null ? "–" : value ? "T" : "F")).join(" ")} → ${
    vector.outcome ? "T" : "F"
  }`;
}

function escapeHtml(value: unknown): string {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function renderHtml(
  report: McdcCoverageView,
  filter: "all" | "passed" | "failed",
  runValid?: boolean,
): string {
  const testNames = new Map(
    report.tests.map((test) => [test.id, test.name] as const),
  );
  const phaseNames = new Map(
    report.phases.map(
      (phase) =>
        [
          phase.id,
          `${phase.operation}${phase.source ? ` at ${phase.source}` : ""}`,
        ] as const,
    ),
  );
  const formatTests = (tests: string[]): string =>
    tests.map((test) => escapeHtml(testNames.get(test) ?? test)).join(", ");
  const decisionRows = report.decisions
    .map((decision) => {
      const conditionRows = decision.conditions
        .map(
          (condition) => `
            <li class="${condition.covered ? "covered" : "missing"}">
              C${condition.index + 1}: <code>${escapeHtml(condition.source)}</code>
              ${condition.assertionCovered ? '<strong class="covered">assertion-linked</strong>' : '<small>execution-only</small>'}
              — ${
                condition.covered
                  ? `covered by ${condition
                      .witness!.map(
                        (vector, index) =>
                          `${formatVector(vector)} [${formatTests(condition.witnessTests?.[index] ?? [])}]`,
                      )
                      .join(" / ")}`
                  : "missing independence pair"
              }
            </li>`,
        )
        .join("");
      const vectorRows = decision.vectorObservations
        .map(
          (observation) =>
            `<li><code>${escapeHtml(formatVector(observation.vector))}</code> — ${formatTests(observation.tests)} <small>confidence: ${escapeHtml(observation.confidence?.level ?? "unknown")}</small>${observation.phases?.length ? `<br><small>${observation.phases.map((phase) => escapeHtml(phaseNames.get(phase) ?? phase)).join(" · ")}</small>` : ""}</li>`,
        )
        .join("");
      return `
        <section>
          <h2>${escapeHtml(decision.meta.file)}:${decision.meta.line}:${decision.meta.column}</h2>
          <pre>${escapeHtml(decision.meta.source)}</pre>
          <p>${decision.vectors.length} distinct vector(s); ${decision.covered ? "fully covered" : "incomplete"}; confidence: <strong>${escapeHtml(decision.confidence?.level ?? "unknown")}</strong></p>
          <ul>${conditionRows}</ul>
          <details><summary>Vector-to-test attribution</summary><ul>${vectorRows || "<li>None</li>"}</ul></details>
        </section>`;
    })
    .join("");
  const branchRows = report.branches
    .map(
      (branch) => `
      <section>
        <h2>${escapeHtml(branch.meta.file)}:${branch.meta.line}:${branch.meta.column}</h2>
        <p><strong>${escapeHtml(branch.meta.kind)}</strong></p>
        <pre>${escapeHtml(branch.meta.source)}</pre>
        <ul>${branch.alternatives
          .map(
            (alternative) => `
          <li class="${alternative.covered ? "covered" : "missing"}">
            ${escapeHtml(alternative.label)} — ${alternative.covered ? `covered by ${formatTests(alternative.tests)}` : "not observed"} <small>confidence: ${escapeHtml(alternative.confidence?.level ?? "unexecuted")}</small>
          </li>`,
          )
          .join("")}</ul>
      </section>`,
    )
    .join("");
  const testRows = report.tests
    .map(
      (test) => `
      <section>
        <h2>${escapeHtml(test.name)}</h2>
        <p><small>${escapeHtml(test.provenance.kind)}/${escapeHtml(test.provenance.runner)}${test.role !== "test" ? ` — ${escapeHtml(test.role)} scope` : ""} — outcome: <strong>${escapeHtml(test.outcome)}</strong>${test.attempts.length ? ` (${test.attempts.map((attempt) => `retry ${attempt.retry}: ${attempt.status}`).join(", ")})` : ""}</small></p>
        <p>${test.lines.length} source line(s), ${test.hits.length} point/alternative hit(s), ${test.decisions.reduce((total, decision) => total + decision.vectors.length, 0)} decision vector(s)</p>
        <details><summary>Covered source lines</summary><p>${
          test.lines.length > 0
            ? test.lines
                .map(
                  (line) =>
                    `<code>${escapeHtml(line.file)}:${line.line}</code>`,
                )
                .join(" · ")
            : "None"
        }</p></details>
      </section>`,
    )
    .join("");
  const phaseRows = report.phases
    .map(
      (phase) => `
      <section>
        <h2><span class="${phase.kind === "assertion" ? "covered" : ""}">${escapeHtml(phase.kind)}</span> — ${escapeHtml(phase.operation)}</h2>
        <p>${phase.source ? `<code>${escapeHtml(phase.source)}</code><br>` : ""}${escapeHtml(testNames.get(phase.test) ?? phase.test)}</p>
        ${phase.causedByPhaseId ? `<p>Observes the result of: <strong>${escapeHtml(phaseNames.get(phase.causedByPhaseId) ?? phase.causedByPhaseId)}</strong></p>` : ""}
        <p>${phase.lines.length} source line(s), ${phase.decisions.reduce((total, decision) => total + decision.vectors.length, 0)} decision vector(s), ${phase.browserEvents} browser and ${phase.serverEvents} server event(s)<br><small>Browser: ${phase.explicitBrowserEvents} explicit / ${phase.inferredBrowserEvents} fallback. Server: ${phase.explicitServerEvents} explicit / ${phase.inferredServerEvents} fallback.</small></p>
        <details><summary>Attributed source lines</summary><p>${
          phase.lines.length > 0
            ? phase.lines
                .map(
                  (line) =>
                    `<code>${escapeHtml(line.file)}:${line.line}</code>`,
                )
                .join(" · ")
            : "None"
        }</p></details>
      </section>`,
    )
    .join("");
  const uncoveredPoints = report.points
    .filter((point) => !point.covered)
    .map(
      (point) => `
      <li><strong>${escapeHtml(point.meta.kind)}</strong> ${escapeHtml(point.meta.file)}:${point.meta.line}:${point.meta.column}
      — <code>${escapeHtml(point.meta.label ?? point.meta.source.slice(0, 160))}</code></li>`,
    )
    .join("");
  const summary = report.summary;
  const verifiedComplete = runValid === true && summary.coverageComplete;
  const verdict =
    filter === "all"
      ? summary.coverageComplete
        ? "OBSERVED COMPLETE"
        : "OBSERVED INCOMPLETE"
      : filter === "failed"
        ? "DIAGNOSTIC"
      : runValid === false
        ? "INVALID"
        : verifiedComplete
          ? "COMPLETE"
          : "INCOMPLETE";
  const verdictComplete = filter === "all"
    ? summary.coverageComplete
    : filter === "passed" && verifiedComplete;
  return `<!doctype html>
<html><head><meta charset="utf-8"><title>Supercov coverage completeness</title>
<style>
body{font:15px/1.45 system-ui,sans-serif;max-width:1100px;margin:40px auto;padding:0 20px;color:#202124}
header{padding:22px;border-radius:12px;background:#f4f6f8}section{border-top:1px solid #ddd;padding:18px 0}
pre,code{font-family:ui-monospace,SFMono-Regular,monospace}pre{white-space:pre-wrap;background:#f7f7f7;padding:12px}
.covered{color:#176b36}.missing{color:#a12622}li{margin:7px 0}
.metrics{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:10px}.metric{background:white;padding:10px;border-radius:8px}
</style></head><body>
<nav><strong>Filter:</strong> ${filter === "all" ? "all attempts" : '<a href="report.html">all attempts</a>'} · ${filter === "passed" ? "passed" : '<a href="report-passed.html">passed</a>'} · ${filter === "failed" ? "failed" : '<a href="report-failed.html">failed</a>'}</nav>
<header><h1>${filter === "all" ? "Observed" : filter === "passed" ? "Passed-only verified" : "Failed-attempt"} coverage report</h1>
${filter === "all" ? "<p>Diagnostic aggregate: includes execution from every attempt.</p>" : filter === "passed" ? "<p>Verified filter: includes only successful attempts belonging to ultimately passing tests. Failed retry attempts are excluded.</p>" : "<p>Diagnostic filter: includes only failed attempts, including failed retries of flaky tests.</p>"}
<p class="${verdictComplete ? "covered" : "missing"}"><strong>${verdict}</strong> — ${filter === "passed" && runValid === false ? "the test command failed, so this run cannot establish completeness even if its passing tests cover every obligation" : filter === "failed" ? "this evidence explains what failing attempts executed and never establishes completeness" : `assuming test expectations are correct, ${summary.coverageComplete ? "all obligations in the measured model were exercised" : summary.completenessBlocked ? "a discovered construct cannot yet receive a truthful denominator" : "uncovered obligations remain in the measured model"}`}.</p>
<div class="metrics">
${[
  ["Lines", summary.lines],
  ["Statements", summary.statements],
  ["Functions", summary.functions],
  ["Branches", summary.branches],
  ["Decision outcomes", summary.decisionOutcomes],
  ["Condition outcomes", summary.conditionOutcomes],
  ["Value selections", summary.valueSelections],
]
  .map(([label, metric]) => {
    const value = metric as McdcReport["summary"]["lines"];
    return `<div class="metric"><strong>${label}</strong><br>${value.percentage}% (${value.covered}/${value.total})</div>`;
  })
  .join("")}
<div class="metric"><strong>Masking MC/DC</strong><br>${summary.conditionCoveragePct}% (${summary.coveredConditions}/${summary.conditions})</div>
</div></header>
<section><h1>What this verdict means</h1>
<p>${escapeHtml(report.model.completenessMeaning)}</p>
<details><summary>Measured obligations</summary><ul>${report.model.measured.map((item) => `<li>${escapeHtml(item)}</li>`).join("")}</ul></details>
<details><summary>Not measured</summary><ul>${report.model.notMeasured.map((item) => `<li>${escapeHtml(item)}</li>`).join("")}</ul></details>
${report.limitations?.length ? `<details open><summary>Completeness blockers discovered in this source</summary><ul>${report.limitations.map((item) => `<li><code>${escapeHtml(item.file)}:${item.line}:${item.column}</code> — ${escapeHtml(item.reason)}<br><code>${escapeHtml(item.source)}</code></li>`).join("")}</ul></details>` : ""}
</section>
<h1>Per-test attribution</h1>
<p>Each source hit and decision vector is attributed to its runner, semantic test level, and individual test. Runner setup/import work and background work are separate scopes. Confidence distinguishes execution-only evidence, actions, and action/request chains ending in a passed assertion.</p>
${testRows || "<p>No tests were collected.</p>"}
<h1>Action and assertion attribution</h1>
<p>Coverage events are assigned to automatically instrumented Playwright actions and assertions. Browser requests carry an explicit phase ID into Remix loaders/actions, where async context preserves it through awaited server work. Events outside a traced request are visibly counted as timing fallbacks.</p>
${phaseRows || "<p>No instrumented actions or assertions were collected.</p>"}
<h1>Uncovered statements and functions</h1><ul>${uncoveredPoints || '<li class="covered">None</li>'}</ul>
<h1>Control decisions</h1>${decisionRows}
<h1>Value and switch alternatives</h1>${branchRows || "<p>None</p>"}
</body></html>`;
}

function rawTestId(raw: McdcRawTestResult): string {
  return raw.testId ?? raw.test;
}

/**
 * Keep only the successful final attempt of ultimately passing tests. Status
 * records and coverage records are separate for some runners, so eligibility
 * is resolved per (stable test ID, retry) before evidence is filtered.
 */
export function passingCoverageResults(
  rawResults: McdcRawTestResult[],
): McdcRawTestResult[] {
  const attemptsByTest = new Map<
    string,
    Map<number, { statuses: Set<string>; expectsFailure: boolean }>
  >();
  for (const raw of rawResults) {
    const retry = raw.retry ?? 0;
    const attempts = attemptsByTest.get(rawTestId(raw)) ?? new Map();
    const attempt = attempts.get(retry) ?? {
      statuses: new Set<string>(),
      expectsFailure: false,
    };
    if (raw.status) attempt.statuses.add(raw.status);
    attempt.expectsFailure ||= raw.expectedStatus === "failed";
    attempts.set(retry, attempt);
    attemptsByTest.set(rawTestId(raw), attempts);
  }

  const accepted = new Set<string>();
  for (const [testId, attempts] of attemptsByTest) {
    const retry = Math.max(...attempts.keys());
    const terminal = attempts.get(retry)!;
    if (terminal.statuses.has("passed") && !terminal.expectsFailure) {
      accepted.add(`${testId}\0${retry}`);
    }
  }
  return rawResults.filter((raw) =>
    accepted.has(`${rawTestId(raw)}\0${raw.retry ?? 0}`),
  );
}

/** Coverage executed by attempts whose actual runner status is failed. */
export function failedCoverageResults(
  rawResults: McdcRawTestResult[],
): McdcRawTestResult[] {
  const failedAttempts = new Set(
    rawResults
      .filter((raw) => raw.status === "failed")
      .map((raw) => `${rawTestId(raw)}\0${raw.retry ?? 0}`),
  );
  return rawResults.filter((raw) =>
    failedAttempts.has(`${rawTestId(raw)}\0${raw.retry ?? 0}`),
  );
}

export function writeMcdcReport(
  outputDir: string,
  runId: string,
  minimumArtifactMtimeMs = 0,
  configuredManifestPath?: string,
  testExitCode?: number | null,
  integrity?: CoverageRunIntegrity,
  publication?: {
    directory: string;
    displayDirectory?: string;
    serverEvidenceRoot?: string;
  },
): McdcReport {
  const manifestPath = configuredManifestPath
    ? resolve(configuredManifestPath)
    : resolve(
        process.cwd(),
        process.env["SUPERCOV_MANIFEST"] ??
          ".supercov/mcdc-manifest.json",
      );
  if (!existsSync(manifestPath)) {
    throw new Error(`Coverage manifest was not found at ${manifestPath}`);
  }
  const manifest = JSON.parse(
    readFileSync(manifestPath, "utf8"),
  ) as CoverageManifest;
  const rawResults = findFiles(outputDir, "mcdc.json")
    .filter((path) => statSync(path).mtimeMs >= minimumArtifactMtimeMs)
    .map((path) => JSON.parse(readFileSync(path, "utf8")) as McdcRawTestResult);
  const background = readBackgroundEvidence(
    runId,
    publication?.serverEvidenceRoot,
  );
  if (background) rawResults.push(background);
  const incompatibleScope = rawResults.find(
    (raw) => raw.scope && raw.scope.runId !== runId,
  );
  if (incompatibleScope) {
    throw new Error(
      `Coverage evidence for run ${incompatibleScope.scope!.runId} cannot be used in run ${runId}`,
    );
  }
  if (rawResults.length === 0) {
    throw new Error(`No coverage evidence was collected under ${outputDir}`);
  }
  const report = createMcdcReport(manifest, rawResults);
  const passed = createMcdcReport(manifest, passingCoverageResults(rawResults));
  const failed = createMcdcReport(manifest, failedCoverageResults(rawResults));
  report.filters = { passed, failed };
  if (integrity) {
    report.integrity = integrity;
    passed.integrity = integrity;
    failed.integrity = integrity;
  }
  if (testExitCode !== undefined) {
    report.execution = { testExitCode, valid: testExitCode === 0 };
  }
  const storedRunDirectory = publication?.directory
    ? resolve(publication.directory)
    : resolve(process.cwd(), ".supercov/runs", runId);
  mkdirSync(storedRunDirectory, { recursive: true });
  const htmlPath = resolve(storedRunDirectory, "report.html");
  const serializedReport = `${JSON.stringify(report, null, 2)}\n`;
  atomicWriteFileSync(
    resolve(storedRunDirectory, "report.json.gz"),
    gzipSync(serializedReport, { level: 9 }),
  );
  atomicWriteFileSync(
    htmlPath,
    renderHtml(report, "all", report.execution?.valid),
  );
  atomicWriteFileSync(
    resolve(storedRunDirectory, "report-passed.html"),
    renderHtml(passed, "passed", report.execution?.valid),
  );
  atomicWriteFileSync(
    resolve(storedRunDirectory, "report-failed.html"),
    renderHtml(failed, "failed", report.execution?.valid),
  );

  const summary = report.summary;
  console.log(
    `[coverage] lines ${summary.lines.percentage}%, statements ${summary.statements.percentage}%, ` +
      `functions ${summary.functions.percentage}%, branches ${summary.branches.percentage}%, ` +
      `MC/DC ${summary.conditionCoveragePct}%`,
  );
  console.log(
    `[coverage] verdict: ${summary.coverageComplete ? "COMPLETE" : "INCOMPLETE"}`,
  );
  console.log(
    `[coverage] passed only: lines ${passed.summary.lines.percentage}%, branches ${passed.summary.branches.percentage}%, MC/DC ${passed.summary.conditionCoveragePct}%`,
  );
  console.log(
    `[coverage] report: ${resolve(publication?.displayDirectory ?? storedRunDirectory, "report.html")}`,
  );
  rmSync(
    serverRunEvidenceDirectory(runId, publication?.serverEvidenceRoot),
    { recursive: true, force: true },
  );
  return report;
}

export default class McdcReporter implements Reporter {
  private outputDir = "";

  onBegin(config: FullConfig): void {
    this.outputDir = config.projects[0]?.outputDir ?? "";
  }

  onEnd(): void {
    const runId =
      basename(dirname(this.outputDir));
    writeMcdcReport(this.outputDir, runId);
  }
}
