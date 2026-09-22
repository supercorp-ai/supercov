"""Import-time probes for the Supercov Python frontend.

The Rust plan names every obligation in a measured file with a stable id and
a byte-exact span. This module turns that plan into probes at import time:
the source is parsed, a probe node is inserted before each planned
statement with the statement's own line number, and the modified tree is
compiled under the file's real name. Nothing on disk changes, every line
number a traceback shows is the file's own, and a probe is a store into the
current context's hit array -- specialised bytecode, not a callback.

Three doors admit measured code into the interpreter, and each is covered:

- `ProbeFinder` sits first on `sys.meta_path`, asks the other finders where
  a module lives, and replaces the loader for a planned file. Path
  resolution is never reimplemented, so editable installs, zip imports,
  namespace packages, `importlib.reload` and lazy loaders behave as they do
  without Supercov.
- `builtins.compile` is wrapped: `runpy.run_path`, `spec_from_file_location`
  and a hand-rolled `exec(compile(open(p).read(), p, "exec"))` all reach it
  with the real filename (verified 2026-09-22), and get probed there.
- The main script of `python script.py` is compiled in C and reaches
  neither; the launcher runs it through `supercov_main` instead.

Stdlib only, CPython 3.9 or newer.
"""

from __future__ import annotations

import ast
import builtins
import hashlib
import importlib.abc
import importlib.machinery
import importlib.util
import marshal
import os
import sys
import tempfile
from typing import Callable

PROBE_VERSION = 1

# Bound by the runtime at install: the callables the probes reach through the
# alias each instrumented module imports from here. Kept as module attributes
# so `from supercov_probes import hits_get as <alias>` works in any namespace
# a planned file is executed in, not only ones the loader controls.
hits_get: Callable[[], bytearray] = lambda: bytearray()  # noqa: E731 - replaced at install
value_probe: Callable[[int, object], object] = lambda k, value: value  # noqa: E731
condition_probe: Callable[[int, int, object, bool], bool] = lambda d, i, value, inv: bool(value)  # noqa: E731
decision_probe: Callable[[int, object], bool] = lambda d, value: bool(value)  # noqa: E731
operand_probe: Callable[[int, int, object], object] = lambda g, i, value: value  # noqa: E731
boolop_probe: Callable[[int, object], object] = lambda g, value: value  # noqa: E731


# -- the plan, indexed for probing -------------------------------------------


class FileProbes:
    """One planned file's obligations, numbered into the run's hit array.

    `base` is the file's first index in the array shared by every planned
    file of the run; `ids[k - base]` is obligation `k`'s id. Keys are
    `(lineno, col_offset)` as `ast` reports them, which for a decorated
    definition is the `def` line while the plan spans the first decorator --
    `_key` normalises the node to the plan's spelling.
    """

    def __init__(self, relative: str, file_plan: dict, base: int) -> None:
        self.relative = relative
        self.base = base
        self.ids: list[str] = []
        self.statements: dict[tuple[int, int], int] = {}
        self.functions: dict[tuple[int, int], int] = {}
        self.lambdas: dict[tuple[int, int, int, int], int] = {}
        # Decisions and their leaves by expression span; logical BoolOps
        # outside any decision tree by span, with their operand plans. Run-wide
        # indexes are assigned by `index_plan`, which owns the counters.
        self.decisions: dict[tuple[int, int, int, int], int] = {}
        self.leaves: dict[tuple[int, int, int, int], tuple[int, int, bool]] = {}
        self.boolops: dict[tuple[int, int, int, int], int] = {}
        self.decision_plans: list = []
        self.boolop_groups: list = []
        for statement in file_plan.get("statements", ()):
            self.statements[(statement["start"][0], statement["start"][1])] = self._number(
                statement["id"]
            )
        for function in file_plan.get("functions", ()):
            (line, column), (end_line, end_column) = function["span"]
            k = self._number(function["id"])
            if function["name"] == "<lambda>":
                self.lambdas[(line, column, end_line, end_column)] = k
            else:
                self.functions[(line, column)] = k
        logical_by_decision: dict[str, list] = {}
        standalone: dict[tuple[int, int, int, int], list] = {}
        for logical in file_plan.get("logical", ()):
            if logical.get("decision") is not None:
                logical_by_decision.setdefault(logical["decision"], []).append(logical)
            else:
                (line, column), (end_line, end_column) = logical["boolop"]
                standalone.setdefault((line, column, end_line, end_column), []).append(logical)
        for decision in file_plan.get("decisions", ()):
            (line, column), (end_line, end_column) = decision["span"]
            d = len(self.decision_plans)
            self.decision_plans.append((decision, logical_by_decision.get(decision["id"], [])))
            self.decisions[(line, column, end_line, end_column)] = d
            for index, condition in enumerate(decision["conditions"]):
                (line, column), (end_line, end_column) = condition["span"]
                self.leaves[(line, column, end_line, end_column)] = (d, index, condition["not"] % 2 == 1)
        for span, plans in standalone.items():
            g = len(self.boolop_groups)
            plans.sort(key=lambda logical: logical["operand"])
            self.boolop_groups.append(plans)
            self.boolops[span] = g
        self.digest = hashlib.sha256(
            repr((PROBE_VERSION, relative, base, self.ids, len(self.decision_plans), len(self.boolop_groups))).encode("utf-8")
        ).hexdigest()

    def _number(self, identifier: str) -> int:
        self.ids.append(identifier)
        return self.base + len(self.ids) - 1

    @property
    def count(self) -> int:
        return len(self.ids)


