import type {
  CoverageCarrier,
  CoverageExecutionScope,
  CoveragePhase,
  CoverageRuntimeSnapshot,
  CoverageRuntimeEvent,
  CoverageServerRecord,
  McdcDecisionMeta,
  McdcDecisionSnapshot,
  McdcVector,
} from "./types.ts";
import {
  backgroundEvidenceDirectory,
  backgroundEvidencePath,
  COVERAGE_CARRIER_ENV,
  COVERAGE_PHASE_HEADER,
  COVERAGE_PHASE_COOKIE,
  COVERAGE_SCOPE_COOKIE,
  COVERAGE_SCOPE_HEADER,
  decodeCoverageCarrier,
  decodeCoverageScope,
  encodeCoverageCarrier,
  encodeCoverageScope,
  serverEvidenceDirectory,
  serverEvidencePath,
} from "./transport.ts";

interface DecisionFrame {
  meta: McdcDecisionMeta;
  values: Array<boolean | null>;
}

export interface ProbeV2FileState {
  decisions: McdcDecisionMeta[];
  pointIds: string[];
  clock: { epoch: number; fast: boolean };
  hitEpochs: Uint32Array;
  decisionEpochs: Array<Uint32Array | Map<number, number>>;
  decisionVectorCounts: number[];
  decisionObservationEpochs: Uint32Array;
  decisionObservationCounts: Uint16Array;
  decisionCompleteEpochs: Uint32Array;
}

interface SelectionFrame {
  shortId: string;
  rightId: string;
  rightEvaluated: boolean;
}

interface OptionalCallFrame {
  shortId: string;
  continuedId: string;
  reached: boolean;
  continued: boolean;
}

interface TryFrame {
  successId: string;
  catchId: string;
  caught: boolean;
}

interface LoopFrame {
  zeroId: string;
  enteredId: string;
  entered: boolean;
}

interface RuntimeState {
  decisions: Map<
    string,
    { meta: McdcDecisionMeta; vectors: Map<string, McdcVector> }
  >;
  hits: Set<string>;
  events: CoverageRuntimeEvent[];
  eventKeys: Set<string>;
  bufferedAttempts: Set<string>;
  serverBuffers: Map<
    string,
    {
      scope: CoverageExecutionScope;
      directory: string;
      path: string;
      records: Map<string, CoverageServerRecord>;
    }
  >;
  persistedServerRecords: Set<string>;
  backgroundBuffers: Map<string, Map<string, CoverageServerRecord>>;
  backgroundSequence: number;
  runtimeSnapshots: boolean;
  assertionPhases: Map<
    string,
    { counter: number; phases: CoveragePhase[] }
  >;
  probeV2Files: Set<ProbeV2FileState>;
  probeV2Clock: { epoch: number; fast: boolean };
  probeV2ContextEpochs: Map<string, number>;
  probeV2NextEpoch: number;
  probeV2HookInstalled: boolean;
}

type McdcGlobal = typeof globalThis & {
  __SUPERCOV_MCDC_STATES__?: Map<string, RuntimeState>;
  __SUPERCOV_MCDC_TEST_ID__?: string;
  __SUPERCOV_PHASE_ID__?: string;
  __SUPERCOV_ACTIVE_SCOPE__?: CoverageExecutionScope;
  __SUPERCOV_SERVER_PHASE_STORAGES__?: Map<string, RequestStorage>;
  __SUPERCOV_FETCH_PATCHED__?: boolean;
  __SUPERCOV_CHILD_PATCHED__?: boolean;
  __SUPERCOV_BUFFER_EXIT_INSTALLED__?: boolean;
  __SUPERCOV_BUFFER_FLUSHERS__?: Set<() => void>;
  __SUPERCOV_MCDC_SNAPSHOT__?: () => McdcDecisionSnapshot[];
  __SUPERCOV_COVERAGE_SNAPSHOT__?: () => CoverageRuntimeSnapshot;
  __SUPERCOV_RESET__?: (testId?: string) => void;
  __SUPERCOV_ACTIVATE_PROBE_CONTEXT__?: (
    testId: string,
    phaseId?: string,
  ) => void;
};

interface CoverageRequestContext {
  scope?: CoverageExecutionScope;
  phaseId?: string;
}

interface RequestStorage {
  getStore(): CoverageRequestContext | undefined;
  run<T>(store: CoverageRequestContext, callback: () => T): T;
}

interface AsyncHooksBuiltin {
  AsyncLocalStorage: new () => RequestStorage;
  createHook?(callbacks: {
    before(asyncId: number): void;
    after(asyncId: number): void;
  }): { enable(): void };
}

interface FsBuiltin {
  appendFileSync(path: string, data: string): void;
  mkdirSync(path: string, options: { recursive: boolean }): void;
  writeFileSync(path: string, data: string, options: { flag: "wx" }): void;
}

const runtimeGlobal = globalThis as McdcGlobal;
const runtimeInstanceToken = "__SUPERCOV_RUNTIME_INSTANCE__";
const runtimeInstance = runtimeInstanceToken === "__SUPERCOV_" + "RUNTIME_INSTANCE__"
  ? "application"
  : runtimeInstanceToken;
const isBrowser = !(
  typeof process !== "undefined" &&
  typeof process.versions?.node === "string"
);
const testId = runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__ ?? "unscoped";
const storageKey = "__supercov_coverage_" + testId;
const phaseStorageKey = "__supercov_phase";
const pendingDefaults = new Map<string, number>();

function vectorKey(vector: McdcVector): string {
  return (
    vector.values
      .map((value) => (value === null ? "-" : value ? "T" : "F"))
      .join("") +
    ":" +
    (vector.outcome ? "T" : "F")
  );
}

