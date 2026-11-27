import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { CONFIRMED_AT_LIMIT, PAGE_BUSY, PAGE_GONE, describeCall, ownDeadline, ownEndPast, pendingBefore, stallDetails, stalledOn, stallWindowOf, watchPageWork, watchStall } from "../../runtime/javascript/playwrightStrong.mjs";

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
  const click = { operation: "Locator.click (#save)", deadline: Infinity, fromTest: true };
  const expectation = { operation: "expect(#done) to.have.text", deadline: 20_000, fromTest: true };
  const poll = { operation: "expect.toBe", deadline: 8_000, fromTest: true };
  // A fixture's own call, such as the new page of a new worker.
  const page = { operation: "BrowserContext.newPage", deadline: Infinity, fromTest: false };
  const at = { limitAt: 10_000, quietSince: 0, now: 5_000 };
  assert.equal(stalledOn({ waits: new Set([click]), newest: click, ...at }), click);
  assert.equal(stalledOn({ waits: new Set([click, expectation]), newest: expectation, ...at }), expectation);
  // Nothing called, or the newest call returned: the test is doing something else.
  assert.equal(stalledOn({ waits: new Set(), newest: undefined, ...at }), undefined);
  assert.equal(stalledOn({ waits: new Set([page]), newest: page, ...at }), undefined);
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
  const running = (extra = {}) => ({ status: "passed", duration: 0, ...extra });
  // Nothing its pages have yet to do.
  const idle = async () => undefined;
  // A control, and an attempt whose own timeout ends it before its window, are not watched.
  assert.equal(watchStall({}, { kind: "control" }, () => ({}), idle), undefined);
  // A test that has already ended is looked at no more.
  // A test that has already ended, or whose body has (its fixtures being
  // torn down), is looked at no more.
  for (const info of [running({ status: "timedOut" }), running({ duration: 900 })]) {
    const ended = watchStall(info, claim(), () => assert.fail("looked at"), idle);
    await new Promise((done) => setTimeout(done, 1_100));
    ended.stop();
    assert.deepEqual(stallDetails(ended), {});
  }
  // Stalled, but the test's own first call was too recent: the window
  // counts from it, so it goes on, waiting.
  const recent = watchStall(running({ setTimeout: () => assert.fail("not past its window") }), claim(), () => ({ stalled: "Locator.click (#a)", waiting: "Locator.click (#a)", since: Date.now() }), idle);
  await new Promise((done) => setTimeout(done, 1_150));
  recent.stop();
  assert.deepEqual(stallDetails(recent), { waitingOn: "Locator.click (#a)" });
  // No call of its own yet: never stalled.
  const unstarted = watchStall(running({ setTimeout: () => assert.fail("no call of its own") }), claim(), () => ({ stalled: "x", waiting: "x" }), idle);
  await new Promise((done) => setTimeout(done, 1_150));
  unstarted.stop();
  assert.equal(watchStall({}, undefined, () => ({}), idle), undefined);
  assert.equal(watchStall({}, claim({ limitAt: now + 500 }), () => ({}), idle), undefined);
  // Waiting, then stalled: its test's timeout is set to the time it has run.
  const since = now - 5_000;
  const looks = [{ waiting: "Locator.click (#a)", since }, { stalled: "Locator.click (#a)", waiting: "Locator.click (#a)", since }];
  const asked = [];
  let set;
  const stalled = await new Promise((done) => {
    const watch = watchStall({ ...running(), setTimeout: (ms) => { set = ms; done(watch); } }, claim(), (limitAt) => {
      asked.push(limitAt);
      return looks.shift() ?? { stalled: "Locator.click (#a)", waiting: "Locator.click (#a)", since };
    }, idle);
  });
  // Looked at again once its pages said they have nothing left to do.
  assert.deepEqual(asked, [now + 60_000, now + 60_000, now + 60_000]);
  assert.ok(set >= 5, `${set}`);
  assert.equal(stalled.stalled.waitingOn, "Locator.click (#a)");
  assert.ok(stalled.stalled.atMs >= 1_000, `${stalled.stalled.atMs}`);
  assert.deepEqual(stallDetails(stalled), { stalledAtMs: stalled.stalled.atMs, waitingOn: "Locator.click (#a)" });
  // Never stalled: what it last waited on, until stopped.
  const waiting = watchStall({ ...running(), setTimeout: () => assert.fail("not stalled") }, claim(), () => ({ waiting: "Page.waitForURL" }), idle);
  await new Promise((done) => setTimeout(done, 1_150));
  waiting.stop();
  assert.deepEqual(stallDetails(waiting), { waitingOn: "Page.waitForURL" });
  // Nothing to say.
  assert.deepEqual(stallDetails(undefined), {});
  assert.deepEqual(stallDetails({ stalled: undefined, waiting: undefined }), {});
});

