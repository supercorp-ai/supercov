// Compiled code for instrumented modules, kept for the rest of the run.
//
// Vitest evaluates every module a test file imports in a fresh worker for
// each test file, and compiles each again: an instrumented module is several
// times its source, so most of what Supercov added to a large unit suite was
// V8 compiling the same instrumented text over and over. V8 can start from
// code it compiled before (a script's cached data), checks it against the
// source and its own version and flags, and compiles afresh when it does not
// match, so a module behaves as it would have either way.
//
// `vm.runInThisContext` is what Vitest's module runner calls; it is wrapped
// for instrumented text alone. Each compiled module's code is written once
// its test file is done, when it holds the functions the file ran as well as
// the module's top level, for the workers after it to start from.
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import vm from "node:vm";

// The installed wrapper carries its state: where the cache is, and the
// scripts compiled with no cached data that matched, each with where to keep
// its code. Any copy of this module saves what the one wrapper compiled.
const STATE = Symbol.for("supercov.compileCache");
const MARK = "__supercov";

/**
 * Wraps `vm.runInThisContext` in this process to compile instrumented text
 * from what `directory` keeps. Once per process: every copy of the runtime
 * shares the one wrapper.
 */
export function installCompileCache(directory) {
    if (!directory || vm.runInThisContext[STATE])
        return;
    const original = vm.runInThisContext;
    const state = { directory, unsaved: [] };
    const cached = function runInThisContext(code, options) {
        // The filename-only form and anything not instrumented go as they
        // came.
        if (typeof code !== "string" || (options !== undefined && (options === null || typeof options !== "object")) || !code.includes(MARK))
            return Reflect.apply(original, this, arguments);
        const key = createHash("sha256")
            .update(code)
            .update("\0")
            .update(JSON.stringify([options?.filename, options?.lineOffset, options?.columnOffset]))
            .digest("hex");
        const file = join(directory, key);
        let cachedData;
        try {
            cachedData = readFileSync(file);
        }
        catch {
            cachedData = undefined;
        }
        // What `vm.runInThisContext` does, with the cached data.
        const script = new vm.Script(code, { ...options, cachedData });
        if (cachedData === undefined || script.cachedDataRejected)
            state.unsaved.push([file, script]);
        return script.runInThisContext(options);
    };
    cached[STATE] = state;
    vm.runInThisContext = cached;
}

/**
 * Writes the code of each module compiled since the last save without cached
 * data: what V8 holds of it now, the functions run so far included. A file
 * another worker wrote meanwhile is replaced whole, never torn.
 */
export function saveCompileCache() {
    const state = vm.runInThisContext[STATE];
    if (!state || state.unsaved.length === 0)
        return;
    const scripts = state.unsaved.splice(0);
    try {
        mkdirSync(state.directory, { recursive: true });
        for (const [file, script] of scripts) {
            const partial = `${file}.${process.pid}.${Math.random().toString(36).slice(2)}`;
            writeFileSync(partial, script.createCachedData());
            renameSync(partial, file);
        }
    }
    catch {
        // A cache that cannot be written is compiled again next time.
    }
}
