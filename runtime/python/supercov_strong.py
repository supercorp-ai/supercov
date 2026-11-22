"""Strong mutation for Python: mutant schemata, the switch, and the driver.

`docs/design/python-mutants.md` and `docs/design/mutation-drivers.md` hold
the design. In a strong pass Supercov's `sitecustomize` installs this module
instead of the coverage runtime, in every interpreter the test command
starts:

- The schemata (`SUPERCOV_PYTHON_SCHEMATA`, written by the engine from
  `python_mutants.rs`) name, for each file the pass switches, the functions
  that hold mutants and each mutant's edit: the bytes it replaces and
  mutmut's text there. As such a file is imported, each of those functions
  gets one switch at its entry: the original body, or -- where the file's
  active mutant is one of the function's -- that mutant's copy of the body,
  parsed from the function's source with the edit applied. A copy is
  mutmut's mutated function, byte for byte where mutmut writes it, so the
  mutant behaves as mutmut's does. A mutant of a parameter's default is
  switched on by giving the function that default.
- The switch reads `strong_slot(file).a`, the active mutant's index in its
  file's list, or -1. `activate(file, index)` sets it; a process started with
  `SUPERCOV_ACTIVE_MUTANT=<file>#<index>` has it from its start. Each entry to
  a mutant's copy, and each iteration of a loop in it, counts against a
  limit (`SUPERCOV_MUTANT_HIT_LIMIT`, 1,000,000); past it the copy raises
  `MutantTimeoutError`, and the mutant is appended to the results'
  `.timeouts.jsonl`, so an endless loop the mutant made ends where it runs.
- The driver runs `SUPERCOV_STRONG_PLAN` once the runner has collected its
  tests (pytest's `pytest_runtestloop`, unittest's `TestProgram.runTests`):
  a forked baseline of the tests the pass needs, then one forked process per
  mutant with the mutant on, running its candidate tests in collection order
  and stopping at the first that fails. Each mutant's verdict is appended to
  `SUPERCOV_STRONG_RESULTS` as it is decided.

Stdlib only, CPython 3.9 or newer, POSIX (the driver forks).
"""

from __future__ import annotations

import ast
import json
import os
import sys
import time

SCHEMATA_ENV = "SUPERCOV_PYTHON_SCHEMATA"
PLAN_ENV = "SUPERCOV_STRONG_PLAN"
RESULTS_ENV = "SUPERCOV_STRONG_RESULTS"
ACTIVE_ENV = "SUPERCOV_ACTIVE_MUTANT"
LIMIT_ENV = "SUPERCOV_MUTANT_HIT_LIMIT"
WORKERS_ENV = "SUPERCOV_STRONG_WORKERS"
ONLY_ENV = "SUPERCOV_STRONG_ONLY"
TIMEOUT_CODE = "SUPERCOV_MUTANT_TIMEOUT"
DEFAULT_LIMIT = 1_000_000
SCHEMATA_VERSION = 1


class MutantTimeoutError(Exception):
    """A mutant's copy ran past the hit limit: the mutant made a loop that
    does not end."""

    code = TIMEOUT_CODE

    def __init__(self, mutant: str) -> None:
        super().__init__(f"{TIMEOUT_CODE}: mutant {mutant} passed its hit limit")
        self.mutant = mutant


def is_mutant_timeout(error: BaseException) -> bool:
    return getattr(error, "code", None) == TIMEOUT_CODE or TIMEOUT_CODE in str(error)


# -- the switch -----------------------------------------------------------------


class Slot:
    """One switched file's state: its active mutant's index, or -1."""

    __slots__ = ("file", "a", "count")

    def __init__(self, file: str) -> None:
        self.file = file
        self.a = -1
        self.count = 0

    def h(self) -> None:
        """A call of the active mutant's copy."""
        self.count += 1

    def over(self) -> int:
        """A loop of the active mutant's copy ran past the hit limit in one
        call: the mutant made it endless."""
        mutant = f"{self.file}#{self.a}"
        _state.note_timeout(mutant)
        raise MutantTimeoutError(mutant)

    def defaults(self, function, entries) -> None:
        """Register `function`'s default mutants: `(index, parameter,
        keyword_only, value)` each, `value` a thunk giving mutmut's default."""
        target = getattr(function, "__func__", function)
        for index, parameter, keyword_only, value in entries:
            _state.defaults.setdefault((self.file, index), []).append((target, parameter, keyword_only, value))
        if self.a >= 0:
            _state.apply_defaults(self.file, self.a)


