// Supercov node-tap adapter. The tap CLI runs each test file as a process of
// its own and marks it with TAP_CHILD_ID; there the preload imports this
// adapter, which patches the class every subtest is made by -- TestBase in
// @tapjs/core (tap 18 and newer), Test in libtap (tap 15-16) -- so a suite
// runs exactly as written. tap may run subtests in parallel (t.jobs)
// and runs each body through an AsyncResource made when the subtest is, so
// each subtest is made, and therefore runs, inside a coverage carrier of its
// own, as node:test subtests do. What ran under it is written with the
// subtest's outcome once it ends. The file's own top level, outside any
// subtest, counts for the run. It computes no coverage.
import { realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { activateCoverageScope, beginBufferedServerEvidence, flushBufferedServerEvidence, takeNodeAssertionPhases, withCoverageCarrier, } from "./runtime.mjs";
import { runnerExecutionScope, writeRunnerEvidence } from "./runnerEvidence.mjs";
import { strongJob } from "./strongJob.mjs";

// The classes that make subtests in the project's tap.
async function subtestClasses() {
    const classes = [];
    let fromTap;
    let tap;
    try {
        const fromProject = createRequire(join(process.cwd(), "package.json"));
        tap = realpathSync(dirname(fromProject.resolve("tap/package.json")));
        fromTap = createRequire(join(tap, "package.json"));
    }
    catch {
        return classes;
    }
    let core;
    try {
        core = dirname(fromTap.resolve("@tapjs/core/package.json"));
    }
    catch {
        core = undefined;
    }
    if (core) {
        // An ESM test file gets the ESM build and a CommonJS one the
        // CommonJS build; they are different classes, so both are patched.
        try {
            classes.push((await import(pathToFileURL(join(core, "dist", "esm", "test-base.js")).href)).TestBase);
        }
        catch {
            // Not every release ships both builds.
        }
        try {
            classes.push(createRequire(import.meta.url)(join(core, "dist", "commonjs", "test-base.js")).TestBase);
        }
        catch {
            // As above.
        }
        return classes;
    }
    // libtap is CommonJS, and its exports hide lib/: reach it from its main.
    try {
        const lib = dirname(fromTap.resolve("libtap"));
        // tap sets libtap up before it loads it: which frames are tap's own,
        // and how a frame is taken through a source map. Loaded from here
        // first, libtap kept its defaults for the whole file, and a failure
        // listed tap's frames under the test's.
        try {
            fromTap(join(tap, "settings.js"));
        }
        catch {
            // A tap that has no settings of its own leaves libtap's.
        }
        try {
            nameFromTheCopy(createRequire(join(lib, "test.js"))("stack-utils"));
        }
        catch {
            // Frames stay as libtap writes them.
        }
        classes.push(createRequire(import.meta.url)(join(lib, "test.js")));
    }
    catch {
        // Neither tap's own classes nor libtap's: tap 14 or older.
    }
    return classes;
}

// libtap takes the directory the test runs in off a frame, which under
// Supercov is the copy, while a measured file's frame names the file its
// author wrote: the whole of `/home/me/shop/src/crypto.js` for
// `src/crypto.js`. A frame in the project is placed in the copy before
// libtap reads it, as Mocha's are.
function nameFromTheCopy(StackUtils) {
    const real = (directory) => {
        try {
            return directory ? realpathSync(directory).replace(/\\/g, "/") : undefined;
        }
        catch {
            return undefined;
        }
    };
    const project = real(process.env.SUPERCOV_SOURCE_PROJECT_ROOT);
    const copy = real(process.env.SUPERCOV_PROJECT_ROOT);
    const { clean, at, parseLine } = StackUtils?.prototype ?? {};
    if (!project || !copy || project === copy || typeof clean !== "function" || clean.supercovTap)
        return;
    const inTheProject = (text) => text.includes(`${project}/`) && !text.includes(`${copy}/`);
    StackUtils.prototype.clean = function supercovTapClean(stack, ...rest) {
        const lines = (Array.isArray(stack) ? stack : String(stack).split("\n")).map((line) => {
            const text = String(line).replace(/\\/g, "/");
            return inTheProject(text) ? text.split(`${project}/`).join(`${copy}/`) : line;
        });
        return Reflect.apply(clean, this, [lines, ...rest]);
    };
    StackUtils.prototype.clean.supercovTap = true;
    const placed = (found) => {
        if (typeof found?.file === "string" && found.file.startsWith(`${project}/`) && !found.file.startsWith(`${copy}/`))
            found.file = found.file.slice(project.length + 1);
        return found;
    };
    if (typeof at === "function")
        StackUtils.prototype.at = function supercovTapAt(...given) {
            return placed(Reflect.apply(at, this, given));
        };
    if (typeof parseLine === "function")
        StackUtils.prototype.parseLine = function supercovTapParseLine(...given) {
            return placed(Reflect.apply(parseLine, this, given));
        };
}

// tap counts a test file as a test of its own: assertions at its top level,
// outside any subtest, pass or fail it. What runs there is that test's, the
// file's top level, which tap's own report names after the file, and it is
// written as a test where the file asserts there (or has no subtest), as the
// file's module setup otherwise.
const topLevel = { root: undefined, subtests: 0 };

export async function adaptTap(testFile) {
    const evidenceDirectory = process.env.SUPERCOV_EVIDENCE_DIR;
    if (!evidenceDirectory)
        return;
    for (const Class of await subtestClasses()) {
        if (typeof Class?.prototype?.sub === "function" && !Class.prototype.sub.supercovTap)
            patch(Class, testFile, evidenceDirectory);
    }
    const identity = { runner: "tap", file: testFile, name: "(top level)" };
    const scope = runnerExecutionScope(identity);
    beginBufferedServerEvidence(scope);
    activateCoverageScope(scope);
    process.once("exit", (code) => {
        const points = topLevel.root?.count ?? topLevel.root?.counts?.total ?? 0;
        const asserts = topLevel.subtests === 0 || points > topLevel.subtests;
        const written = asserts ? identity : { ...identity, role: "setup" };
        writeRunnerEvidence(written, !asserts || code === 0 ? "passed" : "failed", scope, evidenceDirectory, takeNodeAssertionPhases(scope), flushBufferedServerEvidence(scope));
    });
}

function patch(TestBase, testFile, evidenceDirectory) {
    const sub = TestBase.prototype.sub;
    // Subtests of one parent that share a name, told apart by order.
    const registrations = new WeakMap();
    TestBase.prototype.sub = function supercovTapSub(Class, extra = {}, caller = this.sub) {
        const names = [];
        for (let test = this; test?.parent; test = test.parent)
            names.unshift(test.name);
        const name = [...names, extra.name || "(unnamed test)"].join(" > ");
        const counts = registrations.get(this) ?? new Map();
        registrations.set(this, counts);
        const ordinal = counts.get(name) ?? 0;
        counts.set(name, ordinal + 1);
        const identity = {
            runner: "tap",
            file: testFile,
            name,
            ...(ordinal ? { registrationOrdinal: ordinal } : {}),
        };
        const scope = runnerExecutionScope(identity);
        // A strong pass's job runs its own tests, none after its first
        // failure. A subtest runs with its parent, as it does without
        // Supercov.
        const job = strongJob();
        const atTop = !this.parent;
        if (atTop) {
            topLevel.root = this;
            topLevel.subtests += 1;
        }
        if (job && (job.stopped || (atTop && !job.selects(testFile, scope.testId))))
            extra = { ...extra, skip: true };
        beginBufferedServerEvidence(scope);
        let written = false;
        const finish = (status) => {
            if (written)
                return;
            written = true;
            if (status === "failed")
                job?.fail();
            writeRunnerEvidence(identity, status, scope, evidenceDirectory, takeNodeAssertionPhases(scope), flushBufferedServerEvidence(scope));
        };
        // Both libtap and @tapjs/core announce the subtest they make, before
        // they may start it and take it off their lists.
        let subtest;
        const made = (test) => {
            subtest ??= test;
        };
        this.on?.("subtestAdd", made);
        let result;
        try {
            result = withCoverageCarrier({ version: 1, scope }, () => Reflect.apply(sub, this, [Class, extra, caller]));
        }
        finally {
            this.removeListener?.("subtestAdd", made);
        }
        if (!subtest) {
            // Skipped, todo or filtered out: tap records a pass and makes no
            // subtest. Nor does a parent that bailed out or already ended.
            finish("skipped");
            return result;
        }
        const settle = () => finish(extra.skip || extra.todo ? "skipped"
            : typeof subtest.passing === "function" && !subtest.passing() ? "failed"
                : "passed");
        Promise.resolve(result).then(settle, () => finish("failed"));
        return result;
    };
    TestBase.prototype.sub.supercovTap = true;
}
