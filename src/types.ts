export interface McdcDecisionMeta {
  id: string;
  file: string;
  line: number;
  column: number;
  source: string;
  conditions: string[];
  kind: "if" | "ternary" | "while" | "do-while" | "for";
}

export interface McdcVector {
  values: Array<boolean | null>;
  outcome: boolean;
}

export interface McdcDecisionSnapshot {
  meta: McdcDecisionMeta;
  vectors: McdcVector[];
}

export type CoveragePointKind = "statement" | "function";

export interface CoveragePointMeta {
  id: string;
  kind: CoveragePointKind;
  file: string;
  line: number;
  column: number;
  source: string;
  label?: string;
}

export type CoverageBranchKind =
  | "logical-value"
  | "logical-assignment"
  | "optional-chain"
  | "default-value"
  | "try-catch"
  | "for-in"
  | "for-of"
  | "switch"
  | "dynamic-code";

export interface CoverageBranchMeta {
  id: string;
  kind: CoverageBranchKind;
  file: string;
  line: number;
  column: number;
  source: string;
  alternatives: Array<{
    id: string;
    label: string;
  }>;
}

export interface CoverageManifest {
  decisions: McdcDecisionMeta[];
  points: CoveragePointMeta[];
  branches: CoverageBranchMeta[];
  limitations?: CoverageLimitation[];
  scope?: CoverageSourceScope;
}

export interface CoverageSourceScopeEntry {
  file: string;
  status: "included" | "excluded" | "ambiguous";
  reason: string;
  packageRoot?: string;
}

export interface CoverageSourceScope {
  version: 1;
  mode: "automatic" | "explicit";
  roots: string[];
  entries: CoverageSourceScopeEntry[];
}

export interface CoverageLimitation {
  id: string;
  kind: "dynamic-code" | "semantic-safety" | "source-scope";
  file: string;
  line: number;
  column: number;
  source: string;
  reason: string;
}

export interface CoverageRuntimeSnapshot {
  decisions: McdcDecisionSnapshot[];
  hits: string[];
  events?: CoverageRuntimeEvent[];
}

export interface CoverageExecutionScope {
  version: 1;
  runId: string;
  workerId: string;
  testId: string;
  testKey: string;
  retry: number;
  attemptId: string;
}

export type CoverageRuntimeEvent =
  | {
      type: "hit";
      id: string;
      timestampMs: number;
      phaseId?: string;
      environment: "browser" | "server";
    }
  | {
      type: "decision";
      id: string;
      vector: McdcVector;
      timestampMs: number;
      phaseId?: string;
      environment: "browser" | "server";
    };

export type CoverageServerRecord =
  | {
      type: "decision";
      meta: McdcDecisionMeta;
      vector: McdcVector;
      timestampMs?: number;
      phaseId?: string;
      scope?: CoverageExecutionScope;
    }
  | {
      type: "hit";
      id: string;
      timestampMs?: number;
      phaseId?: string;
      scope?: CoverageExecutionScope;
    };

export interface CoverageCarrier {
  version: 1;
  scope?: CoverageExecutionScope;
  phaseId?: string;
}

export interface CoveragePhase {
  id: string;
  kind: "action" | "assertion";
  operation: string;
  source?: string;
  causedByPhaseId?: string;
  startedAtMs: number;
  endedAtMs?: number;
  status?: "passed" | "failed";
  error?: string;
}

export interface TestProvenance {
  /** The process responsible for executing the test, such as playwright or vitest. */
  runner: string;
  /** The semantic testing level, such as unit, integration, e2e, or component. */
  kind: string;
  project?: string;
  /** How the kind was established so inferred labels are never presented as explicit. */
  source: "explicit" | "project" | "path" | "runner-default" | "unknown";
}

export interface McdcRawTestResult {
  testId?: string;
  /** Run/worker/test/retry identity for an exact concurrently executed attempt. */
  scope?: CoverageExecutionScope;
  test: string;
  testFile?: string;
  title?: string;
  retry?: number;
  /** Outcome of this exact attempt, normalized across test runners. */
  status?: TestAttemptStatus;
  /** Runner-level expected outcome (notably Playwright's test.fail()). */
  expectedStatus?: TestAttemptStatus;
  /** True when the runner reports that an earlier attempt failed. */
  flaky?: boolean;
  provenance?: TestProvenance;
  role?: "test" | "setup" | "background";
  phases?: CoveragePhase[];
  runtime?: CoverageRuntimeSnapshot[];
  browser: CoverageRuntimeSnapshot[];
  server: CoverageServerRecord[];
}

export type TestAttemptStatus =
  | "passed"
  | "failed"
  | "skipped"
  | "timedOut"
  | "interrupted"
  | "unknown";

export type TestOutcome =
  | "passed"
  | "failed"
  | "flaky"
  | "skipped"
  | "timedOut"
  | "interrupted"
  | "unknown";

export interface TestAttemptResult {
  retry: number;
  status: TestAttemptStatus;
  expectedStatus?: TestAttemptStatus;
}

export interface McdcVectorObservation {
  vector: McdcVector;
  tests: string[];
  phases?: string[];
  explicitPhases?: string[];
  confidence?: CoverageConfidence;
}