class PlanIndex:
    """Every planned file's obligations numbered into one run-wide space."""

    def __init__(self, plan: dict) -> None:
        self.files: dict[str, FileProbes] = {}
        self.ids: list[str] = []
        # Run-wide: (decision plan, its logical plans) by decision index, and
        # a standalone BoolOp's operand plans by group index.
        self.decisions: list = []
        self.boolop_groups: list = []
        for relative, file_plan in plan["files"].items():
            probes = FileProbes(relative, file_plan, len(self.ids))
            self.files[relative] = probes
            self.ids.extend(probes.ids)
            decision_base = len(self.decisions)
            self.decisions.extend(probes.decision_plans)
            probes.decisions = {span: decision_base + d for span, d in probes.decisions.items()}
            probes.leaves = {span: (decision_base + d, i, inv) for span, (d, i, inv) in probes.leaves.items()}
            group_base = len(self.boolop_groups)
            self.boolop_groups.extend(probes.boolop_groups)
            probes.boolops = {span: group_base + g for span, g in probes.boolops.items()}


def index_plan(plan: dict) -> PlanIndex:
    return PlanIndex(plan)


# -- the transform -------------------------------------------------------------


def _key(node: ast.stmt) -> tuple[int, int]:
    decorators = getattr(node, "decorator_list", None)
    if decorators:
        first = decorators[0]
        # The plan's span opens at the `@`, one column before the expression.
        return (first.lineno, first.col_offset - 1)
    return (node.lineno, node.col_offset)


def _at(node: ast.AST, line: int, column: int) -> ast.AST:
    """Give every node of a generated subtree one source position.

    The position is the statement the probe stands for, so the compiled
    code's line table stays the original file's and a traceback through a
    probe -- there is none, but a profiler's frame is possible -- names the
    real line.
    """
    for child in ast.walk(node):
        child.lineno = line
        child.col_offset = column
        child.end_lineno = line
        child.end_col_offset = column
    return node


def _identifiers(tree: ast.AST) -> set[str]:
    names: set[str] = set()
    for node in ast.walk(tree):
        for field in ("id", "arg", "name", "asname", "attr"):
            value = getattr(node, field, None)
            if isinstance(value, str):
                names.add(value)
        for value in getattr(node, "names", ()):
            if isinstance(value, str):
                names.add(value)
    return names


def choose_alias(tree: ast.AST, stem: str) -> str:
    """A module-level name the file does not use anywhere.

    A single leading underscore: a double one would be mangled inside class
    bodies, and the probes there read the same module global as everywhere.
    """
    taken = _identifiers(tree)
    candidate = f"_scv_{stem}"
    counter = 0
    while candidate in taken:
        counter += 1
        candidate = f"_scv_{stem}{counter}"
    return candidate


def _is_docstring(statement: ast.stmt) -> bool:
    return (
        isinstance(statement, ast.Expr)
        and isinstance(statement.value, ast.Constant)
        and isinstance(statement.value.value, str)
    )


def _is_future_import(statement: ast.stmt) -> bool:
    return isinstance(statement, ast.ImportFrom) and statement.module == "__future__"


