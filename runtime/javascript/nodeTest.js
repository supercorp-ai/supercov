import * as native from "node:test";
import { beginBufferedServerEvidence, flushBufferedServerEvidence, takeNodeAssertionPhases, withCoverageCarrier, } from "./runtime.js";
import { callerLocation, runnerExecutionScope, writeRunnerEvidence, } from "./runnerEvidence.js";
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
                        if (error)
                            status = "failed";
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
                        throw error;
                    });
                emit();
                return result;
            }
            catch (error) {
                status = "failed";
                emit();
                throw error;
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
export const test = wrappedRegistration(native.test);
export const it = wrappedRegistration(native.it);
export const suite = native.suite;
export const describe = native.describe;
export const before = native.before;
export const after = native.after;
export const beforeEach = native.beforeEach;
export const afterEach = native.afterEach;
export const mock = native.mock;
export const snapshot = native.snapshot;
export const run = native.run;
export const assert = native.assert;
export default test;
