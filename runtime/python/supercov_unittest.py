"""Supercov unittest adapter.

Installed by the runtime in every interpreter. It stays inert inside a pytest
process (the pytest adapter owns identity there, including pytest-run
`TestCase` classes) and otherwise assigns exact test and setup/call/teardown
identity around `unittest.TestCase` execution while recording the outcomes
`unittest.TestResult` reports. It computes no coverage.
"""

import unittest
import unittest.case

RUNNER = "unittest"
WORKER = "main"


class _State:
    __slots__ = ("test_id", "phase", "statuses", "xfail")

    def __init__(self, test_id):
        self.test_id = test_id
        self.phase = None
        self.statuses = {}
        self.xfail = False


_active = {}


def install(runtime):
    if getattr(unittest.case.TestCase, "_supercov_patched", False):
        return

    def inert():
        return runtime.pytest_active or runtime.closed

    def enter(test, phase):
        if inert():
            return
        state = _active.get(id(test))
        if state is None:
            state = _active[id(test)] = _State(test.id())
        state.phase = phase
        state.statuses.setdefault(phase, "passed")
        runtime.switch({"worker": WORKER, "test": state.test_id, "retry": 0, "phase": phase})

    def wrap_phase(name, phase):
        original = getattr(unittest.case.TestCase, name)

        def wrapper(self, *args, **kwargs):
            enter(self, phase)
            return original(self, *args, **kwargs)

        setattr(unittest.case.TestCase, name, wrapper)

    wrap_phase("_callSetUp", "setup")
    wrap_phase("_callTestMethod", "call")
    wrap_phase("_callTearDown", "teardown")

    def record(test, status, xfail=False):
        state = _active.get(id(test))
        if state is None or state.phase is None:
            return
        state.statuses[state.phase] = status
        state.xfail = state.xfail or xfail

    def wrap_result(name, status, xfail=False):
        original = getattr(unittest.TestResult, name)

        def wrapper(self, test, *args, **kwargs):
            if not inert():
                record(test, status, xfail)
            return original(self, test, *args, **kwargs)

        setattr(unittest.TestResult, name, wrapper)

    wrap_result("addError", "failed")
    wrap_result("addFailure", "failed")
    wrap_result("addSkip", "skipped")
    wrap_result("addExpectedFailure", "skipped", xfail=True)
    wrap_result("addUnexpectedSuccess", "passed", xfail=True)

    original_sub_test = unittest.TestResult.addSubTest

    def add_sub_test(self, test, subtest, err):
        if not inert() and err is not None:
            record(test, "failed")
        return original_sub_test(self, test, subtest, err)

    unittest.TestResult.addSubTest = add_sub_test

    original_stop = unittest.TestResult.stopTest

    def stop_test(self, test):
        if not inert():
            state = _active.pop(id(test), None)
            if state is not None:
                for phase in ("setup", "call", "teardown"):
                    status = state.statuses.get(phase)
                    if status is not None:
                        runtime.outcome(WORKER, state.test_id, 0, phase, status, state.xfail, RUNNER)
                runtime.switch(None)
        return original_stop(self, test)

    unittest.TestResult.stopTest = stop_test
    unittest.case.TestCase._supercov_patched = True
