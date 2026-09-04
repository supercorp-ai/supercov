// Evidence buffered for the current turn is written by a flush that "exit"
// schedules, and "exit" does not run when a signal ends a process. A suite that
// stops a gateway it started kills the child and moves on, so that child's last
// batch went to the grave with it: the test that caused the coverage lost it,
// and the loss was silent and timing-dependent, which is the worst shape a
// measurement bug can take.
import assert from "node:assert/strict";
import test from "node:test";
import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

const runtime = await import(
  pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/runtime.mjs")).href
);

test("a signal flushes what only exit used to flush", () => {
  let flushed = 0;
  runtime.flushOnTermination(() => {
    flushed += 1;
  });
  // A listener of our own keeps the handler from re-raising and killing this
  // process, which is the same branch a program with its own handler takes.
  // Every signal under test needs one: without it the handler correctly ends
  // this process, which is exactly the default it is there to restore.
  const guard = () => {};
  process.on("SIGTERM", guard);
  process.on("SIGINT", guard);
  try {
    assert.equal(flushed, 0);
    process.emit("SIGTERM");
    assert.equal(flushed, 1, "SIGTERM must flush buffered evidence");
    process.emit("SIGINT");
    assert.equal(flushed, 2, "SIGINT must flush buffered evidence");
  } finally {
    process.off("SIGTERM", guard);
    process.off("SIGINT", guard);
  }
});

// A regression here does not fail, it hangs -- the child simply never dies --
// so the deadline is what turns it back into a test result.
test("a process with no handler of its own still dies from the signal", { timeout: 20000 }, async () => {
  // Listening for a signal suppresses the default action that ends the
  // process. Getting this wrong would be far worse than the loss it fixes:
  // every suite that stops a server it started would hang instead.
  const child = spawn(
    process.execPath,
    [
      "--input-type=module",
      "-e",
      `import ${JSON.stringify(pathToFileURL(resolve(import.meta.dirname, "../../runtime/javascript/runtime.mjs")).href)};` +
        `process.stdout.write('ready');setInterval(() => {}, 1000);`,
    ],
    { stdio: ["ignore", "pipe", "ignore"] },
  );
  try {
    await new Promise((done) => child.stdout.once("data", done));
    child.kill("SIGTERM");
    // A child that ignores the signal would otherwise hold this test open for
    // as long as the runner allows, so the wait is bounded and the survivor is
    // reported rather than left to stall the suite.
    const outcome = await Promise.race([
      new Promise((done) => child.once("exit", (code, signal) => done({ code, signal }))),
      new Promise((done) => setTimeout(() => done({ survived: true }), 10000)),
    ]);
    assert.equal(
      outcome.signal,
      "SIGTERM",
      outcome.survived
        ? "the child survived SIGTERM: the handler suppressed the default action without restoring it"
        : `expected death by SIGTERM, got code ${outcome.code}`,
    );
  } finally {
    child.kill("SIGKILL");
  }
});

test("every flusher shares one listener per signal", () => {
  // Two listeners of ours would each see a sibling, neither would judge itself
  // alone, and a signalled process would never reach the default action that
  // ends it. Registering more flushers must not add more listeners.
  const before = process.listenerCount("SIGTERM");
  runtime.flushOnTermination(() => {});
  runtime.flushOnTermination(() => {});
  assert.equal(
    process.listenerCount("SIGTERM"),
    before,
    "additional flushers must reuse the installed signal listener",
  );
});
