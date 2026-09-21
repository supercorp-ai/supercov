// The Python collector's end of life. `close` runs from atexit, and a daemon
// thread can still be executing measured code after it: that is the setup that
// turned into a FileExistsError traceback in someone's test output (#35).
import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

const runtimeDirectory = resolve(import.meta.dirname, "../../runtime/python");

// sys.monitoring arrived in 3.12, so an older interpreter cannot host the
// collector at all; the gate is skipped rather than failed where none exists.
function supportedPython() {
  const candidates = process.env.SUPERCOV_PYTHON
    ? [process.env.SUPERCOV_PYTHON]
    : ["python3.14", "python3.13", "python3.12", "python3", "python"];
  for (const candidate of candidates) {
    const probe = spawnSync(candidate, ["-c", "import sys; print(sys.version_info >= (3, 12))"], {
      encoding: "utf8",
    });
    if (probe.status === 0 && probe.stdout.trim() === "True") {
      return candidate;
    }
  }
  return null;
}

const python = supportedPython();

// Drives the collector exactly as an interpreter does -- install, record,
// close from atexit -- and then records once more, as a daemon thread still
// running measured code would. It reports what happened rather than asserting,
// so the test can say which of the three properties broke.
const PROBE = `
import json, os, sys, tempfile
sys.path.insert(0, sys.argv[1])
import supercov_runtime as rt

directory = tempfile.mkdtemp()
plan = os.path.join(directory, "plan.json")
with open(plan, "w") as stream:
    json.dump({"version": rt.PLAN_VERSION, "root": directory, "files": {}}, stream)
runtime = rt.Runtime(plan, os.path.join(directory, "evidence"), "run", "main")
runtime.install()
runtime._record({"t": "probe"})
# Taken while the transport is open: close forgets the path along with the
# process that owned it, which is the very thing that made the reopen collide.
output_path = runtime.output_path
runtime.close()

late = "accepted"
try:
    runtime._record({"t": "late"})
except Exception as error:
    late = type(error).__name__

monitoring = sys.monitoring
events = monitoring.events
registered = [events.PY_START, events.LINE, events.INSTRUCTION, events.JUMP, events.PY_RETURN]
registered += (
    [events.BRANCH_LEFT, events.BRANCH_RIGHT] if hasattr(events, "BRANCH_LEFT") else [events.BRANCH]
)
still_registered = [
    event for event in registered
    if monitoring.register_callback(runtime.tool_id, event, None) is not None
]
with open(output_path, "rb") as stream:
    written = stream.read()
print(json.dumps({
    "late": late,
    "lateWritten": b'"t":"late"' in written,
    "exitMarker": b'"t":"exit"' in written,
    "globalEvents": monitoring.get_events(runtime.tool_id),
    "callbacksStillRegistered": len(still_registered),
}))
`;

function probe() {
  const result = spawnSync(python, ["-c", PROBE, runtimeDirectory], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout.trim().split("\n").at(-1));
}

test(
  "a record that arrives after close is turned away rather than crashing",
  { skip: python ? false : "no CPython 3.12+ on PATH (or SUPERCOV_PYTHON)" },
  () => {
    // Close left the evidence file on disk and forgot the process owned it, so
    // the next record reopened the identical path -- same worker, same pid, a
    // token fixed for the collector's life -- and O_EXCL refused it.
    const outcome = probe();
    assert.equal(outcome.late, "accepted", `a late record raised ${outcome.late}`);
    assert.equal(outcome.lateWritten, false, "and it is not written after the run's end");
  },
);

test(
  "close still writes the exit marker",
  { skip: python ? false : "no CPython 3.12+ on PATH (or SUPERCOV_PYTHON)" },
  () => {
    // The marker is the transport's last record. Turning records away once
    // closed must not turn this one away too.
    assert.equal(probe().exitMarker, true);
  },
);

test(
  "close stops observing instead of observing and discarding",
  { skip: python ? false : "no CPython 3.12+ on PATH (or SUPERCOV_PYTHON)" },
  () => {
    // Left registered, every line a daemon thread runs during shutdown would
    // still call into the collector -- paying for records it must throw away,
    // on the path where module globals are already being torn down.
    const outcome = probe();
    assert.equal(outcome.globalEvents, 0, "no global event is left armed");
    assert.equal(outcome.callbacksStillRegistered, 0, "no callback is left registered");
  },
);
