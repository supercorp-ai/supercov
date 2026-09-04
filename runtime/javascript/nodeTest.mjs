import * as native from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { beginBufferedServerEvidence, flushBufferedServerEvidence, takeNodeAssertionPhases, withCoverageCarrier, } from "./runtime.mjs";
import { callerLocation, runnerExecutionScope, writeRunnerEvidence, } from "./runnerEvidence.mjs";
function callbackIndex(args) {
    for (let index = args.length - 1; index >= 0; index -= 1)
        if (typeof args[index] === "function")
            return index;
    return -1;
}
function testName(args, callback) {
    return typeof args[0] === "string" ? args[0] : callback.name || "anonymous test";
}
function testOptions(args, index) {
    const candidate = args.slice(0, index).find((value) => value && typeof value === "object" && !Array.isArray(value));
    return candidate;
}
// node:test derives a test's reported location from the direct caller of the
// registration call, and the adapter is that caller: every failing test
// reported "test at .../nodeTest.mjs". A compiled trampoline carrying the
// user's call site as its script origin registers the test instead, so the
// runner sees the location a direct call would have produced. The padded
// second line puts the call expression at the user's exact line and column.
const registrationSites = new Map();
function registrationAt(location) {
    if (!location.file ||
        location.line === undefined ||
        location.column === undefined ||
        location.line < 2)
        return undefined;
    const key = `${location.file}:${location.line}:${location.column}`;
    let site = registrationSites.get(key);
    if (site === undefined) {
        try {
            const filename = location.file.startsWith("file://")
                ? fileURLToPath(location.file)
                : location.file;
            site = vm.compileFunction(`return (\n${" ".repeat(Math.max(0, location.column - 1))}original(...args));`, ["original", "args"], { filename, lineOffset: location.line - 2 });
        }
        catch {
            site = null;
        }
        registrationSites.set(key, site);
    }
    return site ?? undefined;
}
// A failing test's stack was captured while Supercov's wrappers were on the
// call path and while the code ran inside the mirrored workspace. Neither is
// part of the user's program: drop adapter frames and map workspace paths
// back to the source project so the report matches an uninstrumented run.
const restoredErrors = new WeakSet();
function restoreUserError(error, depth = 0) {
    if (depth > 4 ||
        !error ||
        typeof error !== "object" ||
        restoredErrors.has(error))
        return error;
    restoredErrors.add(error);
    const workspaceRoot = process.env["SUPERCOV_PROJECT_ROOT"];
    const sourceRoot = process.env["SUPERCOV_SOURCE_PROJECT_ROOT"];
    const remap = (text) => workspaceRoot && sourceRoot && workspaceRoot !== sourceRoot
        ? text.split(workspaceRoot).join(sourceRoot)
        : text;
    try {
        if (typeof error.stack === "string")
            error.stack = remap(error.stack
                .split("\n")
                .filter((line) => !(line.trimStart().startsWith("at ") &&
                line.includes("/.supercov/")))
                .join("\n"));
        if (typeof error.message === "string")
            error.message = remap(error.message);
    }
    catch {
        // Frozen or accessor-backed errors keep their original form.
    }
    try {
        restoreUserError(error.cause, depth + 1);
        if (Array.isArray(error.errors))
            for (const aggregated of error.errors)
                restoreUserError(aggregated, depth + 1);
    }
    catch {
        // A throwing accessor never replaces the user's error.
    }
    return error;
}
function wrappedRegistration(original) {
    const wrapped = function supercovNodeTest(...args) {
        const index = callbackIndex(args);
        if (index < 0)
            return Reflect.apply(original, this, args);
        const callback = args[index];
        const location = callerLocation(/(?:nodeTest|runnerEvidence)\.[cm]?[jt]s/);
        const identity = {
            runner: "node:test",
            name: testName(args, callback),
            ...location,
            // Source-map producers disagree about whether a call expression maps to
            // its first token or the first token on its source line. The line and
            // dynamic test name are stable across ahead-of-run and build-tool
            // transforms; the mapped column is not. Canonicalize it so the same test
            // keeps one identity when esbuild, Babel, SWC, or TypeScript rewrites it.
            ...(location.line === undefined ? {} : { column: 1 }),
        };
        const scope = runnerExecutionScope(identity);
        const options = testOptions(args, index);
        const evidenceDirectory = process.env["SUPERCOV_EVIDENCE_DIR"];
        const next = [...args];
        const execute = (callbackThis, context, done) => {
            // A test or its hooks may intentionally modify Supercov's public
            // environment while testing integrations. Keep this attempt's transport
            // destination fixed to the value present when the test was registered.
            beginBufferedServerEvidence(scope);
            let status = options?.skip || options?.todo
                ? "skipped"
                : "passed";
            const contextProxy = new Proxy(context, {
                get(target, property) {
                    // Read with the real context as receiver: TestContext accessors
                    // (t.assert, t.mock, ...) touch private fields that only exist on
                    // the target, never on the proxy.
                    const value = Reflect.get(target, property, target);
                    if ((property === "skip" || property === "todo") && typeof value === "function") {
                        return (...callArgs) => {
                            status = "skipped";
                            return Reflect.apply(value, target, callArgs);
                        };
                    }
                    if (property === "test" && typeof value === "function")
                        return wrappedRegistration(value.bind(target));
                    return typeof value === "function" ? value.bind(target) : value;
                },
            });
            let emitted = false;
            const emit = (nextStatus = status) => {
                if (emitted)
                    return;
                emitted = true;
                const flushedServerEvidence = flushBufferedServerEvidence(scope);
                writeRunnerEvidence(identity, nextStatus, scope, evidenceDirectory, takeNodeAssertionPhases(scope), flushedServerEvidence);
            };
            try {
                if (callback.length >= 2) {
                    const callbackDone = (error) => {
                        if (error) {
                            status = "failed";
                            restoreUserError(error);
                        }
                        emit();
                        done?.(error);
                    };
                    return withCoverageCarrier({ version: 1, scope }, () => Reflect.apply(callback, callbackThis, [contextProxy, callbackDone]));
                }
                const result = withCoverageCarrier({ version: 1, scope }, () => Reflect.apply(callback, callbackThis, [contextProxy]));
                if (result && typeof result.then === "function")
                    return Promise.resolve(result).then((value) => {
                        emit();
                        return value;
                    }, (error) => {
                        status = "failed";
                        emit();
                        throw restoreUserError(error);
                    });
                emit();
                return result;
            }
            catch (error) {
                status = "failed";
                emit();
                throw restoreUserError(error);
            }
        };
        // node:test uses callback arity to distinguish promise/synchronous tests
        // from the legacy done-callback form. Preserve it exactly.
        next[index] = callback.length >= 2
            ? function supercovNodeTestDoneCallback(context, done) {
                return execute(this, context, done);
            }
            : function supercovNodeTestCallback(context) {
                return execute(this, context);
            };
        const registration = registrationAt(location);
        if (registration)
            return registration(this === undefined ? original : original.bind(this), next);
        return Reflect.apply(original, this, next);
    };
    for (const property of ["skip", "todo", "only"]) {
        const member = original[property];
        if (typeof member === "function")
            Object.defineProperty(wrapped, property, {
                configurable: true,
                enumerable: true,
                value: wrappedRegistration(member),
            });
    }
    return wrapped;
}
// Hooks carry no coverage evidence, but an error thrown inside one still
// reaches the report with the adapter's execution context in its stack.
// Restore it at the same boundary the test wrapper uses.
function restoringHook(original) {
    return function supercovNodeTestHook(...args) {
        const index = callbackIndex(args);
        if (index < 0)
            return Reflect.apply(original, this, args);
        const callback = args[index];
        const next = [...args];
        // node:test uses callback arity to distinguish promise/synchronous
        // hooks from the legacy done-callback form. Preserve it exactly.
        next[index] = callback.length >= 2
            ? function supercovNodeTestHookDoneCallback(context, done) {
                const restoringDone = (error) => {
                    if (error)
                        restoreUserError(error);
                    done?.(error);
                };
                return callback.call(this, context, restoringDone);
            }
            : function supercovNodeTestHookCallback(context) {
                try {
                    const result = callback.call(this, context);
                    if (result && typeof result.then === "function")
                        return Promise.resolve(result).then(undefined, (error) => {
                            throw restoreUserError(error);
                        });
                    return result;
                }
                catch (error) {
                    throw restoreUserError(error);
                }
            };
        return Reflect.apply(original, this, next);
    };
}
export const test = wrappedRegistration(native.test);
export const it = wrappedRegistration(native.it);
export const suite = native.suite;
export const describe = native.describe;
export const before = restoringHook(native.before);
export const after = restoringHook(native.after);
export const beforeEach = restoringHook(native.beforeEach);
export const afterEach = restoringHook(native.afterEach);
export const mock = native.mock;
export const snapshot = native.snapshot;
export const run = native.run;
export const assert = native.assert;
export default test;
