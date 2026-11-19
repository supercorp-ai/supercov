// Strong mutation's setup file for Vitest (docs/design/strong-mutation.md).
//
// The strong reporter (vitestStrongReporter.mjs) runs the suite in rounds,
// and in each round gives every test file at most one mutant and the tests
// that may kill it. This file runs in the test file's isolated module graph
// before the test file's imports: it switches that file's mutant on, so a
// mutant evaluated while a module loads is in place too, and skips every test
// the round did not choose. After the file's first chosen test fails, the
// rest are skipped: the mutant is killed.
import { afterAll, afterEach, beforeEach, inject } from "vitest";
import * as localRuntime from "./runtime.mjs";

const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__ ?? localRuntime;

/** The round's assignment for this test file, if any. */
function assignmentForThisFile() {
    let provided;
    try {
        provided = inject("supercovStrong");
    }
    catch {
        return undefined;
    }
    if (!provided)
        return undefined;
    const state = globalThis.__vitest_worker__;
    const filepath = state?.filepath ?? state?.current?.file?.filepath;
    if (!filepath)
        return undefined;
    const relative = filepath.startsWith(provided.root)
        ? filepath.slice(provided.root.length).replace(/^[\\/]+/, "").split("\\").join("/")
        : filepath;
    return provided.files?.[relative];
}

const assignment = assignmentForThisFile();
const chosen = new Set(assignment?.tests ?? []);
let killed = false;

if (assignment?.mutant) {
    runtime.setMutantHitLimit?.(assignment.hitLimit ?? 1_000_000);
    runtime.setActiveMutant(assignment.mutant.file, assignment.mutant.index);
}

beforeEach((context) => {
    if (!assignment)
        return;
    const id = `vitest:${context.task.id}`;
    if (killed || (!assignment.wholeFile && !chosen.has(id))) {
        context.skip();
    }
});

afterEach((context) => {
    if (!assignment?.mutant)
        return;
    if (context.task.result?.state === "fail")
        killed = true;
});

afterAll(() => {
    if (assignment?.mutant)
        runtime.clearActiveMutant();
});
