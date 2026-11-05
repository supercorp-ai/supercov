import assert from "node:assert/strict";
import test from "node:test";
import { COVERAGE_DOCUMENT_PHASE_COOKIE, COVERAGE_DOCUMENT_PHASE_SITE_COOKIE, startDocumentInPhase } from "../../runtime/javascript/transport.mjs";

// What a document runs first: the phase it starts in comes from its cookies.
function start(cookie, attemptId = "attempt-1") {
  const activated = [];
  const previous = { document: globalThis.document, phase: globalThis.__SUPERCOV_PHASE_ID__, activate: globalThis.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__, test: globalThis.__SUPERCOV_MCDC_TEST_ID__ };
  globalThis.document = typeof cookie === "function" ? { get cookie() { return cookie(); } } : { cookie };
  delete globalThis.__SUPERCOV_PHASE_ID__;
  globalThis.__SUPERCOV_MCDC_TEST_ID__ = attemptId;
  globalThis.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__ = (testId, phase) => activated.push([testId, phase]);
  try {
    startDocumentInPhase({ attemptId, phaseCookie: COVERAGE_DOCUMENT_PHASE_COOKIE, phaseSiteCookie: COVERAGE_DOCUMENT_PHASE_SITE_COOKIE });
    return { phase: globalThis.__SUPERCOV_PHASE_ID__, activated };
  } finally {
    globalThis.document = previous.document;
    globalThis.__SUPERCOV_PHASE_ID__ = previous.phase;
    globalThis.__SUPERCOV_ACTIVATE_PROBE_CONTEXT__ = previous.activate;
    globalThis.__SUPERCOV_MCDC_TEST_ID__ = previous.test;
  }
}

const cookie = (name, phase) => `${name}=${encodeURIComponent(phase)}`;

test("a document starts in the phase its cookie names, and tells the runtime", () => {
  const started = start(`theme=dark; ${cookie(COVERAGE_DOCUMENT_PHASE_COOKIE, "attempt-1:phase:3")}; __supercov_phase=attempt-1%3Aphase%3A4`);
  assert.equal(started.phase, "attempt-1:phase:3", "the action's phase, not the later assertion's");
  assert.deepEqual(started.activated, [["attempt-1", "attempt-1:phase:3"]]);
});

test("a frame on another of the app's hosts reads the site cookie", () => {
  assert.equal(start(cookie(COVERAGE_DOCUMENT_PHASE_SITE_COOKIE, "attempt-1:phase:2")).phase, "attempt-1:phase:2");
});

test("the site's own cookie wins over the cross-site one", () => {
  const both = `${cookie(COVERAGE_DOCUMENT_PHASE_SITE_COOKIE, "attempt-1:phase:1")}; ${cookie(COVERAGE_DOCUMENT_PHASE_COOKIE, "attempt-1:phase:5")}`;
  assert.equal(start(both).phase, "attempt-1:phase:5");
});

test("another attempt's phase is left alone", () => {
  // A context kept from an earlier test still carries that test's cookie, and
  // a copy of the script from that test runs in this test's documents too.
  const started = start(cookie(COVERAGE_DOCUMENT_PHASE_COOKIE, "attempt-0:phase:9"));
  assert.equal(started.phase, undefined);
  assert.deepEqual(started.activated, []);
  assert.equal(start(cookie(COVERAGE_DOCUMENT_PHASE_COOKIE, "attempt-10:phase:1"), "attempt-1").phase, undefined, "a prefix of another attempt's id is not that attempt");
});

test("a document without the cookie, or that cannot read cookies, starts in none", () => {
  assert.equal(start("").phase, undefined);
  assert.equal(start(() => { throw new Error("sandboxed"); }).phase, undefined);
});