function getFs(): FsBuiltin | undefined {
  if (isBrowser || typeof process === "undefined") return undefined;
  try {
    const getBuiltinModule = (
      process as typeof process & {
        getBuiltinModule?: (name: string) => FsBuiltin;
      }
    ).getBuiltinModule;
    return getBuiltinModule?.("node:fs");
  } catch {
    return undefined;
  }
}

function createState(): RuntimeState {
  const state: RuntimeState = {
    decisions: new Map(),
    hits: new Set(),
    events: [],
    eventKeys: new Set(),
    bufferedAttempts: new Set(),
    serverBuffers: new Map(),
    persistedServerRecords: new Set(),
    backgroundBuffers: new Map(),
    backgroundSequence: 0,
    runtimeSnapshots: false,
    assertionPhases: new Map(),
    probeV2Files: new Set(),
    probeV2Clock: { epoch: Number.NaN, fast: false },
    probeV2ContextEpochs: new Map(),
    probeV2NextEpoch: 1,
    probeV2HookInstalled: false,
  };
  if (!isBrowser) return state;
  try {
    const stored = JSON.parse(
      localStorage.getItem(storageKey) ?? "{}",
    ) as Partial<CoverageRuntimeSnapshot>;
    for (const snapshot of stored.decisions ?? []) {
      state.decisions.set(snapshot.meta.id, {
        meta: snapshot.meta,
        vectors: new Map(
          snapshot.vectors.map((vector) => [vectorKey(vector), vector]),
        ),
      });
    }
    for (const id of stored.hits ?? []) state.hits.add(id);
    for (const event of stored.events ?? []) {
      state.events.push(event);
      state.eventKeys.add(eventKey(event));
    }
  } catch {
    // Corrupt or unavailable storage must not affect application execution.
  }
  return state;
}

const runtimeStates = runtimeGlobal.__SUPERCOV_MCDC_STATES__ ?? new Map();
runtimeGlobal.__SUPERCOV_MCDC_STATES__ = runtimeStates;
const state = runtimeStates.get(runtimeInstance) ?? createState();
runtimeStates.set(runtimeInstance, state);

function createServerPhaseStorage(): RequestStorage | undefined {
  if (isBrowser || typeof process === "undefined") return undefined;
  try {
    const getBuiltinModule = (
      process as typeof process & {
        getBuiltinModule?: (name: string) => AsyncHooksBuiltin;
      }
    ).getBuiltinModule;
    const AsyncLocalStorage =
      getBuiltinModule?.("node:async_hooks")?.AsyncLocalStorage;
    return AsyncLocalStorage ? new AsyncLocalStorage() : undefined;
  } catch {
    return undefined;
  }
}

const serverPhaseStorages =
  runtimeGlobal.__SUPERCOV_SERVER_PHASE_STORAGES__ ?? new Map();
runtimeGlobal.__SUPERCOV_SERVER_PHASE_STORAGES__ = serverPhaseStorages;
const serverPhaseStorage =
  serverPhaseStorages.get(runtimeInstance) ?? createServerPhaseStorage();
if (serverPhaseStorage)
  serverPhaseStorages.set(runtimeInstance, serverPhaseStorage);

function probeV2ContextKey(context: CoverageRequestContext): string {
  const phase = context.phaseId ?? "unscoped";
  if (!context.scope) {
    const runId =
      typeof process !== "undefined" ? process.env["SUPERCOV_RUN_ID"] : undefined;
    return `background\0${runId ?? runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__ ?? testId}\0${phase}`;
  }
  return `scope\0${attemptKey(context.scope)}\0${phase}`;
}

function activateProbeV2Key(key: string, force = false): number {
  let epoch = force ? undefined : state.probeV2ContextEpochs.get(key);
  if (epoch === undefined) {
    state.probeV2NextEpoch += 1;
    // Uint32Array uses zero as "never observed". A wrap is implausible, but
    // clearing is still safer than silently aliasing two execution contexts.
    if (state.probeV2NextEpoch >= 0xffff_ffff) {
      state.probeV2NextEpoch = 1;
      state.probeV2ContextEpochs.clear();
      for (const file of state.probeV2Files) {
        file.hitEpochs.fill(0);
        file.decisionObservationEpochs.fill(0);
        file.decisionObservationCounts.fill(0);
        file.decisionCompleteEpochs.fill(0);
        for (const vectors of file.decisionEpochs) {
          if (vectors instanceof Uint32Array) vectors.fill(0);
          else vectors.clear();
        }
      }
    }
    epoch = state.probeV2NextEpoch;
    state.probeV2ContextEpochs.set(key, epoch);
  }
  state.probeV2Clock.epoch = epoch;
  return epoch;
}

function activateProbeV2Context(context: CoverageRequestContext): number {
  return activateProbeV2Key(probeV2ContextKey(context));
}

function withProbeV2Context<T>(
  context: CoverageRequestContext,
  callback: () => T,
): T {
  const previous = state.probeV2Clock.epoch;
  activateProbeV2Context(context);
  try {
    return callback();
  } finally {
    state.probeV2Clock.epoch = previous;
  }
}

function installProbeV2AsyncHook(): void {
  if (
    isBrowser ||
    state.probeV2HookInstalled ||
    typeof process === "undefined"
  )
    return;
  try {
    const getBuiltinModule = (
      process as typeof process & {
        getBuiltinModule?: (name: string) => AsyncHooksBuiltin;
      }
    ).getBuiltinModule;
    const createHook = getBuiltinModule?.("node:async_hooks")?.createHook;
    if (!createHook) return;
    const epochs: number[] = [];
    createHook({
      before() {
        epochs.push(state.probeV2Clock.epoch);
        activateProbeV2Context(
          serverPhaseStorage?.getStore() ?? environmentRequestContext() ?? {},
        );
      },
      after() {
        state.probeV2Clock.epoch = epochs.pop() ?? state.probeV2Clock.epoch;
      },
    }).enable();
    state.probeV2HookInstalled = true;
    state.probeV2Clock.fast = true;
  } catch {
    // Probe v2 remains correct for synchronous contexts; the run's
    // compatibility gate keeps this experimental mode from being promoted on
    // hosts without async context hooks.
  }
}