class _State:
    def __init__(self) -> None:
        self.slots: dict[str, Slot] = {}
        self.limit = DEFAULT_LIMIT
        self.timeouts: str | None = None
        # (file, index) -> [(function, parameter, keyword_only, thunk)]
        self.defaults: dict = {}
        # Defaults replaced while a mutant is on, to put back.
        self.saved: list = []
        self.active: tuple[str, int] | None = None

    def slot(self, file: str) -> Slot:
        slot = self.slots.get(file)
        if slot is None:
            slot = self.slots[file] = Slot(file)
            if self.active is not None and self.active[0] == file:
                slot.a = self.active[1]
        return slot

    def note_timeout(self, mutant: str) -> None:
        if not self.timeouts:
            return
        try:
            with open(self.timeouts, "a", encoding="utf-8") as stream:
                stream.write(json.dumps({"mutant": mutant, "attemptId": None, "pass": "main"}) + "\n")
        except OSError:
            pass

    def apply_defaults(self, file: str, index: int) -> None:
        for function, parameter, keyword_only, thunk in self.defaults.get((file, index), ()):
            value = thunk()
            if keyword_only:
                current = dict(function.__kwdefaults__ or {})
                self.saved.append((function, "__kwdefaults__", function.__kwdefaults__))
                current[parameter] = value
                function.__kwdefaults__ = current
                continue
            code = function.__code__
            positional = code.co_varnames[: code.co_argcount]
            defaults = list(function.__defaults__ or ())
            first = len(positional) - len(defaults)
            at = positional.index(parameter) - first
            self.saved.append((function, "__defaults__", function.__defaults__))
            defaults[at] = value
            function.__defaults__ = tuple(defaults)

    def restore_defaults(self) -> None:
        while self.saved:
            function, attribute, value = self.saved.pop()
            setattr(function, attribute, value)


_state = _State()


def strong_slot(file: str) -> Slot:
    """The switch state of a switched file: what each of its switches reads."""
    return _state.slot(file)


def activate(file: str, index: int) -> None:
    """Switch mutant `index` of `file` on, and every other off."""
    deactivate()
    _state.active = (file, index)
    slot = _state.slot(file)
    slot.a = index
    slot.count = 0
    _state.apply_defaults(file, index)
    os.environ[ACTIVE_ENV] = f"{file}#{index}"


def deactivate() -> None:
    _state.restore_defaults()
    if _state.active is not None:
        slot = _state.slots.get(_state.active[0])
        if slot is not None:
            slot.a = -1
            slot.count = 0
    _state.active = None
    os.environ.pop(ACTIVE_ENV, None)


def set_hit_limit(limit: int) -> None:
    _state.limit = limit


def hit_count() -> int:
    if _state.active is None:
        return 0
    slot = _state.slots.get(_state.active[0])
    return 0 if slot is None else slot.count


# -- the schemata -------------------------------------------------------------------


def _alias(tree: ast.AST) -> str:
    taken = set()
    for node in ast.walk(tree):
        for field in ("id", "arg", "name", "asname", "attr"):
            value = getattr(node, field, None)
            if isinstance(value, str):
                taken.add(value)
    candidate, counter = "_scv_strong", 0
    while candidate in taken:
        counter += 1
        candidate = f"_scv_strong{counter}"
    return candidate


def _line_starts(source: bytes) -> list:
    starts = [0]
    at = source.find(b"\n")
    while at != -1:
        starts.append(at + 1)
        at = source.find(b"\n", at + 1)
    return starts


def _copy_of(source: bytes, starts: list, unit: dict, mutant: dict):
    """The unit's function with the mutant's edit applied, parsed, its lines
    numbered as the file's."""
    line, column = unit["line"], unit["column"]
    begin = starts[line - 1]
    start, end = mutant["at"]
    region = source[begin:start] + mutant["text"].encode("utf-8") + source[end : unit["end"]]
    if column > 0:
        tree = ast.parse(b"if 1:\n" + region)
        function = tree.body[0].body[0]
        ast.increment_lineno(function, line - 2)
    else:
        tree = ast.parse(region)
        function = tree.body[0]
        ast.increment_lineno(function, line - 1)
    if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
        raise SyntaxError("the edited function is no function")
    return function


class _Scopes(ast.NodeTransformer):
    """Takes a body's `global` and `nonlocal` declarations out, not entering
    nested scopes: they are declared once at the switched function's top."""

    def __init__(self) -> None:
        self.globals: list = []
        self.nonlocals: list = []

    def visit_Global(self, node):
        self.globals.extend(name for name in node.names if name not in self.globals)
        return ast.copy_location(ast.Pass(), node)

    def visit_Nonlocal(self, node):
        self.nonlocals.extend(name for name in node.names if name not in self.nonlocals)
        return ast.copy_location(ast.Pass(), node)

    def visit_FunctionDef(self, node):
        return node

    visit_AsyncFunctionDef = visit_FunctionDef
    visit_ClassDef = visit_FunctionDef
    visit_Lambda = visit_FunctionDef


class _Loops(ast.NodeTransformer):
    """Counts each iteration of a copy's loops, in one call of the function,
    against the hit limit: `<counter> += 1` and the check first in each loop
    body. Nested functions count their own."""

    def __init__(self, count) -> None:
        self.count = count

    def _count(self, node):
        self.generic_visit(node)
        node.body[0:0] = self.count(node)
        return node

    visit_For = visit_AsyncFor = visit_While = _count

    def visit_FunctionDef(self, node):
        return node

    visit_AsyncFunctionDef = visit_Lambda = visit_ClassDef = visit_FunctionDef


