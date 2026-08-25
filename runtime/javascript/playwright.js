var __rewriteRelativeImportExtension = (this && this.__rewriteRelativeImportExtension) || function (path, preserveJsx) {
    if (typeof path === "string" && /^\.\.?\//.test(path)) {
        return path.replace(/\.(tsx)$|((?:\.d)?)((?:\.[^./]+?)?)\.([cm]?)ts$/i, function (m, tsx, d, ext, cm) {
            return tsx ? preserveJsx ? ".jsx" : ".js" : d && (!ext || !cm) ? m : (d + ext + "." + cm.toLowerCase() + "js");
        });
    }
    return path;
};
import { createHash } from "node:crypto";
import childProcess from "node:child_process";
import { mkdirSync, readFileSync, rmSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import { syncBuiltinESMExports } from "node:module";
import { dirname, relative, resolve, sep } from "node:path";
import * as standardPlaywright from "@playwright/test";
import { inferTestProvenance } from "./provenance.js";
import { atomicWriteFileSync } from "./atomic.js";
import { COVERAGE_PHASE_HEADER, COVERAGE_PHASE_COOKIE, COVERAGE_SCOPE_COOKIE, COVERAGE_SCOPE_HEADER, COVERAGE_CARRIER_ENV, encodeCoverageCarrier, encodeCoverageScope, serverEvidenceDirectory, serverEvidencePath, } from "./transport.js";
export * from "@playwright/test";
const generatedTargetModule = "__SUPERCOV_PLAYWRIGHT_MODULE__";
const targetModule = process.env["SUPERCOV_PLAYWRIGHT_MODULE"] ??
    (generatedTargetModule.startsWith("__")
        ? "@playwright/test"
        : generatedTargetModule);
const generatedTestExport = "__SUPERCOV_PLAYWRIGHT_TEST_EXPORT__";
const targetTestExport = process.env["SUPERCOV_PLAYWRIGHT_TEST_EXPORT"] ??
    (generatedTestExport.startsWith("__") ? "test" : generatedTestExport);
const adapter = (targetModule === "@playwright/test"
    ? standardPlaywright
    : await import(__rewriteRelativeImportExtension(targetModule)));
const base = (adapter[targetTestExport] ?? adapter.test);
const baseExpect = adapter.expect;
const GENERATED_EVIDENCE_DIRECTORY = "__SUPERCOV_EVIDENCE_DIRECTORY__";
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
function isApiRequestContext(value) {
    const candidate = value;
    return (typeof candidate["fetch"] === "function" &&
        typeof candidate["get"] === "function" &&
        typeof candidate["post"] === "function");
}
function callerSource() {
    const stack = new Error().stack?.split("\n").slice(2) ?? [];
    const candidate = stack.find((line) => /[/\\]tests[/\\]/.test(line) &&
        !line.includes(".supercov") &&
        !line.includes("node_modules"));
    if (!candidate)
        return undefined;
    const normalized = candidate.trim().replace(/^at\s+/, "");
    const projectRoot = process.env["SUPERCOV_PROJECT_ROOT"]
        ?.replaceAll("\\", "/")
        .replace(/\/$/, "");
    if (!projectRoot)
        return normalized;
    return normalized
        .replace(`file://${projectRoot}/`, "")
        .replace(`${projectRoot}/`, "");
}
class CoveragePhaseController {
    scope;
    configuredHeaders;
    phases = [];
    counter = 0;
    lastActionId;
    activePhaseId;
    pages = new Set();
    workers = new Set();
    contexts = new Set();
    contextConfiguredHeaders = new Map();
    cdpSessions = new Map();
    newDocumentScriptIds = new Map();
    pendingRegistrations = new Set();
    scriptUpdate = Promise.resolve();
    proxyCache = new WeakMap();
    runtimeSnapshots;
    // Parameter properties are stateful despite the base ESLint rule treating
    // this as an empty constructor.
    // eslint-disable-next-line no-useless-constructor
    constructor(scope, configuredHeaders = {}) {
        this.scope = scope;
        this.configuredHeaders = configuredHeaders;
    }
    allPages() {
        return [...this.pages];
    }
    allWorkers() {
        return [...this.workers];
    }
    async collectRuntimeSnapshots() {
        if (this.runtimeSnapshots)
            return this.runtimeSnapshots;
        const snapshots = [];
        for (const page of this.allPages()) {
            for (const frame of page.frames()) {
                const snapshot = await frame
                    .evaluate(() => {
                    const getSnapshot = globalThis.__SUPERCOV_COVERAGE_SNAPSHOT__;
                    return getSnapshot?.() ?? { decisions: [], hits: [], events: [] };
                })
                    .catch(() => ({
                    decisions: [],
                    hits: [],
                    events: [],
                }));
                snapshots.push(snapshot);
            }
        }
        for (const worker of this.allWorkers()) {
            const snapshot = await worker
                .evaluate(() => {
                const getSnapshot = globalThis.__SUPERCOV_COVERAGE_SNAPSHOT__;
                return getSnapshot?.() ?? { decisions: [], hits: [], events: [] };
            })
                .catch(() => ({
                decisions: [],
                hits: [],
                events: [],
            }));
            snapshots.push(snapshot);
        }
        this.runtimeSnapshots = snapshots;
        return snapshots;
    }
    async registerPage(page) {
        if (this.pages.has(page))
            return;
        this.pages.add(page);
        await this.registerContext(page.context());
        const cdp = await page.context().newCDPSession(page).catch(() => undefined);
        if (cdp)
            this.cdpSessions.set(page, cdp);
        const phaseId = this.requestPhaseId();
        if (phaseId)
            await this.activatePage(page, phaseId);
        page.on("worker", (worker) => {
            void this.registerWorker(worker);
        });
        for (const worker of page.workers())
            void this.registerWorker(worker);
    }
    async registerContext(context, configuredHeaders = this.configuredHeaders) {
        if (this.contexts.has(context))
            return;
        this.contexts.add(context);
        this.contextConfiguredHeaders.set(context, configuredHeaders);
        await context.addInitScript(({ attemptId, scopeHeader, scopeValue, scopeCookie }) => {
            globalThis.__SUPERCOV_MCDC_TEST_ID__ = attemptId;
            try {
                document.cookie = `${scopeCookie}=${encodeURIComponent(scopeValue)}; Path=/; SameSite=Lax`;
                const originalFetch = globalThis.fetch?.bind(globalThis);
                if (originalFetch) {
                    globalThis.fetch = ((input, init) => {
                        const headers = new Headers(init?.headers ??
                            (input instanceof Request ? input.headers : undefined));
                        headers.set(scopeHeader, scopeValue);
                        const phase = globalThis.__SUPERCOV_PHASE_ID__;
                        if (phase)
                            headers.set("x-supercov-phase", phase);
                        return originalFetch(input, { ...init, headers });
                    });
                }
            }
            catch {
                // Browser instrumentation must not change application behavior.
            }
        }, {
            attemptId: this.scope.attemptId,
            scopeHeader: COVERAGE_SCOPE_HEADER,
            scopeValue: encodeCoverageScope(this.scope),
            scopeCookie: COVERAGE_SCOPE_COOKIE,
        });
        const register = (page) => {
            const pending = this.registerPage(page).finally(() => this.pendingRegistrations.delete(pending));
            this.pendingRegistrations.add(pending);
        };
        context.on("page", register);
        context.on("serviceworker", (worker) => {
            void this.registerWorker(worker);
        });
        for (const worker of context.serviceWorkers())
            void this.registerWorker(worker);
        await this.updateContextHeaders(context, this.requestPhaseId());
        for (const page of context.pages())
            register(page);
    }
    async registerWorker(worker) {
        if (this.workers.has(worker))
            return;
        this.workers.add(worker);
        const phaseId = this.requestPhaseId();
        await worker
            .evaluate(({ attemptId, scopeHeader, scopeValue, phaseHeader, phase }) => {
            globalThis.__SUPERCOV_MCDC_TEST_ID__ = attemptId;
            if (phase)
                globalThis.__SUPERCOV_PHASE_ID__ = phase;
            globalThis.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__?.(attemptId, phase);
            const originalFetch = globalThis.fetch?.bind(globalThis);
            if (!originalFetch)
                return;
            globalThis.fetch = ((input, init) => {
                const headers = new Headers(init?.headers ??
                    (input instanceof Request ? input.headers : undefined));
                headers.set(scopeHeader, scopeValue);
                if (phase)
                    headers.set(phaseHeader, phase);
                return originalFetch(input, { ...init, headers });
            });
        }, {
            attemptId: this.scope.attemptId,
            scopeHeader: COVERAGE_SCOPE_HEADER,
            scopeValue: encodeCoverageScope(this.scope),
            phaseHeader: COVERAGE_PHASE_HEADER,
            phase: phaseId,
        })
            .catch(() => undefined);
    }
    async beginAction(operation) {
        const phase = this.createPhase("action", operation);
        this.lastActionId = phase.id;
        this.activePhaseId = phase.id;
        await this.activateInBrowser(phase.id);
        return phase;
    }
    beginAssertion(operation, source = callerSource()) {
        const phase = this.createPhase("assertion", operation, this.lastActionId, source);
        this.activePhaseId = phase.id;
        // Playwright queues browser protocol commands in order. Starting this
        // evaluation before an async locator assertion is sufficient to tag its
        // polling work without turning synchronous expect matchers into promises.
        void this.activateInBrowser(phase.id);
        return phase;
    }
    requestPhaseId() {
        return this.activePhaseId;
    }
    async dispose() {
        await Promise.all([...this.pendingRegistrations]);
        await this.scriptUpdate;
        for (const [page, cdp] of this.cdpSessions) {
            const identifier = this.newDocumentScriptIds.get(page);
            if (identifier)
                await cdp
                    .send("Page.removeScriptToEvaluateOnNewDocument", {
                    identifier,
                })
                    .catch(() => undefined);
            await cdp.detach().catch(() => undefined);
        }
    }
    finish(phase, error) {
        phase.endedAtMs = Date.now();
        phase.status = error === undefined ? "passed" : "failed";
        if (error !== undefined)
            phase.error = error instanceof Error ? error.message : String(error);
    }
    wrap(target) {
        const cached = this.proxyCache.get(target);
        if (cached)
            return cached;
        const proxy = new Proxy(target, {
            get: (object, property, receiver) => {
                if (property === "then")
                    return undefined;
                // Playwright's locator matchers validate `receiver.constructor.name`.
                // Preserve the native constructor rather than wrapping it as a method.
                if (property === "constructor")
                    return object.constructor;
                const value = Reflect.get(object, property, receiver);
                if (typeof value !== "function")
                    return value;
                const method = String(property);
                return (...args) => {
                    const isRequest = REQUEST_METHODS.has(method) &&
                        isApiRequestContext(object);
                    if (ACTION_METHODS.has(method) || isRequest) {
                        return (async () => {
                            const operation = `${object.constructor?.name ?? "Playwright"}.${method}`;
                            const phase = await this.beginAction(operation);
                            try {
                                const result = await Reflect.apply(value, object, isRequest ? this.scopeApiRequest(args) : args);
                                this.finish(phase);
                                return this.prepareResult(result);
                            }
                            catch (error) {
                                this.finish(phase, error);
                                throw error;
                            }
                        })();
                    }
                    const invokedArgs = method === "newContext" && object.constructor?.name === "Browser"
                        ? this.scopeBrowserContext(args)
                        : args;
                    const result = Reflect.apply(value, object, invokedArgs);
                    return this.wrapResult(result, method === "newContext" ? invokedArgs : undefined);
                };
            },
        });
        this.proxyCache.set(target, proxy);
        return proxy;
    }
    createPhase(kind, operation, causedByPhaseId, source = callerSource()) {
        const phase = {
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
    wrapResult(result, sourceArgs) {
        if (result instanceof Promise)
            return result.then((resolved) => this.prepareResult(resolved, sourceArgs));
        if (result === null ||
            typeof result !== "object" ||
            Array.isArray(result) ||
            ArrayBuffer.isView(result))
            return result;
        return this.wrap(result);
    }
    async prepareResult(result, sourceArgs) {
        if (!result ||
            typeof result !== "object" ||
            Array.isArray(result) ||
            ArrayBuffer.isView(result))
            return result;
        const candidate = result;
        if (typeof candidate["pages"] === "function" &&
            typeof candidate["route"] === "function")
            await this.registerContext(result, (sourceArgs?.[0]
                ?.extraHTTPHeaders ?? this.configuredHeaders));
        if (typeof candidate["frames"] === "function" &&
            typeof candidate["context"] === "function")
            await this.registerPage(result);
        return this.wrap(result);
    }
    scopeApiRequest(args) {
        const scoped = [...args];
        const optionIndex = 1;
        const phaseId = this.requestPhaseId();
        const coverageHeaders = {
            [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(this.scope),
            ...(phaseId ? { [COVERAGE_PHASE_HEADER]: phaseId } : {}),
        };
        const options = scoped[optionIndex] && typeof scoped[optionIndex] === "object"
            ? scoped[optionIndex]
            : {};
        scoped[optionIndex] = {
            ...options,
            headers: mergeRequestHeaders(options.headers, coverageHeaders),
        };
        return scoped;
    }
    scopeBrowserContext(args) {
        const scoped = [...args];
        const options = scoped[0] && typeof scoped[0] === "object"
            ? scoped[0]
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
    async activateInBrowser(phaseId) {
        await Promise.all([...this.contexts].map((context) => this.updateContextHeaders(context, phaseId)));
        await Promise.all([...this.pages].flatMap((page) => page.frames()).map((frame) => frame
            .evaluate(({ id, storageKey, scopeCookie, scopeValue, phaseCookie }) => {
            globalThis.__SUPERCOV_PHASE_ID__ = id;
            const coverageGlobal = globalThis;
            coverageGlobal.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__?.(coverageGlobal.__SUPERCOV_MCDC_TEST_ID__ ?? "unscoped", id);
            try {
                localStorage.setItem(storageKey, id);
                document.cookie = `${scopeCookie}=${encodeURIComponent(scopeValue)}; Path=/; SameSite=Lax`;
                document.cookie = `${phaseCookie}=${encodeURIComponent(id)}; Path=/; SameSite=Lax`;
            }
            catch {
                // Sandboxed/cross-origin frames may not expose localStorage.
            }
        }, {
            id: phaseId,
            storageKey: PHASE_STORAGE_KEY,
            scopeCookie: COVERAGE_SCOPE_COOKIE,
            scopeValue: encodeCoverageScope(this.scope),
            phaseCookie: COVERAGE_PHASE_COOKIE,
        })
            .catch(() => undefined)));
        await Promise.all([...this.workers].map((worker) => worker
            .evaluate((id) => {
            const coverageGlobal = globalThis;
            coverageGlobal.__SUPERCOV_PHASE_ID__ = id;
            coverageGlobal.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__?.(coverageGlobal.__SUPERCOV_MCDC_TEST_ID__ ?? "unscoped", id);
        }, phaseId)
            .catch(() => undefined)));
        await Promise.all([...this.pages].map((page) => this.activatePage(page, phaseId)));
    }
    async updateContextHeaders(context, phaseId) {
        await context
            .setExtraHTTPHeaders({
            ...(this.contextConfiguredHeaders.get(context) ?? this.configuredHeaders),
            [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(this.scope),
            ...(phaseId ? { [COVERAGE_PHASE_HEADER]: phaseId } : {}),
        })
            .catch(() => undefined);
    }
    async activatePage(page, phaseId) {
        const cdp = this.cdpSessions.get(page);
        if (!cdp)
            return;
        this.scriptUpdate = this.scriptUpdate.then(async () => {
            const previous = this.newDocumentScriptIds.get(page);
            if (previous) {
                await cdp.send("Page.removeScriptToEvaluateOnNewDocument", {
                    identifier: previous,
                }).catch(() => undefined);
            }
            const installed = (await cdp
                .send("Page.addScriptToEvaluateOnNewDocument", {
                source: `globalThis.__SUPERCOV_PHASE_ID__=${JSON.stringify(phaseId)};globalThis.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__?.(globalThis.__SUPERCOV_MCDC_TEST_ID__??"unscoped",${JSON.stringify(phaseId)});`,
                runImmediately: true,
            })
                .catch(() => undefined));
            if (installed?.identifier)
                this.newDocumentScriptIds.set(page, installed.identifier);
        });
        await this.scriptUpdate.catch(() => undefined);
    }
}
let activeController;
let bridgedAssertionDepth = 0;
const controllers = new Map();
const directRuntime = () => globalThis.__SUPERCOV_DIRECT_RUNTIME__;
globalThis.__SUPERCOV_ASSERTION_PHASE_BRIDGE__ = (operation, source, callback) => {
    const controller = activeController;
    const runtime = directRuntime();
    if (!controller || !runtime)
        return { handled: false };
    const phase = controller.beginAssertion(operation, source);
    bridgedAssertionDepth += 1;
    try {
        const value = runtime.withCoverageCarrier({ version: 1, scope: controller.scope, phaseId: phase.id }, callback);
        bridgedAssertionDepth -= 1;
        if (value &&
            typeof value.then === "function")
            return {
                handled: true,
                value: Promise.resolve(value).then((resolved) => {
                    controller.finish(phase);
                    return resolved;
                }, (error) => {
                    controller.finish(phase, error);
                    throw error;
                }),
            };
        controller.finish(phase);
        return { handled: true, value };
    }
    catch (error) {
        bridgedAssertionDepth -= 1;
        controller.finish(phase, error);
        throw error;
    }
};
function activeCoverageHeaders() {
    const controller = activeController;
    if (!controller)
        return undefined;
    const phaseId = controller.requestPhaseId();
    return {
        [COVERAGE_SCOPE_HEADER]: encodeCoverageScope(controller.scope),
        ...(phaseId ? { [COVERAGE_PHASE_HEADER]: phaseId } : {}),
    };
}
function mergeRequestHeaders(existing, coverage) {
    if (existing instanceof Headers) {
        return { ...Object.fromEntries(existing.entries()), ...coverage };
    }
    if (Array.isArray(existing)) {
        const normalized = {};
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
function scopedNodeRequestArguments(args) {
    const coverage = activeCoverageHeaders();
    if (!coverage || args.length === 0)
        return args;
    const scoped = [...args];
    const first = scoped[0];
    const startsWithUrl = typeof first === "string" || first instanceof URL;
    const candidate = startsWithUrl ? scoped[1] : first;
    const hasOptions = candidate !== null &&
        typeof candidate === "object" &&
        !(candidate instanceof URL);
    if (hasOptions) {
        const index = startsWithUrl ? 1 : 0;
        const options = candidate;
        scoped[index] = {
            ...options,
            headers: mergeRequestHeaders(options.headers, coverage),
        };
    }
    else if (startsWithUrl) {
        scoped.splice(1, 0, { headers: coverage });
    }
    return scoped;
}
function installNodeRequestScopePropagation() {
    const originalHttpRequest = http.request;
    const originalHttpGet = http.get;
    const originalHttpsRequest = https.request;
    const originalHttpsGet = https.get;
    http.request = ((...args) => Reflect.apply(originalHttpRequest, http, scopedNodeRequestArguments(args)));
    http.get = ((...args) => Reflect.apply(originalHttpGet, http, scopedNodeRequestArguments(args)));
    https.request = ((...args) => Reflect.apply(originalHttpsRequest, https, scopedNodeRequestArguments(args)));
    https.get = ((...args) => Reflect.apply(originalHttpsGet, https, scopedNodeRequestArguments(args)));
    // Existing ESM named imports such as `import { request } from "node:http"`
    // must observe the patched CommonJS-compatible builtin exports too.
    syncBuiltinESMExports();
    if (typeof globalThis.fetch === "function") {
        const originalFetch = globalThis.fetch.bind(globalThis);
        globalThis.fetch = ((input, init) => {
            const coverage = activeCoverageHeaders();
            if (!coverage)
                return originalFetch(input, init);
            const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
            for (const [name, value] of Object.entries(coverage))
                headers.set(name, value);
            return originalFetch(input, { ...init, headers });
        });
    }
}
installNodeRequestScopePropagation();
function scopedChildOptions(args, optionIndex) {
    const controller = activeController;
    if (!controller)
        return args;
    const scoped = [...args];
    const existing = scoped[optionIndex] && typeof scoped[optionIndex] === "object"
        ? scoped[optionIndex]
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
    else
        scoped[optionIndex] = options;
    return scoped;
}
function childOptionIndex(method, args) {
    if (method === "spawn" || method === "spawnSync" || method === "fork")
        return Array.isArray(args[1]) ? 2 : 1;
    if (method === "execFile" || method === "execFileSync")
        return Array.isArray(args[1]) ? 2 : 1;
    return 1;
}
function installChildProcessScopePropagation() {
    for (const method of [
        "exec",
        "execFile",
        "execFileSync",
        "execSync",
        "fork",
        "spawn",
        "spawnSync",
    ]) {
        const original = childProcess[method];
        childProcess[method] = function (...args) {
            return Reflect.apply(original, childProcess, scopedChildOptions(args, childOptionIndex(method, args)));
        };
    }
    syncBuiltinESMExports();
}
installChildProcessScopePropagation();
function wrapMatchers(matchers, path = "expect") {
    return new Proxy(matchers, {
        get(target, property) {
            // Matcher objects and built-ins can expose getters whose internal slots
            // require the original receiver (for example RegExp.prototype.dotAll).
            // Passing this proxy as the receiver changes observable behaviour.
            const value = Reflect.get(target, property, target);
            const name = String(property);
            if (value &&
                typeof value === "object" &&
                (name === "not" || name === "resolves" || name === "rejects"))
                return wrapMatchers(value, `${path}.${name}`);
            if (typeof value !== "function")
                return value;
            return (...args) => {
                const controller = activeController;
                const phase = bridgedAssertionDepth === 0
                    ? controller?.beginAssertion(`${path}.${name}`)
                    : undefined;
                try {
                    const result = Reflect.apply(value, target, args);
                    if (result instanceof Promise) {
                        return result.then((resolved) => {
                            if (phase && controller)
                                controller.finish(phase);
                            return resolved;
                        }, (error) => {
                            if (phase && controller)
                                controller.finish(phase, error);
                            throw error;
                        });
                    }
                    if (phase && controller)
                        controller.finish(phase);
                    return result;
                }
                catch (error) {
                    if (phase && controller)
                        controller.finish(phase, error);
                    throw error;
                }
            };
        },
    });
}
function wrapExpectCallable(callable) {
    return new Proxy(callable, {
        apply(target, thisArg, argumentsList) {
            const result = Reflect.apply(target, thisArg, argumentsList);
            if (typeof result === "function")
                return wrapExpectCallable(result);
            if (result && typeof result === "object")
                return wrapMatchers(result);
            return result;
        },
        get(target, property, receiver) {
            const value = Reflect.get(target, property, receiver);
            if (typeof value !== "function")
                return value;
            return wrapExpectCallable(value.bind(target));
        },
    });
}
export const expect = wrapExpectCallable(baseExpect);
function currentRunId() {
    return (process.env["SUPERCOV_RUN_ID"] ??
        (GENERATED_RUN_ID.startsWith("__") ? "unscoped" : GENERATED_RUN_ID));
}
function executionScope(testInfo) {
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
function readServerRecords(scope) {
    try {
        return readFileSync(serverEvidencePath(scope), "utf8")
            .split("\n")
            .filter(Boolean)
            .flatMap((line) => {
            try {
                const record = JSON.parse(line);
                return record.scope?.attemptId === scope.attemptId ? [record] : [];
            }
            catch {
                // Ignore a final partial line if the server was writing during collection.
                return [];
            }
        });
    }
    catch {
        return [];
    }
}
function phaseSourceLine(source) {
    return source?.replace(/:\d+$/, "");
}
/**
 * Playwright's redirected `expect` is the preferred assertion boundary, but
 * its transform loader can bypass Node's resolve hook for project-owned
 * fixture modules. The ahead-of-run frontend therefore keeps a lexical
 * fallback. If both paths observe the same assertion, retain the richer
 * Playwright phase (including action causality) exactly once.
 */
function mergeCollectedPhases(controllerPhases, fallbackPhases) {
    const observed = new Map();
    for (const phase of controllerPhases) {
        if (phase.kind !== "assertion")
            continue;
        const key = `${phase.operation}\0${phaseSourceLine(phase.source) ?? ""}`;
        observed.set(key, (observed.get(key) ?? 0) + 1);
    }
    const remaining = fallbackPhases.filter((phase) => {
        if (phase.kind !== "assertion")
            return true;
        const key = `${phase.operation}\0${phaseSourceLine(phase.source) ?? ""}`;
        const count = observed.get(key) ?? 0;
        if (count === 0)
            return true;
        observed.set(key, count - 1);
        return false;
    });
    return [...controllerPhases, ...remaining];
}
const instrumentedTest = base.extend({
    page: async ({ page }, use, testInfo) => {
        const scope = executionScope(testInfo);
        const controller = controllers.get(scope.attemptId);
        if (!controller)
            throw new Error(`Supercov's automatic Playwright collector was not initialized for ${scope.attemptId}`);
        await controller.registerPage(page);
        try {
            await use(controller.wrap(page));
        }
        finally {
            // This fixture still owns a live page. The automatic test fixture tears
            // down after page dependencies and therefore cannot reliably evaluate
            // frames on every Playwright version.
            await controller.collectRuntimeSnapshots();
        }
    },
    browser: [
        async ({ browser }, use) => {
            await use(new Proxy(browser, {
                get(target, property, receiver) {
                    const controller = activeController;
                    return controller
                        ? Reflect.get(controller.wrap(target), property, receiver)
                        : Reflect.get(target, property, receiver);
                },
            }));
        },
        { scope: "worker" },
    ],
    request: async ({ request }, use) => {
        await use(new Proxy(request, {
            get(target, property, receiver) {
                const controller = activeController;
                return controller
                    ? Reflect.get(controller.wrap(target), property, receiver)
                    : Reflect.get(target, property, receiver);
            },
        }));
    },
    mcdcAutoCollect: [
        async ({}, use, testInfo) => {
            const scope = executionScope(testInfo);
            const configuredHeaders = Object.fromEntries(Object.entries(testInfo.project.use.extraHTTPHeaders ?? {}).map(([name, value]) => [name, String(value)]));
            const controller = new CoveragePhaseController(scope, configuredHeaders);
            controllers.set(scope.attemptId, controller);
            activeController = controller;
            directRuntime()?.activateCoverageScope(scope);
            const serverOutput = serverEvidencePath(scope);
            mkdirSync(serverEvidenceDirectory(scope), { recursive: true });
            rmSync(serverOutput, { force: true });
            try {
                await use();
            }
            finally {
                try {
                    const browser = await controller.collectRuntimeSnapshots();
                    const server = readServerRecords(scope);
                    // Emit an artifact even when this test touched no application source.
                    // A complete test-to-coverage matrix must also identify tests that are
                    // removable without changing coverage.
                    const outputPath = testInfo.outputPath("mcdc.json");
                    mkdirSync(dirname(outputPath), { recursive: true });
                    const testFile = relative(process.cwd(), testInfo.file)
                        .split(sep)
                        .join("/");
                    const payload = {
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
                        phases: mergeCollectedPhases(controller.phases, directRuntime()?.takeNodeAssertionPhases(scope) ?? []),
                        browser,
                        server,
                    };
                    const serialized = `${JSON.stringify(payload)}\n`;
                    atomicWriteFileSync(outputPath, serialized);
                    // Pool runners may cycle-restore a VM immediately after Playwright
                    // exits, which can discard or overwrite the normal artifact copy.
                    // Write one uniquely named, one-shot evidence file to the runner's
                    // shared directory as well. No test streams into this path.
                    const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"] ??
                        (GENERATED_EVIDENCE_DIRECTORY.startsWith("__")
                            ? undefined
                            : GENERATED_EVIDENCE_DIRECTORY);
                    if (evidenceDirectory) {
                        const resolvedDirectory = resolve(process.cwd(), evidenceDirectory);
                        const safeTestId = testInfo.testId.replace(/[^a-zA-Z0-9_-]/g, "_");
                        const testEvidenceDirectory = resolve(resolvedDirectory, `${safeTestId}-${testInfo.retry}`);
                        mkdirSync(testEvidenceDirectory, { recursive: true });
                        atomicWriteFileSync(resolve(testEvidenceDirectory, "mcdc.json"), serialized);
                    }
                }
                finally {
                    await controller.dispose();
                    directRuntime()?.activateCoverageScope();
                    if (activeController === controller)
                        activeController = undefined;
                    controllers.delete(scope.attemptId);
                }
            }
        },
        { auto: true },
    ],
});
export const test = instrumentedTest;
/*__SUPERCOV_ADAPTER_EXPORTS__*/
