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
  neither. Supercov measures test-runner commands, which never put a
  planned file in `sys.argv[0]`; a script-shaped runner such as Django's
  `manage.py test` is a detection feature first, and this is where its
  entry would go.

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
single_probe: Callable[[int, object], bool] = lambda k, value: bool(value)  # noqa: E731
operand_probe: Callable[[int, int, object], object] = lambda g, i, value: value  # noqa: E731
boolop_probe: Callable[[int, object], object] = lambda g, value: value  # noqa: E731
iter_probe: Callable[[int, int, object], object] = lambda entered, zero, iterable: iterable  # noqa: E731
aiter_probe: Callable[[int, int, object], object] = lambda entered, zero, iterable: iterable  # noqa: E731
site_probe: Callable[[str, int], None] = lambda file, line: None  # noqa: E731


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
        # Loops by the iterable's span: (entered k, zero k). Tries by their
        # body's first statement: (success k, [(selected k, missed k, bare)],
        # raised k). Matches by the statement's span: ([(selected k, missed k,
        # irrefutable)], (matched k, unmatched k) or None).
        self.loops: dict[tuple[int, int, int, int], tuple[int, int]] = {}
        self.tries: dict[tuple[int, int], tuple[int, list, int]] = {}
        self.matches: dict[tuple[int, int], tuple[list, tuple | None]] = {}
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
        for loop in file_plan.get("loops", ()):
            (line, column), (end_line, end_column) = loop["iter"]
            self.loops[(line, column, end_line, end_column)] = (
                self._number(loop["entered"]),
                self._number(loop["zero"]),
            )
        for try_plan in file_plan.get("tries", ()):
            (line, column), _ = try_plan["body"]
            handlers = [
                (self._number(h["selected"]), self._number(h["missed"]), h["bare"])
                for h in try_plan["handlers"]
            ]
            self.tries[(line, column)] = (
                self._number(try_plan["success"]),
                handlers,
                self._number(try_plan["raised"]),
                try_plan.get("finalbody") is not None,
            )
        for match in file_plan.get("matches", ()):
            (line, column), _ = match["span"]
            cases = [
                (self._number(c["selected"]), self._number(c["missed"]), c["irrefutable"])
                for c in match["cases"]
            ]
            no_case = match.get("noCase")
            self.matches[(line, column)] = (
                cases,
                None if no_case is None else (self._number(no_case["matched"]), self._number(no_case["unmatched"])),
            )
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
        # A decision with one condition needs no evaluation state: its
        # outcome is its vector. Two slots in the hit array, one per truth,
        # that the harvest turns into the vector and the outcome hit.
        self.singles: dict[tuple[int, int, int, int], int] = {}
        for span, d in list(self.decisions.items()):
            if len(self.decision_plans[d][0]["conditions"]) == 1:
                slot = len(self.ids) + base
                self.ids.append(("vector", d, False))
                self.ids.append(("vector", d, True))
                self.singles[span] = slot
                del self.decisions[span]
                leaf = next(key for key, (dd, i, inv) in self.leaves.items() if dd == d)
                del self.leaves[leaf]
        self.digest = hashlib.sha256(
            repr((PROBE_VERSION, relative, base, self.ids, len(self.decision_plans), len(self.boolop_groups))).encode("utf-8")
        ).hexdigest()

    def _number(self, identifier: str) -> int:
        self.ids.append(identifier)
        return self.base + len(self.ids) - 1

    @property
    def count(self) -> int:
        return len(self.ids)


class SiteProbes:
    """A test file: not measured, but its assertion sites are recorded.

    The plan names the lines holding an inventoried `assert`; a site probe
    goes before the first statement on each of those lines -- which, once
    pytest has rewritten the assert, is the rewrite's first statement.
    """

    def __init__(self, relative: str, lines) -> None:
        self.relative = relative
        self.lines = frozenset(lines)
        self.digest = hashlib.sha256(repr((PROBE_VERSION, "sites", relative, sorted(self.lines))).encode()).hexdigest()


class PlanIndex:
    """Every planned file's obligations numbered into one run-wide space."""

    def __init__(self, plan: dict) -> None:
        self.files: dict[str, FileProbes] = {}
        self.ids: list[str] = []
        # Run-wide: (decision plan, its logical plans) by decision index, and
        # a standalone BoolOp's operand plans by group index.
        self.decisions: list = []
        self.boolop_groups: list = []
        self.sites: dict[str, SiteProbes] = {
            relative: SiteProbes(relative, lines)
            for relative, lines in plan.get("assertionSites", {}).items()
            if relative not in plan["files"]
        }
        for relative, file_plan in plan["files"].items():
            probes = FileProbes(relative, file_plan, len(self.ids))
            self.files[relative] = probes
            self.ids.extend(probes.ids)
            decision_base = len(self.decisions)
            self.decisions.extend(probes.decision_plans)
            probes.decisions = {span: decision_base + d for span, d in probes.decisions.items()}
            for offset, entry in enumerate(probes.ids):
                if isinstance(entry, tuple):
                    self.ids[probes.base + offset] = (entry[0], decision_base + entry[1], entry[2])
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


