"""Full kill matrix for mutmut 3.x mutants.

Usage (run with the project's own venv python):
    <proj>/.venv/bin/python kill_matrix.py <proj> [--jobs 4] [--max-seconds 540] [--baseline-runs 3]
        [--shard k/n] [--out FILE]

<proj>/src must already contain mutmut's generated `mutants/` dir and its *.meta files (`generate.py`,
or a finished `mutmut run`).
The script starts pytest once inside <proj>/src/mutants with the same arguments mutmut uses (minus -x,
plus -p no:cacheprovider), collects every test, then forks one child per mutant. Each child activates the
mutant through mutmut's trampoline and runs EVERY collected test in collection order with
runtestprotocol, recording each nodeid that reports a failure in setup, call or teardown (subtest failures
included, since they come through pytest_runtest_logreport with the parent's nodeid).

Outputs:
    <proj>/baseline.json      failures with no mutant active (union over --baseline-runs runs) + timing
    <proj>/kill-matrix.jsonl  one line per mutant: {"mutant", "status", "killers", "ms"}
                              timeout lines add "hung_in" (test running when killed; killers = failures before it);
                              error lines add "exit" (child exit code / -signal) and "last_test"
Resumable: mutants already present in kill-matrix.jsonl are skipped. --max-seconds stops launching new
mutants after that many seconds (running children are finished), so long matrices can be done in chunks.
"""

from __future__ import annotations

import argparse
import gc
import json
import os
import signal
import sys
import tempfile
import time
from pathlib import Path


def parse_args() -> argparse.Namespace:
    ap = argparse.ArgumentParser()
    ap.add_argument("project")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--max-seconds", type=float, default=540.0)
    ap.add_argument("--baseline-runs", type=int, default=3)
    ap.add_argument("--only", nargs="*", default=None, help="restrict to these mutant names (debugging)")
    ap.add_argument("--shard", default="1/1", help="k/n: only every n-th mutant, from the k-th")
    ap.add_argument("--out", default=None, help="where the matrix lines go (default <proj>/kill-matrix.jsonl)")
    return ap.parse_args()


ARGS = parse_args()
PROJECT = Path(ARGS.project).resolve()
SRC = PROJECT / "src"
OUT = Path(ARGS.out).resolve() if ARGS.out else PROJECT / "kill-matrix.jsonl"
BASELINE = PROJECT / "baseline.json"
PROGRESS = PROJECT / "kill-matrix.progress.log"


def log(msg: str) -> None:
    with open(PROGRESS, "a") as f:
        f.write(f"{time.strftime('%H:%M:%S')} {msg}\n")


def all_mutant_names() -> list[str]:
    names: list[str] = []
    for meta in sorted((SRC / "mutants").rglob("*.meta")):
        data = json.loads(meta.read_text())
        names.extend(data["exit_code_by_key"].keys())
    return names


def done_mutants() -> set[str]:
    done: set[str] = set()
    if OUT.exists():
        for line in OUT.read_text().splitlines():
            if line.strip():
                done.add(json.loads(line)["mutant"])
    return done


# ---------------------------------------------------------------------------------------------
# Environment set up exactly like `mutmut run` does before running tests (cwd = project root).
os.chdir(SRC)
from mutmut.configuration import config  # noqa: E402
from mutmut.mutation.trampoline import set_mutant_under_test  # noqa: E402
from mutmut.utils.file_utils import setup_source_paths  # noqa: E402

CFG = config()
setup_source_paths()
os.environ["PY_IGNORE_IMPORTMISMATCH"] = "1"
set_mutant_under_test("")  # mutmut's parent uses "" for clean runs

import pytest  # noqa: E402
from _pytest.runner import runtestprotocol  # noqa: E402

PYTEST_ARGS = (
    ["--rootdir=.", "--tb=native", "-q", "-p", "no:randomly", "-p", "no:random-order", "-p", "no:cacheprovider"]
    + list(CFG.pytest_add_cli_args_test_selection)
    + list(CFG.pytest_add_cli_args)
)


class FailureRecorder:
    """Collects failing nodeids; also streams "S <nodeid>" (test started) and "F <nodeid>" (first failure)
    lines to a file, so that a child killed on timeout still yields its failures and the test it hung in."""

    def __init__(self, stream_path: str) -> None:
        self.failed: list[str] = []
        self._seen: set[str] = set()
        self._stream = open(stream_path, "w", buffering=1)

    def started(self, nodeid: str) -> None:
        self._stream.write("S " + nodeid + "\n")

    @pytest.hookimpl
    def pytest_runtest_logreport(self, report) -> None:  # noqa: ANN001
        if report.failed and report.nodeid not in self._seen:
            self._seen.add(report.nodeid)
            self.failed.append(report.nodeid)
            self._stream.write("F " + report.nodeid + "\n")