runtimeGlobal.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__ = (
  browserTestId,
  browserPhaseId,
) => {
  state.probeV2Clock.fast = true;
  activateProbeV2Key(
    `browser\0${browserTestId}\0${browserPhaseId ?? "unscoped"}`,
  );
};

function decisionSnapshot(): McdcDecisionSnapshot[] {
  return [...state.decisions.values()].map((decision) => ({
    meta: decision.meta,
    vectors: [...decision.vectors.values()],
  }));
}

export function coverageSnapshot(): CoverageRuntimeSnapshot {
  return {
    decisions: decisionSnapshot(),
    hits: [...state.hits],
    events: state.events,
  };
}

export function resetCoverage(testId?: string): void {
  state.decisions.clear();
  state.hits.clear();
  state.events.length = 0;
  state.eventKeys.clear();
  state.probeV2ContextEpochs.clear();
  activateProbeV2Key(`reset\0${state.probeV2NextEpoch}`, true);
  if (testId) runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__ = testId;
  if (isBrowser) {
    try {
      localStorage.removeItem(storageKey);
    } catch {
      // Storage is optional in test environments.
    }
  }
}

runtimeGlobal.__SUPERCOV_MCDC_SNAPSHOT__ = decisionSnapshot;
runtimeGlobal.__SUPERCOV_COVERAGE_SNAPSHOT__ = coverageSnapshot;
runtimeGlobal.__SUPERCOV_RESET__ = resetCoverage;

function persistBrowser(): void {
  if (!isBrowser) return;
  try {
    localStorage.setItem(storageKey, JSON.stringify(coverageSnapshot()));
  } catch {
    // Coverage persistence is best-effort and must never change app behavior.
  }
}

function attemptKey(scope: CoverageExecutionScope): string {
  return `${scope.runId}\0${scope.workerId}\0${scope.attemptId}`;
}

function serverRecordKey(record: CoverageServerRecord): string {
  const suffix = record.type === "decision"
    ? `${record.meta.id}:${vectorKey(record.vector)}`
    : record.id;
  return `${record.phaseId ?? "unscoped"}:${record.type}:${suffix}`;
}

/**
 * Buffer and de-duplicate local Node evidence until its test attempt ends.
 * The transport destination is pinned here: a test may legitimately mutate
 * SUPERCOV_SERVER_EVIDENCE_ROOT while exercising Supercov's own transport,
 * and the eventual flush must land where this attempt's reader will look.
 */
export function beginBufferedServerEvidence(scope: CoverageExecutionScope): void {
  if (isBrowser) return;
  const key = attemptKey(scope);
  state.bufferedAttempts.add(key);
  if (!state.serverBuffers.has(key))
    state.serverBuffers.set(key, {
      scope,
      directory: serverEvidenceDirectory(scope),
      path: serverEvidencePath(scope),
      records: new Map<string, CoverageServerRecord>(),
    });
}

/**
 * Publish one local test attempt with one filesystem append. Returns the
 * pinned evidence path when records were written so the attempt's reader
 * never re-derives it from mutable environment.
 */
export function flushBufferedServerEvidence(
  scope: CoverageExecutionScope,
): string | undefined {
  if (isBrowser) return undefined;
  const key = attemptKey(scope);
  state.bufferedAttempts.delete(key);
  const buffered = state.serverBuffers.get(key);
  if (!buffered) return undefined;
  state.serverBuffers.delete(key);
  const fs = getFs();
  if (!fs || buffered.records.size === 0) return undefined;
  try {
    fs.mkdirSync(buffered.directory, { recursive: true });
    fs.appendFileSync(
      buffered.path,
      [...buffered.records.values()]
        .map((record) => JSON.stringify(record))
        .join("\n") + "\n",
    );
    return buffered.path;
  } catch {
    // Collection is best-effort and must never change test behavior.
    return undefined;
  }
}

/** A local runner will persist coverageSnapshot(), so avoid duplicate files. */
export function enableRuntimeSnapshotEvidence(): void {
  state.runtimeSnapshots = true;
}

function flushAllBufferedServerEvidence(): void {
  for (const buffered of [...state.serverBuffers.values()])
    flushBufferedServerEvidence(buffered.scope);
  for (const runId of [...state.backgroundBuffers.keys()])
    flushBufferedBackgroundEvidence(runId);
}

/**
 * Background execution has no test attempt that can own a normal buffer.
 * Still, one immutable file per probe is catastrophic for hot code: a single
 * compatibility run produced millions of directory entries and made report
 * publication consume gigabytes. De-duplicate in memory and claim one batch
 * file per runtime at flush. O_EXCL retains clone safety for snapshot-derived
 * processes with identical PIDs and counters.
 */
export function flushBufferedBackgroundEvidence(
  runId: string,
): string | undefined {
  if (isBrowser) return undefined;
  const records = state.backgroundBuffers.get(runId);
  if (!records || records.size === 0) return undefined;
  const fs = getFs();
  if (!fs) return undefined;
  const directory = backgroundEvidenceDirectory(runId);
  const shard = process.env["SUPERCOV_EXECUTION_LOG_SHARD"] ?? "process";
  const writer = `${shard}-${process.pid}`;
  const payload = [...records.values()]
    .map((record) => JSON.stringify(record))
    .join("\n") + "\n";
  try {
    fs.mkdirSync(directory, { recursive: true });
    const nextSequence = writeExclusiveBackgroundRecord(
      fs,
      runId,
      writer,
      state.backgroundSequence,
      payload,
    );
    state.backgroundSequence = nextSequence;
    state.backgroundBuffers.delete(runId);
    return backgroundEvidencePath(runId, `${writer}-${nextSequence - 1}`);
  } catch {
    // Keep the buffer so a later explicit/exit flush can retry. Collection is
    // best-effort and must never change application behavior.
    return undefined;
  }
}