def _exits(body: list, make_hit, k: int) -> None:
    """Insert `hit(k)` before every statement that leaves a try body early.

    A `return` anywhere in the body leaves it; a `break` or `continue` leaves
    it only when the loop it belongs to encloses the try. Nested functions,
    classes and lambdas are their own scopes and are not entered.
    """

    def walk(statements: list, loop_depth: int) -> None:
        index = 0
        while index < len(statements):
            statement = statements[index]
            leaves = isinstance(statement, ast.Return) or (
                isinstance(statement, (ast.Break, ast.Continue)) and loop_depth == 0
            )
            if leaves:
                statements.insert(index, make_hit(k, statement.lineno, statement.col_offset))
                index += 1
            elif not isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                nested_loop = isinstance(statement, (ast.For, ast.AsyncFor, ast.While))
                for field, value in ast.iter_fields(statement):
                    if isinstance(value, list) and value and all(isinstance(item, ast.stmt) for item in value):
                        walk(value, loop_depth + (1 if nested_loop else 0))
                    elif isinstance(value, list):
                        for item in value:
                            # except handlers and match cases carry bodies
                            for inner_field, inner in ast.iter_fields(item) if isinstance(item, ast.AST) else ():
                                if isinstance(inner, list) and inner and all(isinstance(x, ast.stmt) for x in inner):
                                    walk(inner, loop_depth)
            index += 1

    walk(body, 0)


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
        # (limitation id, reason, obligation id): what this file's probes
        # cannot observe, for the runtime to declare.
        self.limitations: list = []

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
        slot = self.probes.singles.get(span)
        if slot is not None:
            # `d1(slot, test)`: the truth as written is the whole vector.
            node = self._call("single_probe", [ast.Constant(value=slot), node], node)
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

    def _hits(self, ks, anchor: ast.AST) -> list[ast.stmt]:
        return [self._hit(k, anchor.lineno, anchor.col_offset) for k in ks]

    def _body(self, statements: list[ast.stmt]) -> list[ast.stmt]:
        out: list[ast.stmt] = []
        for statement in statements:
            k = self.probes.statements.get(_key(statement))
            probe = None if k is None else self._hit(k, statement.lineno, statement.col_offset)
            # Nothing may precede a `from __future__` import, so its probe
            # follows it; if the import raises, the module never runs at all.
            if probe is not None and not _is_future_import(statement):
                out.append(probe)
            visited = self.visit(statement)
            if isinstance(visited, list):
                out.extend(visited)
            else:
                out.append(visited)
            if probe is not None and _is_future_import(statement):
                out.append(probe)
        return out

    # -- loops ---------------------------------------------------------------

    def _loop(self, node: ast.AST):
        """`for`: a local flag set by the body's first statement, checked after
        the whole statement. `entered` and `zero` are exact per execution."""
        span = self._span(node.iter)  # type: ignore[attr-defined]
        plan = self.probes.loops.get(span)
        node = self.generic_visit(node)
        if plan is None:
            return node
        entered, zero = plan
        flag = f"{self.hits}_f{entered}"
        line, column = node.lineno, node.col_offset
        reset = _at(ast.Assign(targets=[ast.Name(id=flag, ctx=ast.Store())], value=ast.Constant(value=0)), line, column)
        first = node.body[0]  # type: ignore[attr-defined]
        # `flag = hits()[entered] = 1`: the flag and the hit in one statement.
        mark = _at(
            ast.Assign(
                targets=[
                    ast.Name(id=flag, ctx=ast.Store()),
                    ast.Subscript(
                        value=ast.Call(func=ast.Name(id=self.hits, ctx=ast.Load()), args=[], keywords=[]),
                        slice=ast.Constant(value=entered),
                        ctx=ast.Store(),
                    ),
                ],
                value=ast.Constant(value=1),
            ),
            first.lineno,
            first.col_offset,
        )
        node.body.insert(0, mark)  # type: ignore[attr-defined]
        after = _at(
            ast.If(
                test=ast.UnaryOp(op=ast.Not(), operand=ast.Name(id=flag, ctx=ast.Load())),
                body=[self._hit(zero, line, column)],
                orelse=[],
            ),
            line,
            column,
        )
        self.inserted += 2
        return [reset, node, after]

    visit_For = _loop
    visit_AsyncFor = _loop

    def visit_comprehension(self, node: ast.comprehension) -> ast.AST:
        span = self._span(node.iter)
        node = self.generic_visit(node)  # type: ignore[assignment]
        plan = self.probes.loops.get(span)
        if plan is not None:
            entered, zero = plan
            node.iter = self._call(
                "aiter_probe" if node.is_async else "iter_probe",
                [ast.Constant(value=entered), ast.Constant(value=zero), node.iter],
                node.iter,
            )
        return node

    # -- try -----------------------------------------------------------------

    def _try(self, node: ast.AST):
        first = node.body[0]  # type: ignore[attr-defined]
        plan = self.probes.tries.get((first.lineno, first.col_offset))
        star = type(node).__name__ == "TryStar"
        node = self.generic_visit(node)
        if plan is None:
            return node
        success, handlers, raised, has_finally = plan
        body = node.body  # type: ignore[attr-defined]
        # Completing the body is success, and so is leaving it early: a
        # `return`, or a `break`/`continue` whose loop is outside the try.
        _exits(body, self._hit, success)
        last = body[-1]
        body.append(self._hit(success, last.end_lineno or last.lineno, last.col_offset))
        for index, handler in enumerate(node.handlers):  # type: ignore[attr-defined]
            selected, _, _ = handlers[index]
            anchor = handler.body[0]
            probes = [raised, selected] + [
                missed for selected_, missed, bare in handlers[:index] if not bare
            ]
            handler.body[0:0] = self._hits(probes, anchor)
        catch_all = node.handlers and (  # type: ignore[attr-defined]
            node.handlers[-1].type is None  # type: ignore[attr-defined]
            or (isinstance(node.handlers[-1].type, ast.Name) and node.handlers[-1].type.id == "BaseException")  # type: ignore[attr-defined]
        )
        if star:
            # `except*` splits an exception group across handlers; a clause
            # that matched nothing is not a jump a probe can stand in for.
            for _, missed, bare in handlers:
                if not bare:
                    self.limitations.append(
                        (
                            "python-try-star-miss-unobserved",
                            "an except* clause that matched no exception in the group is not observed",
                            self.probes.relative,
                            # A limitation names the obligation, not one of
                            # its alternatives: the handler's id without the
                            # `:missed` the alternative carries.
                            self.probes.ids[missed - self.probes.base].rsplit(":", 1)[0],
                        )
                    )
        if not star and not catch_all:
            # No handler matched: every typed handler was missed, and a
            # `finally` is reached the way an exception reaches it.
            probes = [missed for _, missed, bare in handlers if not bare]
            if has_finally:
                probes.append(raised)
            anchor = node.handlers[-1] if node.handlers else node  # type: ignore[attr-defined]
            handler = ast.ExceptHandler(
                type=ast.Name(id="BaseException", ctx=ast.Load()),
                name=None,
                body=self._hits(probes, anchor) + [ast.Raise(exc=None, cause=None)],
            )
            _at(handler, anchor.lineno, anchor.col_offset)
            for probe in handler.body[:-1]:
                _at(probe, anchor.lineno, anchor.col_offset)
            node.handlers.append(handler)  # type: ignore[attr-defined]
            self.inserted += 1
        return node

    visit_Try = _try
    visit_TryStar = _try

    # -- match ---------------------------------------------------------------

    def visit_Match(self, node: ast.Match) -> ast.AST:
        plan = self.probes.matches.get((node.lineno, node.col_offset))
        node = self.generic_visit(node)  # type: ignore[assignment]
        if plan is None:
            return node
        cases, no_case = plan
        for index, case in enumerate(node.cases):
            selected, _, _ = cases[index]
            anchor = case.body[0]
            probes = [selected]
            if no_case is not None:
                probes.append(no_case[0])
            probes += [missed for _, missed, irrefutable in cases[:index] if not irrefutable]
            # A later irrefutable case is "not selected" exactly when an
            # earlier one was.
            probes += [missed for _, missed, irrefutable in cases[index + 1 :] if irrefutable]
            case.body[0:0] = self._hits(probes, anchor)
        if not any(irrefutable for _, _, irrefutable in cases):
            probes = [missed for _, missed, irrefutable in cases if not irrefutable]
            if no_case is not None:
                probes.append(no_case[1])
            anchor = node.cases[-1]
            wildcard = ast.match_case(
                pattern=ast.MatchAs(pattern=None, name=None),
                guard=None,
                body=self._hits(probes, anchor) or [ast.Pass()],
            )
            _at(wildcard.pattern, anchor.lineno, anchor.col_offset)
            for statement in wildcard.body:
                _at(statement, anchor.lineno, anchor.col_offset)
            node.cases.append(wildcard)
            self.inserted += 1
        return node

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
    "single_probe": "d1",
    "operand_probe": "o",
    "boolop_probe": "b",
    "iter_probe": "i",
    "aiter_probe": "ai",
    "site_probe": "s",
}


