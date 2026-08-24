import type {
  CoverageCarrier,
  CoverageExecutionScope,
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

interface SelectionFrame {
  shortId: string;
  rightId: string;
  rightEvaluated: boolean;
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
  backgroundSequence: number;
  runtimeSnapshots: boolean;
}

type McdcGlobal = typeof globalThis & {
  __SUPERCOV_MCDC_STATES__?: Map<string, RuntimeState>;
  __SUPERCOV_MCDC_TEST_ID__?: string;
  __SUPERCOV_PHASE_ID__?: string;
  __SUPERCOV_SERVER_PHASE_STORAGES__?: Map<string, RequestStorage>;
  __SUPERCOV_FETCH_PATCHED__?: boolean;
  __SUPERCOV_CHILD_PATCHED__?: boolean;
  __SUPERCOV_BUFFER_EXIT_INSTALLED__?: boolean;
  __SUPERCOV_BUFFER_FLUSHERS__?: Set<() => void>;
  __SUPERCOV_MCDC_SNAPSHOT__?: () => McdcDecisionSnapshot[];
  __SUPERCOV_COVERAGE_SNAPSHOT__?: () => CoverageRuntimeSnapshot;
  __SUPERCOV_RESET__?: (testId?: string) => void;
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
    backgroundSequence: 0,
    runtimeSnapshots: false,
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

/** Buffer and de-duplicate local Node evidence until its test attempt ends. */
export function beginBufferedServerEvidence(scope: CoverageExecutionScope): void {
  if (isBrowser) return;
  state.bufferedAttempts.add(attemptKey(scope));
}

/** Publish one local test attempt with one filesystem append. */
export function flushBufferedServerEvidence(scope: CoverageExecutionScope): void {
  if (isBrowser) return;
  const key = attemptKey(scope);
  state.bufferedAttempts.delete(key);
  const buffered = state.serverBuffers.get(key);
  if (!buffered) return;
  state.serverBuffers.delete(key);
  const fs = getFs();
  if (!fs || buffered.records.size === 0) return;
  try {
    fs.mkdirSync(buffered.directory, { recursive: true });
    fs.appendFileSync(
      buffered.path,
      [...buffered.records.values()]
        .map((record) => JSON.stringify(record))
        .join("\n") + "\n",
    );
  } catch {
    // Collection is best-effort and must never change test behavior.
  }
}

/** A local runner will persist coverageSnapshot(), so avoid duplicate files. */
export function enableRuntimeSnapshotEvidence(): void {
  state.runtimeSnapshots = true;
}

function flushAllBufferedServerEvidence(): void {
  for (const buffered of [...state.serverBuffers.values()])
    flushBufferedServerEvidence(buffered.scope);
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
  if (state.runtimeSnapshots) return;
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
    const directory = scope
      ? serverEvidenceDirectory(scope)
      : backgroundEvidenceDirectory(runId);
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
    fs.mkdirSync(directory, { recursive: true });
    if (!path) {
      // A warm process can be snapshotted and cloned into many remote VMs.
      // Those clones share pid, environment and in-memory counters, so they
      // must not append to one JSONL file on a shared mount. Claim one
      // immutable record file with O_EXCL instead. Colliding clones simply
      // advance until one filename is theirs; no provider-specific VM identity
      // or append-atomicity guarantee is required.
      const shard = process.env["SUPERCOV_EXECUTION_LOG_SHARD"] ?? "process";
      const writer = `${shard}-${process.pid}`;
      const payload = JSON.stringify(serialized) + "\n";
      state.backgroundSequence = writeExclusiveBackgroundRecord(
        fs,
        runId,
        writer,
        state.backgroundSequence,
        payload,
      );
      return;
    }
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
  return serverPhaseStorage?.getStore() ?? environmentRequestContext() ?? {};
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
  return serverPhaseStorage.run(
    {
      ...(decoded.scope ? { scope: decoded.scope } : {}),
      ...(decoded.phaseId ? { phaseId: decoded.phaseId } : {}),
    },
    callback,
  );
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

function requestCoverageContext(value: unknown): CoverageRequestContext {
  const headers = requestHeaders(value);
  if (!headers) return {};
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
      .find((context) => context.scope || context.phaseId) ?? {};
    const inheritedContext = currentRequestContext();
    const context = {
      ...(requestContext.scope ?? inheritedContext.scope
        ? { scope: requestContext.scope ?? inheritedContext.scope }
        : {}),
      ...(requestContext.phaseId ?? inheritedContext.phaseId
        ? { phaseId: requestContext.phaseId ?? inheritedContext.phaseId }
        : {}),
    };
    const invoke = () => Reflect.apply(handler, this, args) as ReturnType<T>;
    return context.scope || context.phaseId
      ? serverPhaseStorage.run(context, invoke)
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