class _Inserter(ast.NodeTransformer):
    """Insert probes into every statement list of a tree.

    Works field by field rather than by node kind, so `Try`'s handlers and
    `finalbody`, `match_case.body`, `TryStar` on 3.11+ and whatever a later
    interpreter adds are covered the same way.
    """

    def __init__(self, probes: FileProbes, names: dict[str, str]) -> None:
        self.probes = probes
        self.hits = names["hits_get"]
        self.value = names["value_probe"]
        self.names = names
        self.inserted = 0

    def _call(self, alias: str, arguments: list, node: ast.AST) -> ast.Call:
        """`alias(*arguments)` positioned on `node`; constants get positions
        too, the wrapped expression keeps its own."""
        call = ast.Call(
            func=ast.Name(id=self.names[alias], ctx=ast.Load()), args=arguments, keywords=[]
        )
        for argument in arguments:
            if isinstance(argument, ast.Constant) and not hasattr(argument, "lineno"):
                _at(argument, node.lineno, node.col_offset)
        _at(call.func, node.lineno, node.col_offset)
        call.lineno, call.col_offset = node.lineno, node.col_offset
        call.end_lineno, call.end_col_offset = node.end_lineno, node.end_col_offset
        self.inserted += 1
        return call

    @staticmethod
    def _span(node: ast.AST):
        return (node.lineno, node.col_offset, node.end_lineno, node.end_col_offset)

    def visit(self, node: ast.AST) -> ast.AST:
        node = super().visit(node)
        if not isinstance(node, ast.expr) or not hasattr(node, "end_col_offset"):
            return node
        span = self._span(node)
        # Innermost first: a test that is its own single condition becomes
        # `decision(d, condition(d, 0, test, inv))`.
        leaf = self.probes.leaves.get(span)
        if leaf is not None:
            d, index, inverted = leaf
            node = self._call(
                "condition_probe",
                [ast.Constant(value=d), ast.Constant(value=index), node, ast.Constant(value=inverted)],
                node,
            )
        d = self.probes.decisions.get(span)
        if d is not None:
            node = self._call("decision_probe", [ast.Constant(value=d), node], node)
        return node

    def visit_BoolOp(self, node: ast.BoolOp) -> ast.AST:
        # A BoolOp inside a decision's tree is observed through its leaves;
        # one outside any decision reports its own short-circuits.
        g = self.probes.boolops.get(self._span(node))
        node = self.generic_visit(node)  # type: ignore[assignment]
        if g is None:
            return node
        for index in range(1, len(node.values)):
            operand = node.values[index]
            node.values[index] = self._call(
                "operand_probe", [ast.Constant(value=g), ast.Constant(value=index), operand], operand
            )
        return self._call("boolop_probe", [ast.Constant(value=g), node], node)

    def _hit(self, k: int, line: int, column: int) -> ast.stmt:
        node = ast.Assign(
            targets=[
                ast.Subscript(
                    value=ast.Call(func=ast.Name(id=self.hits, ctx=ast.Load()), args=[], keywords=[]),
                    slice=ast.Constant(value=k),
                    ctx=ast.Store(),
                )
            ],
            value=ast.Constant(value=1),
        )
        self.inserted += 1
        return _at(node, line, column)

    def _body(self, statements: list[ast.stmt]) -> list[ast.stmt]:
        out: list[ast.stmt] = []
        for statement in statements:
            k = self.probes.statements.get(_key(statement))
            probe = None if k is None else self._hit(k, statement.lineno, statement.col_offset)
            # Nothing may precede a `from __future__` import, so its probe
            # follows it; if the import raises, the module never runs at all.
            if probe is not None and not _is_future_import(statement):
                out.append(probe)
            out.append(self.visit(statement))
            if probe is not None and _is_future_import(statement):
                out.append(probe)
        return out

    def generic_visit(self, node: ast.AST) -> ast.AST:
        for field, value in ast.iter_fields(node):
            if isinstance(value, list):
                if value and all(isinstance(item, ast.stmt) for item in value):
                    setattr(node, field, self._body(value))
                else:
                    setattr(
                        node,
                        field,
                        [self.visit(item) if isinstance(item, ast.AST) else item for item in value],
                    )
            elif isinstance(value, ast.AST):
                setattr(node, field, self.visit(value))
        return node

    def _function(self, node: ast.AST) -> ast.AST:
        k = self.probes.functions.get(_key(node))  # type: ignore[arg-type]
        node = self.generic_visit(node)
        if k is not None:
            body = node.body  # type: ignore[attr-defined]
            first = body[0]
            # The entry probe is the first statement, after a docstring so
            # `__doc__` is what the file says it is.
            position = 1 if _is_docstring(first) else 0
            anchor = body[position] if position < len(body) else first
            body.insert(position, self._hit(k, anchor.lineno, anchor.col_offset))
        return node

    visit_FunctionDef = _function
    visit_AsyncFunctionDef = _function

    def visit_Lambda(self, node: ast.Lambda) -> ast.AST:
        node = self.generic_visit(node)  # type: ignore[assignment]
        k = self.probes.lambdas.get(
            (node.lineno, node.col_offset, node.end_lineno, node.end_col_offset)
        )
        if k is not None:
            # A lambda has no statements: its body is wrapped in a call that
            # records the entry and hands the value back unchanged.
            node.body = _at(
                ast.Call(
                    func=ast.Name(id=self.value, ctx=ast.Load()),
                    args=[ast.Constant(value=k), node.body],
                    keywords=[],
                ),
                node.body.lineno,
                node.body.col_offset,
            )  # type: ignore[assignment]
            self.inserted += 1
        return node


