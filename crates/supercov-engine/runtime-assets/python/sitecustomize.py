"""Supercov interpreter start-up hook.

Placed first on `PYTHONPATH` by the Supercov CLI so every interpreter the
wrapped test command launches installs the runtime before user code runs.
Any `sitecustomize` the environment already provides is imported afterwards
so its behaviour is preserved.
"""

import importlib.machinery
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))

import os as _os
if _os.environ.get("SUPERCOV_PYSPY"):
    import subprocess as _sp
    _sp.Popen(["sudo", "-n", _os.environ["SUPERCOV_PYSPY_BIN"], "record", "--pid", str(_os.getpid()), "--rate", "500", "--nonblocking", "--format", "raw", "-o", _os.path.join(_os.environ["SUPERCOV_PYSPY"], "%d.txt" % _os.getpid())], stdout=_sp.DEVNULL, stderr=_sp.DEVNULL, start_new_session=True)
    import time as _t; _t.sleep(0.3)

try:
    if os.environ.get("SUPERCOV_PYTHON_SCHEMATA"):
        # A strong pass: mutants behind switches, and no coverage.
        import supercov_strong

        supercov_strong.install()
    elif os.environ.get("SUPERCOV_PYTHON_PLAN"):
        import supercov_runtime

        supercov_runtime.install()
except Exception as error:  # noqa: BLE001 - never break the user's interpreter
    sys.stderr.write(f"[supercov] start-up hook failed: {error!r}\n")

try:
    _remaining = [
        entry
        for entry in sys.path
        if os.path.abspath(entry or os.getcwd()) != _HERE
    ]
    _spec = importlib.machinery.PathFinder.find_spec("sitecustomize", _remaining)
    if _spec is not None and _spec.loader is not None:
        try:
            # What `importlib.util.module_from_spec` is; importing that module
            # pulls in contextlib, a millisecond of every interpreter's start.
            from importlib._bootstrap import module_from_spec as _module_from_spec
        except ImportError:  # pragma: no cover
            from importlib.util import module_from_spec as _module_from_spec
        _module = _module_from_spec(_spec)
        _spec.loader.exec_module(_module)
except Exception as error:  # noqa: BLE001
    sys.stderr.write(f"[supercov] chained sitecustomize failed: {error!r}\n")