/**
 * Claim one immutable background record. Starting from the same sequence is
 * intentional: a snapshotted process can be cloned with identical pid,
 * environment and memory. O_EXCL makes the shared filesystem the allocator.
 */
export function writeExclusiveBackgroundRecord(
  fs: Pick<FsBuiltin, "writeFileSync">,
  runId: string,
  writer: string,
  initialSequence: number,
  payload: string,
): number {
  let sequence = initialSequence;
  for (let attempt = 0; attempt < 10_000; attempt += 1) {
    const candidate = backgroundEvidencePath(
      runId,
      `${writer}-${sequence++}`,
    );
    try {
      fs.writeFileSync(candidate, payload, { flag: "wx" });
      return sequence;
    } catch (error) {
      if ((error as { code?: string }).code === "EEXIST") continue;
      throw error;
    }
  }
  throw Object.assign(
    new Error("Could not allocate a collision-free Supercov background evidence record"),
    { code: "SUPERCOV_BACKGROUND_COLLISION_LIMIT" },
  );
}

if (!isBrowser) {
  const flushers = runtimeGlobal.__SUPERCOV_BUFFER_FLUSHERS__ ?? new Set();
  runtimeGlobal.__SUPERCOV_BUFFER_FLUSHERS__ = flushers;
  flushers.add(flushAllBufferedServerEvidence);
  if (!runtimeGlobal.__SUPERCOV_BUFFER_EXIT_INSTALLED__) {
    process.once("exit", () => {
      for (const flush of runtimeGlobal.__SUPERCOV_BUFFER_FLUSHERS__ ?? [])
        flush();
    });
    runtimeGlobal.__SUPERCOV_BUFFER_EXIT_INSTALLED__ = true;
  }
}

function appendServer(record: CoverageServerRecord): void {
  if (state.runtimeSnapshots) {
    const timestampMs = record.timestampMs ?? Date.now();
    if (record.type === "decision") {
      recordBrowserEvent({
        type: "decision",
        id: record.meta.id,
        vector: record.vector,
        timestampMs,
        ...(record.phaseId ? { phaseId: record.phaseId } : {}),
        environment: "server",
      });
    } else {
      recordBrowserEvent({
        type: "hit",
        id: record.id,
        timestampMs,
        ...(record.phaseId ? { phaseId: record.phaseId } : {}),
        environment: "server",
      });
    }
    return;
  }
  const fs = getFs();
  if (!fs) return;
  const context = currentRequestContext();
  const scope = context.scope;
  const runId =
    scope?.runId ??
    (typeof process !== "undefined"
      ? process.env["SUPERCOV_RUN_ID"]
      : undefined);
  if (!runId) return;
  try {
    const directory = scope ? serverEvidenceDirectory(scope) : undefined;
    const path = scope ? serverEvidencePath(scope) : undefined;
    const serialized = { ...record, ...(scope ? { scope } : {}) };
    if (scope && state.bufferedAttempts.has(attemptKey(scope))) {
      const key = attemptKey(scope);
      const buffered = state.serverBuffers.get(key) ?? {
        scope,
        directory,
        path,
        records: new Map<string, CoverageServerRecord>(),
      };
      buffered.records.set(serverRecordKey(serialized), serialized);
      state.serverBuffers.set(key, buffered);
      return;
    }
    const deduplicationKey =
      scope && serialized.phaseId
        ? `${attemptKey(scope)}:${serverRecordKey(serialized)}`
        : undefined;
    if (
      deduplicationKey &&
      state.persistedServerRecords.has(deduplicationKey)
    )
      return;
    if (!path) {
      const buffered = state.backgroundBuffers.get(runId) ?? new Map();
      buffered.set(serverRecordKey(serialized), serialized);
      state.backgroundBuffers.set(runId, buffered);
      // Bound loss and memory for very large/dynamic denominators while still
      // reducing millions of probe writes to at most one file per 4K records.
      if (buffered.size >= 4_096) flushBufferedBackgroundEvidence(runId);
      return;
    }
    fs.mkdirSync(directory!, { recursive: true });
    fs.appendFileSync(
      path,
      JSON.stringify(serialized) + "\n",
    );
    if (deduplicationKey)
      state.persistedServerRecords.add(deduplicationKey);
  } catch {
    // The instrumented build must remain behaviorally identical if collection fails.
  }
}

function environmentRequestContext(): CoverageRequestContext | undefined {
  if (isBrowser || typeof process === "undefined") return undefined;
  const carrier = decodeCoverageCarrier(process.env[COVERAGE_CARRIER_ENV]);
  return carrier
    ? {
        ...(carrier.scope ? { scope: carrier.scope } : {}),
        ...(carrier.phaseId ? { phaseId: carrier.phaseId } : {}),
      }
    : undefined;
}

function currentRequestContext(): CoverageRequestContext {
  const stored = serverPhaseStorage?.getStore();
  if (stored !== undefined) {
    if (stored.scope || stored.phaseId) return stored;
    return runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__
      ? { scope: runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ }
      : {};
  }
  const environment = environmentRequestContext();
  if (environment?.scope || environment?.phaseId) return environment;
  return runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__
    ? { scope: runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ }
    : {};
}

/**
 * Activate a serial runner's exact test-attempt identity. Runners using this
 * API must guarantee that tests do not overlap inside one process; parallel
 * worker processes remain independent. Async/concurrent runners must use
 * withCoverageCarrier instead.
 */
