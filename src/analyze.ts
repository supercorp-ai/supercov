import type {
  CoverageCount,
  CoverageConfidence,
  CoverageSummary,
  CoverageManifest,
  CoveragePhase,
  CoverageRuntimeEvent,
  McdcDecisionResult,
  McdcDecisionSnapshot,
  McdcRawTestResult,
  McdcReport,
  McdcVector,
  TestAttemptResult,
  TestOutcome,
  TestProvenance,
} from "./types.ts";

interface MutableTestCoverage {
  id: string;
  name: string;
  file?: string;
  title?: string;
  retries: Set<number>;
  attempts: Map<number, TestAttemptResult>;
  runnerReportedFlaky: boolean;
  provenance: TestProvenance;
  role: "test" | "setup" | "background";
  hits: Set<string>;
  decisions: Map<string, Map<string, McdcVector>>;
}

interface MutablePhaseCoverage {
  phase: CoveragePhase;
  test: string;
  hits: Set<string>;
  decisions: Map<string, Map<string, McdcVector>>;
  browserEvents: number;
  serverEvents: number;
  explicitEvents: number;
  inferredEvents: number;
  explicitBrowserEvents: number;
  inferredBrowserEvents: number;
  explicitServerEvents: number;
  inferredServerEvents: number;
}

function vectorKey(vector: McdcVector): string {
  return (
    vector.values
      .map((value) => (value === null ? "-" : value ? "T" : "F"))
      .join("") +
    ":" +
    (vector.outcome ? "T" : "F")
  );
}

function percentage(covered: number, total: number): number {
  return total === 0 ? 100 : Number(((covered / total) * 100).toFixed(2));
}

function count(covered: number, total: number): CoverageCount {
  return { covered, total, percentage: percentage(covered, total) };
}

function testOutcome(test: MutableTestCoverage): TestOutcome {
  const attempts = [...test.attempts.values()].sort(
    (left, right) => left.retry - right.retry,
  );
  const terminal = attempts.at(-1);
  if (!terminal) return "unknown";
  if (
    terminal.status === "passed" &&
    (test.runnerReportedFlaky ||
      attempts.slice(0, -1).some((attempt) => attempt.status !== "passed"))
  ) {
    return "flaky";
  }
  return terminal.status;
}

function recordAttempt(
  test: MutableTestCoverage,
  raw: McdcRawTestResult,
): void {
  if (raw.retry === undefined || !raw.status) return;
  const previous = test.attempts.get(raw.retry);
  const status =
    raw.status === "unknown" && previous ? previous.status : raw.status;
  const expectedStatus = raw.expectedStatus ?? previous?.expectedStatus;
  test.attempts.set(raw.retry, {
    retry: raw.retry,
    status,
    ...(expectedStatus ? { expectedStatus } : {}),
  });
}

/**
 * Masking MC/DC pair test for short-circuit languages.
 *
 * The target condition must change and change the decision. Every other
 * condition must either retain its value or be unevaluated in at least one
 * vector (and therefore masked by the short-circuit path).
 */
export function isIndependencePair(
  first: McdcVector,
  second: McdcVector,
  conditionIndex: number,
): boolean {
  const firstTarget = first.values[conditionIndex];
  const secondTarget = second.values[conditionIndex];
  if (
    firstTarget === null ||
    secondTarget === null ||
    firstTarget === secondTarget ||
    first.outcome === second.outcome
  ) {
    return false;
  }

  for (let index = 0; index < first.values.length; index += 1) {
    if (index === conditionIndex) continue;
    const left = first.values[index];
    const right = second.values[index];
    if (left !== null && right !== null && left !== right) return false;
  }
  return true;
}

