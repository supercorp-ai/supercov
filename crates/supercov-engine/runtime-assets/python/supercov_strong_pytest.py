"""Supercov's pytest plugin for a strong pass (`supercov_strong`).

Loaded through `PYTEST_PLUGINS` only in a strong pass: it runs the pass's
plan once pytest has collected the tests, in place of pytest's own loop.
"""

import os

import pytest

import supercov_strong


@pytest.hookimpl(tryfirst=True)
def pytest_cmdline_main(config):
    if os.environ.get(supercov_strong.PLAN_ENV):
        supercov_strong.configure_pytest(config)


def pytest_runtest_logreport(report):
    supercov_strong._reports.append(report)


@pytest.hookimpl(tryfirst=True)
def pytest_runtestloop(session):
    if not os.environ.get(supercov_strong.PLAN_ENV) or session.config.option.collectonly:
        return None
    return supercov_strong.run_pytest(session)