def _is_docstring(statement) -> bool:
    return (
        isinstance(statement, ast.Expr)
        and isinstance(statement.value, ast.Constant)
        and isinstance(statement.value.value, str)
    )


def _locate(node: ast.AST, tree: ast.AST) -> ast.AST:
    """Gives every generated node in `tree` `node`'s position."""
    for child in ast.walk(tree):
        if isinstance(child, (ast.expr, ast.stmt)) and not hasattr(child, "end_lineno"):
            ast.copy_location(child, node)
    return ast.fix_missing_locations(tree)


def instrument(tree: ast.Module, source: bytes, relative: str, plan: dict) -> list:
    """Puts the plan's switches into a parsed module, in place. The indexes
    of mutants that could not be switched are returned."""
    alias = _alias(tree)
    starts = _line_starts(source)
    units = {(unit["line"], unit["column"]): unit for unit in plan["units"]}
    failed: list = []

    def slot_attribute(name, node):
        return ast.copy_location(
            ast.Attribute(value=ast.copy_location(ast.Name(id=alias, ctx=ast.Load()), node), attr=name, ctx=ast.Load()),
            node,
        )

    def hit(node):
        call = ast.Call(func=slot_attribute("h", node), args=[], keywords=[])
        return ast.fix_missing_locations(ast.copy_location(ast.Expr(value=ast.copy_location(call, node)), node))

    def switch(function, unit):
        copies = {}
        defaults = []
        for mutant in unit["mutants"]:
            if mutant.get("default") is not None:
                parameter, keyword_only = mutant["default"]
                try:
                    value = ast.parse(mutant["text"], mode="eval").body
                except SyntaxError:
                    failed.append(mutant["index"])
                    continue
                defaults.append((mutant["index"], parameter, keyword_only, value))
                continue
            try:
                copy = _copy_of(source, starts, unit, mutant)
            except (SyntaxError, ValueError, IndexError):
                failed.append(mutant["index"])
                continue
            copies[mutant["index"]] = copy.body
        if copies:
            scopes = _Scopes()
            original = function.body
            leading = [original[0]] if original and _is_docstring(original[0]) else []
            rest = [scopes.visit(statement) for statement in original[len(leading) :]]
            branches = {}
            counter = f"{alias}_n"

            def count(node):
                # `<counter> += 1`, and past the limit the slot's `over()`.
                increment = ast.AugAssign(
                    target=ast.Name(id=counter, ctx=ast.Store()), op=ast.Add(), value=ast.Constant(value=1)
                )
                check = ast.If(
                    test=ast.Compare(
                        left=ast.Name(id=counter, ctx=ast.Load()),
                        ops=[ast.Gt()],
                        comparators=[ast.Constant(value=_state.limit)],
                    ),
                    body=[ast.Expr(value=ast.Call(func=slot_attribute("over", node), args=[], keywords=[]))],
                    orelse=[],
                )
                return [_locate(node, increment), _locate(node, check)]

            loops = _Loops(count)
            for index, body in copies.items():
                body = [loops.visit(scopes.visit(statement)) for statement in body]
                start = ast.Assign(targets=[ast.Name(id=counter, ctx=ast.Store())], value=ast.Constant(value=0))
                branches[index] = [hit(function), _locate(function, start)] + body
            declared = []
            if scopes.globals:
                declared.append(ast.copy_location(ast.Global(names=scopes.globals), function))
            if scopes.nonlocals:
                declared.append(ast.copy_location(ast.Nonlocal(names=scopes.nonlocals), function))

            def tree_of(indexes):
                if len(indexes) == 1:
                    return branches[indexes[0]]
                half = len(indexes) // 2
                test = ast.Compare(
                    left=slot_attribute("a", function), ops=[ast.Lt()], comparators=[ast.Constant(value=indexes[half])]
                )
                return [
                    ast.copy_location(
                        ast.If(test=ast.copy_location(test, function), body=tree_of(indexes[:half]), orelse=tree_of(indexes[half:])),
                        function,
                    )
                ]

            indexes = sorted(branches)
            guard = ast.Compare(
                left=slot_attribute("a", function),
                ops=[ast.In()],
                comparators=[ast.Set(elts=[ast.Constant(value=index) for index in indexes])],
            )
            dispatch = ast.copy_location(
                ast.If(test=ast.copy_location(guard, function), body=tree_of(indexes), orelse=rest or [ast.Pass()]),
                function,
            )
            function.body = leading + declared + [dispatch]
            ast.fix_missing_locations(function)
        return defaults

    def walk(body: list, top: bool) -> None:
        index = 0
        while index < len(body):
            statement = body[index]
            if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef)):
                unit = units.get((statement.lineno, statement.col_offset))
                if unit is not None:
                    defaults = switch(statement, unit)
                    if defaults:
                        entries = ast.Tuple(
                            elts=[
                                ast.Tuple(
                                    elts=[
                                        ast.Constant(value=mutant),
                                        ast.Constant(value=parameter),
                                        ast.Constant(value=keyword_only),
                                        ast.Lambda(
                                            args=ast.arguments(
                                                posonlyargs=[], args=[], vararg=None, kwonlyargs=[],
                                                kw_defaults=[], kwarg=None, defaults=[],
                                            ),
                                            body=value,
                                        ),
                                    ],
                                    ctx=ast.Load(),
                                )
                                for mutant, parameter, keyword_only, value in defaults
                            ],
                            ctx=ast.Load(),
                        )
                        call = ast.Call(
                            func=slot_attribute("defaults", statement),
                            args=[ast.Name(id=statement.name, ctx=ast.Load()), entries],
                            keywords=[],
                        )
                        registration = ast.Expr(value=call)
                        ast.copy_location(registration, statement)
                        _locate(statement, registration)
                        body.insert(index + 1, registration)
                        index += 1
            elif top and isinstance(statement, ast.ClassDef):
                walk(statement.body, False)
            index += 1

    walk(tree.body, True)
    # `<alias> = supercov_strong.strong_slot("<file>")`, after the docstring
    # and the `__future__` imports.
    at = 0
    if tree.body and _is_docstring(tree.body[0]):
        at = 1
    while at < len(tree.body) and isinstance(tree.body[at], ast.ImportFrom) and tree.body[at].module == "__future__":
        at += 1
    anchor = tree.body[at] if at < len(tree.body) else (tree.body[-1] if tree.body else None)
    binding = ast.parse(f"from supercov_strong import strong_slot as {alias}\n{alias} = {alias}({relative!r})").body
    for statement in binding:
        for node in ast.walk(statement):
            if anchor is not None:
                ast.copy_location(node, anchor)
    ast.fix_missing_locations(ast.Module(body=binding, type_ignores=[]))
    tree.body[at:at] = binding
    return failed


