// Supercov Mocha adapter. The preload imports it in every Mocha process --
// the CLI, the child the CLI forks for Node options, and each --parallel
// worker -- and it patches Mocha's own Runner, so a suite runs exactly as
// written: no reporter, --require or configuration is added. Each test
// attempt gets its exact scope when Mocha announces it, before its
// beforeEach hooks, and its coverage, assertion phases and outcome are
// written after its afterEach hooks. Code that runs outside any test --
// loading the test files, before/after all hooks -- is written as setup. It
// computes no coverage.
import { realpathSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";
import { appendEvidenceRecord } from "./atomic.mjs";
import { inferTestProvenance, testTags } from "./provenance.mjs";
import { runnerExecutionScope, runnerTestId } from "./runnerEvidence.mjs";
import { strongJob } from "./strongJob.mjs";

const SETUP_HOOK = /^"(?:before|after) all" hook/;

function localFile(path) {
    return path ? relative(process.cwd(), path).split(sep).join("/") : undefined;
}

// The installed mocha package the entrypoint belongs to.
function mochaRoot(entrypoint) {
    let real;
    try {
        real = realpathSync(entrypoint).replaceAll("\\", "/");
    }
    catch {
        return undefined;
    }
    const at = real.lastIndexOf("/node_modules/mocha/");
    return at < 0 ? undefined : real.slice(0, at + "/node_modules/mocha".length);
}

export async function adaptMocha(entrypoint) {
    const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__ ?? process.__SUPERCOV_DIRECT_RUNTIME__;
    const evidenceDirectory = process.env.SUPERCOV_EVIDENCE_DIR;
    const root = mochaRoot(entrypoint);
    if (!runtime || !evidenceDirectory || !root)
        return;
    // Mocha 12 is ESM and exports { Runner }; 10 and 11 are CommonJS and
    // export the class itself. Either way this is the instance Mocha uses.
    let loaded;
    try {
        loaded = await import(pathToFileURL(join(root, "lib", "runner.js")).href);
    }
    catch {
        return;
    }
    const Runner = loaded.Runner ?? loaded.default?.Runner ?? loaded.default;
    const run = Runner?.prototype?.run;
    if (typeof run !== "function" || run.supercovMocha)
        return;
    // One snapshot per test, as under Jest; the server transport would only
    // repeat it, unattributed.
    runtime.enableRuntimeSnapshotEvidence();
    // Read as each record is written: a resident Mocha (strongMochaHost.mjs)
    // writes each job's into the job's own directory.
    const write = (payload) => appendEvidenceRecord(resolve(process.cwd(), process.env.SUPERCOV_EVIDENCE_DIR ?? evidenceDirectory), "mocha", payload);
    // A --parallel parent overrides run() and never reaches this: its workers
    // run the tests, each with a Runner of its own.
    Runner.prototype.run = function supercovMochaRun(...args) {
        const job = strongJob();
        // --check-leaks fails a test that adds a global, and Supercov's
        // runtime keeps its state on globals it may first set during a test
        // (the test's id, an async scope). They are Supercov's, not the
        // suite's: they are allowed, as a project's own `--global` would be.
        if (typeof this.globals === "function")
            this.globals(["__SUPERCOV_*"]);
        observe(this, runtime, write);
        if (job) {
            keepJobTests(this.suite, job);
            // A failure settles the job's mutant: the rest need not run.
            this.on("fail", (runnable) => {
                job.fail();
                if (job.stopped && typeof this.abort === "function")
                    this.abort();
            });
        }
        return Reflect.apply(run, this, args);
    };
    Runner.prototype.run.supercovMocha = true;
    // A strong pass's job loads only its own test file, of the run's.
    if (strongJob())
        await keepJobFiles(root);
    // Mocha takes its working directory off the front of each frame and
    // prints the rest. Under Supercov that is the copy, while an application
    // frame names the project's own file, so it printed whole:
    // `at seal (/home/me/shop/src/crypto.js:3:11)` for `src/crypto.js:3:11`.
    // And Supercov's hooks into asynchronous work put a frame of Node's under
    // every stack that starts in a timer. A failure reads as it does without
    // Supercov before Mocha or a reporter sees it.
    const fail = Runner.prototype.fail;
    if (typeof fail === "function") {
        const place = asWritten();
        Runner.prototype.fail = function supercovMochaFail(runnable, error, ...rest) {
            place(error);
            return Reflect.apply(fail, this, [runnable, error, ...rest]);
        };
    }
}

// Mocha's test ids, as `observe` gives them, in the order Mocha runs the
// tests (a suite's own tests, then its suites'); a strong pass's job removes
// every test it does not run, and a suite left with none runs no hook.
function keepJobTests(root, job) {
    const occurrences = new Map();
    const visit = (suite) => {
        suite.tests = suite.tests.filter((test) => {
            const name = test.titlePath().join(" > ");
            const key = `${test.file ?? ""}\0${name}`;
            const occurrence = occurrences.get(key) ?? 0;
            occurrences.set(key, occurrence + 1);
            test.supercovOccurrence = occurrence;
            const testId = runnerTestId({
                runner: "mocha",
                file: test.file,
                name,
                ...(occurrence ? { registrationOrdinal: occurrence } : {}),
            });
            return job.selects(test.file, testId);
        });
        suite.suites.forEach(visit);
    };
    visit(root);
}

// Leaves out the run's other test files before Mocha loads them; under
// --parallel, before it hands them to its workers.
async function keepJobFiles(root) {
    let loaded;
    try {
        loaded = await import(pathToFileURL(join(root, "lib", "mocha.js")).href);
    }
    catch {
        return;
    }
    const Mocha = loaded.Mocha ?? loaded.default?.Mocha ?? loaded.default;
    const prototype = Mocha?.prototype;
    if (!prototype || prototype.supercovJobFiles)
        return;
    prototype.supercovJobFiles = true;
    for (const name of ["loadFiles", "loadFilesAsync", "run"]) {
        const original = prototype[name];
        if (typeof original !== "function")
            continue;
        prototype[name] = function supercovJobFiles(...args) {
            const job = strongJob();
            if (job && Array.isArray(this.files))
                this.files = this.files.filter((file) => job.runsFile(file));
            return Reflect.apply(original, this, args);
        };
    }
}

const HOOK_FRAME = /^\s*at process\.(?:callbackTrampoline|topLevelDomainCallback) \(node:(?:internal\/async_hooks|domain):\d+:\d+\)$/;

function asWritten() {
    const real = (directory) => {
        try {
            return directory ? realpathSync(directory) : undefined;
        }
        catch {
            return undefined;
        }
    };
    const project = real(process.env.SUPERCOV_SOURCE_PROJECT_ROOT);
    const copy = real(process.env.SUPERCOV_PROJECT_ROOT);
    return (error) => {
        if (!error || typeof error.stack !== "string")
            return;
        const lines = error.stack.split("\n").filter((line) => !HOOK_FRAME.test(line));
        const placed = project && copy && project !== copy
            ? lines.map((line) => /^\s*at /.test(line) && line.includes(project + sep) && !line.includes(copy + sep)
                ? line.replace(`(${project}${sep}`, `(${copy}${sep}`).replace(` at ${project}${sep}`, ` at ${copy}${sep}`)
                : line)
            : lines;
        try {
            error.stack = placed.join("\n");
        }
        catch {
            // A stack that cannot be written is printed as it is.
        }
    };
}

function observe(runner, runtime, write) {
    // Tests that share a file and a full title are told apart by the order in
    // which their first attempts ran; a retry keeps its test's identity.
    const firstAttempts = new Map();
    const identities = new WeakMap();
    let active;
    let setups = 0;
    function identity(test) {
        const known = identities.get(test);
        if (known)
            return known;
        const file = test.file;
        const name = test.titlePath().join(" > ");
        const key = `${file ?? ""}\0${name}`;
        let occurrence = firstAttempts.get(key) ?? 0;
        if (test.currentRetry() > 0)
            occurrence = Math.max(occurrence - 1, 0);
        else
            firstAttempts.set(key, occurrence + 1);
        // A strong pass's job numbered its tests before it left some out.
        if (test.supercovOccurrence !== undefined)
            occurrence = test.supercovOccurrence;
        const value = {
            runner: "mocha",
            file,
            name,
            ...(occurrence ? { registrationOrdinal: occurrence } : {}),
            retry: test.currentRetry(),
        };
        identities.set(test, value);
        return value;
    }
    function record(id, status, scope, snapshot) {
        const testFile = localFile(id.file);
        write({
            testId: scope ? scope.testId : runnerExecutionScope(id).testId,
            ...(scope ? { scope } : {}),
            test: id.name,
            ...(testFile ? { testFile } : {}),
            title: id.name.split(" > ").at(-1) ?? id.name,
            retry: id.retry ?? 0,
            status,
            ...(id.role ? { role: id.role } : {}),
            provenance: inferTestProvenance({
                runner: "mocha",
                file: testFile,
                explicitKind: process.env.SUPERCOV_TEST_KIND,
                tags: id.role ? [] : testTags(undefined, [id.name]),
            }),
            ...(scope ? { phases: runtime.takeNodeAssertionPhases(scope) } : {}),
            runtime: snapshot ? [snapshot] : [],
            browser: [],
            server: [],
        });
    }
    // What ran since the last test or setup ended, outside any test.
    function flushSetup(name) {
        const snapshot = runtime.coverageSnapshot();
        runtime.resetCoverage();
        if (!snapshot.hits.length && !snapshot.decisions.length)
            return;
        setups += 1;
        const id = { runner: "mocha", name, role: "setup", registrationOrdinal: setups };
        record(id, "passed", runnerExecutionScope(id), snapshot);
    }
    function flushActive() {
        const current = active;
        active = undefined;
        if (!current)
            return;
        record(current.id, current.status, current.scope, runtime.coverageSnapshot());
        runtime.resetCoverage();
        runtime.activateCoverageScope();
    }
    runner.on("start", () => flushSetup("module setup"));
    runner.on("test", (test) => {
        flushActive();
        flushSetup(`${test.parent?.fullTitle() || "root suite"} > hooks`);
        const id = identity(test);
        const scope = runnerExecutionScope(id);
        active = { test, id, scope, status: "unknown" };
        runtime.activateCoverageScope(scope);
        runtime.resetCoverage(scope.testId);
    });
    runner.on("pass", (test) => {
        if (active?.test === test)
            active.status = "passed";
    });
    // A failed attempt Mocha will run again, and a failing beforeEach or
    // afterEach hook, fail the attempt that is running.
    runner.on("retry", (test) => {
        if (active?.test === test)
            active.status = "failed";
    });
    runner.on("fail", (runnable) => {
        if (active && (active.test === runnable || runnable.type === "hook"))
            active.status = "failed";
        else if (runnable.type === "test")
            record(identity(runnable), "failed");
    });
    runner.on("pending", (test) => {
        if (active?.test === test)
            active.status = "skipped";
        else
            record(identity(test), "skipped");
    });
    runner.on("hook", (hook) => {
        if (SETUP_HOOK.test(hook.title ?? ""))
            flushActive();
    });
    runner.on("suite end", flushActive);
    runner.on("end", () => {
        flushActive();
        flushSetup("hooks");
    });
}
