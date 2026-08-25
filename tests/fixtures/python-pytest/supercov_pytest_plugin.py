import base64
import json
import os
from pathlib import Path

import coverage


RUN_ID = os.environ["SUPERCOV_RUN_ID"]
OUTCOME_BASE = Path(os.environ["SUPERCOV_PYTEST_OUTCOMES"])
WORKER_ID = os.environ.get("PYTEST_XDIST_WORKER", "main")
DATA_FILE = os.environ.get("SUPERCOV_PYTHON_DATA")
SOURCE = os.environ.get("SUPERCOV_PYTHON_SOURCE")
_OWNED_COVERAGE = None
_IS_XDIST_CONTROLLER = False


def pytest_configure(config):
    global _IS_XDIST_CONTROLLER, _OWNED_COVERAGE
    workers = getattr(config.option, "numprocesses", None)
    _IS_XDIST_CONTROLLER = WORKER_ID == "main" and workers not in (None, 0, "0")
    if _IS_XDIST_CONTROLLER:
        return
    if coverage.Coverage.current() is not None:
        return
    if not DATA_FILE or not SOURCE:
        raise RuntimeError(
            "Supercov pytest hook requires SUPERCOV_PYTHON_DATA and SUPERCOV_PYTHON_SOURCE"
        )
    _OWNED_COVERAGE = coverage.Coverage(
        data_file=DATA_FILE,
        data_suffix=True,
        branch=True,
        source=[SOURCE],
        config_file=False,
        context=f"supercov-worker-v1:{WORKER_ID}",
    )
    _OWNED_COVERAGE.start()


def pytest_unconfigure(config):
    del config
    if _OWNED_COVERAGE is not None:
        _OWNED_COVERAGE.stop()
        _OWNED_COVERAGE.save()


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
    if _IS_XDIST_CONTROLLER:
        return
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