def read_stream(stream_path: str) -> tuple[list[str], str | None]:
    failed: list[str] = []
    last_started = None
    try:
        with open(stream_path) as f:
            for line in f:
                if not line.endswith("\n"):
                    break
                kind, nodeid = line[0], line[2:-1]
                if kind == "S":
                    last_started = nodeid
                elif kind == "F":
                    failed.append(nodeid)
    except OSError:
        pass
    return failed, last_started


def run_child(session, items, mutant: str | None, result_path: str) -> None:  # noqa: ANN001
    """Runs in the forked child; never returns."""
    code = 0
    try:
        os.setpgid(0, 0)  # own process group so the parent can kill grandchildren (subprocess tests) on timeout
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, 1)
        os.dup2(devnull, 2)
        reporter = session.config.pluginmanager.get_plugin("terminalreporter")
        if reporter is not None:
            from _pytest._io import TerminalWriter

            reporter._tw = TerminalWriter(open(os.devnull, "w"))
        set_mutant_under_test(mutant if mutant is not None else "")
        recorder = FailureRecorder(result_path + ".stream")
        session.config.pluginmanager.register(recorder, "kill-matrix-recorder")
        start = time.monotonic()
        for i, item in enumerate(items):
            nextitem = items[i + 1] if i + 1 < len(items) else None
            recorder.started(item.nodeid)
            runtestprotocol(item, nextitem=nextitem, log=True)
        duration = time.monotonic() - start
        with open(result_path + ".part", "w") as f:
            json.dump({"failed": recorder.failed, "seconds": duration}, f)
        os.rename(result_path + ".part", result_path)
    except BaseException:  # noqa: BLE001
        code = 70
    os._exit(code)


class Pool:
    def __init__(self, session, items, jobs: int, tmpdir: str) -> None:  # noqa: ANN001
        self.session = session
        self.items = items
        self.jobs = jobs
        self.tmpdir = tmpdir
        self.running: dict[int, tuple[str | None, float, str, float]] = {}

    def spawn(self, mutant: str | None, timeout: float) -> None:
        result_path = os.path.join(self.tmpdir, f"r{len(self.running)}_{time.monotonic_ns()}.json")
        pid = os.fork()
        if pid == 0:
            run_child(self.session, self.items, mutant, result_path)
        try:
            os.setpgid(pid, pid)
        except OSError:
            pass
        self.running[pid] = (mutant, time.monotonic(), result_path, timeout)

    def reap(self, block: bool) -> list[tuple[str | None, dict]]:
        """Returns finished (mutant, outcome) pairs. outcome: {"kind": ok|timeout|error, ...}."""
        finished: list[tuple[str | None, dict]] = []
        while True:
            now = time.monotonic()
            for pid, (mutant, start, result_path, timeout) in list(self.running.items()):
                if now - start > timeout:
                    try:
                        os.killpg(pid, signal.SIGKILL)
                    except OSError:
                        pass
                    os.waitpid(pid, 0)
                    del self.running[pid]
                    failed, last_started = read_stream(result_path + ".stream")
                    _cleanup(result_path)
                    finished.append(
                        (
                            mutant,
                            {
                                "kind": "timeout",
                                "ms": round((now - start) * 1000),
                                "failed": failed,
                                "hung_in": last_started,
                            },
                        )
                    )
            while self.running:
                pid, status = os.waitpid(-1, os.WNOHANG)
                if pid == 0:
                    break
                if pid not in self.running:
                    continue
                mutant, start, result_path, _ = self.running.pop(pid)
                ms = round((time.monotonic() - start) * 1000)
                # kill any leftover grandchildren in the child's group
                try:
                    os.killpg(pid, signal.SIGKILL)
                except OSError:
                    pass
                exit_code = os.waitstatus_to_exitcode(status)
                if os.path.exists(result_path):
                    data = json.loads(Path(result_path).read_text())
                    _cleanup(result_path)
                    finished.append((mutant, {"kind": "ok", "ms": ms, **data}))
                else:
                    failed, last_started = read_stream(result_path + ".stream")
                    _cleanup(result_path)
                    finished.append(
                        (mutant, {"kind": "error", "ms": ms, "exit": exit_code, "failed": failed, "last_test": last_started})
                    )
            if finished or not block or not self.running:
                return finished
            time.sleep(0.005)