test("a mutant's own failure keeps its worker; a control's, or a mutant that ran in the worker, does not", async () => {
  const { keepsWorker } = await import("../../runtime/javascript/playwrightStrong.mjs");
  const mutant = { kind: "mutant" };
  assert.equal(keepsWorker(mutant, false, "failed"), true);
  assert.equal(keepsWorker(mutant, false, "timedOut"), true);
  assert.equal(keepsWorker(mutant, false, "passed"), false);
  assert.equal(keepsWorker(mutant, true, "failed"), false);
  assert.equal(keepsWorker({ kind: "control" }, false, "failed"), false);
  assert.equal(keepsWorker(undefined, false, "failed"), false);
});

test("what a page could still do before the limit puts a stall in doubt", () => {
  const at = { requests: 0, workers: 0, limitAt: 20_000, now: 10_000 };
  const frame = (answer, url = "http://app/") => ({ url, answer });
  // Nothing it has yet to do, or nothing before the limit.
  assert.equal(pendingBefore({ ...at, frames: [] }), undefined);
  assert.equal(pendingBefore({ ...at, frames: [frame({ due: null, sockets: 0 })] }), undefined);
  assert.equal(pendingBefore({ ...at, frames: [frame({ due: 20_000, sockets: 0 })] }), undefined);
  // An empty frame runs nothing.
  assert.equal(pendingBefore({ ...at, frames: [frame(null, "about:blank"), frame(undefined, "")] }), undefined);
  // A timer due before the limit: looked at again once it is due.
  assert.deepEqual(pendingBefore({ ...at, frames: [frame({ due: 18_000, sockets: 0 }), frame({ due: 12_500, sockets: 0 })] }), { because: "a timer due in 2.5 s", retryAt: 12_600 });
  assert.deepEqual(pendingBefore({ ...at, frames: [frame({ due: 9_000, sockets: 0 })] }), { because: "a timer due in 0.0 s", retryAt: 10_100 });
  // What it cannot say when it ends.
  const again = 10_500;
  assert.deepEqual(pendingBefore({ ...at, requests: 2, frames: [] }), { because: "2 unanswered request(s)", retryAt: again });
  assert.deepEqual(pendingBefore({ ...at, workers: 1, frames: [] }), { because: "1 worker(s) running in its pages", retryAt: again });
  assert.deepEqual(pendingBefore({ ...at, frames: [frame(PAGE_BUSY)] }), { because: "a page that did not answer", retryAt: again });
  assert.deepEqual(pendingBefore({ ...at, frames: [frame(PAGE_GONE)] }), { because: "a page loading a new document", retryAt: again });
  assert.deepEqual(pendingBefore({ ...at, frames: [frame(null, "http://ads/")] }), { because: "a frame with no watch on its timers (http://ads/)", retryAt: again });
  assert.deepEqual(pendingBefore({ ...at, frames: [frame({ due: null, sockets: 1 })] }), { because: "1 open WebSocket(s)", retryAt: again });
});

