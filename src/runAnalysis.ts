import { createMcdcReport } from "./analyze.ts";
import { readEvidenceArchive } from "./evidenceArchive.ts";
import type {
  CoverageManifest,
  CoverageRunIntegrity,
  CoverageServerRecord,
  McdcRawTestResult,
  McdcReport,
} from "./types.ts";

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
    if (terminal.statuses.has("passed") && !terminal.expectsFailure)
      accepted.add(`${testId}\0${retry}`);
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

export interface AnalyzeCoverageOptions {
  runId: string;
  testExitCode?: number | null;
  integrity?: CoverageRunIntegrity;
  generatedAt?: string;
}

export function analyzeCoverageResults(
  manifest: CoverageManifest,
  rawResults: McdcRawTestResult[],
  options: AnalyzeCoverageOptions,
): McdcReport {
  const incompatibleScope = rawResults.find(
    (raw) => raw.scope && raw.scope.runId !== options.runId,
  );
  if (incompatibleScope) {
    throw new Error(
      `Coverage evidence for run ${incompatibleScope.scope!.runId} cannot be used in run ${options.runId}`,
    );
  }
  if (rawResults.length === 0)
    throw new Error(`No coverage evidence was collected for run ${options.runId}`);

  const report = createMcdcReport(manifest, rawResults);
  const passed = createMcdcReport(manifest, passingCoverageResults(rawResults));
  const failed = createMcdcReport(manifest, failedCoverageResults(rawResults));
  if (options.generatedAt) {
    report.generatedAt = options.generatedAt;
    passed.generatedAt = options.generatedAt;
    failed.generatedAt = options.generatedAt;
  }
  report.filters = { passed, failed };
  if (options.integrity) {
    report.integrity = options.integrity;
    passed.integrity = options.integrity;
    failed.integrity = options.integrity;
  }
  if (options.testExitCode !== undefined)
    report.execution = {
      testExitCode: options.testExitCode,
      valid: options.testExitCode === 0,
    };
  return report;
}

function parseJson<T>(contents: string, source: string): T {
  try {
    return JSON.parse(contents) as T;
  } catch (error) {
    throw new Error(`Invalid JSON in ${source}: ${String(error)}`);
  }
}

/** Reconstruct every derived coverage view directly from one immutable run. */
export function analyzeCoverageArchive(
  archivePath: string,
  options: AnalyzeCoverageOptions,
): McdcReport {
  const archive = readEvidenceArchive(archivePath);
  const manifestEntry = archive.files.find((entry) => entry.path === "manifest.json");
  if (!manifestEntry)
    throw new Error(`Coverage manifest is missing from ${archivePath}`);
  const manifest = parseJson<CoverageManifest>(manifestEntry.contents, "manifest.json");
  const rawResults = archive.files
    .filter((entry) => /(?:^|\/)mcdc\.json$/.test(entry.path))
    .map((entry) => parseJson<McdcRawTestResult>(entry.contents, entry.path));
  const backgroundRecords = archive.files
    .filter((entry) => /^server\/background\/.*\.jsonl$/.test(entry.path))
    .flatMap((entry) =>
      entry.contents
        .split("\n")
        .filter(Boolean)
        .map((line, index) =>
          parseJson<CoverageServerRecord>(line, `${entry.path}:${index + 1}`),
        ),
    );
  if (backgroundRecords.length > 0) {
    rawResults.push({
      testId: `background:${options.runId}`,
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
      server: backgroundRecords,
    });
  }
  return analyzeCoverageResults(manifest, rawResults, options);
}