export function activateCoverageScope(scope?: CoverageExecutionScope): void {
  if (scope) runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__ = scope;
  else delete runtimeGlobal.__SUPERCOV_ACTIVE_SCOPE__;
  activateProbeV2Context(scope ? { scope } : {});
}

export function coverageCarrier(): CoverageCarrier {
  const context = currentRequestContext();
  return {
    version: 1,
    ...(context.scope ? { scope: context.scope } : {}),
    ...(context.phaseId ? { phaseId: context.phaseId } : {}),
  };
}

export function withCoverageCarrier<T>(
  carrier: CoverageCarrier | string | undefined,
  callback: () => T,
): T {
  const decoded =
    typeof carrier === "string" ? decodeCoverageCarrier(carrier) : carrier;
  if (!serverPhaseStorage || !decoded) return callback();
  const context = {
    ...(decoded.scope ? { scope: decoded.scope } : {}),
    ...(decoded.phaseId ? { phaseId: decoded.phaseId } : {}),
  };
  return serverPhaseStorage.run(context, () =>
    withProbeV2Context(context, callback)
  );
}

function assertionPhaseState(scope: CoverageExecutionScope): {
  counter: number;
  phases: CoveragePhase[];
} {
  const key = attemptKey(scope);
  const existing = state.assertionPhases.get(key);
  if (existing) return existing;
  const created = { counter: 0, phases: [] as CoveragePhase[] };
  state.assertionPhases.set(key, created);
  return created;
}

function finishAssertionPhase(phase: CoveragePhase, error?: unknown): void {
  phase.endedAtMs = Date.now();
  phase.status = error === undefined ? "passed" : "failed";
  if (error !== undefined)
    phase.error = error instanceof Error ? error.message : String(error);
}

/**
 * Execute an assertion and its argument evaluation under one explicit phase.
 * The source transformer supplies the thunk so JavaScript does not evaluate
 * assertion arguments before the phase exists. Builtin wrappers also call
 * this as a fallback for dynamic assertion references.
 */
export function withNodeAssertionPhase<T>(
  operation: string,
  source: string | undefined,
  callback: () => T,
): T {
  const context = currentRequestContext();
  const scope = context.scope;
  if (!scope) return callback();
  const existing = context.phaseId
    ? assertionPhaseState(scope).phases.find(
        (phase) => phase.id === context.phaseId && phase.kind === "assertion",
      )
    : undefined;
  if (existing) return callback();
  const attempt = assertionPhaseState(scope);
  const phase: CoveragePhase = {
    id: `${scope.attemptId}:assertion:${++attempt.counter}`,
    kind: "assertion",
    operation,
    ...(source ? { source } : {}),
    startedAtMs: Date.now(),
  };
  attempt.phases.push(phase);
  try {
    const result = withCoverageCarrier(
      { version: 1, scope, phaseId: phase.id },
      callback,
    );
    if (
      result &&
      typeof (result as unknown as PromiseLike<unknown>).then === "function"
    )
      return Promise.resolve(result).then(
        (value) => {
          finishAssertionPhase(phase);
          return value;
        },
        (error: unknown) => {
          finishAssertionPhase(phase, error);
          throw error;
        },
      ) as T;
    finishAssertionPhase(phase);
    return result;
  } catch (error) {
    finishAssertionPhase(phase, error);
    throw error;
  }
}

/** Consume the assertion phases recorded for one exact runner attempt. */
export function takeNodeAssertionPhases(
  scope: CoverageExecutionScope,
): CoveragePhase[] {
  const key = attemptKey(scope);
  const phases = state.assertionPhases.get(key)?.phases ?? [];
  state.assertionPhases.delete(key);
  return phases;
}

export function bindCoverageContext<T extends (...args: never[]) => unknown>(
  callback: T,
  carrier = coverageCarrier(),
): T {
  return function boundCoverageContext(
    this: unknown,
    ...args: Parameters<T>
  ): ReturnType<T> {
    return withCoverageCarrier(carrier, () =>
      Reflect.apply(callback, this, args),
    ) as ReturnType<T>;
  } as T;
}

export function coverageContextHeaders(): Record<string, string> {
  const context = currentRequestContext();
  if (!context.scope) return {};
  return {
    [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(context.scope),
    ...(context.phaseId
      ? { [COVERAGE_PHASE_HEADER]: context.phaseId }
      : {}),
  };
}

export function coverageContextEnvironment(): Record<string, string> {
  return { [COVERAGE_CARRIER_ENV]: encodeCoverageCarrier(coverageCarrier()) };
}

function installServerFetchPropagation(): void {
  if (
    isBrowser ||
    runtimeGlobal.__SUPERCOV_FETCH_PATCHED__ ||
    typeof globalThis.fetch !== "function"
  )
    return;
  const originalFetch = globalThis.fetch.bind(globalThis);
  globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
    const coverage = coverageContextHeaders();
    if (Object.keys(coverage).length === 0) return originalFetch(input, init);
    const headers = new Headers(
      init?.headers ?? (input instanceof Request ? input.headers : undefined),
    );
    for (const [name, value] of Object.entries(coverage))
      headers.set(name, value);
    return originalFetch(input, { ...init, headers });
  }) as typeof globalThis.fetch;
  runtimeGlobal.__SUPERCOV_FETCH_PATCHED__ = true;
}

installServerFetchPropagation();

