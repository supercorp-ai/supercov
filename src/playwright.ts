import { createHash } from "node:crypto";
import childProcess from "node:child_process";
import { mkdirSync, readFileSync, rmSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import { syncBuiltinESMExports } from "node:module";
import { dirname, relative, resolve, sep } from "node:path";
import * as standardPlaywright from "@playwright/test";
import type {
  APIRequestContext,
  Browser,
  BrowserContext,
  CDPSession,
  Page,
  PlaywrightTestArgs,
  PlaywrightWorkerArgs,
  Route,
  TestInfo,
  TestType,
  Worker,
} from "@playwright/test";
import type {
  CoverageExecutionScope,
  CoveragePhase,
  CoverageRuntimeSnapshot,
  McdcRawTestResult,
} from "./types.ts";
import { inferTestProvenance } from "./provenance.ts";
import { atomicWriteFileSync } from "./atomic.ts";
import {
  COVERAGE_PHASE_HEADER,
  COVERAGE_PHASE_COOKIE,
  COVERAGE_SCOPE_COOKIE,
  COVERAGE_SCOPE_HEADER,
  COVERAGE_CARRIER_ENV,
  encodeCoverageCarrier,
  encodeCoverageScope,
  serverEvidenceDirectory,
  serverEvidencePath,
} from "./transport.ts";

export * from "@playwright/test";

type PlaywrightAdapterModule = typeof standardPlaywright & {
  offlineTest?: TestType<Record<string, unknown>, Record<string, unknown>>;
  WebhookBodiesContract?: unknown;
};

const generatedTargetModule = "__SUPERCOV_PLAYWRIGHT_MODULE__";
const targetModule =
  process.env["SUPERCOV_PLAYWRIGHT_MODULE"] ??
  (generatedTargetModule.startsWith("__")
    ? "@playwright/test"
    : generatedTargetModule);
const adapter = (
  targetModule === "@playwright/test"
    ? standardPlaywright
    : await import(targetModule)
) as PlaywrightAdapterModule;
type BaseTestArgs = PlaywrightTestArgs & Record<string, unknown>;
type BaseWorkerArgs = PlaywrightWorkerArgs & Record<string, unknown>;

const base = (adapter.offlineTest ?? adapter.test) as TestType<
  BaseTestArgs,
  BaseWorkerArgs
>;
const baseExpect = adapter.expect;

const GENERATED_EVIDENCE_DIRECTORY =
  "__SUPERCOV_EVIDENCE_DIRECTORY__";
const GENERATED_RUN_ID = "__SUPERCOV_RUN_ID__";
const PHASE_STORAGE_KEY = "__supercov_phase";
const ACTION_METHODS = new Set([
  "blur",
  "check",
  "click",
  "dblclick",
  "dispatchEvent",
  "dragTo",
  "evaluate",
  "evaluateHandle",
  "fill",
  "focus",
  "goBack",
  "goForward",
  "goto",
  "hover",
  "press",
  "reload",
  "selectOption",
  "setInputFiles",
  "tap",
  "type",
  "uncheck",
]);
const REQUEST_METHODS = new Set([
  "delete",
  "fetch",
  "get",
  "head",
  "patch",
  "post",
  "put",
]);

function isApiRequestContext(value: object): value is APIRequestContext {
  const candidate = value as unknown as Record<string, unknown>;
  return (
    typeof candidate["fetch"] === "function" &&
    typeof candidate["get"] === "function" &&
    typeof candidate["post"] === "function"
  );
}

function callerSource(): string | undefined {
  const stack = new Error().stack?.split("\n").slice(2) ?? [];
  const candidate = stack.find(
    (line) =>
      /[/\\]tests[/\\]/.test(line) &&
      !line.includes(".supercov") &&
      !line.includes("node_modules"),
  );
  if (!candidate) return undefined;
  return candidate
    .trim()
    .replace(/^at\s+/, "")
    .replace("file:///workspace/", "")
    .replace("/workspace/", "");
}

class CoveragePhaseController {
  readonly phases: CoveragePhase[] = [];
  private counter = 0;
  private lastActionId: string | undefined;
  private activePhaseId: string | undefined;
  private readonly pages = new Set<Page>();
  private readonly workers = new Set<Worker>();
  private readonly contexts = new Set<BrowserContext>();
  private readonly contextConfiguredHeaders = new Map<BrowserContext, Record<string, string>>();
  private readonly contextRoutes = new Map<BrowserContext, (route: Route) => Promise<void>>();
  private readonly cdpSessions = new Map<Page, CDPSession>();
  private readonly newDocumentScriptIds = new Map<Page, string>();
  private readonly pendingRegistrations = new Set<Promise<void>>();
  private scriptUpdate: Promise<void> = Promise.resolve();
  private readonly proxyCache = new WeakMap<object, object>();

  // Parameter properties are stateful despite the base ESLint rule treating
  // this as an empty constructor.
  // eslint-disable-next-line no-useless-constructor
  constructor(
    readonly scope: CoverageExecutionScope,
    private readonly configuredHeaders: Record<string, string> = {},
  ) {}

  allPages(): Page[] {
    return [...this.pages];
  }

  allWorkers(): Worker[] {
    return [...this.workers];
  }

  async registerPage(page: Page): Promise<void> {
    if (this.pages.has(page)) return;
    this.pages.add(page);
    await this.registerContext(page.context());
    const cdp = await page.context().newCDPSession(page).catch(() => undefined);
    if (cdp) this.cdpSessions.set(page, cdp);
    const phaseId = this.requestPhaseId();
    if (phaseId) await this.activatePage(page, phaseId);
    page.on("worker", (worker) => {
      void this.registerWorker(worker);
    });
    for (const worker of page.workers()) void this.registerWorker(worker);
  }

  async registerContext(
    context: BrowserContext,
    configuredHeaders: Record<string, string> = this.configuredHeaders,
  ): Promise<void> {
    if (this.contexts.has(context)) return;
    this.contexts.add(context);
    this.contextConfiguredHeaders.set(context, configuredHeaders);
    await context.addInitScript(
      ({ attemptId, scopeHeader, scopeValue, scopeCookie }) => {
        (
          globalThis as typeof globalThis & {
            __SUPERCOV_MCDC_TEST_ID__?: string;
          }
        ).__SUPERCOV_MCDC_TEST_ID__ = attemptId;
        try {
          document.cookie = `${scopeCookie}=${encodeURIComponent(scopeValue)}; Path=/; SameSite=Lax`;
          const originalFetch = globalThis.fetch?.bind(globalThis);
          if (originalFetch) {
            globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
              const headers = new Headers(
                init?.headers ??
                  (input instanceof Request ? input.headers : undefined),
              );
              headers.set(scopeHeader, scopeValue);
              const phase = (
                globalThis as typeof globalThis & {
                  __SUPERCOV_PHASE_ID__?: string;
                }
              ).__SUPERCOV_PHASE_ID__;
              if (phase) headers.set("x-supercov-phase", phase);
              return originalFetch(input, { ...init, headers });
            }) as typeof globalThis.fetch;
          }
        } catch {
          // Browser instrumentation must not change application behavior.
        }
      },
      {
        attemptId: this.scope.attemptId,
        scopeHeader: COVERAGE_SCOPE_HEADER,
        scopeValue: encodeCoverageScope(this.scope),
        scopeCookie: COVERAGE_SCOPE_COOKIE,
      },
    );
    const attachPhase = async (route: Route): Promise<void> => {
      const phaseId = this.requestPhaseId();
      await route.continue({
        headers: {
          ...route.request().headers(),
          [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(this.scope),
          ...(phaseId ? { [COVERAGE_PHASE_HEADER]: phaseId } : {}),
        },
      });
    };
    this.contextRoutes.set(context, attachPhase);
    await context.route("**/*", attachPhase);
    const register = (page: Page): void => {
      const pending = this.registerPage(page).finally(() =>
        this.pendingRegistrations.delete(pending),
      );
      this.pendingRegistrations.add(pending);
    };
    context.on("page", register);
    context.on("serviceworker", (worker: Worker) => {
      void this.registerWorker(worker);
    });
    for (const worker of context.serviceWorkers()) void this.registerWorker(worker);
    await this.updateContextHeaders(context, this.requestPhaseId());
    for (const page of context.pages()) register(page);
  }

  private async registerWorker(worker: Worker): Promise<void> {
    if (this.workers.has(worker)) return;
    this.workers.add(worker);
    const phaseId = this.requestPhaseId();
    await worker
        .evaluate(
          ({ attemptId, scopeHeader, scopeValue, phaseHeader, phase }) => {
            (
              globalThis as typeof globalThis & {
                __SUPERCOV_MCDC_TEST_ID__?: string;
                __SUPERCOV_PHASE_ID__?: string;
              }
            ).__SUPERCOV_MCDC_TEST_ID__ = attemptId;
            if (phase)
              (
                globalThis as typeof globalThis & {
                  __SUPERCOV_PHASE_ID__?: string;
                }
              ).__SUPERCOV_PHASE_ID__ = phase;
            const originalFetch = globalThis.fetch?.bind(globalThis);
            if (!originalFetch) return;
            globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
              const headers = new Headers(
                init?.headers ??
                  (input instanceof Request ? input.headers : undefined),
              );
              headers.set(scopeHeader, scopeValue);
              if (phase) headers.set(phaseHeader, phase);
              return originalFetch(input, { ...init, headers });
            }) as typeof globalThis.fetch;
          },
          {
            attemptId: this.scope.attemptId,
            scopeHeader: COVERAGE_SCOPE_HEADER,
            scopeValue: encodeCoverageScope(this.scope),
            phaseHeader: COVERAGE_PHASE_HEADER,
            phase: phaseId,
          },
        )
        .catch(() => undefined);
  }

  async beginAction(operation: string): Promise<CoveragePhase> {
    const phase = this.createPhase("action", operation);
    this.lastActionId = phase.id;
    this.activePhaseId = phase.id;
    await this.activateInBrowser(phase.id);
    return phase;
  }

  beginAssertion(operation: string): CoveragePhase {
    const phase = this.createPhase("assertion", operation, this.lastActionId);
    this.activePhaseId = phase.id;
    // Playwright queues browser protocol commands in order. Starting this
    // evaluation before an async locator assertion is sufficient to tag its
    // polling work without turning synchronous expect matchers into promises.
    void this.activateInBrowser(phase.id);
    return phase;
  }

  requestPhaseId(): string | undefined {
    return this.activePhaseId;
  }

  async dispose(): Promise<void> {
    await Promise.all([...this.pendingRegistrations]);
    await this.scriptUpdate;
    for (const [page, cdp] of this.cdpSessions) {
      const identifier = this.newDocumentScriptIds.get(page);
      if (identifier) await cdp
        .send("Page.removeScriptToEvaluateOnNewDocument", {
          identifier,
        })
        .catch(() => undefined);
      await cdp.detach().catch(() => undefined);
    }
    for (const [context, route] of this.contextRoutes)
      await context.unroute("**/*", route).catch(() => undefined);
  }

  finish(phase: CoveragePhase, error?: unknown): void {
    phase.endedAtMs = Date.now();
    phase.status = error === undefined ? "passed" : "failed";
    if (error !== undefined)
      phase.error = error instanceof Error ? error.message : String(error);
  }

  wrap<T extends object>(target: T): T {
    const cached = this.proxyCache.get(target);
    if (cached) return cached as T;
    const proxy = new Proxy(target, {
      get: (object, property, receiver) => {
        if (property === "then") return undefined;
        // Playwright's locator matchers validate `receiver.constructor.name`.
        // Preserve the native constructor rather than wrapping it as a method.
        if (property === "constructor") return object.constructor;
        const value = Reflect.get(object, property, receiver) as unknown;
        if (typeof value !== "function") return value;
        const method = String(property);
        return (...args: unknown[]) => {
          const isRequest = REQUEST_METHODS.has(method) &&
            isApiRequestContext(object);
          if (ACTION_METHODS.has(method) || isRequest) {
            return (async () => {
              const operation = `${object.constructor?.name ?? "Playwright"}.${method}`;
              const phase = await this.beginAction(operation);
              try {
                const result = await Reflect.apply(
                  value as (...innerArgs: unknown[]) => unknown,
                  object,
                  isRequest ? this.scopeApiRequest(args) : args,
                );
                this.finish(phase);
                return this.prepareResult(result);
              } catch (error) {
                this.finish(phase, error);
                throw error;
              }
            })();
          }
          const invokedArgs =
            method === "newContext" && object.constructor?.name === "Browser"
              ? this.scopeBrowserContext(args)
              : args;
          const result = Reflect.apply(
            value as (...innerArgs: unknown[]) => unknown,
            object,
            invokedArgs,
          );
          return this.wrapResult(result, method === "newContext" ? invokedArgs : undefined);
        };
      },
    });
    this.proxyCache.set(target, proxy);
    return proxy;
  }

  private createPhase(
    kind: CoveragePhase["kind"],
    operation: string,
    causedByPhaseId?: string,
  ): CoveragePhase {
    const source = callerSource();
    const phase: CoveragePhase = {
      id: `${this.scope.attemptId}:phase:${++this.counter}`,
      kind,
      operation,
      ...(source ? { source } : {}),
      ...(causedByPhaseId ? { causedByPhaseId } : {}),
      startedAtMs: Date.now(),
    };
    this.phases.push(phase);
    return phase;
  }

  private wrapResult(result: unknown, sourceArgs?: unknown[]): unknown {
    if (result instanceof Promise)
      return result.then((resolved) => this.prepareResult(resolved, sourceArgs));
    if (
      result === null ||
      typeof result !== "object" ||
      Array.isArray(result) ||
      ArrayBuffer.isView(result)
    )
      return result;
    return this.wrap(result);
  }

  private async prepareResult(
    result: unknown,
    sourceArgs?: unknown[],
  ): Promise<unknown> {
    if (
      !result ||
      typeof result !== "object" ||
      Array.isArray(result) ||
      ArrayBuffer.isView(result)
    )
      return result;
    const candidate = result as Record<string, unknown>;
    if (
      typeof candidate["pages"] === "function" &&
      typeof candidate["route"] === "function"
    )
      await this.registerContext(
        result as BrowserContext,
        ((sourceArgs?.[0] as { extraHTTPHeaders?: Record<string, string> } | undefined)
          ?.extraHTTPHeaders ?? this.configuredHeaders),
      );
    if (
      typeof candidate["frames"] === "function" &&
      typeof candidate["context"] === "function"
    )
      await this.registerPage(result as Page);
    return this.wrap(result);
  }

  private scopeApiRequest(args: unknown[]): unknown[] {
    const scoped = [...args];
    const optionIndex = 1;
    const phaseId = this.requestPhaseId();
    const coverageHeaders = {
      [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(this.scope),
      ...(phaseId ? { [COVERAGE_PHASE_HEADER]: phaseId } : {}),
    };
    const options =
      scoped[optionIndex] && typeof scoped[optionIndex] === "object"
        ? (scoped[optionIndex] as { headers?: unknown })
        : {};
    scoped[optionIndex] = {
      ...options,
      headers: mergeRequestHeaders(
        options.headers,
        coverageHeaders,
      ),
    };
    return scoped;
  }

  private scopeBrowserContext(args: unknown[]): unknown[] {
    const scoped = [...args];
    const options =
      scoped[0] && typeof scoped[0] === "object"
        ? (scoped[0] as { extraHTTPHeaders?: Record<string, string> })
        : {};
    scoped[0] = {
      ...options,
      extraHTTPHeaders: {
        ...(options.extraHTTPHeaders ?? {}),
        ...(activeCoverageHeaders() ?? {}),
      },
    };
    return scoped;
  }

  private async activateInBrowser(phaseId: string): Promise<void> {
    await Promise.all(
      [...this.contexts].map((context) =>
        this.updateContextHeaders(context, phaseId),
      ),
    );
    await Promise.all(
      [...this.pages].flatMap((page) => page.frames()).map((frame) =>
        frame
          .evaluate(
            ({ id, storageKey, scopeCookie, scopeValue, phaseCookie }) => {
              (
                globalThis as typeof globalThis & {
                  __SUPERCOV_PHASE_ID__?: string;
                }
              ).__SUPERCOV_PHASE_ID__ = id;
              try {
                localStorage.setItem(storageKey, id);
                document.cookie = `${scopeCookie}=${encodeURIComponent(scopeValue)}; Path=/; SameSite=Lax`;
                document.cookie = `${phaseCookie}=${encodeURIComponent(id)}; Path=/; SameSite=Lax`;
              } catch {
                // Sandboxed/cross-origin frames may not expose localStorage.
              }
            },
            {
              id: phaseId,
              storageKey: PHASE_STORAGE_KEY,
              scopeCookie: COVERAGE_SCOPE_COOKIE,
              scopeValue: encodeCoverageScope(this.scope),
              phaseCookie: COVERAGE_PHASE_COOKIE,
            },
          )
          .catch(() => undefined),
      ),
    );
    await Promise.all(
      [...this.workers].map((worker) =>
        worker
          .evaluate((id) => {
            (
              globalThis as typeof globalThis & {
                __SUPERCOV_PHASE_ID__?: string;
              }
            ).__SUPERCOV_PHASE_ID__ = id;
          }, phaseId)
          .catch(() => undefined),
      ),
    );
    await Promise.all([...this.pages].map((page) => this.activatePage(page, phaseId)));
  }

  private async updateContextHeaders(
    context: BrowserContext,
    phaseId?: string,
  ): Promise<void> {
    await context
      .setExtraHTTPHeaders({
        ...(this.contextConfiguredHeaders.get(context) ?? this.configuredHeaders),
        [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(this.scope),
        ...(phaseId ? { [COVERAGE_PHASE_HEADER]: phaseId } : {}),
      })
      .catch(() => undefined);
  }

  private async activatePage(page: Page, phaseId: string): Promise<void> {
    const cdp = this.cdpSessions.get(page);
    if (!cdp) return;
    this.scriptUpdate = this.scriptUpdate.then(async () => {
      const previous = this.newDocumentScriptIds.get(page);
      if (previous) {
        await cdp.send("Page.removeScriptToEvaluateOnNewDocument", {
          identifier: previous,
        }).catch(() => undefined);
      }
      const installed = (await cdp
        .send("Page.addScriptToEvaluateOnNewDocument", {
          source: `globalThis.__SUPERCOV_PHASE_ID__=${JSON.stringify(phaseId)};`,
          runImmediately: true,
        })
        .catch(() => undefined)) as { identifier?: string } | undefined;
      if (installed?.identifier)
        this.newDocumentScriptIds.set(page, installed.identifier);
    });
    await this.scriptUpdate.catch(() => undefined);
  }
}

let activeController: CoveragePhaseController | undefined;
const controllers = new Map<string, CoveragePhaseController>();

function activeCoverageHeaders(): Record<string, string> | undefined {
  const controller = activeController;
  if (!controller) return undefined;
  const phaseId = controller.requestPhaseId();
  return {
    [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(controller.scope),
    ...(phaseId ? { [COVERAGE_PHASE_HEADER]: phaseId } : {}),
  };
}

function mergeRequestHeaders(
  existing: unknown,
  coverage: Record<string, string>,
): Record<string, unknown> {
  if (existing instanceof Headers) {
    return { ...Object.fromEntries(existing.entries()), ...coverage };
  }
  if (Array.isArray(existing)) {
    const normalized: Record<string, unknown> = {};
    for (let index = 0; index + 1 < existing.length; index += 2) {
      normalized[String(existing[index])] = existing[index + 1];
    }
    return { ...normalized, ...coverage };
  }
  return {
    ...(existing && typeof existing === "object" ? existing : {}),
    ...coverage,
  };
}

function scopedNodeRequestArguments(args: unknown[]): unknown[] {
  const coverage = activeCoverageHeaders();
  if (!coverage || args.length === 0) return args;
  const scoped = [...args];
  const first = scoped[0];
  const startsWithUrl = typeof first === "string" || first instanceof URL;
  const candidate = startsWithUrl ? scoped[1] : first;
  const hasOptions =
    candidate !== null &&
    typeof candidate === "object" &&
    !(candidate instanceof URL);
  if (hasOptions) {
    const index = startsWithUrl ? 1 : 0;
    const options = candidate as { headers?: unknown };
    scoped[index] = {
      ...options,
      headers: mergeRequestHeaders(options.headers, coverage),
    };
  } else if (startsWithUrl) {
    scoped.splice(1, 0, { headers: coverage });
  }
  return scoped;
}

function installNodeRequestScopePropagation(): void {
  const originalHttpRequest = http.request;
  const originalHttpGet = http.get;
  const originalHttpsRequest = https.request;
  const originalHttpsGet = https.get;
  http.request = ((...args: unknown[]) =>
    Reflect.apply(originalHttpRequest, http, scopedNodeRequestArguments(args))) as typeof http.request;
  http.get = ((...args: unknown[]) =>
    Reflect.apply(originalHttpGet, http, scopedNodeRequestArguments(args))) as typeof http.get;
  https.request = ((...args: unknown[]) =>
    Reflect.apply(originalHttpsRequest, https, scopedNodeRequestArguments(args))) as typeof https.request;
  https.get = ((...args: unknown[]) =>
    Reflect.apply(originalHttpsGet, https, scopedNodeRequestArguments(args))) as typeof https.get;
  // Existing ESM named imports such as `import { request } from "node:http"`
  // must observe the patched CommonJS-compatible builtin exports too.
  syncBuiltinESMExports();

  if (typeof globalThis.fetch === "function") {
    const originalFetch = globalThis.fetch.bind(globalThis);
    globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const coverage = activeCoverageHeaders();
      if (!coverage) return originalFetch(input, init);
      const headers = new Headers(
        init?.headers ?? (input instanceof Request ? input.headers : undefined),
      );
      for (const [name, value] of Object.entries(coverage))
        headers.set(name, value);
      return originalFetch(input, { ...init, headers });
    }) as typeof globalThis.fetch;
  }
}