PROBE_NAMES = {
    "hits_get": "h",
    "value_probe": "v",
    "condition_probe": "c",
    "decision_probe": "d",
    "operand_probe": "o",
    "boolop_probe": "b",
}


def instrument(tree: ast.Module, probes: FileProbes) -> ast.Module:
    """Insert the plan's probes into a parsed module, in place."""
    taken = _identifiers(tree)
    names = {}
    for attribute, stem in PROBE_NAMES.items():
        candidate = f"_scv_{stem}"
        counter = 0
        while candidate in taken:
            counter += 1
            candidate = f"_scv_{stem}{counter}"
        taken.add(candidate)
        names[attribute] = candidate
    inserter = _Inserter(probes, names)
    inserter.visit(tree)
    # The aliases are imported from this module so the probes resolve in
    # whatever namespace the file is executed in. After the docstring and
    # any `from __future__` imports, which nothing may precede.
    position = 0
    while position < len(tree.body) and (
        _is_future_import(tree.body[position])
        or (position == 0 and _is_docstring(tree.body[position]))
    ):
        position += 1
    anchor = tree.body[position] if position < len(tree.body) else None
    line, column = (anchor.lineno, anchor.col_offset) if anchor is not None else (1, 0)
    tree.body.insert(
        position,
        _at(
            ast.ImportFrom(
                module="supercov_probes",
                names=[ast.alias(name=attribute, asname=alias) for attribute, alias in names.items()],
                level=0,
            ),
            line,
            column,
        ),
    )
    return tree


# -- compiling with a cache ---------------------------------------------------


class Probing:
    """The per-process state the finder, loader and compile wrapper share."""

    def __init__(
        self,
        files: dict[str, FileProbes],
        relative_for: Callable[[str], str | None],
        cache_directory: str | None,
    ) -> None:
        self.files = files
        self.relative_for = relative_for
        self.cache_directory = cache_directory
        self.code_objects: set[int] = set()

    def probes_for(self, filename: str | None) -> FileProbes | None:
        if not filename or filename.startswith("<"):
            return None
        relative = self.relative_for(filename)
        return None if relative is None else self.files.get(relative)

    def compile(self, source: bytes | str | ast.AST, filename: str, probes: FileProbes, *, flags: int = 0, dont_inherit: bool = False, optimize: int = -1):
        """Probed code for `source`, from the cache when it has been seen.

        The key is the source, the file's numbered obligations and the
        interpreter's cache tag: a file that changed, a plan that renumbered,
        or a different interpreter each compile afresh.
        """
        cached_path = None
        if self.cache_directory is not None and isinstance(source, (bytes, str)):
            text = source if isinstance(source, bytes) else source.encode("utf-8")
            key = hashlib.sha256(
                b"\0".join(
                    [
                        text,
                        probes.digest.encode("ascii"),
                        sys.implementation.cache_tag.encode("ascii"),
                        str((flags, optimize)).encode("ascii"),
                    ]
                )
            ).hexdigest()
            cached_path = os.path.join(self.cache_directory, f"{key}.pyc")
            try:
                with open(cached_path, "rb") as stream:
                    code = marshal.load(stream)
                self._register(code)
                return code
            except (OSError, EOFError, ValueError, TypeError):
                pass
        tree = source if isinstance(source, ast.AST) else _original_compile(
            source, filename, "exec", flags | ast.PyCF_ONLY_AST, dont_inherit, optimize
        )
        instrument(tree, probes)
        code = _original_compile(tree, filename, "exec", flags, dont_inherit, optimize)
        self._register(code)
        if cached_path is not None:
            self._store(cached_path, code)
        return code

    def _register(self, code) -> None:
        # Every code object the probed module owns, so a detector can tell a
        # probed function from one that reached the interpreter another way.
        self.code_objects.add(id(code))
        for constant in code.co_consts:
            if hasattr(constant, "co_code"):
                self._register(constant)

    def _store(self, path: str, code) -> None:
        try:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            descriptor, temporary = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
            with os.fdopen(descriptor, "wb") as stream:
                marshal.dump(code, stream)
            os.replace(temporary, path)
        except OSError:
            pass