function installServerChildPropagation(): void {
  if (
    isBrowser ||
    runtimeGlobal.__SUPERCOV_CHILD_PATCHED__ ||
    typeof process === "undefined"
  )
    return;
  const getBuiltinModule = (
    process as typeof process & {
      getBuiltinModule?: (name: string) => Record<string, unknown>;
    }
  ).getBuiltinModule;
  const child = getBuiltinModule?.("node:child_process");
  if (!child) return;
  const mutableChild = child as Record<string, unknown>;
  const optionIndex = (method: string, args: unknown[]): number => {
    if (
      method === "spawn" ||
      method === "spawnSync" ||
      method === "fork" ||
      method === "execFile" ||
      method === "execFileSync"
    )
      return Array.isArray(args[1]) || (args.length > 2 && args[2] !== undefined)
        ? 2
        : 1;
    return 1;
  };
  for (const method of [
    "exec",
    "execFile",
    "execFileSync",
    "execSync",
    "fork",
    "spawn",
    "spawnSync",
  ]) {
    const original = mutableChild[method];
    if (typeof original !== "function") continue;
    mutableChild[method] = function (...args: unknown[]): unknown {
      const index = optionIndex(method, args);
      const existing =
        args[index] && typeof args[index] === "object"
          ? (args[index] as { env?: Record<string, string | undefined> })
          : {};
      const options = {
        ...existing,
        env: {
          ...process.env,
          ...(existing.env ?? {}),
          ...coverageContextEnvironment(),
        },
      };
      const scoped = [...args];
      if (typeof scoped[index] === "function") scoped.splice(index, 0, options);
      else scoped[index] = options;
      return Reflect.apply(original, child, scoped);
    };
  }
  const moduleBuiltin = getBuiltinModule?.("node:module") as
    | { syncBuiltinESMExports?: () => void }
    | undefined;
  moduleBuiltin?.syncBuiltinESMExports?.();
  runtimeGlobal.__SUPERCOV_CHILD_PATCHED__ = true;
}

installServerChildPropagation();

function currentPhaseId(): string | undefined {
  if (runtimeGlobal.__SUPERCOV_PHASE_ID__)
    return runtimeGlobal.__SUPERCOV_PHASE_ID__;
  if (!isBrowser) return currentRequestContext().phaseId;
  try {
    const local = localStorage.getItem(phaseStorageKey);
    if (local) return local;
    return undefined;
  } catch {
    return undefined;
  }
}

function requestHeaders(
  value: unknown,
): { get(name: string): unknown } | undefined {
  if (!value || typeof value !== "object") return undefined;
  const directHeaders = (value as { headers?: unknown }).headers;
  const request =
    directHeaders && typeof directHeaders === "object"
      ? value
      : (value as { request?: unknown }).request;
  if (!request || typeof request !== "object") return undefined;
  const headers = (request as { headers?: unknown }).headers;
  if (!headers || typeof headers !== "object") return undefined;
  const get = (headers as { get?: unknown }).get;
  if (typeof get === "function")
    return {
      get(name: string): unknown {
        return Reflect.apply(get, headers, [name]);
      },
    };
  const values = headers as Record<string, unknown>;
  return Object.keys(values).length > 0
    ? {
        get(name: string): unknown {
          return values[name] ?? values[name.toLowerCase()];
        },
      }
    : undefined;
}

function requestCoverageContext(
  value: unknown,
): CoverageRequestContext | undefined {
  const headers = requestHeaders(value);
  if (!headers) return undefined;
  const rawCookie = headers.get("cookie");
  const cookies = new Map<string, string>();
  if (typeof rawCookie === "string") {
    for (const part of rawCookie.split(";")) {
      const separator = part.indexOf("=");
      if (separator < 0) continue;
      const name = part.slice(0, separator).trim();
      const encoded = part.slice(separator + 1).trim();
      try {
        cookies.set(name, decodeURIComponent(encoded));
      } catch {
        // Ignore a malformed unrelated cookie.
      }
    }
  }
  const encodedScope =
    headers.get(COVERAGE_SCOPE_HEADER) ?? cookies.get(COVERAGE_SCOPE_COOKIE);
  const rawPhaseId =
    headers.get(COVERAGE_PHASE_HEADER) ?? cookies.get(COVERAGE_PHASE_COOKIE);
  const scope = decodeCoverageScope(
    typeof encodedScope === "string" ? encodedScope : undefined,
  );
  const phaseId =
    typeof rawPhaseId === "string" && rawPhaseId.length > 0
      ? rawPhaseId
      : undefined;
  return {
    ...(scope ? { scope } : {}),
    ...(phaseId ? { phaseId } : {}),
  };
}

export function withRequestPhase<T extends (...args: never[]) => unknown>(
  handler: T,
): T {
  if (!serverPhaseStorage) return handler;
  return function coverageRequestPhase(
    this: unknown,
    ...args: Parameters<T>
  ): ReturnType<T> {
    const requestContext = args
      .map((argument) => requestCoverageContext(argument))
      .find(
        (context): context is CoverageRequestContext => context !== undefined,
      );
    // A recognized HTTP/WebSocket request is a context boundary even when it
    // carries no Supercov headers or cookies. In particular, health/readiness
    // requests must not inherit the SUPERCOV_CONTEXT of the process that
    // happened to launch a long-lived application server. Inner framework
    // callbacks with no request argument still inherit the surrounding
    // AsyncLocalStorage context.
    const inheritedContext =
      requestContext === undefined ? currentRequestContext() : {};
    const context = {
      ...(requestContext?.scope ?? inheritedContext.scope
        ? { scope: requestContext?.scope ?? inheritedContext.scope }
        : {}),
      ...(requestContext?.phaseId ?? inheritedContext.phaseId
        ? { phaseId: requestContext?.phaseId ?? inheritedContext.phaseId }
        : {}),
    };
    const invoke = () => Reflect.apply(handler, this, args) as ReturnType<T>;
    return requestContext !== undefined || context.scope || context.phaseId
      ? serverPhaseStorage.run(context, () =>
          withProbeV2Context(context, invoke)
        )
      : invoke();
  } as T;
}