installNodeRequestScopePropagation();

function scopedChildOptions(
  args: unknown[],
  optionIndex: number,
): unknown[] {
  const controller = activeController;
  if (!controller) return args;
  const scoped = [...args];
  const existing =
    scoped[optionIndex] && typeof scoped[optionIndex] === "object"
      ? (scoped[optionIndex] as { env?: NodeJS.ProcessEnv })
      : {};
  const options = {
    ...existing,
    env: {
      ...process.env,
      ...(existing.env ?? {}),
      SUPERCOV_RUN_ID: controller.scope.runId,
      [COVERAGE_CARRIER_ENV]: encodeCoverageCarrier({
        version: 1,
        scope: controller.scope,
        ...(controller.requestPhaseId()
          ? { phaseId: controller.requestPhaseId() }
          : {}),
      }),
    },
  };
  if (typeof scoped[optionIndex] === "function")
    scoped.splice(optionIndex, 0, options);
  else scoped[optionIndex] = options;
  return scoped;
}

function childOptionIndex(
  method: string,
  args: unknown[],
): number {
  if (method === "spawn" || method === "spawnSync" || method === "fork")
    return Array.isArray(args[1]) ? 2 : 1;
  if (method === "execFile" || method === "execFileSync")
    return Array.isArray(args[1]) ? 2 : 1;
  return 1;
}

