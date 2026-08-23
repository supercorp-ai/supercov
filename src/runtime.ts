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
}

type McdcGlobal = typeof globalThis & {
  __SUPERCOV_MCDC_STATE__?: RuntimeState;
  __SUPERCOV_MCDC_TEST_ID__?: string;
  __SUPERCOV_PHASE_ID__?: string;
  __SUPERCOV_SERVER_PHASE_STORAGE__?: RequestStorage;
  __SUPERCOV_FETCH_PATCHED__?: boolean;
  __SUPERCOV_CHILD_PATCHED__?: boolean;
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
}

const runtimeGlobal = globalThis as McdcGlobal;
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

const state = runtimeGlobal.__SUPERCOV_MCDC_STATE__ ?? createState();
runtimeGlobal.__SUPERCOV_MCDC_STATE__ = state;

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

const serverPhaseStorage =
  runtimeGlobal.__SUPERCOV_SERVER_PHASE_STORAGE__ ??
  createServerPhaseStorage();
if (serverPhaseStorage)
  runtimeGlobal.__SUPERCOV_SERVER_PHASE_STORAGE__ =
    serverPhaseStorage;

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

function appendServer(record: CoverageServerRecord): void {
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
    const path = scope
      ? serverEvidencePath(scope)
      : backgroundEvidencePath(runId);
    fs.mkdirSync(directory, { recursive: true });
    fs.appendFileSync(
      path,
      JSON.stringify({ ...record, ...(scope ? { scope } : {}) }) + "\n",
    );
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
      return Array.isArray(args[1]) ? 2 : 1;
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
  const encodedScope = headers.get(COVERAGE_SCOPE_HEADER);
  const rawPhaseId = headers.get(COVERAGE_PHASE_HEADER);
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
    // The server process cannot directly see the browser's active phase.
    // Keep repeated executions and correlate them to phase time windows in
    // the analyzer; global first-hit de-duplication would lose later actions.
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