function eventKey(event: CoverageRuntimeEvent): string {
  const suffix =
    event.type === "decision"
      ? `${event.id}:${vectorKey(event.vector)}`
      : event.id;
  return `${event.phaseId ?? "unscoped"}:${event.type}:${suffix}`;
}

function recordBrowserEvent(event: CoverageRuntimeEvent): boolean {
  const key = eventKey(event);
  if (state.eventKeys.has(key)) return false;
  state.eventKeys.add(key);
  state.events.push(event);
  return true;
}

export function coverageHit(id: string): void {
  state.hits.add(id);
  const timestampMs = Date.now();
  const phaseId = currentPhaseId();
  if (isBrowser) {
    if (
      recordBrowserEvent({
        type: "hit",
        id,
        timestampMs,
        ...(phaseId ? { phaseId } : {}),
        environment: "browser",
      })
    )
      persistBrowser();
  } else {
    // Request servers retain repeated executions for phase-window correlation.
    // Local runner adapters explicitly buffer and de-duplicate per test/phase.
    appendServer({
      type: "hit",
      id,
      timestampMs,
      ...(phaseId ? { phaseId } : {}),
    });
  }
}

/** Register immutable, file-local numeric probe tables once at module load. */
export function registerProbeV2(definition: {
  decisions: McdcDecisionMeta[];
  pointIds: string[];
  decisionVectorCounts?: number[];
}): ProbeV2FileState {
  const file: ProbeV2FileState = {
    decisions: definition.decisions,
    pointIds: definition.pointIds,
    clock: state.probeV2Clock,
    hitEpochs: new Uint32Array(definition.pointIds.length),
    decisionEpochs: definition.decisions.map((meta) =>
      meta.conditions.length <= 6
        ? new Uint32Array(2 * 3 ** meta.conditions.length)
        : new Map<number, number>()
    ),
    decisionVectorCounts: definition.decisions.map(
      (_, index) => definition.decisionVectorCounts?.[index] ?? 0,
    ),
    decisionObservationEpochs: new Uint32Array(definition.decisions.length),
    decisionObservationCounts: new Uint16Array(definition.decisions.length),
    decisionCompleteEpochs: new Uint32Array(definition.decisions.length),
  };
  state.probeV2Files.add(file);
  if (isBrowser) {
    state.probeV2Clock.fast = true;
    activateProbeV2Key(
      `browser\0${runtimeGlobal.__SUPERCOV_MCDC_TEST_ID__ ?? testId}\0${runtimeGlobal.__SUPERCOV_PHASE_ID__ ?? "unscoped"}`,
    );
  } else {
    installProbeV2AsyncHook();
  }
  return file;
}

/** Numeric point probes deduplicate before timestamps, records, and serialization. */
export function coverageHitV2(file: ProbeV2FileState, index: number): void {
  const id = file.pointIds[index];
  if (!id) return;
  const fallbackEpoch = !file.clock.fast;
  const previousEpoch = file.clock.epoch;
  if (fallbackEpoch)
    activateProbeV2Context(currentRequestContext());
  try {
    const epoch = file.clock.epoch;
    if (file.hitEpochs[index] === epoch) return;
    file.hitEpochs[index] = epoch;
    coverageHit(id);
  } finally {
    if (fallbackEpoch) file.clock.epoch = previousEpoch;
  }
}

/** Decode the exact v1 masking vector from the allocation-free base-3 frame. */
export function decodeProbeV2Vector(
  conditionCount: number,
  encoded: number,
  outcome: boolean,
): McdcVector | undefined {
  if (
    !Number.isSafeInteger(conditionCount) ||
    conditionCount < 0 ||
    conditionCount > 32 ||
    !Number.isSafeInteger(encoded) ||
    encoded < 0
  )
    return undefined;
  const values: Array<boolean | null> = [];
  let remaining = encoded;
  for (let index = 0; index < conditionCount; index += 1) {
    const digit = remaining % 3;
    values.push(digit === 0 ? null : digit === 2);
    remaining = Math.floor(remaining / 3);
  }
  return remaining === 0 ? { values, outcome } : undefined;
}

/** Record one exact decision vector, at most once per attempt/phase epoch. */
export function mcdcEndV2<T>(
  file: ProbeV2FileState,
  decisionIndex: number,
  encoded: number,
  value: T,
): T {
  const meta = file.decisions[decisionIndex];
  if (!meta || !Number.isSafeInteger(encoded) || encoded < 0) return value;
  const outcome = Boolean(value);
  const vectorIndex = encoded * 2 + (outcome ? 1 : 0);
  if (!Number.isSafeInteger(vectorIndex)) return value;
  const fallbackEpoch = !file.clock.fast;
  const previousEpoch = file.clock.epoch;
  if (fallbackEpoch)
    activateProbeV2Context(currentRequestContext());
  try {
    const epoch = file.clock.epoch;
    const seen = file.decisionEpochs[decisionIndex];
    if (!seen) return value;
    if (seen instanceof Uint32Array) {
      if (seen[vectorIndex] === epoch) return value;
      seen[vectorIndex] = epoch;
    } else {
      if (seen.get(vectorIndex) === epoch) return value;
      seen.set(vectorIndex, epoch);
    }
    const expectedCount = file.decisionVectorCounts[decisionIndex] ?? 0;
    if (expectedCount > 0) {
      if (file.decisionObservationEpochs[decisionIndex] !== epoch) {
        file.decisionObservationEpochs[decisionIndex] = epoch;
        file.decisionObservationCounts[decisionIndex] = 0;
      }
      const observedCount =
        (file.decisionObservationCounts[decisionIndex] ?? 0) + 1;
      file.decisionObservationCounts[decisionIndex] = observedCount;
      if (observedCount >= expectedCount)
        file.decisionCompleteEpochs[decisionIndex] = epoch;
    }
    if (!state.decisions.has(meta.id))
      state.decisions.set(meta.id, { meta, vectors: new Map() });
    const vector = decodeProbeV2Vector(
      meta.conditions.length,
      encoded,
      outcome,
    );
    return vector ? mcdcEnd({ meta, values: vector.values }, value) : value;
  } finally {
    if (fallbackEpoch) file.clock.epoch = previousEpoch;
  }
}