function installChildProcessScopePropagation(): void {
  for (const method of [
    "exec",
    "execFile",
    "execFileSync",
    "execSync",
    "fork",
    "spawn",
    "spawnSync",
  ] as const) {
    const original = childProcess[method] as unknown as (
      ...args: unknown[]
    ) => unknown;
    (childProcess as unknown as Record<string, unknown>)[method] = function (
      ...args: unknown[]
    ): unknown {
      return Reflect.apply(
        original,
        childProcess,
        scopedChildOptions(args, childOptionIndex(method, args)),
      );
    };
  }
  syncBuiltinESMExports();
}

installChildProcessScopePropagation();

function wrapMatchers<T extends object>(matchers: T, path = "expect"): T {
  return new Proxy(matchers, {
    get(target, property, receiver) {
      const value = Reflect.get(target, property, receiver) as unknown;
      const name = String(property);
      if (value && typeof value === "object")
        return wrapMatchers(value, `${path}.${name}`);
      if (typeof value !== "function") return value;
      return (...args: unknown[]) => {
        const controller = activeController;
        const phase = controller?.beginAssertion(`${path}.${name}`);
        try {
          const result = Reflect.apply(
            value as (...innerArgs: unknown[]) => unknown,
            target,
            args,
          );
          if (result instanceof Promise) {
            return result.then(
              (resolved) => {
                if (phase && controller) controller.finish(phase);
                return resolved;
              },
              (error) => {
                if (phase && controller) controller.finish(phase, error);
                throw error;
              },
            );
          }
          if (phase && controller) controller.finish(phase);
          return result;
        } catch (error) {
          if (phase && controller) controller.finish(phase, error);
          throw error;
        }
      };
    },
  });
}

