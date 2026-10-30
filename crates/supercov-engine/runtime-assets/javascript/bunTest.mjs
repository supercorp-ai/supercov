// Bun does not load the Node --import preload. BUN_OPTIONS loads this before
// its test files and their configured preloads. Older Bun versions create a
// native API per file; newer versions share one. Adapt the factory and the
// existing API so both keep native test registration and execution semantics.
import * as runtime from "./runtime.mjs";
import { callerLocation, runnerExecutionScope, runnerTestId, writeRunnerEvidence } from "./runnerEvidence.mjs";

const adapted = new WeakSet();
const nativeJest = Bun.jest;
const attempts = new Map();
function writeEvidence(identity, status, scope, directory, phases = [], source) {
    return writeRunnerEvidence(identity, status, scope, directory, phases, source, true);
}
const previousPhaseObserver = globalThis.__SUPERCOV_ASSERTION_PHASE_STARTED__;
globalThis.__SUPERCOV_ASSERTION_PHASE_STARTED__ = (scope, phase) => {
    previousPhaseObserver?.(scope, phase);
    const attempt = attempts.get(scope.attemptId);
    if (attempt) {
        attempt.phases.push(phase);
        writeEvidence(attempt.identity, "unknown", scope, undefined, attempt.phases);
    }
};
function adapt(api) {
    if (adapted.has(api)) return api;
    adapted.add(api);
    const originals = { ...api };
    let suites = [];
    const registrations = new Map();
    const evidenceDirectory = process.env.SUPERCOV_EVIDENCE_DIR;

    function callbackIndex(args) {
        return args.findLastIndex(value => typeof value === "function");
    }

    function wrap(original, kind, flags = {}, row = {}) {
        const members = new Map();
        function supercovBunRegistration(...args) {
            const index = callbackIndex(args);
            const callback = index < 0 ? undefined : args[index];
            const name = row.name ?? (typeof args[0] === "string" ? args[0] : callback?.name || "anonymous test");
            const location = row.location ?? callerLocation(supercovBunRegistration);
            if (kind === "suite") {
                if (!callback) return Reflect.apply(original, this, args);
                const next = [...args];
                next[index] = function () {
                    const values = [...arguments];
                    const parent = suites;
                    suites = [...parent, { name, flags }];
                    try { return Reflect.apply(callback, this, values); }
                    finally { suites = parent; }
                };
                return Reflect.apply(original, this, next);
            }
            const identity = {
                runner: "bun:test",
                name: kind === "test" ? [...suites.map(suite => suite.name), name].join(" > ") : `[${kind}] ${name}`,
                ...location,
                ...(location.line === undefined ? {} : { column: 1 }),
                ...(kind === "test" ? {} : { role: "setup" }),
            };
            const key = runnerTestId(identity);
            identity.registrationOrdinal = registrations.get(key) ?? 0;
            registrations.set(key, identity.registrationOrdinal + 1);
            const effective = {};
            for (const active of [...suites.map(suite => suite.flags), flags]) {
                for (const [key, value] of Object.entries(active)) effective[key] ||= value;
            }
            if (!callback) {
                const result = Reflect.apply(original, this, args);
                writeEvidence(identity, effective.todo ? "todo" : "skipped", runnerExecutionScope(identity), evidenceDirectory);
                return result;
            }
            let attempt = 0;
            const next = [...args];
            next[index] = function () {
                const values = [...arguments];
                const current = { ...identity, retry: attempt++ };
                const scope = runnerExecutionScope(current);
                // Bun may terminate a timed-out callback without running its
                // finally or Node exit hooks. Persist identity before execution,
                // and use the runtime's per-turn transport instead of buffering
                // the whole attempt until callback completion.
                attempts.set(scope.attemptId, { identity: current, phases: [] });
                writeEvidence(current, "unknown", scope, evidenceDirectory);
                let finished = false;
                function finish(failed = false) {
                    if (finished) return;
                    finished = true;
                    attempts.delete(scope.attemptId);
                    const status = effective.todo ? "todo" : failed === Boolean(effective.failing) ? "passed" : "failed";
                    writeEvidence(current, status, scope, evidenceDirectory, runtime.takeNodeAssertionPhases(scope));
                }
                const actual = [...values];
                const bound = row.bound ?? 0;
                if (callback.length > bound && typeof actual[bound] === "function") {
                    const done = actual[bound];
                    actual[bound] = error => { finish(error != null); return done(error); };
                }
                try {
                    const result = runtime.withCoverageCarrier({ version: 1, scope }, () => Reflect.apply(callback, this, actual));
                    if (result && typeof result.then === "function") {
                        return Promise.resolve(result).then(value => { finish(); return value; }, error => { finish(true); throw error; });
                    }
                    if (callback.length <= bound) finish();
                    return result;
                } catch (error) {
                    finish(true);
                    throw error;
                }
            };
            Object.defineProperty(next[index], "length", { value: callback.length });
            const result = Reflect.apply(original, this, next);
            if (effective.skip || effective.todo) {
                writeEvidence(identity, effective.todo ? "todo" : "skipped", runnerExecutionScope(identity), evidenceDirectory);
            }
            return result;
        }
        return new Proxy(supercovBunRegistration, {
            get(target, property) {
                if (["call", "apply", "bind"].includes(property)) return Reflect.get(target, property);

                if (members.has(property)) return members.get(property);
                const member = Reflect.get(original, property, original);
                if (typeof member !== "function") return member;
                if (property === "each") {
                    const each = table => {
                        const nativeEach = Reflect.apply(member, original, [table]);
                        if (!Array.isArray(table) || table.length === 0) return nativeEach;
                        return function supercovBunEach(...args) {
                            const location = callerLocation(supercovBunEach);
                            for (const [index, value] of table.entries()) {
                                const bound = Array.isArray(value) ? value.length : 1;
                                const next = [...args];
                                // Keep Bun's own formatting and binding. Only %# needs
                                // the original index when registering one row at a time.
                                let consumed = 0;
                                if (typeof next[0] === "string") next[0] = next[0].replace(/%([sidfjop#%])/g, (token, format) => {
                                    if (consumed >= bound) return token;
                                    if (format === "#") return String(index);
                                    if (format !== "%") consumed++;
                                    return token;
                                });
                                const register = Reflect.apply(member, original, [[value]]);
                                Reflect.apply(wrap(register, kind, flags, {
                                    location, bound,
                                    name: `${args[0]} [row ${index + 1}]`,
                                }), this, next);
                            }
                        };
                    };
                    members.set(property, each);
                    return each;
                }
                if (["skip", "todo", "only", "concurrent", "serial", "failing"].includes(property)) {
                    const wrapped = wrap(member, kind, { ...flags, [property]: true });
                    members.set(property, wrapped);
                    return wrapped;
                }
                if (["if", "skipIf", "todoIf", "failingIf", "concurrentIf", "serialIf"].includes(property)) {
                    return (...values) => wrap(Reflect.apply(member, original, values), kind, {
                        ...flags,
                        [property === "if" ? "skip" : property.slice(0, -2)]: flags[property === "if" ? "skip" : property.slice(0, -2)] || (property === "if" ? !values[0] : Boolean(values[0])),
                    });
                }
                return member.bind(original);
            },
        });
    }

    api.test = api.it = wrap(originals.test, "test");
    api.describe = wrap(originals.describe, "suite");
    for (const [name, kind, flags] of [
        ["xtest", "test", { skip: true }], ["xit", "test", { skip: true }],
        ["xdescribe", "suite", { skip: true }],
        ["beforeAll", "beforeAll", {}], ["afterAll", "afterAll", {}],
        ["beforeEach", "beforeEach", {}], ["afterEach", "afterEach", {}],
    ]) {
        if (typeof originals[name] === "function") api[name] = wrap(originals[name], kind, flags);
    }
    return api;
}
if (typeof nativeJest === "function") {
    Bun.jest = function (...args) { return adapt(Reflect.apply(nativeJest, this, args)); };
    adapt(Reflect.apply(nativeJest, Bun, [import.meta.path]));
}
