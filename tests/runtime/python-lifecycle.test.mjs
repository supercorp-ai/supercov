// The Python runtime's end of life, and a fork in the middle of it. `close`
// runs from atexit, and a daemon thread can still be executing measured code
// after it: that is the setup that turned into a FileExistsError traceback in
// someone's test output (#35). A forked child inherits the parent's slots,
// which are shared pages, not copies.
import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

const runtimeDirectory = resolve(import.meta.dirname, "../../runtime/python");

function supportedPython() {
  const candidates = process.env.SUPERCOV_PYTHON
    ? [process.env.SUPERCOV_PYTHON]
    : ["python3.14", "python3.13", "python3.12", "python3.11", "python3.10", "python3.9", "python3", "python"];
  for (const candidate of candidates) {
    const probe = spawnSync(candidate, ["-c", "import sys; print(sys.version_info >= (3, 9))"], {
      encoding: "utf8",
    });
    if (probe.status === 0 && probe.stdout.trim() === "True") {
      return candidate;
    }
  }
  return null;
}

const python = supportedPython();
const skip = python ? false : "no CPython 3.9+ on PATH (or SUPERCOV_PYTHON)";

// One planned file with one statement, so a slot has a byte to write.
const SETUP = `
import json, os, sys, tempfile
sys.path.insert(0, sys.argv[1])
import supercov_runtime as rt
import supercov_probes

directory = tempfile.mkdtemp()
plan = os.path.join(directory, "plan.json")
statement = {"id": "s1", "start": [1, 0], "end": [1, 5], "lines": [1, 1], "exact": False}
file_plan = {key: [] for key in ("functions", "decisions", "loops", "logical", "matches", "tries")}
file_plan["statements"] = [statement]
with open(plan, "w") as stream:
    json.dump({"version": rt.PLAN_VERSION, "root": directory, "files": {"m.py": file_plan}}, stream)
evidence = os.path.join(directory, "evidence")
runtime = rt.Runtime(plan, evidence, "run", "main")
runtime.install()
HIT = supercov_probes.SLOT_HEADER
`;

// Drives the runtime exactly as an interpreter does -- install, record,
// close from atexit -- and then records once more, as a daemon thread still
// running measured code would. It reports what happened rather than
// asserting, so the test can say which property broke.
const CLOSE = `${SETUP}
runtime._record({"t": "probe"})
array = runtime.hits_var.get()
# Taken while the transport is open: close forgets the path along with the
# process that owned it, which is the very thing that made the reopen collide.
output_path = runtime.output_path
slot_path = array.path
runtime.close()

late = "accepted"
try:
    runtime._record({"t": "late"})
    array[HIT] = 1
except Exception as error:
    late = type(error).__name__

monitoring = getattr(sys, "monitoring", None)
detector = None
if monitoring is not None:
    detector = {
        "globalEvents": monitoring.get_events(runtime.tool_id) if runtime.tool_id is not None else 0,
        "stillRegistered": runtime.tool_id is not None
        and monitoring.register_callback(runtime.tool_id, monitoring.events.PY_START, None) is not None,
    }
with open(output_path, "rb") as stream:
    written = stream.read()
leftover = None
if os.path.exists(slot_path):
    with open(slot_path, "rb") as stream:
        leftover = stream.read(16)[8:16].hex()
print(json.dumps({
    "late": late,
    "lateWritten": b'"t":"late"' in written,
    "exitMarker": b'"t":"exit"' in written,
    "detector": detector,
    "slotLeftover": leftover,
}))
`;

// A fork: the child must write into a slot of its own, and the parent's
// slot must not see what the child executed.
const FORK = `${SETUP}
runtime.switch({"test": "t", "phase": "call"})
parent = runtime.hits_var.get()
reader, writer = os.pipe()
pid = os.fork()
if pid == 0:
    child = runtime.hits_var.get()
    child[HIT] = 1
    os.write(writer, json.dumps({"separate": child.path != parent.path, "child": child[HIT]}).encode())
    os._exit(0)
os.waitpid(pid, 0)
report = json.loads(os.read(reader, 4096))
report["parentSawChild"] = parent[HIT] == 1
print(json.dumps(report))
`;

// Many phases, each starting a thread, with the cycle collector off: a
// finished thread must be freed by reference counting alone, and slots must
// be reused rather than piling up -- one per phase would mean a mapping per
// test, and on macOS unmapping a dirty one costs milliseconds.
const POOL = `${SETUP}
import gc, threading, weakref
gc.disable()
threads = []
for index in range(50):
    runtime.switch({"test": f"t{index}", "phase": "call"})
    thread = threading.Thread(target=lambda: runtime.hits_var.get().__setitem__(HIT, 1))
    thread.start()
    thread.join()
    threads.append(weakref.ref(thread))
    del thread
    runtime.switch(None)
print(json.dumps({
    "slots": runtime.next_slot,
    "threadsAlive": sum(1 for reference in threads if reference() is not None),
}))
`;

function run(script) {
  const result = spawnSync(python, ["-c", script, runtimeDirectory], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout.trim().split("\n").at(-1));
}

test("a record that arrives after close is turned away rather than crashing", { skip }, () => {
  // Close left the evidence file on disk and forgot the process owned it, so
  // the next record reopened the identical path -- same worker, same pid, a
  // token fixed for the collector's life -- and O_EXCL refused it.
  const outcome = run(CLOSE);
  assert.equal(outcome.late, "accepted", `a late record raised ${outcome.late}`);
  assert.equal(outcome.lateWritten, false, "and it is not written after the run's end");
});

test("close still writes the exit marker", { skip }, () => {
  // The marker is the transport's last record. Turning records away once
  // closed must not turn this one away too.
  assert.equal(run(CLOSE).exitMarker, true);
});

test("close takes back its slots, so a late write is never counted", { skip }, () => {
  // A daemon thread still holding its context's array writes into pages the
  // reader would otherwise read as that test's. Close removes the slot or,
  // where a live mapping keeps the file (Windows), zeroes the tag the reader
  // requires.
  const leftover = run(CLOSE).slotLeftover;
  assert.ok(leftover === null || /^0+$/.test(leftover), `slot tag left as ${leftover}`);
});

test("close stops the detector instead of observing and discarding", { skip }, () => {
  const { detector } = run(CLOSE);
  if (detector === null) return; // before 3.12 there is no detector to stop
  assert.equal(detector.globalEvents, 0, "no global event is left armed");
  assert.equal(detector.stillRegistered, false, "no callback is left registered");
});

test(
  "a forked child writes into a slot of its own",
  { skip: skip || (process.platform === "win32" ? "no fork on Windows" : false) },
  () => {
    const outcome = run(FORK);
    assert.equal(outcome.separate, true, "the child got its own slot");
    assert.equal(outcome.child, 1);
    assert.equal(outcome.parentSawChild, false, "the parent's slot does not see the child's hit");
  },
);

test("finished threads free their phase's slot, and slots are reused", { skip }, () => {
  const outcome = run(POOL);
  assert.equal(outcome.threadsAlive, 0, "a finished thread is freed without the cycle collector");
  assert.ok(outcome.slots <= 3, `${outcome.slots} slots for 50 phases`);
});