def _cleanup(path: str) -> None:
    for p in (path, path + ".part", path + ".stream"):
        try:
            os.unlink(p)
        except OSError:
            pass


class MatrixPlugin:
    def __init__(self) -> None:
        self.exit_status = 0

    @pytest.hookimpl(tryfirst=True)
    def pytest_runtestloop(self, session):  # noqa: ANN001
        items = list(session.items)
        log(f"collected {len(items)} tests; pytest args {PYTEST_ARGS}")
        gc.collect()
        gc.freeze()
        with tempfile.TemporaryDirectory(prefix="km_", dir=str(PROJECT)) as tmpdir:
            pool = Pool(session, items, ARGS.jobs, tmpdir)
            baseline = self.baseline(pool, items)
            self.matrix(pool, baseline)
        return True

    def baseline(self, pool: Pool, items) -> dict:  # noqa: ANN001
        if BASELINE.exists():
            baseline = json.loads(BASELINE.read_text())
            if baseline["tests"] == len(items):
                return baseline
            log("baseline test count changed; recomputing baseline")
        runs = []
        for _ in range(ARGS.baseline_runs):
            pool.spawn(None, timeout=3600)
        while pool.running:
            for _, outcome in pool.reap(block=True):
                if outcome["kind"] != "ok":
                    raise RuntimeError(f"baseline child failed: {outcome}")
                runs.append(outcome)
        failing = sorted(set().union(*(set(r["failed"]) for r in runs)))
        seconds = max(r["seconds"] for r in runs)
        baseline = {
            "tests": len(items),
            "test_nodeids": [it.nodeid for it in items],
            "failing": failing,
            "failing_per_run": [r["failed"] for r in runs],
            "run_seconds": [round(r["seconds"], 3) for r in runs],
            "child_wall_ms": [r["ms"] for r in runs],
            "timeout_seconds": round(10 * seconds + 10, 1),
            "pytest_args": PYTEST_ARGS,
        }
        BASELINE.write_text(json.dumps(baseline, indent=1) + "\n")
        log(f"baseline: {len(failing)} failing, {seconds:.2f}s per full run")
        return baseline

    def matrix(self, pool: Pool, baseline: dict) -> None:
        names = all_mutant_names()
        if ARGS.only:
            names = [n for n in names if n in set(ARGS.only)]
        shard, shards = (int(part) for part in ARGS.shard.split("/"))
        names = [n for at, n in enumerate(names) if at % shards == shard - 1]
        done = done_mutants()
        todo = [n for n in names if n not in done]
        log(f"{len(names)} mutants, {len(done)} done, {len(todo)} to do")
        baseline_failing = set(baseline["failing"])
        timeout = baseline["timeout_seconds"]
        started = time.monotonic()
        written = 0
        with open(OUT, "a") as out:
            queue = list(todo)
            while queue or pool.running:
                while queue and len(pool.running) < pool.jobs and time.monotonic() - started < ARGS.max_seconds:
                    pool.spawn(queue.pop(0), timeout=timeout)
                if not pool.running:
                    break
                for mutant, outcome in pool.reap(block=True):
                    rec: dict = {"mutant": mutant}
                    if outcome["kind"] == "ok":
                        killers = [n for n in outcome["failed"] if n not in baseline_failing]
                        rec["status"] = "killed" if killers else "survived"
                        rec["killers"] = killers
                    elif outcome["kind"] == "timeout":
                        # killers: tests that failed before the hang; hung_in: the test running when it was killed
                        rec["status"] = "timeout"
                        rec["killers"] = [n for n in outcome["failed"] if n not in baseline_failing]
                        rec["hung_in"] = outcome["hung_in"]
                    else:
                        rec["status"] = "error"
                        rec["killers"] = [n for n in outcome["failed"] if n not in baseline_failing]
                        rec["exit"] = outcome["exit"]
                        rec["last_test"] = outcome["last_test"]
                    rec["ms"] = outcome["ms"]
                    out.write(json.dumps(rec) + "\n")
                    out.flush()
                    written += 1
            log(f"chunk wrote {written} in {time.monotonic() - started:.0f}s; {len(queue)} left")


if __name__ == "__main__":
    os.chdir(SRC / "mutants")
    log(f"start pid {os.getpid()}")
    rc = pytest.main(list(PYTEST_ARGS), plugins=[MatrixPlugin()])
    log(f"pytest.main returned {int(rc)}")
    remaining = len([n for n in all_mutant_names() if n not in done_mutants()])
    print(f"remaining={remaining}")
    sys.exit(0)