export interface CoverageConfidence {
  level: "unexecuted" | "executed" | "action" | "asserted";
  setupOnly: boolean;
  backgroundOnly: boolean;
  asserted: boolean;
  tests: string[];
  assertedTests: string[];
  runners: string[];
  kinds: string[];
  e2e: boolean;
}

export interface McdcConditionResult {
  index: number;
  source: string;
  covered: boolean;
  witness?: [McdcVector, McdcVector];
  witnessTests?: [string[], string[]];
  assertionCovered?: boolean;
}

export interface McdcDecisionResult {
  meta: McdcDecisionMeta;
  executed: boolean;
  covered: boolean;
  vectors: McdcVector[];
  vectorObservations: McdcVectorObservation[];
  conditions: McdcConditionResult[];
  tests: string[];
  confidence?: CoverageConfidence;
}

export interface CoverageCount {
  covered: number;
  total: number;
  percentage: number;
}

export interface CoveragePointResult {
  meta: CoveragePointMeta;
  covered: boolean;
  tests: string[];
  phases?: string[];
  confidence?: CoverageConfidence;
}

export interface CoverageBranchResult {
  meta: CoverageBranchMeta;
  covered: boolean;
  alternatives: Array<{
    id: string;
    label: string;
    covered: boolean;
    tests: string[];
    phases?: string[];
    confidence?: CoverageConfidence;
  }>;
}

export interface CoveragePhaseResult extends CoveragePhase {
  test: string;
  hits: string[];
  decisions: Array<{
    id: string;
    vectors: McdcVector[];
  }>;
  lines: Array<{
    file: string;
    line: number;
  }>;
  browserEvents: number;
  serverEvents: number;
  explicitEvents: number;
  inferredEvents: number;
  explicitBrowserEvents: number;
  inferredBrowserEvents: number;
  explicitServerEvents: number;
  inferredServerEvents: number;
}

export interface TestCoverageResult {
  id: string;
  name: string;
  file?: string;
  title?: string;
  retries: number[];
  attempts: TestAttemptResult[];
  outcome: TestOutcome;
  provenance: TestProvenance;
  role: "test" | "setup" | "background";
  hits: string[];
  decisions: Array<{
    id: string;
    vectors: McdcVector[];
  }>;
  lines: Array<{
    file: string;
    line: number;
  }>;
}

export interface TestFileCoverageResult {
  file: string;
  tests: string[];
  runners: string[];
  kinds: string[];
  lines: Array<{
    file: string;
    line: number;
  }>;
}

export interface CoverageSummary {
  decisions: number;
  executedDecisions: number;
  coveredDecisions: number;
  conditions: number;
  coveredConditions: number;
  conditionCoveragePct: number;
  lines: CoverageCount;
  statements: CoverageCount;
  functions: CoverageCount;
  branches: CoverageCount;
  decisionOutcomes: CoverageCount;
  conditionOutcomes: CoverageCount;
  valueSelections: CoverageCount;
  coverageComplete: boolean;
  completenessBlocked?: boolean;
}

export interface CoverageRunFingerprint {
  algorithm: "sha256";
  source: string;
  tests: string;
  dependencies: string;
  configuration: string;
  instrumenter: string;
  execution: string;
  combined: string;
  sourceFiles: number;
  testFiles: number;
}

export interface CoverageRunIntegrity {
  schemaVersion: number;
  instrumenterVersion: string;
  git?: {
    revision?: string;
    dirty: boolean;
  };
  fingerprint: CoverageRunFingerprint;
  stale?: boolean;
  staleReasons?: string[];
}

export interface McdcCoverageView {
  generatedAt: string;
  variant: "masking-short-circuit";
  model: {
    name: string;
    completenessMeaning: string;
    measured: string[];
    notMeasured: string[];
  };
  integrity?: CoverageRunIntegrity;
  scope?: CoverageSourceScope;
  limitations?: CoverageLimitation[];
  transport?: {
    processes: number;
    childLaunches: number;
    remoteLaunches: number;
    workspaceCapabilities: number;
    scopedServerRecords: number;
    backgroundServerRecords: number;
    corruptRecords: number;
    corruptFiles: number;
  };
  summary: CoverageSummary;
  coverageByKind: Array<{
    kind: string;
    tests: number;
    setups: number;
    summary: CoverageSummary;
  }>;
  coverageByRunner: Array<{
    runner: string;
    tests: number;
    setups: number;
    summary: CoverageSummary;
  }>;
  decisions: McdcDecisionResult[];
  points: CoveragePointResult[];
  branches: CoverageBranchResult[];
  tests: TestCoverageResult[];
  testFiles: TestFileCoverageResult[];
  phases: CoveragePhaseResult[];
  lines: Array<{
    file: string;
    line: number;
    covered: boolean;
    tests: string[];
    runners: string[];
    kinds: string[];
    exclusiveKind?: string;
    phases?: string[];
    confidence?: CoverageConfidence;
  }>;
}

export interface McdcReport extends McdcCoverageView {
  execution?: {
    testExitCode?: number | null;
    valid: boolean;
  };
  /** Materialized evidence filters; top-level coverage contains all attempts. */
  filters?: {
    passed: McdcCoverageView;
    failed: McdcCoverageView;
  };
}