# -- import interception ------------------------------------------------------


class ProbeLoader:
    """A loader that compiles a planned file with its probes.

    Wraps the loader the real finder chose and delegates everything but the
    compile to it, so `get_source`, `get_data`, `is_package` and
    `get_filename` -- what `inspect`, `linecache`, `pkgutil` and `runpy` ask
    for -- answer as the original would.
    """

    def __init__(self, inner, probing: Probing, probes: FileProbes) -> None:
        self._inner = inner
        self._probing = probing
        self._probes = probes

    def __getattr__(self, name: str):
        return getattr(self._inner, name)

    def create_module(self, spec):
        create = getattr(self._inner, "create_module", None)
        return None if create is None else create(spec)

    def _source(self, filename: str) -> bytes:
        get_data = getattr(self._inner, "get_data", None)
        if get_data is not None:
            return get_data(filename)
        with open(filename, "rb") as stream:
            return stream.read()

    def get_code(self, fullname: str):
        filename = self._inner.get_filename(fullname)
        return self._probing.compile(self._source(filename), filename, self._probes)

    def exec_module(self, module) -> None:
        filename = module.__spec__.origin
        code = self._probing.compile(self._source(filename), filename, self._probes)
        exec(code, module.__dict__)


class ProbeFinder(importlib.abc.MetaPathFinder):
    """First on `sys.meta_path`: the other finders locate, this one probes."""

    def __init__(self, probing: Probing) -> None:
        self._probing = probing

    def find_spec(self, fullname, path, target=None):
        for finder in sys.meta_path:
            if isinstance(finder, ProbeFinder):
                continue
            find = getattr(finder, "find_spec", None)
            if find is None:
                continue
            spec = find(fullname, path, target)
            if spec is None:
                continue
            probes = self._probing.probes_for(spec.origin)
            if probes is not None and spec.loader is not None:
                spec.loader = ProbeLoader(spec.loader, self._probing, probes)
                # Bytecode lives in Supercov's cache, never the project's.
                spec.cached = None
            return spec
        return None

    def invalidate_caches(self) -> None:
        pass


_original_compile = builtins.compile
_probing: Probing | None = None


def _compile_probed(source, filename, mode, flags=0, dont_inherit=False, optimize=-1, **keywords):
    if (
        _probing is not None
        and mode == "exec"
        and not (flags & ast.PyCF_ONLY_AST)
        and not keywords
    ):
        probes = _probing.probes_for(filename if isinstance(filename, str) else None)
        if probes is not None:
            return _probing.compile(
                source, filename, probes, flags=flags, dont_inherit=dont_inherit, optimize=optimize
            )
    return _original_compile(source, filename, mode, flags, dont_inherit, optimize, **keywords)


def install(
    files: dict[str, FileProbes],
    relative_for: Callable[[str], str | None],
    cache_directory: str | None,
    entry_points: dict[str, Callable],
) -> Probing:
    """Arm the finder and the compile wrapper for this process.

    `entry_points` binds every name in `PROBE_NAMES` to the runtime's callable.
    """
    global _probing
    for attribute in PROBE_NAMES:
        globals()[attribute] = entry_points[attribute]
    probing = Probing(files, relative_for, cache_directory)
    _probing = probing
    if not any(isinstance(finder, ProbeFinder) for finder in sys.meta_path):
        sys.meta_path.insert(0, ProbeFinder(probing))
    if builtins.compile is not _compile_probed:
        builtins.compile = _compile_probed
    # Modules imported before this point cannot be re-executed; the detector
    # reports any planned file among them.
    return probing


if __name__ == "__main__":  # pragma: no cover - a development aid
    # python supercov_probes.py <plan.json> <relative file>: print the probed
    # source, and compile it, so a transform can be eyeballed and checked.
    import json

    plan_path, relative = sys.argv[1], sys.argv[2]
    plan = json.load(open(plan_path, encoding="utf-8"))
    index = index_plan(plan)
    source_path = os.path.join(plan["root"], relative)
    tree = ast.parse(open(source_path, "rb").read(), filename=source_path)
    instrument(tree, index.files[relative])
    print(ast.unparse(tree))
    _original_compile(tree, source_path, "exec")
    probes = index.files[relative]
    print(f"# compiled; {probes.count} obligations, {len(probes.decisions)} decisions, {len(probes.boolops)} standalone boolops; {len(index.ids)} obligations in the run", file=sys.stderr)