export function selectionBegin(
  shortId: string,
  rightId: string,
): SelectionFrame {
  return { shortId, rightId, rightEvaluated: false };
}

function applyInferredName<T>(value: T, inferredName?: string): T {
  if (
    inferredName &&
    typeof value === "function" &&
    value.name === ""
  ) {
    Object.defineProperty(value, "name", {
      value: inferredName,
      configurable: true,
    });
  }
  return value;
}

// ECMAScript does not infer a name through a parenthesized assignment target,
// but JavaScriptCore historically does. oxc cannot retain those parentheses in
// its assignment-target AST. Detect the executing host once so instrumentation
// preserves the application's real behavior instead of normalizing it to one
// engine's interpretation.
const hostNamesParenthesizedAssignments = (() => {
  let candidate: (() => void) | undefined;
  (candidate) = function () {};
  return candidate.name === "candidate";
})();

export function parenthesizedAssignmentValue<T>(
  value: T,
  inferredName: string,
): T {
  return hostNamesParenthesizedAssignments
    ? applyInferredName(value, inferredName)
    : value;
}

export function selectionRight<T>(
  frame: SelectionFrame,
  value: T,
  inferredName?: string,
): T {
  frame.rightEvaluated = true;
  return applyInferredName(value, inferredName);
}

export function selectionEnd<T>(frame: SelectionFrame, value: T): T {
  coverageHit(frame.rightEvaluated ? frame.rightId : frame.shortId);
  return value;
}

export function optionalSelect<T>(
  shortId: string,
  continuedId: string,
  value: T,
): T {
  coverageHit(value === null || value === undefined ? shortId : continuedId);
  return value;
}

export function optionalCallBegin(
  shortId: string,
  continuedId: string,
): OptionalCallFrame {
  return { shortId, continuedId, reached: false, continued: false };
}

/** Mark callee-reference evaluation while returning its operand unchanged. */
export function optionalCallReached<T>(
  frame: OptionalCallFrame,
  value: T,
): T {
  frame.reached = true;
  return value;
}

const optionalCallEmptySpread = {
  [Symbol.iterator](): Iterator<never> {
    return {
      next(): IteratorResult<never> {
        return { done: true, value: undefined as never };
      },
    };
  },
};

/** Native optional calls evaluate argument spreads only on continuation. */
export function optionalCallContinued(
  frame: OptionalCallFrame,
): Iterable<never> {
  frame.continued = true;
  return optionalCallEmptySpread;
}

export function optionalCallEnd<T>(frame: OptionalCallFrame, value: T): T {
  if (frame.reached)
    coverageHit(frame.continued ? frame.continuedId : frame.shortId);
  return value;
}

export function defaultSelected<T>(
  defaultId: string,
  value: T,
  inferredName?: string,
): T {
  pendingDefaults.set(defaultId, (pendingDefaults.get(defaultId) ?? 0) + 1);
  return applyInferredName(value, inferredName);
}

export function defaultEntered(defaultId: string, providedId: string): void {
  const pending = pendingDefaults.get(defaultId) ?? 0;
  if (pending > 0) {
    pendingDefaults.set(defaultId, pending - 1);
    coverageHit(defaultId);
  } else {
    coverageHit(providedId);
  }
}

export function tryBegin(successId: string, catchId: string): TryFrame {
  return { successId, catchId, caught: false };
}

export function tryCatch<T>(frame: TryFrame, value: T): T {
  frame.caught = true;
  return value;
}

export function tryEnd(frame: TryFrame): void {
  coverageHit(frame.caught ? frame.catchId : frame.successId);
}

export function loopBegin(zeroId: string, enteredId: string): LoopFrame {
  return { zeroId, enteredId, entered: false };
}

export function loopEntered(frame: LoopFrame): void {
  frame.entered = true;
}

export function loopEnd(frame: LoopFrame): void {
  coverageHit(frame.entered ? frame.enteredId : frame.zeroId);
}

export function mcdcBegin(id: string, meta: McdcDecisionMeta): DecisionFrame {
  if (!state.decisions.has(id)) {
    state.decisions.set(id, { meta, vectors: new Map() });
  }
  return {
    meta,
    values: Array.from({ length: meta.conditions.length }, () => null),
  };
}

export function mcdcCondition<T>(
  frame: DecisionFrame,
  index: number,
  value: T,
): T {
  frame.values[index] = Boolean(value);
  return value;
}

export function mcdcEnd<T>(frame: DecisionFrame, value: T): T {
  const decision = state.decisions.get(frame.meta.id);
  if (!decision) return value;

  const vector: McdcVector = { values: frame.values, outcome: Boolean(value) };
  const key = vectorKey(vector);
  decision.vectors.set(key, vector);
  const timestampMs = Date.now();
  const phaseId = currentPhaseId();
  if (isBrowser) {
    if (
      recordBrowserEvent({
        type: "decision",
        id: decision.meta.id,
        vector,
        timestampMs,
        ...(phaseId ? { phaseId } : {}),
        environment: "browser",
      })
    )
      persistBrowser();
  } else {
    appendServer({
      type: "decision",
      meta: decision.meta,
      vector,
      timestampMs,
      ...(phaseId ? { phaseId } : {}),
    });
  }
  return value;
}