# -- import interception ------------------------------------------------------------


class _Schemata:
    def __init__(self, path: str) -> None:
        with open(path, encoding="utf-8") as stream:
            plan = json.load(stream)
        if plan.get("version") != SCHEMATA_VERSION:
            raise RuntimeError(f"unsupported Supercov schemata version {plan.get('version')!r}")
        self.root = os.path.realpath(plan["root"])
        self.files = plan["files"]
        # Mutants a file could not switch: reported by the driver as errors.
        self.failed: dict = {}
        # Each switched file's code, by its path: compiled once, before the
        # driver forks, and shared by every fork.
        self.code: dict = {}

    def relative(self, filename: str | None) -> str | None:
        if not filename or filename.startswith("<"):
            return None
        try:
            real = os.path.realpath(filename)
        except OSError:
            return None
        if not real.startswith(self.root + os.sep):
            return None
        relative = real[len(self.root) + 1 :].replace(os.sep, "/")
        return relative if relative in self.files else None

    def compile(self, source: bytes, filename: str, relative: str):
        cached = self.code.get(relative)
        if cached is not None and cached[0] == source and cached[1] == filename:
            return cached[2]
        code = self._compile(source, filename, relative)
        self.code[relative] = (source, filename, code)
        return code

    def precompile(self) -> None:
        """Every switched file's code, so that a mutant whose switch could
        not be placed is known before it runs."""
        for relative in self.files:
            filename = os.path.join(self.root, *relative.split("/"))
            try:
                with open(filename, "rb") as stream:
                    source = stream.read()
            except OSError:
                self.failed[relative] = "changed"
                continue
            try:
                self.compile(source, filename, relative)
            except SyntaxError:
                self.failed[relative] = "changed"

    def _compile(self, source: bytes, filename: str, relative: str):
        import zlib

        plan = self.files[relative]
        if len(source) != plan["size"] or zlib.crc32(source) != plan["crc32"]:
            self.failed[relative] = "changed"
            return compile(source, filename, "exec", dont_inherit=True)
        tree = ast.parse(source, filename=filename)
        failed = instrument(tree, source, relative, plan)
        if failed:
            self.failed[relative] = failed
        return compile(tree, filename, "exec", dont_inherit=True)


class _Loader:
    def __init__(self, inner, schemata: _Schemata, relative: str) -> None:
        self._inner = inner
        self._schemata = schemata
        self._relative = relative

    def __getattr__(self, name):
        return getattr(self._inner, name)

    def create_module(self, spec):
        create = getattr(self._inner, "create_module", None)
        return None if create is None else create(spec)

    def get_code(self, fullname):
        filename = self._inner.get_filename(fullname)
        return self._schemata.compile(self._inner.get_data(filename), filename, self._relative)

    def exec_module(self, module) -> None:
        filename = module.__spec__.origin
        code = self._schemata.compile(self._inner.get_data(filename), filename, self._relative)
        exec(code, module.__dict__)


class _Finder:
    def __init__(self, schemata: _Schemata) -> None:
        self._schemata = schemata

    def find_spec(self, fullname, path, target=None):
        for finder in sys.meta_path:
            if finder is self:
                continue
            find = getattr(finder, "find_spec", None)
            if find is None:
                continue
            spec = find(fullname, path, target)
            if spec is None:
                continue
            relative = self._schemata.relative(spec.origin)
            if relative is not None and spec.loader is not None and hasattr(spec.loader, "get_data"):
                spec.loader = _Loader(spec.loader, self._schemata, relative)
                spec.cached = None
            return spec
        return None

    def invalidate_caches(self) -> None:
        pass


