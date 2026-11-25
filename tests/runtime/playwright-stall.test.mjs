import assert from "node:assert/strict";
import test from "node:test";
import { describeCall, ownDeadline, stallDetails, stalledOn, stallWindowOf, watchStall } from "../../runtime/javascript/playwrightStrong.mjs";

const timeouts = { action: 0, navigation: 0, expect: 5_000, toPass: 0 };
const deadline = (method, args, extra = {}) => ownDeadline({ method, args, startedAt: 1_000, request: false, timeouts, ...extra });

test("a call ends by the timeout it was given, else by the test's default for its kind", () => {
  // Given: 0 is none.
  assert.equal(deadline("click", [{ timeout: 2_000 }]), 3_000);
  assert.equal(deadline("click", [{ timeout: 0 }]), Infinity);
  // The last options object with a timeout counts; one without is passed over.
  assert.equal(deadline("fill", ["text", { force: true }, null, { timeout: 500 }]), 1_500);
  // An `expect` on a locator carries the timeout it runs under.
  assert.equal(deadline("_expect", ["to.be.visible", { isNot: false, timeout: 15_000 }]), 16_000);
  // A sleep ends when it was asked to.
  assert.equal(deadline("waitForTimeout", [250]), 1_250);
  assert.equal(deadline("waitForTimeout", []), 1_000);
  // Playwright's defaults: none for actions and navigations ...
  assert.equal(deadline("click", []), Infinity);
  assert.equal(deadline("goto", ["/"]), Infinity);
  // ... a navigation under the navigation timeout, else the action one ...
  const set = { action: 4_000, navigation: 9_000 };
  assert.equal(deadline("waitForURL", ["/done"], { timeouts: set }), 10_000);
  assert.equal(deadline("reload", [], { timeouts: { action: 4_000, navigation: 0 } }), 5_000);
  assert.equal(deadline("click", [], { timeouts: set }), 5_000);
  // ... and an API request 30 s, or the action timeout where shorter.
  assert.equal(deadline("get", ["/api"], { request: true }), 31_000);
  assert.equal(deadline("get", ["/api"], { request: true, timeouts: { action: 2_000 } }), 3_000);
  assert.equal(deadline("get", ["/api", { timeout: 0 }], { request: true }), Infinity);
});

test("an attempt is stalled only on its newest call, with nothing able to end before the limit, its pages quiet", () => {
  const click = { operation: "Locator.click (#save)", deadline: Infinity };
  const expectation = { operation: "expect(#done) to.have.text", deadline: 20_000 };
  const poll = { operation: "expect.toBe", deadline: 8_000 };
  const at = { limitAt: 10_000, quietSince: 0, now: 5_000 };
  assert.equal(stalledOn({ waits: new Set([click]), newest: click, ...at }), click);
  assert.equal(stalledOn({ waits: new Set([click, expectation]), newest: expectation, ...at }), expectation);
  // Nothing called, or the newest call returned: the test is doing something else.
  assert.equal(stalledOn({ waits: new Set(), newest: undefined, ...at }), undefined);
  assert.equal(stalledOn({ waits: new Set([click]), newest: expectation, ...at }), undefined);
  // A poll that fails by itself before the limit: a kill to come.
  assert.equal(stalledOn({ waits: new Set([poll, click]), newest: click, ...at }), undefined);
  // A request sent or answered within the last second: the page is busy.
  assert.equal(stalledOn({ waits: new Set([click]), newest: click, ...at, quietSince: 4_500 }), undefined);
  assert.equal(stalledOn({ waits: new Set([click]), newest: click, ...at, quietSince: 4_000 }), click);
});

test("calls are named for a timeout's reason", () => {
  class Locator {
    _selector = "#save";
  }
  class Page {}
  assert.equal(describeCall(new Locator(), "click", []), "Locator.click (#save)");
  assert.equal(describeCall(new Locator(), "_expect", ["to.be.visible", {}]), "expect(#save) to.be.visible");
  assert.equal(describeCall(new Page(), "_expect", ["to.have.url", {}]), "expect to.have.url");
  assert.equal(describeCall(new Page(), "waitForURL", ["/done"]), "Page.waitForURL");
  assert.equal(describeCall(undefined, "get", []), "Playwright.get");
  class _Locator {
    _selector = "#a";
  }
  assert.equal(describeCall(new _Locator(), "fill", ["x"]), "Locator.fill (#a)");
  const long = describeCall(Object.assign(new Locator(), { _selector: "x".repeat(300) }), "click", []);
  assert.equal(long.length, 160);
  assert.ok(long.endsWith("…"));
});

test("an attempt's stall window is past every passing run, inside its limit", () => {
  assert.equal(stallWindowOf(1_000, 1.5), 2_500);
  assert.equal(stallWindowOf(3_333, 1.5), 6_000);
});

test("a stall watch ends a stalled mutant attempt as a timeout, and says what it waited on", async () => {
  const now = Date.now();
  const claim = (extra) => ({ kind: "mutant", startedAt: now, claimedAt: now - 5, baselineMs: 1, timeoutFactor: 1, limitAt: now + 60_000, ...extra });
  // A control, and an attempt whose own timeout ends it before its window, are not watched.
  assert.equal(watchStall({}, { kind: "control" }, () => ({})), undefined);
  assert.equal(watchStall({}, undefined, () => ({})), undefined);
  assert.equal(watchStall({}, claim({ limitAt: now + 500 }), () => ({})), undefined);
  // Waiting, then stalled: its test's timeout is set to the time it has run.
  const looks = [{ waiting: "Locator.click (#a)" }, { stalled: "Locator.click (#a)", waiting: "Locator.click (#a)" }];
  const asked = [];
  let set;
  const stalled = await new Promise((done) => {
    const watch = watchStall({ setTimeout: (ms) => { set = ms; done(watch); } }, claim(), (limitAt) => {
      asked.push(limitAt);
      return looks.shift();
    });
  });
  assert.deepEqual(asked, [now + 60_000, now + 60_000]);
  assert.ok(set >= 5, `${set}`);
  assert.equal(stalled.stalled.waitingOn, "Locator.click (#a)");
  assert.ok(stalled.stalled.atMs >= 1_000, `${stalled.stalled.atMs}`);
  assert.deepEqual(stallDetails(stalled), { stalledAtMs: stalled.stalled.atMs, waitingOn: "Locator.click (#a)" });
  // Never stalled: what it last waited on, until stopped.
  const waiting = watchStall({ setTimeout: () => assert.fail("not stalled") }, claim(), () => ({ waiting: "Page.waitForURL" }));
  await new Promise((done) => setTimeout(done, 1_150));
  waiting.stop();
  assert.deepEqual(stallDetails(waiting), { waitingOn: "Page.waitForURL" });
  // Nothing to say.
  assert.deepEqual(stallDetails(undefined), {});
  assert.deepEqual(stallDetails({ stalled: undefined, waiting: undefined }), {});
});