/** A document's globals, the browser's functions recorded. */
function page({ idle = true, socket = true, document } = {}) {
  let next = 0;
  const scheduled = new Map();
  const add = (handler, rest) => {
    next += 1;
    scheduled.set(next, { handler, rest });
    return next;
  };
  const schedule = (handler, _delay, ...rest) => add(handler, rest);
  const frame = (handler) => add(handler, []);
  const cancel = (id) => scheduled.delete(id);
  const globals = {
    setTimeout: schedule,
    setInterval: schedule,
    clearTimeout: cancel,
    clearInterval: cancel,
    requestAnimationFrame: frame,
    cancelAnimationFrame: cancel,
    ...(idle ? { requestIdleCallback: frame, cancelIdleCallback: cancel } : {}),
    ...(socket
      ? {
          WebSocket: class WebSocket {
            readyState = 1;
            listeners = [];
            constructor(url) {
              this.url = url;
            }
            addEventListener(_, listener) {
              this.listeners.push(listener);
            }
            close() {
              this.readyState = 3;
              for (const listener of this.listeners) listener();
            }
          },
        }
      : {}),
    ...(document ? { document } : {}),
  };
  const context = vm.createContext(globals);
  vm.runInContext(`(${watchPageWork})()`, context);
  const fire = (id, self) => {
    const { handler, rest } = scheduled.get(id);
    scheduled.delete(id);
    return handler.apply(self, rest);
  };
  return { context, run: (code) => vm.runInContext(code, context), // A plain object of this realm.
    work: () => ({ ...context[Symbol.for("supercov.pageWork")]() }), fire, scheduled };
}

test("a page's watch keeps the timers, frames and sockets it has yet to run", async () => {
  const { context, run, work, fire, scheduled } = page();
  assert.deepEqual(work(), { due: null, sockets: 0 });
  // Installed once: a second script leaves the first.
  const wrapped = context.setTimeout;
  run(`(${watchPageWork})()`);
  assert.equal(context.setTimeout, wrapped);
  // A timeout: due at its time, forgotten once it ran, with what it was given.
  const before = Date.now();
  const timeout = run("setTimeout(function (value) { globalThis.ran = [String(this), value]; return 7; }, 5000, 'given')");
  assert.ok(work().due >= before + 5_000 && work().due <= Date.now() + 5_000);
  assert.equal(fire(timeout, "self"), 7);
  assert.deepEqual([...context.ran], ["self", "given"]);
  assert.deepEqual(work(), { due: null, sockets: 0 });
  // Cleared by either clear, which the browser hears too.
  for (const clear of ["clearTimeout", "clearInterval"]) {
    const id = run("setTimeout(() => {}, 100)");
    run(`${clear}(${id})`);
    assert.equal(scheduled.has(id), false);
    assert.equal(work().due, null);
  }
  // An interval: its next time, from when it was set.
  const interval = run("setInterval(() => {}, 1)");
  await new Promise((done) => setTimeout(done, 20));
  const asked = Date.now();
  const due = work().due;
  const answered = Date.now();
  assert.ok(due >= asked && due <= answered + 1, `${asked} ${due} ${answered}`);
  run(`clearInterval(${interval})`);
  run("setInterval(() => {}, 0)");
  assert.ok(work().due <= Date.now());
  run(`clearInterval(${interval + 1})`);
  assert.equal(work().due, null);
  // A string's timer: due until a second past its time.
  run("setTimeout('void 0', 0)");
  assert.ok(work().due <= Date.now());
  await new Promise((done) => setTimeout(done, 1_050));
  assert.equal(work().due, null);
  // An animation frame and an idle callback: due now, until they run.
  for (const [request, cancelled] of [["requestAnimationFrame", "cancelAnimationFrame"], ["requestIdleCallback", "cancelIdleCallback"]]) {
    const id = run(`${request}(function (time) { globalThis.frame = [String(this), time]; })`);
    assert.ok(work().due <= Date.now());
    fire(id, "self");
    assert.deepEqual([...context.frame], ["self", undefined]);
    assert.equal(work().due, null);
    const other = run(`${request}(() => {})`);
    run(`${cancelled}(${other})`);
    assert.equal(work().due, null);
  }
  // A handler that is not a function is the browser's to refuse.
  const refused = run("requestAnimationFrame('not a function')");
  assert.ok(work().due <= Date.now());
  run(`cancelAnimationFrame(${refused})`);
  assert.equal(work().due, null);
  // A socket, while it is open; it is still the browser's constructor.
  const sockets = run("[new WebSocket('ws://app/a'), new WebSocket('ws://app/b')]");
  assert.ok(run("new WebSocket('ws://app/c') instanceof WebSocket"));
  assert.equal(sockets[0].url, "ws://app/a");
  assert.equal(work().sockets, 3);
  sockets[0].close();
  sockets[1].readyState = 3;
  assert.equal(work().sockets, 1);
});