def instrument_sites(tree: ast.Module, sites: SiteProbes, alias: str) -> None:
    """Record an assertion site before the first statement on each of its lines."""
    seen: set[int] = set()

    def walk(statements: list) -> None:
        index = 0
        while index < len(statements):
            statement = statements[index]
            if statement.lineno in sites.lines and statement.lineno not in seen:
                seen.add(statement.lineno)
                call = ast.Expr(
                    value=ast.Call(
                        func=ast.Name(id=alias, ctx=ast.Load()),
                        args=[ast.Constant(value=sites.relative), ast.Constant(value=statement.lineno)],
                        keywords=[],
                    )
                )
                statements.insert(index, _at(call, statement.lineno, statement.col_offset))
                index += 1
            for field, value in ast.iter_fields(statement):
                if isinstance(value, list) and value and all(isinstance(item, ast.stmt) for item in value):
                    walk(value)
                elif isinstance(value, list):
                    for item in value:
                        if isinstance(item, ast.AST):
                            for _, inner in ast.iter_fields(item):
                                if isinstance(inner, list) and inner and all(isinstance(x, ast.stmt) for x in inner):
                                    walk(inner)
            index += 1

    walk(tree.body)


def instrument(tree: ast.Module, probes) -> ast.Module:
    """Insert the plan's probes into a parsed module, in place."""
    if isinstance(probes, SiteProbes):
        alias = choose_alias(tree, "s")
        instrument_sites(tree, probes, alias)
        _import_aliases(tree, {"site_probe": alias})
        return tree
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
    _import_aliases(tree, names)
    if inserter.limitations and _probing is not None:
        for limitation in inserter.limitations:
            _probing.report(*limitation)
    return tree