_schemata: _Schemata | None = None


def install() -> None:
    """Arms the schemata in this interpreter: from `sitecustomize`."""
    global _schemata
    if _schemata is not None:
        return
    path = os.environ.get(SCHEMATA_ENV)
    if not path:
        return
    _schemata = _Schemata(path)
    limit = os.environ.get(LIMIT_ENV)
    if limit:
        _state.limit = int(limit)
    results = os.environ.get(RESULTS_ENV)
    if results:
        _state.timeouts = results + ".timeouts.jsonl"
    active = os.environ.get(ACTIVE_ENV, "-")
    if active and active != "-" and "#" in active:
        file, _, index = active.rpartition("#")
        _state.active = (file, int(index))
    sys.meta_path.insert(0, _Finder(_schemata))
    if os.environ.get(PLAN_ENV):
        _patch_unittest()


# -- the driver -----------------------------------------------------------------


class Plan:
    def __init__(self, path: str) -> None:
        with open(path, encoding="utf-8") as stream:
            plan = json.load(stream)
        self.tests = plan["tests"]
        self.mutants = [mutant for mutant in plan["mutants"] if mutant.get("decided") is None]


def _workers() -> int:
    configured = os.environ.get(WORKERS_ENV)
    if configured:
        return max(1, int(configured))
    try:
        return max(1, len(os.sched_getaffinity(0)))
    except AttributeError:
        return max(1, os.cpu_count() or 1)


class _Child:
    """A forked process running tests, and what it reported so far."""

    def __init__(self, pid, reader, job, limit_s) -> None:
        self.pid = pid
        self.reader = reader
        self.job = job
        self.started = time.monotonic()
        self.limit_s = limit_s
        self.buffer = b""
        self.events: list = []
        self.ended = False
        self.status = None


