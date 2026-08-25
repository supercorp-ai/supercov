import base64
import json
import os
from pathlib import Path

import coverage


RUN_ID = os.environ["SUPERCOV_RUN_ID"]
OUTCOME_BASE = Path(os.environ["SUPERCOV_PYTEST_OUTCOMES"])
WORKER_ID = os.environ.get("PYTEST_XDIST_WORKER", "main")


def _context(nodeid: str, phase: str) -> str:
    value = {
        "runId": RUN_ID,
        "workerId": WORKER_ID,
        "testId": nodeid,
        "retry": 0,
        "phase": phase,
    }
    encoded = base64.urlsafe_b64encode(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).decode().rstrip("=")
    return f"supercov-v1:{encoded}"


def _switch(item, phase: str) -> None:
    current = coverage.Coverage.current()
    if current is None:
        raise RuntimeError("Supercov pytest hook ran without active coverage.py")
    current.switch_context(_context(item.nodeid, phase))


def pytest_runtest_setup(item):
    _switch(item, "setup")


def pytest_runtest_call(item):
    _switch(item, "call")


def pytest_runtest_teardown(item):
    _switch(item, "teardown")


def pytest_runtest_logreport(report):
    path = OUTCOME_BASE.with_name(f"{OUTCOME_BASE.name}.{WORKER_ID}.jsonl")
    record = {
        "runId": RUN_ID,
        "workerId": WORKER_ID,
        "testId": report.nodeid,
        "retry": 0,
        "phase": report.when,
        "outcome": report.outcome,
        "wasXfail": bool(getattr(report, "wasxfail", False)),
    }
    with path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n")