function wrapExpectCallable<T extends (...args: never[]) => unknown>(
  callable: T,
): T {
  return new Proxy(callable, {
    apply(target, thisArg, argumentsList) {
      const result = Reflect.apply(target, thisArg, argumentsList) as unknown;
      if (typeof result === "function") return wrapExpectCallable(result as T);
      if (result && typeof result === "object") return wrapMatchers(result);
      return result;
    },
    get(target, property, receiver) {
      const value = Reflect.get(target, property, receiver) as unknown;
      if (typeof value !== "function") return value;
      return wrapExpectCallable(value.bind(target) as T);
    },
  });
}

export const expect = wrapExpectCallable(
  baseExpect as unknown as (...args: never[]) => unknown,
) as typeof baseExpect;

function currentRunId(): string {
  return (
    process.env["SUPERCOV_RUN_ID"] ??
    (GENERATED_RUN_ID.startsWith("__") ? "unscoped" : GENERATED_RUN_ID)
  );
}

function executionScope(testInfo: {
  testId: string;
  retry: number;
  workerIndex: number;
}): CoverageExecutionScope {
  const runId = currentRunId();
  const workerId = `pid-${process.pid}-worker-${testInfo.workerIndex}`;
  const testKey = createHash("sha256")
    .update(testInfo.testId)
    .digest("hex")
    .slice(0, 24);
  const attemptId = createHash("sha256")
    .update(`${runId}\0${workerId}\0${testInfo.testId}\0${testInfo.retry}`)
    .digest("hex")
    .slice(0, 24);
  return {
    version: 1,
    runId,
    workerId,
    testId: testInfo.testId,
    testKey,
    retry: testInfo.retry,
    attemptId,
  };
}