function summarizeCoverage(
  decisions: McdcDecisionResult[],
  points: McdcReport["points"],
  branches: McdcReport["branches"],
  lines: McdcReport["lines"],
): CoverageSummary {
  const conditions = decisions.flatMap((decision) => decision.conditions);
  const coveredConditions = conditions.filter(
    (condition) => condition.covered,
  ).length;
  const statements = points.filter((point) => point.meta.kind === "statement");
  const functions = points.filter((point) => point.meta.kind === "function");
  const decisionAlternativeTotal = decisions.length * 2;
  const decisionAlternativeCovered = decisions.reduce(
    (total, decision) =>
      total +
      Number(decision.vectors.some((vector) => vector.outcome === false)) +
      Number(decision.vectors.some((vector) => vector.outcome === true)),
    0,
  );
  const conditionOutcomeTotal = conditions.length * 2;
  const conditionOutcomeCovered = decisions.reduce(
    (total, decision) =>
      total +
      decision.conditions.reduce(
        (conditionTotal, condition) =>
          conditionTotal +
          Number(
            decision.vectors.some(
              (vector) => vector.values[condition.index] === false,
            ),
          ) +
          Number(
            decision.vectors.some(
              (vector) => vector.values[condition.index] === true,
            ),
          ),
        0,
      ),
    0,
  );
  const genericAlternativeTotal = branches.reduce(
    (total, branch) => total + branch.alternatives.length,
    0,
  );
  const genericAlternativeCovered = branches.reduce(
    (total, branch) =>
      total +
      branch.alternatives.filter((alternative) => alternative.covered).length,
    0,
  );
  const valueBranches = branches.filter(
    (branch) => branch.meta.kind === "logical-value",
  );
  const valueAlternativeTotal = valueBranches.reduce(
    (total, branch) => total + branch.alternatives.length,
    0,
  );
  const valueAlternativeCovered = valueBranches.reduce(
    (total, branch) =>
      total +
      branch.alternatives.filter((alternative) => alternative.covered).length,
    0,
  );

  const summary: CoverageSummary = {
    decisions: decisions.length,
    executedDecisions: decisions.filter((decision) => decision.executed).length,
    coveredDecisions: decisions.filter((decision) => decision.covered).length,
    conditions: conditions.length,
    coveredConditions,
    conditionCoveragePct: percentage(coveredConditions, conditions.length),
    lines: count(lines.filter((line) => line.covered).length, lines.length),
    statements: count(
      statements.filter((point) => point.covered).length,
      statements.length,
    ),
    functions: count(
      functions.filter((point) => point.covered).length,
      functions.length,
    ),
    branches: count(
      decisionAlternativeCovered + genericAlternativeCovered,
      decisionAlternativeTotal + genericAlternativeTotal,
    ),
    decisionOutcomes: count(
      decisionAlternativeCovered,
      decisionAlternativeTotal,
    ),
    conditionOutcomes: count(conditionOutcomeCovered, conditionOutcomeTotal),
    valueSelections: count(valueAlternativeCovered, valueAlternativeTotal),
    coverageComplete: false,
  };
  summary.coverageComplete =
    summary.lines.percentage === 100 &&
    summary.statements.percentage === 100 &&
    summary.functions.percentage === 100 &&
    summary.branches.percentage === 100 &&
    summary.conditionOutcomes.percentage === 100 &&
    summary.conditionCoveragePct === 100;
  return summary;
}

