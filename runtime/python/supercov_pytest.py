"""Supercov pytest adapter.

Loaded through the `PYTEST_PLUGINS` environment variable, so it activates
before conftest files import and the user's command stays untouched. It
assigns the exact worker, test, retry and setup/call/teardown identity before
each phase runs and records pytest's phase outcomes. It computes no coverage.
"""

import importlib
import os

import pytest

import supercov_runtime

_runtime = supercov_runtime.install()
_worker = os.environ.get("PYTEST_XDIST_WORKER", os.environ.get(supercov_runtime.WORKER_ENV, "main"))
_xdist_controller = False


def _retry_from_item(item) -> int:
    # pytest-rerunfailures sets `execution_count` before the ordinary phase
    # hooks run; plain pytest never defines it, so the first attempt is zero.
    execution_count = getattr(item, "execution_count", 1)
    try:
        return max(int(execution_count) - 1, 0)
    except (TypeError, ValueError):
        return 0


def _switch(item, phase: str) -> None:
    if _runtime is None or _xdist_controller:
        return
    _runtime.switch(
        {
            "worker": _worker,
            "test": item.nodeid,
            "retry": _retry_from_item(item),
            "phase": phase,
        }
    )


@pytest.hookimpl(tryfirst=True)
def pytest_load_initial_conftests(early_config, parser, args):
    """Name the bytecode cache for rewrites made with the assertion-pass hook.

    Supercov turns `enable_assertion_pass_hook` on through `PYTEST_ADDOPTS`,
    and pytest only calls `pytest_assertion_pass` from modules rewritten with
    it on -- but it caches rewritten modules by pytest version alone, so a
    module a plain run had cached would keep its silent bytecode, and a
    Supercov run would leave hook calls in the plain run's cache. Rewrites
    made with the hook on go under a name of their own; with the hook off
    (the user's own `-o` wins) the bytecode is pytest's, and shares its
    cache. This runs before pytest loads the first conftest, the first module
    it rewrites, and after the options are parsed, so the effective value is
    known. The name lives in pytest's private surface; when it is missing,
    plain `assert` stays unlinked and the run says so.
    """
    del parser, args
    if _runtime is None:
        return
    try:
        enabled = bool(early_config.getini("enable_assertion_pass_hook"))
        from _pytest.assertion import rewrite

        tail = rewrite.PYC_TAIL
        if enabled and "-supercov" not in tail:
            stem, extension = tail.rsplit(".", 1)
            rewrite.PYC_TAIL = f"{stem}-supercov.{extension}"
    except Exception as error:  # noqa: BLE001 - never break the user's test run
        _runtime.limitation(
            "python-pytest-assertion-hook-unavailable",
            f"this pytest exposes no rewrite cache Supercov can name ({error!r}); plain assert statements are not linked to assertions",
        )


def _hook_expectation_contexts() -> None:
    """Count `pytest.raises`, `pytest.warns` and `RaisesGroup` as assertions.

    They check without an `assert` statement, so the assertion-pass hook
    never sees them, and a test whose only check is an expected exception
    would link nothing. Their `__exit__` returning normally is the check
    passing; an unmatched exception propagates or fails through it.
    """
    if _runtime is None:
        return
    for module_name, class_name in (
        ("_pytest.raises", "RaisesExc"),
        ("_pytest.raises", "RaisesGroup"),
        ("_pytest.python_api", "RaisesContext"),
        ("_pytest.recwarn", "WarningsChecker"),
    ):
        try:
            cls = getattr(importlib.import_module(module_name), class_name)
            original = cls.__dict__["__exit__"]
        except Exception:  # noqa: BLE001 - this pytest has no such class
            continue

        def exit_and_mark(self, exc_type, exc_val, exc_tb, _original=original):
            __tracebackhide__ = True
            result = _original(self, exc_type, exc_val, exc_tb)
            if result is True or (result is None and exc_type is None):
                _runtime.assertion()
            return result

        cls.__exit__ = exit_and_mark


# At import, which `PYTEST_PLUGINS` places before any test runs.
_hook_expectation_contexts()


def pytest_assertion_pass(item, lineno, orig, expl):
    del item, lineno, orig, expl
    if _runtime is None or _xdist_controller:
        return
    if _runtime.assertion():
        # The phase is sampled; the remaining assertions of this test need
        # not build their explanation strings for a hook that ignores them.
        # pytest arms the hook again for the next test.
        try:
            from _pytest.assertion import util

            util._assertion_pass = None
        except Exception:  # noqa: BLE001
            pass


def pytest_configure(config):
    global _worker, _xdist_controller
    worker_input = getattr(config, "workerinput", None)
    if isinstance(worker_input, dict):
        _worker = str(worker_input.get("workerid", _worker))
    workers = getattr(config.option, "numprocesses", None)
    _xdist_controller = _worker == "main" and workers not in (None, 0, "0")
    if _runtime is not None:
        _runtime.set_worker(_worker)
        # pytest owns identity here, including unittest.TestCase classes it
        # runs; the unittest adapter stays inert in this process.
        _runtime.pytest_active = True


@pytest.hookimpl(tryfirst=True)
def pytest_runtest_setup(item):
    _switch(item, "setup")


@pytest.hookimpl(tryfirst=True)
def pytest_runtest_call(item):
    _switch(item, "call")


@pytest.hookimpl(tryfirst=True)
def pytest_runtest_teardown(item):
    _switch(item, "teardown")


def pytest_runtest_logreport(report):
    if _runtime is None:
        return
    if _xdist_controller:
        # Workers record their own phases; the controller only sees crashes,
        # which xdist reports with `when == "???"`.
        if report.when != "???":
            return
        node = getattr(report, "node", None)
        gateway = getattr(node, "gateway", None)
        worker = getattr(gateway, "id", None) or "unknown-worker"
        _runtime.outcome(worker, report.nodeid, 0, "call", "failed", False)
        return
    _runtime.outcome(
        _worker,
        report.nodeid,
        max(int(getattr(report, "rerun", 0) or 0), 0),
        report.when,
        report.outcome,
        bool(getattr(report, "wasxfail", False)),
    )


def pytest_runtest_logfinish(nodeid, location):
    del nodeid, location
    if _runtime is not None and not _xdist_controller:
        _runtime.switch(None)


def pytest_unconfigure(config):
    del config
    if _runtime is not None:
        _runtime.flush()
