"use strict";

// Keep Babel-transformed test modules on the same runtime as the Jest adapter.
// Loading a second runtime here would split assertion and coverage evidence.
// jsdom owns a separate global and copies process without the preload's
// private property. Retrieve the host runtime through Node's VM API, then
// expose it before user setup files can import instrumented application code.
const runtime = globalThis.__SUPERCOV_DIRECT_RUNTIME__ ?? process.__SUPERCOV_DIRECT_RUNTIME__ ??
    require("node:vm").runInThisContext("globalThis.__SUPERCOV_DIRECT_RUNTIME__");
if (!runtime) throw new Error("[supercov] Jest runtime was not initialized by the preload");
globalThis.__SUPERCOV_DIRECT_RUNTIME__ = runtime;
// A process that turns out to run Jest (a wrapper that calls it in place)
// prints a failure's paths as the project's too. See register.mjs.
const failureOutput = process.__SUPERCOV_FAILURE_OUTPUT__ ??
    require("node:vm").runInThisContext("process.__SUPERCOV_FAILURE_OUTPUT__");
if (typeof failureOutput === "function") failureOutput("jest");
module.exports = runtime;