function coverageSummaryForTestIds(
  decisions: McdcReport["decisions"],
  points: McdcReport["points"],
  branches: McdcReport["branches"],
  lines: McdcReport["lines"],
  testIds: Set<string>,
): CoverageSummary {
  const includesTest = (tests: string[]): boolean =>
    tests.some((test) => testIds.has(test));
  const filteredDecisions = decisions.map((decision) => {
    const vectorObservations = decision.vectorObservations.filter(
      (observation) => includesTest(observation.tests),
    );
    const vectors = vectorObservations.map((observation) => observation.vector);
    const conditions = decision.meta.conditions.map((source, index) => ({
      index,
      source,
      covered: Boolean(findWitness(vectors, index)),
    }));
    return {
      ...decision,
      executed: vectors.length > 0,
      covered: conditions.every((condition) => condition.covered),
      vectors,
      vectorObservations,
      conditions,
      tests: decision.tests.filter((test) => testIds.has(test)),
    };
  });
  const filteredPoints = points.map((point) => ({
    ...point,
    covered: includesTest(point.tests),
    tests: point.tests.filter((test) => testIds.has(test)),
  }));
  const filteredBranches = branches.map((branch) => {
    const alternatives = branch.alternatives.map((alternative) => ({
      ...alternative,
      covered: includesTest(alternative.tests),
      tests: alternative.tests.filter((test) => testIds.has(test)),
    }));
    return {
      ...branch,
      covered: alternatives.every((alternative) => alternative.covered),
      alternatives,
    };
  });
  const filteredLines = lines.map((line) => ({
    ...line,
    covered: includesTest(line.tests),
    tests: line.tests.filter((test) => testIds.has(test)),
  }));
  return summarizeCoverage(
    filteredDecisions,
    filteredPoints,
    filteredBranches,
    filteredLines,
  );
}

export function coverageSummaryForTests(
  report: McdcReport,
  testIds: Iterable<string>,
): CoverageSummary {
  return coverageSummaryForTestIds(
    report.decisions,
    report.points,
    report.branches,
    report.lines,
    new Set(testIds),
  );
}

function findWitness(
  vectors: McdcVector[],
  conditionIndex: number,
): [McdcVector, McdcVector] | undefined {
  for (let left = 0; left < vectors.length; left += 1) {
    for (let right = left + 1; right < vectors.length; right += 1) {
      const first = vectors[left];
      const second = vectors[right];
      if (
        first &&
        second &&
        isIndependencePair(first, second, conditionIndex)
      ) {
        return [first, second];
      }
    }
  }
  return undefined;
}

function addTest(
  map: Map<string, Set<string>>,
  id: string,
  test: string,
): void {
  const tests = map.get(id) ?? new Set<string>();
  tests.add(test);
  map.set(id, tests);
}