test("a page's watch counts its running animations by their end, and its playing media now", () => {
  const animations = [
    { playState: "running", playbackRate: 2, effect: { getComputedTiming: () => ({ endTime: 1_000, localTime: 400 }) } },
    { playState: "running", playbackRate: 0, effect: { getComputedTiming: () => ({ endTime: 3_000, localTime: undefined }) } },
    // A spinner, one paused, one with no effect: never due.
    { playState: "running", playbackRate: 1, effect: { getComputedTiming: () => ({ endTime: Infinity, localTime: 0 }) } },
    { playState: "paused", playbackRate: 1, effect: { getComputedTiming: () => ({ endTime: 10, localTime: 0 }) } },
    { playState: "running", playbackRate: 1, effect: null },
  ];
  const media = [{ paused: true, ended: false }, { paused: false, ended: true }];
  const { work } = page({ document: { getAnimations: () => animations, querySelectorAll: () => media } });
  const before = Date.now();
  const due = work().due;
  assert.ok(due >= before + 300 && due <= Date.now() + 300, `${due - before}`);
  media.push({ paused: false, ended: false });
  assert.ok(work().due <= Date.now());
  // A document with neither, and a page with no idle callbacks or sockets.
  assert.deepEqual(page({ idle: false, socket: false, document: {} }).work(), { due: null, sockets: 0 });
});

test("a stall its pages could still end goes on, and ends once they could not", async () => {
  const now = Date.now();
  const claim = { kind: "mutant", startedAt: now, claimedAt: now - 5, baselineMs: 1, timeoutFactor: 1, limitAt: now + 60_000 };
  const stalledLook = () => ({ stalled: "Page.waitForResponse", waiting: "Page.waitForResponse", since: now - 5_000 });
  // A timer due, then nothing: it ends at the second look.
  const doubts = [{ because: "a timer due in 0.1 s", retryAt: Date.now() }];
  let set;
  const ended = await new Promise((done) => {
    const watch = watchStall({ status: "passed", duration: 0, setTimeout: (ms) => { set = ms; done(watch); } }, claim, stalledLook, async () => doubts.shift());
  });
  assert.ok(set >= 5);
  assert.equal(ended.doubted.because, "a timer due in 0.1 s");
  assert.deepEqual(stallDetails(ended), { stalledAtMs: ended.stalled.atMs, waitingOn: "Page.waitForResponse" });
  // Always something to do: it goes on to its limit, and says why.
  const doubted = watchStall({ status: "passed", duration: 0, setTimeout: () => assert.fail("in doubt") }, claim, stalledLook, async () => ({ because: "1 unanswered request(s)", retryAt: Date.now() + 100 }));
  await new Promise((done) => setTimeout(done, 1_250));
  doubted.stop();
  const details = stallDetails(doubted);
  assert.equal(details.waitingOn, "Page.waitForResponse");
  assert.equal(details.doubtedBy, "1 unanswered request(s)");
  assert.ok(details.doubtedAtMs >= 0);
  // Its pages answered after the call returned, the test ended, or the
  // watch stopped: nothing is ended.
  const calls = [stalledLook(), { waiting: "Page.click" }];
  const moved = watchStall({ status: "passed", duration: 0, setTimeout: () => assert.fail("moved on") }, claim, () => calls.shift() ?? { waiting: "Page.click" }, async () => undefined);
  const info = { status: "passed", duration: 0, setTimeout: () => assert.fail("ended") };
  const finished = watchStall(info, claim, stalledLook, async () => {
    info.duration = 10;
    return undefined;
  });
  let answer;
  const stopping = watchStall({ status: "passed", duration: 0, setTimeout: () => assert.fail("stopped") }, claim, stalledLook, () => new Promise((done) => {
    answer = done;
  }));
  // A page whose question failed is asked again.
  let failures = 0;
  const failing = watchStall({ status: "passed", duration: 0, setTimeout: () => assert.fail("failed") }, claim, stalledLook, async () => {
    failures += 1;
    throw new Error("closed");
  });
  await new Promise((done) => setTimeout(done, 1_300));
  stopping.stop();
  answer(undefined);
  await new Promise((done) => setTimeout(done, 50));
  for (const watch of [moved, finished, failing]) watch.stop();
  assert.ok(failures >= 2, `${failures}`);
  assert.equal(stopping.stalled, undefined);
});