class Driver:
    """Runs a pass's mutants against the tests a runner collected.

    `runner.items` are the collected tests in collection order, each with
    `runner.test_id(item)`; `runner.run(items, report)` runs them in this
    process in that order, calling `report(position, outcome, ms)` after each,
    and stops at the first whose outcome is `failed` when told to.
    """

    def __init__(self, plan: Plan, runner, results: str) -> None:
        self.plan = plan
        self.runner = runner
        self.results = results
        self.timeouts = results + ".timeouts.jsonl"
        self.position = {runner.test_id(item): at for at, item in enumerate(runner.items)}
        # Whether a test fails without a mutant after given tests, by the
        # tests before it and it: what a confirmation found.
        self.confirmed: dict = {}
        self.summary = {
            "summary": True,
            "runner": runner.name,
            "mutants": 0,
            "forks": 0,
            "fresh": 0,
            "confirmations": 0,
            "overturned": 0,
            "baselineMs": 0,
        }

    # A test's place in collection order, or None where this runner did not
    # collect it.
    def _place(self, test: int):
        return self.position.get(self.plan.tests[test]["id"])

    def _write(self, record: dict) -> None:
        with open(self.results, "a", encoding="utf-8") as stream:
            stream.write(json.dumps(record) + "\n")

    def _verdict(self, mutant, status, *, killed_by=None, tests=0, started=None, reason=None) -> None:
        self.summary["mutants"] += 1
        self._write(
            {
                "id": mutant["id"],
                "status": status,
                "killedBy": killed_by,
                "tests": tests,
                "durationMs": int((time.monotonic() - started) * 1000) if started else 0,
                "reason": reason,
            }
        )

    def _timed_out(self, mutant) -> bool:
        name = f"{mutant['file']}#{mutant['index']}"
        try:
            with open(self.timeouts, encoding="utf-8") as stream:
                return any(json.loads(line).get("mutant") == name for line in stream if line.strip())
        except (OSError, ValueError):
            return False

    def _fork(self, job, places, stop_at_failure, limit_s):
        reader, writer = os.pipe()
        pid = os.fork()
        if pid == 0:  # pragma: no cover - the child reports through the pipe
            os.close(reader)
            code = 0
            try:
                devnull = os.open(os.devnull, os.O_RDWR)
                os.dup2(devnull, 1)
                os.dup2(devnull, 2)
                if job["mutant"] is not None:
                    activate(job["mutant"]["file"], job["mutant"]["index"])
                items = [self.runner.items[place] for place in places]

                def report(at, outcome, ms):
                    os.write(writer, f"{places[at]} {outcome} {ms}\n".encode())

                self.runner.run(items, report, stop_at_failure)
                os.write(writer, b"end\n")
            except BaseException:  # noqa: BLE001 - the parent reads the missing end
                code = 1
            finally:
                os._exit(code)
        os.close(writer)
        os.set_blocking(reader, False)
        self.summary["forks"] += 1
        return _Child(pid, reader, job, limit_s)

    def _spawn_fresh(self, job, places, limit_s):
        """A mutant whose code ran while modules loaded: a new interpreter of
        the same command, with the mutant on from its start."""
        import subprocess

        reader, writer = os.pipe()
        environment = dict(os.environ)
        environment[ACTIVE_ENV] = f"{job['mutant']['file']}#{job['mutant']['index']}"
        environment[ONLY_ENV] = json.dumps({"places": places, "fd": writer, "ids": [self.runner.test_id(self.runner.items[p]) for p in places]})
        arguments = list(getattr(sys, "orig_argv", None) or [sys.executable] + sys.argv)
        arguments[0] = sys.executable
        process = subprocess.Popen(
            arguments,
            env=environment,
            pass_fds=(writer,),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        os.close(writer)
        os.set_blocking(reader, False)
        self.summary["fresh"] += 1
        child = _Child(process.pid, reader, job, limit_s)
        child.process = process
        return child

    def _poll(self, child: _Child) -> bool:
        """Reads what the child reported; whether it ended."""
        while True:
            try:
                chunk = os.read(child.reader, 65536)
            except BlockingIOError:
                break
            if not chunk:
                break
            child.buffer += chunk
        *lines, child.buffer = child.buffer.split(b"\n")
        for line in lines:
            if line == b"end":
                child.ended = True
                continue
            place, outcome, ms = line.decode().split(" ")
            child.events.append((int(place), outcome, int(ms)))
        process = getattr(child, "process", None)
        if process is not None:
            done = process.poll()
            if done is None:
                return False
            child.status = done
        else:
            try:
                pid, status = os.waitpid(child.pid, os.WNOHANG)
            except ChildProcessError:
                pid, status = child.pid, 0
            if pid == 0:
                return False
            child.status = status
        # Whatever it wrote before it ended.
        while True:
            try:
                chunk = os.read(child.reader, 65536)
            except (BlockingIOError, OSError):
                break
            if not chunk:
                break
            child.buffer += chunk
        *lines, child.buffer = child.buffer.split(b"\n")
        for line in lines:
            if line == b"end":
                child.ended = True
            elif line:
                place, outcome, ms = line.decode().split(" ")
                child.events.append((int(place), outcome, int(ms)))
        os.close(child.reader)
        return True

    def _kill(self, child: _Child) -> None:
        import signal

        try:
            os.kill(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process = getattr(child, "process", None)
        if process is not None:
            process.wait()
        else:
            try:
                os.waitpid(child.pid, 0)
            except ChildProcessError:
                pass
        try:
            self._poll_closed(child)
        except OSError:
            pass

    def _poll_closed(self, child):
        while True:
            try:
                chunk = os.read(child.reader, 65536)
            except BlockingIOError:
                break
            if not chunk:
                break
            child.buffer += chunk
        for line in child.buffer.split(b"\n"):
            if line and line != b"end":
                place, outcome, ms = line.decode().split(" ")
                child.events.append((int(place), outcome, int(ms)))
        os.close(child.reader)

    def _run_jobs(self, jobs, on_done) -> None:
        """Runs jobs, `workers` at a time, each in a fork (or a fresh
        interpreter), calling `on_done(job, child, timed_out)` as each ends."""
        workers = _workers()
        pending = list(jobs)
        running: list = []
        while pending or running:
            while pending and len(running) < workers:
                job = pending.pop(0)
                job.setdefault("began", time.monotonic())
                if job.get("fresh"):
                    running.append(self._spawn_fresh(job, job["places"], job["limit"]))
                else:
                    running.append(self._fork(job, job["places"], job["stop"], job["limit"]))
            time.sleep(0.002)
            still = []
            for child in running:
                if self._poll(child):
                    more = on_done(child.job, child, False)
                    pending[0:0] = more or []
                elif time.monotonic() - child.started > child.limit_s:
                    self._kill(child)
                    more = on_done(child.job, child, True)
                    pending[0:0] = more or []
                else:
                    still.append(child)
            running = still

    def run(self) -> None:
        started = time.monotonic()
        plan = self.plan
        candidates = {}
        for mutant in plan.mutants:
            places = []
            # A test the measured run skipped or failed runs too: the
            # baseline says whether it passes without the mutant, and a
            # mutant can turn a skip into a failure (a subtest skipped
            # before a failing assertion).
            for candidate in mutant["candidates"]:
                place = self._place(candidate["test"])
                if place is not None:
                    places.append(place)
            candidates[mutant["id"]] = sorted(set(places))
        if _schemata is not None:
            _schemata.precompile()
        failed_switches = _schemata.failed if _schemata is not None else {}
        # The baseline: every test some mutant runs, without a mutant.
        union = sorted({place for places in candidates.values() for place in places})
        baseline: dict = {}
        if union:
            limit = 600.0
            done = {}

            def on_baseline(job, child, timed_out):
                done["child"] = child
                done["timed_out"] = timed_out

            self._run_jobs([{"mutant": None, "places": union, "stop": False, "limit": limit}], on_baseline)
            for place, outcome, ms in done["child"].events:
                baseline[place] = (outcome, ms)
        self.summary["baselineMs"] = int((time.monotonic() - started) * 1000)
        failing = {place for place, (outcome, _) in baseline.items() if outcome == "failed"}
        jobs = []
        for mutant in plan.mutants:
            switched = failed_switches.get(mutant["file"])
            if switched == "changed" or (isinstance(switched, list) and mutant["index"] in switched):
                self._verdict(mutant, "error", reason="its switch could not be placed: the file changed since the run, or its edit does not parse")
                continue
            places = candidates[mutant["id"]]
            if not places:
                ran_elsewhere = any(
                    self._place(candidate["test"]) is None for candidate in mutant["candidates"]
                )
                if ran_elsewhere:
                    self._verdict(mutant, "error", reason=f"its tests are not ones this {self.runner.name} run collected")
                else:
                    self._verdict(mutant, "survived", reason="no-passing-candidate")
                continue
            if baseline and all(place in failing for place in places):
                self._verdict(mutant, "survived", reason="every test that reaches it fails without it too")
                continue
            places = [place for place in places if place not in failing and (not baseline or place in baseline)]
            if baseline and not places:
                self._verdict(mutant, "error", reason="its tests did not finish without the mutant")
                continue
            budget = sum(baseline.get(place, ("passed", 1000))[1] for place in places)
            fresh = any(candidate["reason"] in ("module-load", "unattributed") for candidate in mutant["candidates"])
            jobs.append(
                {
                    "mutant": mutant,
                    "places": places,
                    "stop": True,
                    "limit": 3 * budget / 1000 + 10 + (5 if fresh else 0),
                    "fresh": fresh,
                }
            )

        def on_mutant(job, child, timed_out):
            mutant = job["mutant"]
            ran = len(child.events)
            failure = next((event for event in child.events if event[1] == "failed"), None)
            if job.get("confirming"):
                return self._confirmed(job, child, failure)
            if self._timed_out(mutant):
                self._verdict(mutant, "timeout", tests=ran, started=job["began"], reason="it passed its hit limit: a loop that does not end")
                return None
            if failure is not None:
                place = failure[0]
                # A kill counts at once where the baseline ran the same tests
                # before the killing one; otherwise it is confirmed.
                before = [p for p in job["places"] if p < place]
                baseline_before = [p for p in union if p < place]
                if before != baseline_before:
                    key = (tuple(before), place)
                    known = self.confirmed.get(key)
                    if known is not None:
                        # Another mutant's kill already ran these tests
                        # without a mutant.
                        return self._settle(job, place, ran, fails_alone=known)
                    self.summary["confirmations"] += 1
                    return [
                        {
                            "mutant": None,
                            "confirming": job,
                            "killer": place,
                            "places": before + [place],
                            "stop": False,
                            "limit": job["limit"],
                        }
                    ]
                self._verdict(mutant, "killed", killed_by=self.runner.test_id(self.runner.items[place]), tests=ran, started=job["began"])
                return None
            if timed_out:
                self._verdict(mutant, "timeout", tests=ran, started=job["began"], reason="its tests ran past their time limit")
                return None
            if not child.ended:
                at = job["places"][ran] if ran < len(job["places"]) else None
                test = self.runner.test_id(self.runner.items[at]) if at is not None else None
                self._verdict(
                    mutant,
                    "killed",
                    killed_by=test,
                    tests=ran + 1,
                    started=job["began"],
                    reason="the test process ended before its tests did",
                )
                return None
            self._verdict(mutant, "survived", tests=ran, started=job["began"])
            return None

        self._run_jobs(jobs, on_mutant)
        self.summary["durationMs"] = int((time.monotonic() - started) * 1000)
        self._write(self.summary)

    def _confirmed(self, confirming, child, failure):
        job = confirming["confirming"]
        killer = confirming["killer"]
        fails_alone = not child.ended or any(
            place == killer and outcome == "failed" for place, outcome, _ in child.events
        )
        self.confirmed[(tuple(confirming["places"][:-1]), killer)] = fails_alone
        return self._settle(job, killer, len(confirming["places"]), fails_alone=fails_alone)

    def _settle(self, job, killer, ran, *, fails_alone):
        """A kill by `killer`, where the same tests before it ran without the
        mutant: it stands unless the test fails that way too."""
        mutant = job["mutant"]
        if not fails_alone:
            self._verdict(mutant, "killed", killed_by=self.runner.test_id(self.runner.items[killer]), tests=ran, started=job["began"])
            return None
        # The test fails without the mutant after the same tests: no kill.
        # The mutant runs again without it.
        self.summary["overturned"] += 1
        rest = [place for place in job["places"] if place != killer]
        if not rest:
            self._verdict(mutant, "survived", tests=len(job["places"]), started=job["began"], reason="its only killing test fails without it after the same tests")
            return None
        return [dict(job, places=rest)]


def _only():
    """In a fresh interpreter a driver started for one mutant: the places of
    its tests and the descriptor to report on."""
    only = os.environ.get(ONLY_ENV)
    return None if not only else json.loads(only)


def run_only(runner) -> None:
    """Runs one mutant's tests in this interpreter, the mutant on from its
    start, and reports each to the driver that started it."""
    only = _only()
    position = {runner.test_id(item): item for item in runner.items}
    writer = only["fd"]
    places = only["places"]
    missing = [at for at, test in enumerate(only["ids"]) if test not in position]
    if missing:
        # A test the driver collected without the mutant is not there with
        # it: its module failed to load, or its parameters changed. The
        # suite fails where it ran it.
        os.write(writer, f"{places[missing[0]]} failed 0\n".encode())
        os.write(writer, b"end\n")
        return
    items = [position[test] for test in only["ids"]]

    def report(at, outcome, ms):
        os.write(writer, f"{places[at]} {outcome} {ms}\n".encode())

    runner.run(items, report, True)
    os.write(writer, b"end\n")


# -- pytest -----------------------------------------------------------------------


class _PytestRunner:
    name = "pytest"

    def __init__(self, session) -> None:
        self.session = session
        self.items = list(session.items)

    @staticmethod
    def test_id(item) -> str:
        return item.nodeid

    def run(self, items, report, stop_at_failure) -> None:
        for at, item in enumerate(items):
            nextitem = items[at + 1] if at + 1 < len(items) else None
            _reports.clear()
            started = time.monotonic()
            item.ihook.pytest_runtest_protocol(item=item, nextitem=nextitem)
            outcome = "failed" if any(report.failed for report in _reports) else "passed"
            report(at, outcome, int((time.monotonic() - started) * 1000))
            if outcome == "failed" and stop_at_failure:
                return


_reports: list = []


def configure_pytest(config) -> None:
    """pytest's options a pass overrides: it forks after collection in this
    process, so pytest-xdist's workers must not collect and run elsewhere,
    and no failure may end the session."""
    for option, value in (("numprocesses", 0), ("dist", "no"), ("tx", []), ("maxfail", 0), ("exitfirst", False)):
        if hasattr(config.option, option):
            setattr(config.option, option, value)


def run_pytest(session) -> bool:
    """The pass, in place of pytest's loop over its items."""
    runner = _PytestRunner(session)
    if _only() is not None:
        run_only(runner)
        return True
    Driver(Plan(os.environ[PLAN_ENV]), runner, os.environ[RESULTS_ENV]).run()
    return True


# -- unittest ----------------------------------------------------------------------


class _UnittestRunner:
    name = "unittest"

    def __init__(self, suite) -> None:
        self.items = list(_flatten(suite))

    @staticmethod
    def test_id(item) -> str:
        return item.id()

    def run(self, items, report, stop_at_failure) -> None:
        import unittest

        suite = unittest.TestSuite(items)
        result = _Outcomes(items, report, stop_at_failure)
        suite.run(result)
        result.finish()


def _flatten(suite):
    import unittest

    for test in suite:
        if isinstance(test, unittest.TestSuite):
            yield from _flatten(test)
        else:
            yield test


try:
    import unittest as _unittest_module

    _TestResult = _unittest_module.TestResult
except Exception:  # pragma: no cover
    _TestResult = object


class _Outcomes(_TestResult):
    """Each test's outcome as it ends: failed where it failed, erred, passed
    unexpectedly, or a class or module fixture around it failed."""

    def __init__(self, items, report, stop_at_failure) -> None:
        super().__init__()
        self.order = {id(item): at for at, item in enumerate(items)}
        self.items = items
        self.report = report
        self.stop_at_failure = stop_at_failure
        self.current = None
        self.failed = False
        self.started = 0.0
        self.reported: set = set()

    def startTest(self, test):
        super().startTest(test)
        self.current = test
        self.failed = False
        self.started = time.monotonic()

    def _fail(self, test):
        if id(test) in self.order:
            self.failed = True
        else:
            # A fixture of a class or module: the next test it was for.
            reported_up_to = max(self.reported, default=-1)
            at = reported_up_to + 1
            if at < len(self.items):
                self._emit(at, "failed")

    def addError(self, test, err):
        super().addError(test, err)
        self._fail(test)

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self._fail(test)

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        self._fail(test)

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        if err is not None:
            self.failed = True

    def _emit(self, at, outcome):
        if at in self.reported:
            return
        self.reported.add(at)
        self.report(at, outcome, int((time.monotonic() - self.started) * 1000))
        if outcome == "failed" and self.stop_at_failure:
            self.stop()

    def stopTest(self, test):
        super().stopTest(test)
        at = self.order.get(id(test))
        if at is not None:
            self._emit(at, "failed" if self.failed else "passed")

    def finish(self):
        pass


def _patch_unittest() -> None:
    import unittest.main

    # `unittest.main` is the class itself once the package is imported; the
    # module is in `sys.modules`.
    program = sys.modules["unittest.main"]
    if getattr(program.TestProgram, "_supercov_strong", False):
        return
    original = program.TestProgram.runTests

    def run_tests(self):
        if not os.environ.get(PLAN_ENV):
            return original(self)
        runner = _UnittestRunner(self.test)
        if _only() is not None:
            run_only(runner)
        else:
            Driver(Plan(os.environ[PLAN_ENV]), runner, os.environ[RESULTS_ENV]).run()
        import unittest

        self.result = unittest.TestResult()
        if self.exit:
            sys.exit(0)

    program.TestProgram.runTests = run_tests
    program.TestProgram._supercov_strong = True