export function createMcdcReport(
  manifest: CoverageManifest,
  rawResults: McdcRawTestResult[],
): McdcReport {
  const decisionMetadata = new Map(
    manifest.decisions.map((entry) => [entry.id, entry]),
  );
  const vectorsByDecision = new Map<
    string,
    Map<
      string,
      {
        vector: McdcVector;
        tests: Set<string>;
        phases: Set<string>;
        explicitPhases: Set<string>;
      }
    >
  >();
  const testsByDecision = new Map<string, Set<string>>();
  const testsByHit = new Map<string, Set<string>>();
  const testsById = new Map<string, MutableTestCoverage>();
  const phasesById = new Map<string, MutablePhaseCoverage>();
  const phasesByHit = new Map<string, Set<string>>();
  const explicitPhasesByHit = new Map<string, Set<string>>();
  const phasesByDecisionVector = new Map<string, Set<string>>();

  const registerTest = (raw: McdcRawTestResult): MutableTestCoverage => {
    const id = raw.testId ?? raw.test;
    const existing = testsById.get(id);
    if (existing) {
      if (raw.retry !== undefined) existing.retries.add(raw.retry);
      recordAttempt(existing, raw);
      existing.runnerReportedFlaky ||= raw.flaky === true;
      return existing;
    }
    const test: MutableTestCoverage = {
      id,
      name: raw.test,
      ...(raw.testFile ? { file: raw.testFile } : {}),
      ...(raw.title ? { title: raw.title } : {}),
      retries: new Set(raw.retry === undefined ? [] : [raw.retry]),
      attempts: new Map(
        raw.retry === undefined || !raw.status
          ? []
          : [
              [
                raw.retry,
                {
                  retry: raw.retry,
                  status: raw.status,
                  ...(raw.expectedStatus
                    ? { expectedStatus: raw.expectedStatus }
                    : {}),
                },
              ],
            ],
      ),
      runnerReportedFlaky: raw.flaky === true,
      provenance: raw.provenance ?? {
        runner: "unknown",
        kind: "unknown",
        source: "unknown",
      },
      role: raw.role ?? "test",
      hits: new Set(),
      decisions: new Map(),
    };
    recordAttempt(test, raw);
    testsById.set(id, test);
    return test;
  };

  const addSnapshot = (
    snapshot: McdcDecisionSnapshot,
    test: MutableTestCoverage,
  ): void => {
    decisionMetadata.set(snapshot.meta.id, snapshot.meta);
    const vectors =
      vectorsByDecision.get(snapshot.meta.id) ??
      new Map<
        string,
        {
          vector: McdcVector;
          tests: Set<string>;
          phases: Set<string>;
          explicitPhases: Set<string>;
        }
      >();
    const testVectors =
      test.decisions.get(snapshot.meta.id) ?? new Map<string, McdcVector>();
    for (const vector of snapshot.vectors) {
      const key = vectorKey(vector);
      const observation = vectors.get(key) ?? {
        vector,
        tests: new Set<string>(),
        phases: new Set<string>(),
        explicitPhases: new Set<string>(),
      };
      observation.tests.add(test.id);
      vectors.set(key, observation);
      testVectors.set(key, vector);
    }
    vectorsByDecision.set(snapshot.meta.id, vectors);
    test.decisions.set(snapshot.meta.id, testVectors);
    if (snapshot.vectors.length > 0)
      addTest(testsByDecision, snapshot.meta.id, test.id);
  };

  const addHit = (id: string, test: MutableTestCoverage): void => {
    addTest(testsByHit, id, test.id);
    test.hits.add(id);
  };

  const addPhaseReference = (
    map: Map<string, Set<string>>,
    id: string,
    phaseId: string,
  ): void => {
    const phases = map.get(id) ?? new Set<string>();
    phases.add(phaseId);
    map.set(id, phases);
  };

  const correlatePhase = (
    phases: CoveragePhase[],
    event: CoverageRuntimeEvent,
  ): string | undefined => {
    if (event.phaseId) return event.phaseId;
    let matched: CoveragePhase | undefined;
    for (const phase of phases) {
      if (phase.startedAtMs > event.timestampMs) break;
      matched = phase;
    }
    return matched?.id;
  };

  const addPhaseEvent = (
    raw: McdcRawTestResult,
    event: CoverageRuntimeEvent,
  ): void => {
    const explicit = Boolean(event.phaseId);
    const phaseId = correlatePhase(raw.phases ?? [], event);
    if (!phaseId) return;
    const phase = phasesById.get(phaseId);
    if (!phase) return;
    if (event.environment === "browser") {
      phase.browserEvents += 1;
      if (explicit) phase.explicitBrowserEvents += 1;
      else phase.inferredBrowserEvents += 1;
    } else {
      phase.serverEvents += 1;
      if (explicit) phase.explicitServerEvents += 1;
      else phase.inferredServerEvents += 1;
    }
    if (explicit) phase.explicitEvents += 1;
    else phase.inferredEvents += 1;
    if (event.type === "hit") {
      phase.hits.add(event.id);
      addPhaseReference(phasesByHit, event.id, phaseId);
      if (explicit)
        addPhaseReference(explicitPhasesByHit, event.id, phaseId);
      return;
    }
    const vectors =
      phase.decisions.get(event.id) ?? new Map<string, McdcVector>();
    vectors.set(vectorKey(event.vector), event.vector);
    phase.decisions.set(event.id, vectors);
    addPhaseReference(
      phasesByDecisionVector,
      `${event.id}:${vectorKey(event.vector)}`,
      phaseId,
    );
    const observation = vectorsByDecision
      .get(event.id)
      ?.get(vectorKey(event.vector));
    observation?.phases.add(phaseId);
    if (explicit) observation?.explicitPhases.add(phaseId);
  };

  for (const raw of rawResults) {
    const test = registerTest(raw);
    const orderedPhases = [...(raw.phases ?? [])].sort(
      (left, right) => left.startedAtMs - right.startedAtMs,
    );
    raw.phases = orderedPhases;
    for (const phase of orderedPhases) {
      phasesById.set(phase.id, {
        phase,
        test: test.id,
        hits: new Set(),
        decisions: new Map(),
        browserEvents: 0,
        serverEvents: 0,
        explicitEvents: 0,
        inferredEvents: 0,
        explicitBrowserEvents: 0,
        inferredBrowserEvents: 0,
        explicitServerEvents: 0,
        inferredServerEvents: 0,
      });
    }
    for (const runtime of raw.runtime ?? []) {
      for (const snapshot of runtime.decisions) addSnapshot(snapshot, test);
      for (const id of runtime.hits) addHit(id, test);
      for (const event of runtime.events ?? []) addPhaseEvent(raw, event);
    }
    for (const browser of raw.browser) {
      for (const snapshot of browser.decisions) addSnapshot(snapshot, test);
      for (const id of browser.hits) addHit(id, test);
      for (const event of browser.events ?? []) addPhaseEvent(raw, event);
    }
    for (const record of raw.server) {
      if (record.type === "decision") {
        addSnapshot({ meta: record.meta, vectors: [record.vector] }, test);
      } else {
        addHit(record.id, test);
      }
      if (record.timestampMs !== undefined) {
        addPhaseEvent(raw, {
          ...record,
          id: record.type === "decision" ? record.meta.id : record.id,
          timestampMs: record.timestampMs,
          environment: "server",
        });
      }
    }
  }

  const assertedPhaseIds = new Set<string>();
  for (const phase of phasesById.values()) {
    if (phase.phase.kind !== "assertion" || phase.phase.status !== "passed")
      continue;
    assertedPhaseIds.add(phase.phase.id);
    if (phase.phase.causedByPhaseId)
      assertedPhaseIds.add(phase.phase.causedByPhaseId);
  }

  const confidenceFor = (
    testIds: Iterable<string>,
    phaseIds: Iterable<string>,
    explicitPhaseIds: Iterable<string> = phaseIds,
  ): CoverageConfidence => {
    const tests = [...new Set(testIds)].sort();
    const phases = [...new Set(phaseIds)];
    const assertedPhases = [...new Set(explicitPhaseIds)].filter((id) =>
      assertedPhaseIds.has(id),
    );
    const assertedTests = [
      ...new Set(
        assertedPhases
          .map((id) => phasesById.get(id)?.test)
          .filter((value): value is string => Boolean(value)),
      ),
    ].sort();
    const provenances = tests
      .map((id) => testsById.get(id)?.provenance)
      .filter((value): value is TestProvenance => Boolean(value));
    const roles = tests
      .map((id) => testsById.get(id)?.role)
      .filter((value): value is MutableTestCoverage["role"] => Boolean(value));
    const hasAction = phases.some(
      (id) => phasesById.get(id)?.phase.kind === "action",
    );
    const level: CoverageConfidence["level"] =
      tests.length === 0
        ? "unexecuted"
        : assertedTests.length > 0
          ? "asserted"
          : hasAction
            ? "action"
            : "executed";
    const kinds = [...new Set(provenances.map((value) => value.kind))].sort();
    return {
      level,
      setupOnly: roles.length > 0 && roles.every((role) => role === "setup"),
      backgroundOnly:
        roles.length > 0 && roles.every((role) => role === "background"),
      asserted: assertedTests.length > 0,
      tests,
      assertedTests,
      runners: [...new Set(provenances.map((value) => value.runner))].sort(),
      kinds,
      e2e: kinds.includes("e2e"),
    };
  };

  const decisions: McdcDecisionResult[] = [...decisionMetadata.values()]
    .sort((left, right) =>
      left.file === right.file
        ? left.line - right.line || left.column - right.column
        : left.file.localeCompare(right.file),
    )
    .map((meta) => {
      const vectorObservations = [
        ...(vectorsByDecision.get(meta.id)?.values() ?? []),
      ].map((observation) => ({
        vector: observation.vector,
        tests: [...observation.tests].sort(),
        ...(observation.phases.size > 0
          ? { phases: [...observation.phases].sort() }
          : {}),
        ...(observation.explicitPhases.size > 0
          ? { explicitPhases: [...observation.explicitPhases].sort() }
          : {}),
        confidence: confidenceFor(
          observation.tests,
          observation.phases,
          observation.explicitPhases,
        ),
      }));
      const vectors = vectorObservations.map(
        (observation) => observation.vector,
      );
      const conditions = meta.conditions.map((source, index) => {
        const witness = findWitness(vectors, index);
        const witnessTests = witness?.map(
          (vector) =>
            vectorObservations.find(
              (observation) =>
                vectorKey(observation.vector) === vectorKey(vector),
            )?.tests ?? [],
        ) as [string[], string[]] | undefined;
        let assertionCovered = false;
        for (let left = 0; left < vectorObservations.length; left += 1) {
          for (let right = left + 1; right < vectorObservations.length; right += 1) {
            const first = vectorObservations[left];
            const second = vectorObservations[right];
            if (
              first &&
              second &&
              first.confidence.asserted &&
              second.confidence.asserted &&
              isIndependencePair(first.vector, second.vector, index)
            ) {
              assertionCovered = true;
              break;
            }
          }
          if (assertionCovered) break;
        }
        return {
          index,
          source,
          covered: Boolean(witness),
          assertionCovered,
          ...(witness ? { witness } : {}),
          ...(witnessTests ? { witnessTests } : {}),
        };
      });
      return {
        meta,
        executed: vectors.length > 0,
        covered: conditions.every((condition) => condition.covered),
        vectors,
        vectorObservations,
        conditions,
        tests: [...(testsByDecision.get(meta.id) ?? [])].sort(),
        confidence: confidenceFor(
          testsByDecision.get(meta.id) ?? [],
          vectorObservations.flatMap((observation) => observation.phases ?? []),
          vectorObservations.flatMap(
            (observation) => observation.explicitPhases ?? [],
          ),
        ),
      };
    });

  const points = manifest.points.map((meta) => {
    const tests = [...(testsByHit.get(meta.id) ?? [])].sort();
    const phases = [...(phasesByHit.get(meta.id) ?? [])].sort();
    const explicitPhases = [
      ...(explicitPhasesByHit.get(meta.id) ?? []),
    ].sort();
    return {
      meta,
      covered: testsByHit.has(meta.id),
      tests,
      phases,
      confidence: confidenceFor(tests, phases, explicitPhases),
    };
  });

  const branches = manifest.branches.map((meta) => {
    const alternatives = meta.alternatives.map((alternative) => {
      const tests = [...(testsByHit.get(alternative.id) ?? [])].sort();
      const phases = [...(phasesByHit.get(alternative.id) ?? [])].sort();
      const explicitPhases = [
        ...(explicitPhasesByHit.get(alternative.id) ?? []),
      ].sort();
      return {
        ...alternative,
        covered: testsByHit.has(alternative.id),
        tests,
        phases,
        confidence: confidenceFor(tests, phases, explicitPhases),
      };
    });
    return {
      meta,
      covered: alternatives.every((alternative) => alternative.covered),
      alternatives,
    };
  });

  const lineMap = new Map<
    string,
    {
      file: string;
      line: number;
      covered: boolean;
      tests: Set<string>;
      phases: Set<string>;
      explicitPhases: Set<string>;
    }
  >();
  for (const point of points) {
    const key = point.meta.file + ":" + point.meta.line;
    const line = lineMap.get(key) ?? {
      file: point.meta.file,
      line: point.meta.line,
      covered: false,
      tests: new Set<string>(),
      phases: new Set<string>(),
      explicitPhases: new Set<string>(),
    };
    line.covered ||= point.covered;
    for (const test of point.tests) line.tests.add(test);
    for (const phase of point.phases) line.phases.add(phase);
    for (const phase of explicitPhasesByHit.get(point.meta.id) ?? [])
      line.explicitPhases.add(phase);
    lineMap.set(key, line);
  }
  const lines = [...lineMap.values()]
    .sort(
      (left, right) =>
        left.file.localeCompare(right.file) || left.line - right.line,
    )
    .map((line) => {
      const testIds = [...line.tests].sort();
      const provenances = testIds
        .map((id) => testsById.get(id)?.provenance)
        .filter((value): value is TestProvenance => Boolean(value));
      const runners = [
        ...new Set(provenances.map((value) => value.runner)),
      ].sort();
      const kinds = [...new Set(provenances.map((value) => value.kind))].sort();
      return {
        ...line,
        tests: testIds,
        runners,
        kinds,
        ...(kinds.length === 1 ? { exclusiveKind: kinds[0] } : {}),
        phases: [...line.phases].sort(),
        confidence: confidenceFor(testIds, line.phases, line.explicitPhases),
      };
    });

  const pointLocations = new Map(
    manifest.points.map((point) => [
      point.id,
      { file: point.file, line: point.line },
    ]),
  );
  const tests = [...testsById.values()]
    .sort((left, right) => left.name.localeCompare(right.name))
    .map((test) => {
      const testLines = new Map<string, { file: string; line: number }>();
      for (const hit of test.hits) {
        const location = pointLocations.get(hit);
        if (location)
          testLines.set(`${location.file}:${location.line}`, location);
      }
      return {
        id: test.id,
        name: test.name,
        ...(test.file ? { file: test.file } : {}),
        ...(test.title ? { title: test.title } : {}),
        retries: [...test.retries].sort((left, right) => left - right),
        attempts: [...test.attempts.values()].sort(
          (left, right) => left.retry - right.retry,
        ),
        outcome: testOutcome(test),
        provenance: test.provenance,
        role: test.role,
        hits: [...test.hits].sort(),
        decisions: [...test.decisions.entries()]
          .sort(([left], [right]) => left.localeCompare(right))
          .map(([id, decisionVectors]) => ({
            id,
            vectors: [...decisionVectors.values()],
          })),
        lines: [...testLines.values()].sort(
          (left, right) =>
            left.file.localeCompare(right.file) || left.line - right.line,
        ),
      };
    });
  const testFilesByName = new Map<
    string,
    {
      tests: Set<string>;
      runners: Set<string>;
      kinds: Set<string>;
      lines: Map<string, { file: string; line: number }>;
    }
  >();
  for (const test of tests) {
    const file = test.file ?? "(unknown test file)";
    const aggregate = testFilesByName.get(file) ?? {
      tests: new Set<string>(),
      runners: new Set<string>(),
      kinds: new Set<string>(),
      lines: new Map<string, { file: string; line: number }>(),
    };
    aggregate.tests.add(test.id);
    aggregate.runners.add(test.provenance.runner);
    aggregate.kinds.add(test.provenance.kind);
    for (const line of test.lines)
      aggregate.lines.set(`${line.file}:${line.line}`, line);
    testFilesByName.set(file, aggregate);
  }
  const testFiles = [...testFilesByName.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([file, aggregate]) => ({
      file,
      tests: [...aggregate.tests].sort(),
      runners: [...aggregate.runners].sort(),
      kinds: [...aggregate.kinds].sort(),
      lines: [...aggregate.lines.values()].sort(
        (left, right) =>
          left.file.localeCompare(right.file) || left.line - right.line,
      ),
    }));

  const phases = [...phasesById.values()]
    .sort(
      (left, right) =>
        left.phase.startedAtMs - right.phase.startedAtMs ||
        left.phase.id.localeCompare(right.phase.id),
    )
    .map((phase) => {
      const phaseLines = new Map<string, { file: string; line: number }>();
      for (const hit of phase.hits) {
        const location = pointLocations.get(hit);
        if (location)
          phaseLines.set(`${location.file}:${location.line}`, location);
      }
      return {
        ...phase.phase,
        test: phase.test,
        hits: [...phase.hits].sort(),
        decisions: [...phase.decisions.entries()]
          .sort(([left], [right]) => left.localeCompare(right))
          .map(([id, vectors]) => ({ id, vectors: [...vectors.values()] })),
        lines: [...phaseLines.values()].sort(
          (left, right) =>
            left.file.localeCompare(right.file) || left.line - right.line,
        ),
        browserEvents: phase.browserEvents,
        serverEvents: phase.serverEvents,
        explicitEvents: phase.explicitEvents,
        inferredEvents: phase.inferredEvents,
        explicitBrowserEvents: phase.explicitBrowserEvents,
        inferredBrowserEvents: phase.inferredBrowserEvents,
        explicitServerEvents: phase.explicitServerEvents,
        inferredServerEvents: phase.inferredServerEvents,
      };
    });

  const summary = summarizeCoverage(decisions, points, branches, lines);
  if ((manifest.limitations?.length ?? 0) > 0) {
    summary.coverageComplete = false;
    summary.completenessBlocked = true;
  }
  const coverageByDimension = (
    field: "kind" | "runner",
  ): Array<{
    value: string;
    tests: number;
    setups: number;
    summary: CoverageSummary;
  }> => {
    const values = [
      ...new Set(tests.map((test) => test.provenance[field])),
    ].sort();
    return values.map((value) => {
      const testIds = new Set(
        tests
          .filter((test) => test.provenance[field] === value)
          .map((test) => test.id),
      );
      return {
        value,
        tests: tests.filter(
          (test) => test.provenance[field] === value && test.role === "test",
        ).length,
        setups: tests.filter(
          (test) => test.provenance[field] === value && test.role === "setup",
        ).length,
        summary: coverageSummaryForTestIds(
          decisions,
          points,
          branches,
          lines,
          testIds,
        ),
      };
    });
  };
  const coverageByKind = coverageByDimension("kind").map(
    ({ value: kind, ...entry }) => ({ kind, ...entry }),
  );
  const coverageByRunner = coverageByDimension("runner").map(
    ({ value: runner, ...entry }) => ({ runner, ...entry }),
  );

  return {
    generatedAt: new Date().toISOString(),
    variant: "masking-short-circuit",
    ...(manifest.scope ? { scope: manifest.scope } : {}),
    model: {
      name: "coverage-completeness-v2",
      completenessMeaning:
        "Every obligation in the measured model was observed by at least one existing test; test assertions and product correctness are separate assumptions.",
      measured: [
        "executable source lines",
        "executable statements",
        "function entries",
        "true and false outcomes of if, ternary, while, do/while, and classic for decisions",
        "true and false outcomes of every atomic condition in those decisions",
        "masking MC/DC independence for every atomic condition in those decisions",
        "short-circuit and right-evaluated selections for &&, ||, and ?? value expressions, including JSX",
        "short-circuit and evaluated alternatives for logical assignments and optional chains",
        "provided and default-evaluated parameter and destructuring values",
        "try success and catch entry",
        "zero and entered for-in/for-of loops",
        "entered switch cases, defaults, and implicit no-match alternatives",
      ],
      notMeasured: [
        "all input values or semantic input partitions",
        "all execution paths or ordering/concurrency interleavings",
        "destructuring defaults in classic for initializers (reported as blockers when discovered)",
        "the internal statements and decisions of runtime-generated eval/Function source",
        "mutation score or assertion fault-detection strength",
      ],
    },
    limitations: manifest.limitations ?? [],
    summary,
    coverageByKind,
    coverageByRunner,
    decisions,
    points,
    branches,
    tests,
    testFiles,
    phases,
    lines,
  };
}