def _import_aliases(tree: ast.Module, names: dict[str, str]) -> None:
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
    # Fail-safe: probed bytecode can outlive the run in a cache Supercov does
    # not own -- pytest's rewriter caches what it compiles -- and a plain run
    # loading it must still import. Without the runtime the aliases become
    # sinks that accept every probe and record nothing.
    imports = ", ".join(f"{attribute} as {alias}" for attribute, alias in names.items())
    sinks = "\n".join(
        f"    {alias} = _scv_sink"
        for alias in names.values()
    )
    fallback = ast.parse(
        f"try:\n    from supercov_probes import {imports}\nexcept ImportError:\n"
        f"    class _scv_sink:\n        def __setitem__(self, key, value):\n            pass\n"
        f"        def __call__(self, *arguments):\n            return arguments[-1] if arguments else self\n"
        f"    _scv_sink = _scv_sink()\n{sinks}\n"
    ).body
    for statement in fallback:
        _at(statement, line, column)
    tree.body[position:position] = fallback


# -- compiling with a cache ---------------------------------------------------


class Probing:
    """The per-process state the finder, loader and compile wrapper share."""

    def __init__(
        self,
        files: dict[str, FileProbes],
        relative_for: Callable[[str], str | None],
        cache_directory: str | None,
        sites: dict[str, SiteProbes] | None = None,
    ) -> None:
        self.files = files
        self.sites = sites or {}
        self.relative_for = relative_for
        self.cache_directory = cache_directory
        self.code_objects: set[int] = set()
        # The runtime's `limitation(identifier, reason, file, obligation)`, once installed.
        self.report: Callable[[str, str, str, str], None] = lambda identifier, reason, file, obligation: None

    def probes_for(self, filename: str | None) -> FileProbes | None:
        """A measured file's probes: what the finder swaps a loader for."""
        if not filename or filename.startswith("<"):
            return None
        relative = self.relative_for(filename)
        return None if relative is None else self.files.get(relative)

    def sites_for(self, filename: str | None) -> SiteProbes | None:
        """A test file's assertion sites: applied where the file is compiled,
        which pytest's rewriter does itself, so the finder leaves it alone."""
        if not filename or filename.startswith("<"):
            return None
        relative = self.relative_for(filename)
        return None if relative is None else self.sites.get(relative)

    def compile(self, source: bytes | str | ast.AST, filename: str, probes, *, flags: int = 0, dont_inherit: bool = False, optimize: int = -1):
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
        name = filename if isinstance(filename, str) else None
        probes = _probing.probes_for(name) or _probing.sites_for(name)
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
    sites: dict[str, SiteProbes] | None = None,
) -> Probing:
    """Arm the finder and the compile wrapper for this process.

    `entry_points` binds every name in `PROBE_NAMES` to the runtime's callable.
    """
    global _probing
    for attribute in PROBE_NAMES:
        globals()[attribute] = entry_points[attribute]
    probing = Probing(files, relative_for, cache_directory, sites)
    probing.report = entry_points.get("limitation", probing.report)
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