test("a timeout's reason says it ran to its limit past a stall its pages could have ended", async () => {
  const { verdictOf } = await import("../../runtime/javascript/playwrightStrong.mjs");
  const attempt = { outcome: "timed-out", limitMs: 16_300, baselineMs: 7_500, timeoutFactor: 1.5, waitingOn: "Page.waitForResponse at a.spec.ts:9:27" };
  assert.equal(verdictOf(attempt).reason, "ran past its time limit of 16.3 s (1.5 × its 7.5 s baseline + 5 s), waiting on Page.waitForResponse at a.spec.ts:9:27");
  assert.equal(
    verdictOf({ ...attempt, doubtedAtMs: 12_400, doubtedBy: "a timer due in 0.1 s" }).reason,
    `ran past its time limit of 16.3 s (1.5 × its 7.5 s baseline + 5 s), waiting on Page.waitForResponse at a.spec.ts:9:27; it stalled at 12.4 s, with a timer due in 0.1 s that could still end the wait, so ${CONFIRMED_AT_LIMIT}`,
  );
});

test("an attempt runs to its waits' own timeouts past the limit, where every one of them has one", () => {
  const wait = (deadline, startedAt = 1_000) => ({ operation: "x", deadline, startedAt, fromTest: true });
  const at = { limitAt: 10_000 };
  // An `expect` that started late, and ends by its own timeout after the limit.
  assert.equal(ownEndPast({ waits: new Set([wait(12_000, 7_000)]), ...at }), 12_000);
  // The latest of several, every one of them with a timeout of its own.
  assert.equal(ownEndPast({ waits: new Set([wait(12_000), wait(11_000)]), ...at }), 12_000);
  // Nothing to wait for, or all of it ends before the limit: the limit holds.
  assert.equal(ownEndPast({ waits: new Set(), ...at }), undefined);
  assert.equal(ownEndPast({ waits: new Set([wait(9_000)]), ...at }), undefined);
  // One call could wait for ever: the limit holds.
  assert.equal(ownEndPast({ waits: new Set([wait(12_000), wait(Infinity)]), ...at }), undefined);
  // A call made after the limit is none of this.
  assert.equal(ownEndPast({ waits: new Set([wait(12_000, 10_500)]), ...at }), undefined);
  // The test's own timeout ends it first, with no time for the wait to fail.
  assert.equal(ownEndPast({ waits: new Set([wait(12_000)]), ...at, testTimeoutAt: 12_500 }), undefined);
  assert.equal(ownEndPast({ waits: new Set([wait(12_000)]), ...at, testTimeoutAt: 13_000 }), 12_000);
});

test("a watch lets the waits run to their own end, and holds the limit again once they end before it", async () => {
  const now = Date.now();
  const claim = { kind: "mutant", startedAt: now - 3_000, claimedAt: now - 3_000, baselineMs: 1, timeoutFactor: 1, limitAt: now + 400, testTimeoutMs: 30_000 };
  const set = [];
  const info = { status: "passed", duration: 0, setTimeout: (ms) => set.push(ms) };
  let extension;
  const watch = watchStall(info, claim, (limitAt, testTimeoutAt) => {
    assert.equal(testTimeoutAt, claim.claimedAt + 30_000);
    return { stalled: "expect(#out) to.have.text", waiting: "expect(#out) to.have.text", since: now - 3_000, ownEnd: claim.limitAt + 2_000 };
  }, async () => assert.fail("no stall while its waits run to their end"), (limitAt, restore) => {
    extension = { limitAt, restore };
  });
  await new Promise((done) => setTimeout(done, 300));
  // Raised to the waits' own end, and a second to fail.
  assert.deepEqual(set, [claim.limitAt + 3_000 - claim.claimedAt]);
  assert.equal(extension.limitAt, claim.limitAt);
  // Ended before the limit: the limit holds again.
  extension.restore();
  assert.equal(set[1], claim.limitAt - claim.claimedAt);
  assert.equal(watch.extended, undefined);
  watch.stop();
});