function readServerRecords(
  scope: CoverageExecutionScope,
): McdcRawTestResult["server"] {
  try {
    return readFileSync(serverEvidencePath(scope), "utf8")
      .split("\n")
      .filter(Boolean)
      .flatMap((line) => {
        try {
          const record = JSON.parse(
            line,
          ) as McdcRawTestResult["server"][number];
          return record.scope?.attemptId === scope.attemptId ? [record] : [];
        } catch {
          // Ignore a final partial line if the server was writing during collection.
          return [];
        }
      });
  } catch {
    return [];
  }
}

const instrumentedTest = base.extend<{ mcdcAutoCollect: void }>({
  page: async ({ page }, use, testInfo) => {
    const scope = executionScope(testInfo);
    const configuredHeaders = Object.fromEntries(
      Object.entries(testInfo.project.use.extraHTTPHeaders ?? {}).map(
        ([name, value]) => [name, String(value)],
      ),
    );
    const controller = new CoveragePhaseController(
      scope,
      configuredHeaders,
    );
    controllers.set(scope.attemptId, controller);
    activeController = controller;
    await controller.registerPage(page);
    try {
      await use(controller.wrap(page));
    } finally {
      await controller.dispose();
      if (activeController === controller) activeController = undefined;
      controllers.delete(scope.attemptId);
    }
  },
  browser: [
    async ({ browser }, use) => {
      await use(
        new Proxy(browser, {
          get(target, property, receiver) {
            const controller = activeController;
            return controller
              ? Reflect.get(controller.wrap(target), property, receiver)
              : Reflect.get(target, property, receiver);
          },
        }) as Browser,
      );
    },
    { scope: "worker" },
  ],
  request: async ({ request }, use) => {
    await use(
      new Proxy(request, {
        get(target, property, receiver) {
          const controller = activeController;
          return controller
            ? Reflect.get(controller.wrap(target), property, receiver)
            : Reflect.get(target, property, receiver);
        },
      }) as APIRequestContext,
    );
  },
  mcdcAutoCollect: [
    async (
      { page }: { page: Page },
      use: (value: void) => Promise<void>,
      testInfo: TestInfo,
    ) => {
      const scope = executionScope(testInfo);
      const serverOutput = serverEvidencePath(scope);
      mkdirSync(serverEvidenceDirectory(scope), { recursive: true });
      rmSync(serverOutput, { force: true });

      try {
        await use();
      } finally {
        const controller = controllers.get(scope.attemptId);
        const browser: CoverageRuntimeSnapshot[] = [];
        const pages = controller?.allPages() ?? [page];
        for (const currentPage of pages) {
          for (const frame of currentPage.frames()) {
            const frameSnapshot = await frame
              .evaluate(() => {
                const getSnapshot = (
                  globalThis as typeof globalThis & {
                    __SUPERCOV_COVERAGE_SNAPSHOT__?: () => CoverageRuntimeSnapshot;
                  }
                ).__SUPERCOV_COVERAGE_SNAPSHOT__;
                return getSnapshot?.() ?? { decisions: [], hits: [], events: [] };
              })
              .catch(
                () =>
                  ({
                    decisions: [],
                    hits: [],
                    events: [],
                  }) as CoverageRuntimeSnapshot,
              );
            browser.push(frameSnapshot);
          }
        }
        for (const worker of controller?.allWorkers() ?? []) {
          const workerSnapshot = await worker
            .evaluate(() => {
              const getSnapshot = (
                globalThis as typeof globalThis & {
                  __SUPERCOV_COVERAGE_SNAPSHOT__?: () => CoverageRuntimeSnapshot;
                }
              ).__SUPERCOV_COVERAGE_SNAPSHOT__;
              return getSnapshot?.() ?? { decisions: [], hits: [], events: [] };
            })
            .catch(
              () =>
                ({
                  decisions: [],
                  hits: [],
                  events: [],
                }) as CoverageRuntimeSnapshot,
            );
          browser.push(workerSnapshot);
        }

        const server = readServerRecords(scope);
        // Emit an artifact even when this test touched no application source.
        // A complete test-to-coverage matrix must also identify tests that are
        // removable without changing coverage.
        const outputPath = testInfo.outputPath("mcdc.json");
        mkdirSync(dirname(outputPath), { recursive: true });
        const testFile = relative(process.cwd(), testInfo.file)
          .split(sep)
          .join("/");
        const payload: McdcRawTestResult = {
          testId: testInfo.testId,
          scope,
          test: testInfo.titlePath.join(" > "),
          testFile,
          title: testInfo.title,
          retry: testInfo.retry,
          status: testInfo.status ?? "unknown",
          expectedStatus: testInfo.expectedStatus,
          provenance: inferTestProvenance({
            runner: "playwright",
            file: testFile,
            project: testInfo.project.name,
            explicitKind: process.env["SUPERCOV_TEST_KIND"],
          }),
          phases: controller?.phases ?? [],
          browser,
          server,
        };
        const serialized = `${JSON.stringify(payload)}\n`;
        atomicWriteFileSync(outputPath, serialized);

        // Pool runners may cycle-restore a VM immediately after Playwright
        // exits, which can discard or overwrite the normal artifact copy.
        // Write one uniquely named, one-shot evidence file to the runner's
        // shared directory as well. No test streams into this path.
        const evidenceDirectory =
          process.env["SUPERCOV_EVIDENCE_DIR"] ??
          (GENERATED_EVIDENCE_DIRECTORY.startsWith("__")
            ? undefined
            : GENERATED_EVIDENCE_DIRECTORY);
        if (evidenceDirectory) {
          const resolvedDirectory = resolve(process.cwd(), evidenceDirectory);
          const safeTestId = testInfo.testId.replace(/[^a-zA-Z0-9_-]/g, "_");
          const testEvidenceDirectory = resolve(
            resolvedDirectory,
            `${safeTestId}-${testInfo.retry}`,
          );
          mkdirSync(testEvidenceDirectory, { recursive: true });
          atomicWriteFileSync(
            resolve(testEvidenceDirectory, "mcdc.json"),
            serialized,
          );
        }
      }
    },
    { auto: true },
  ],
});

export const test = instrumentedTest;
export const offlineTest = instrumentedTest;
export const WebhookBodiesContract = adapter.WebhookBodiesContract;
