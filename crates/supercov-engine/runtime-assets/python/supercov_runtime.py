"""Supercov's stdlib-only Python runtime.

Rust decides the denominator ahead of the run and ships it as a probe plan:
source spans, `not` polarity, and/or trees and trigger lines for every
obligation. This module maps CPython `sys.monitoring` events back onto those
obligations and reports what it observed, per test phase. It never modifies
source or bytecode, never computes a coverage verdict, and never imports
anything outside the standard library.

Supported interpreters: CPython 3.12 and newer. 3.14 provides
`code.co_branches()` and the `BRANCH_LEFT`/`BRANCH_RIGHT` events; 3.12 and
3.13 derive the same branch table through `dis` and receive the single
`BRANCH` event.
"""

from __future__ import annotations

import atexit
import contextvars
import dis
import json
import mmap
import os
import struct
import sys
import threading
import time
import weakref

PLAN_VERSION = 1
EVIDENCE_VERSION = 1
CONTEXT_ENV = "SUPERCOV_CONTEXT"
PLAN_ENV = "SUPERCOV_PYTHON_PLAN"
EVIDENCE_DIR_ENV = "SUPERCOV_PYTHON_EVIDENCE_DIR"
RUN_ID_ENV = "SUPERCOV_RUN_ID"
WORKER_ENV = "SUPERCOV_PYTHON_WORKER"
DEBUG = bool(os.environ.get("SUPERCOV_PYTHON_DEBUG"))
TIMING = bool(os.environ.get("SUPERCOV_PYTHON_TIMING"))
_timing = {
    "start": [0, 0.0],
    "line": [0, 0.0],
    "branch": [0, 0.0],
    "instruction": [0, 0.0],
    "jump": [0, 0.0],
    "return": [0, 0.0],
    "switch": [0, 0.0],
}


def _timed(name, function):
    if not TIMING:
        return function
    counter = _timing[name]
    clock = time.perf_counter

    def wrapper(*args):
        began = clock()
        try:
            return function(*args)
        finally:
            counter[0] += 1
            counter[1] += clock() - began

    return wrapper
TRANSPORT_MAGIC = b"SCVPYTH1"
TRANSPORT_VERSION = 1
TRANSPORT_HEADER_SIZE = 64
TRANSPORT_RECORD_HEADER_SIZE = 16
TRANSPORT_INITIAL_CAPACITY = 1024 * 1024
TRANSPORT_MAX_CAPACITY = 512 * 1024 * 1024
TRANSPORT_MAX_RECORD_SIZE = 4 * 1024 * 1024
MAX_OPEN_EVALUATIONS = 64
# How many times per phase a loop's exit re-arms its code object so a later
# zero-iteration execution is still observed before the loop goes quiet.
MAX_LOOP_REARMS = 16

_monitoring = sys.monitoring
_BRANCH_OPNAMES = frozenset(
    {
        "POP_JUMP_IF_FALSE",
        "POP_JUMP_IF_TRUE",
        "POP_JUMP_IF_NONE",
        "POP_JUMP_IF_NOT_NONE",
        "FOR_ITER",
    }
)
# Jump senses: does the *taken* jump mean the tested value was truthy?
# `x is not None` compiles to POP_JUMP_IF_NONE, so "jump when None" is a
# jump on the condition being false; `x is None` compiles to
# POP_JUMP_IF_NOT_NONE, again a jump on false.
_JUMPS_IF_TRUE = {
    "POP_JUMP_IF_TRUE": True,
    "POP_JUMP_IF_FALSE": False,
}


def _jump_sense(opname: str, condition: dict) -> bool | None:
    ordinary = _JUMPS_IF_TRUE.get(opname)
    if ordinary is not None:
        return ordinary
    none_when_true = condition.get("noneWhenTrue")
    if none_when_true is None:
        return None
    if opname == "POP_JUMP_IF_NONE":
        return bool(none_when_true)
    if opname == "POP_JUMP_IF_NOT_NONE":
        return not bool(none_when_true)
    return None


def _now_ms() -> int:
    return int(time.time() * 1000)


def _span_contains(span, lineno, end_lineno, col, end_col) -> bool:
    """Whether an instruction position lies inside a plan span (inclusive)."""
    (start_line, start_col), (finish_line, finish_col) = span
    if lineno < start_line or end_lineno > finish_line:
        return False
    if lineno == start_line and col < start_col:
        return False
    if end_lineno == finish_line and end_col > finish_col:
        return False
    return True


def _span_contains_start(span, lineno, end_lineno, col, end_col) -> bool:
    """Whether an instruction *starts* inside a plan span. CPython stamps some
    instructions (exception type checks) with an end that runs into the
    following block, so only the start is a reliable membership test."""
    del end_lineno, end_col
    (start_line, start_col), (finish_line, finish_col) = span
    return (start_line, start_col) <= (lineno, col) <= (finish_line, finish_col)


def _span_equals(span, lineno, end_lineno, col, end_col) -> bool:
    (start_line, start_col), (finish_line, finish_col) = span
    return (
        lineno == start_line
        and col == start_col
        and end_lineno == finish_line
        and end_col == finish_col
    )


def _line_in_span(span, lineno) -> bool:
    return span[0][0] <= lineno <= span[1][0]


class _Evaluation:
    __slots__ = ("values", "last", "exited")

    def __init__(self, width: int) -> None:
        self.values = [None] * width
        self.last = -1
        self.exited = True


MAX_ENUMERATED_VECTORS = 256


def _possible_vectors(tree, width):
    """Every vector a short-circuit evaluation of `tree` can produce, or None
    when the decision is too wide to enumerate cheaply."""
    def paths(node):
        # Yields (assignments, outcome) where assignments maps leaf -> bool.
        if isinstance(node, int):
            yield ({node: True}, True)
            yield ({node: False}, False)
            return
        op = node["op"]
        negate = node.get("negate", False)
        stop_on = op == "or"  # `or` stops on True, `and` stops on False
        prefixes = [({}, None)]
        for item in node["items"]:
            next_prefixes = []
            for assignments, _ in prefixes:
                for sub_assignments, sub_outcome in paths(item):
                    merged = dict(assignments)
                    merged.update(sub_assignments)
                    if sub_outcome == stop_on:
                        yield (merged, (not stop_on) if negate else stop_on)
                    else:
                        next_prefixes.append((merged, sub_outcome))
                    if len(next_prefixes) > MAX_ENUMERATED_VECTORS:
                        raise OverflowError
            prefixes = next_prefixes
        for assignments, outcome in prefixes:
            yield (assignments, (not outcome) if negate else outcome)

    vectors = set()
    try:
        for assignments, _ in paths(tree):
            vectors.add(
                "".join(
                    "0" if assignments.get(index) is None else ("2" if assignments[index] else "1")
                    for index in range(width)
                )
            )
            if len(vectors) > MAX_ENUMERATED_VECTORS:
                return None
    except OverflowError:
        return None
    return vectors


class _Decision:
    """A decision plan compiled for one code object."""

    __slots__ = (
        "id",
        "width",
        "conditions",
        "tree",
        "regions",
        "region_union",
        "outcome_true",
        "outcome_false",
        "logical",
        "possible",
        "prefixes",
    )

    def __init__(self, plan: dict, logical: list) -> None:
        self.id = plan["id"]
        self.conditions = plan["conditions"]
        self.width = len(self.conditions)
        self.tree = plan["tree"]
        self.regions = [set() for _ in self.conditions]
        self.region_union = set()
        self.outcome_true = plan["outcomeTrue"]
        self.outcome_false = plan["outcomeFalse"]
        self.logical = logical
        possible = _possible_vectors(self.tree, self.width)
        self.possible = len(possible) if possible is not None else None
        self.prefixes = _unique_prefixes(self.tree, self.width)


def _unique_prefixes(tree, width):
    """For each leaf, the unique assignment of earlier leaves that reaches it,
    or None when several paths reach it. In `a and (b or c)` every leaf has a
    unique prefix (c is reached only through a=True, b=False); in
    `(a or b) and c` the leaf c does not (a=True, or a=False then b=True)."""
    prefixes = [None] * width

    def walk(node, prefix, unique):
        if isinstance(node, int):
            prefixes[node] = dict(prefix) if unique else None
            return
        op = node["op"]
        continue_value = op == "and"  # `and` continues on True, `or` on False
        seen_compound = False
        for item in node["items"]:
            walk(item, prefix, unique and not seen_compound)
            if isinstance(item, int):
                prefix = dict(prefix)
                prefix[item] = continue_value
            else:
                seen_compound = True

    walk(tree, {}, True)
    return prefixes


def _evaluate_tree(tree, values, reached):
    """Short-circuit evaluation over observed leaf values.

    Returns the outcome and records every leaf the evaluation consulted in
    `reached`. A consulted leaf without a value means the observation is
    inconsistent with the source structure.
    """
    if isinstance(tree, int):
        reached.add(tree)
        return values[tree]
    op = tree["op"]
    result = None
    for item in tree["items"]:
        result = _evaluate_tree(item, values, reached)
        if result is None:
            return None
        if (op == "and" and result is False) or (op == "or" and result is True):
            break
    if result is not None and tree.get("negate"):
        return not result
    return result


class _CodeInfo:
    __slots__ = (
        "code",
        "file",
        "function_id",
        "statements",
        "line_alternatives",
        "consumers",
        "positions",
        "armed",
        "has_branches",
        "touched",
        "events",
        "instructions",
        "jumps",
        "returns",
        "live_lines",
        "aliases",
    )

    def __init__(self, code, file_plan) -> None:
        self.code = code
        self.file = file_plan
        self.function_id = None
        self.statements = file_plan.statements_by_line
        self.line_alternatives = file_plan.alternatives_by_line
        self.consumers = {}
        self.positions = {}
        self.armed = False
        self.has_branches = False
        self.touched = False
        self.events = 0
        self.instructions = {}
        self.jumps = {}
        self.returns = {}
        self.live_lines = frozenset()
        self.aliases = {}


class _FilePlan:
    __slots__ = (
        "path",
        "plan",
        "statements_by_line",
        "exact_statements",
        "alternatives_by_line",
        "functions",
        "decisions",
        "loops",
        "value_logical",
        "matches",
        "tries",
    )

    def __init__(self, path: str, plan: dict) -> None:
        self.path = path
        self.plan = plan
        self.statements_by_line = {}
        self.exact_statements = []
        for statement in plan.get("statements", ()):
            if statement.get("exact"):
                self.exact_statements.append(((statement["start"], statement["end"]), statement["id"]))
                continue
            start, end = statement["lines"]
            for line in range(start, end + 1):
                self.statements_by_line[line] = statement["id"]
        self.functions = {}
        for function in plan.get("functions", ()):
            self.functions.setdefault((function["line"], function["name"]), []).append(function)
        self.tries = plan.get("tries", [])
        decision_logical = {}
        self.value_logical = []
        for logical in plan.get("logical", ()):
            if logical.get("decision") is None:
                self.value_logical.append(logical)
            else:
                decision_logical.setdefault(logical["decision"], []).append(logical)
        self.decisions = [
            (decision, decision_logical.get(decision["id"], []))
            for decision in plan.get("decisions", ())
        ]
        self.loops = plan.get("loops", [])
        self.matches = plan.get("matches", [])
        self.alternatives_by_line = {}
        for match in self.matches:
            cases = match["cases"]
            for index, case in enumerate(cases):
                start, end = case["bodyLines"]
                alternatives = [case["selected"]]
                if match.get("noCase"):
                    alternatives.append(match["noCase"]["matched"])
                # An irrefutable case has no test to fail; it is "not selected"
                # exactly when an earlier case was.
                alternatives.extend(
                    later["missed"] for later in cases[index + 1 :] if later["irrefutable"]
                )
                for line in range(start, end + 1):
                    self.alternatives_by_line.setdefault(line, []).extend(alternatives)
        for try_plan in self.tries:
            for handler in try_plan["handlers"]:
                start, end = handler["bodyLines"]
                for line in range(start, end + 1):
                    self.alternatives_by_line.setdefault(line, []).extend(
                        [handler["selected"], try_plan["raised"]]
                    )
                if not handler["bare"]:
                    # The type-match instructions are stamped with the clause.
                    (header_start, _), (header_end, _) = handler["header"]
                    for line in range(header_start, header_end + 1):
                        self.alternatives_by_line.setdefault(line, []).append(try_plan["raised"])


class Runtime:
    def __init__(self, plan_path: str, evidence_dir: str, run_id: str, worker: str) -> None:
        with open(plan_path, "r", encoding="utf-8") as stream:
            plan = json.load(stream)
        if plan.get("version") != PLAN_VERSION:
            raise RuntimeError(f"unsupported Supercov Python plan version {plan.get('version')!r}")
        self.root = os.path.realpath(plan["root"])
        self.files = {path: _FilePlan(path, file_plan) for path, file_plan in plan["files"].items()}
        self.evidence_dir = evidence_dir
        self.run_id = run_id
        self.worker = worker
        self.tool_id = None
        self.lock = threading.RLock()
        self.path_cache: dict[str, str | None] = {}
        self.code_cache: dict[int, tuple] = {}
        self.context = contextvars.ContextVar("supercov_python_context", default=0)
        self.identities: dict[int, dict] = {}
        self.next_context = 1
        self.seen_hits: set = set()
        self.seen_vectors: set = set()
        self.vector_counts: dict = {}
        self.open_evaluations: dict = {}
        self.loop_entered: dict = {}
        self.loop_rearms: dict = {}
        self.pending_handler_exits: dict = {}
        self.touched: list = []
        self.pytest_active = False
        self.reported_limitations: set = set()
        self.output_path = None
        self.output_descriptor = None
        self.output = None
        self.output_capacity = 0
        self.output_cursor = TRANSPORT_HEADER_SIZE
        self.output_pid = None
        self.output_token = f"{time.time_ns():x}-{id(self) & 0xFFFF:x}"
        self.dropped_records = 0
        self.closed = False
        self.branch_pairs = hasattr(_monitoring.events, "BRANCH_LEFT")

    # -- evidence transport -------------------------------------------------

    def _open_output(self) -> None:
        os.makedirs(self.evidence_dir, exist_ok=True)
        pid = os.getpid()
        safe_worker = "".join(
            character if character.isalnum() or character in "-_." else "_"
            for character in self.worker
        )
        self.output_path = os.path.join(
            self.evidence_dir,
            f"{safe_worker}.{pid}.{self.output_token}.mmap",
        )
        descriptor = os.open(self.output_path, os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            os.ftruncate(descriptor, TRANSPORT_INITIAL_CAPACITY)
            output = mmap.mmap(descriptor, TRANSPORT_INITIAL_CAPACITY, access=mmap.ACCESS_WRITE)
        except Exception:
            os.close(descriptor)
            raise
        self.output_descriptor = descriptor
        self.output = output
        self.output_capacity = TRANSPORT_INITIAL_CAPACITY
        self.output_cursor = TRANSPORT_HEADER_SIZE
        self.output_pid = pid
        self.dropped_records = 0
        output[:TRANSPORT_HEADER_SIZE] = b"\0" * TRANSPORT_HEADER_SIZE
        struct.pack_into(
            "<8sIIQQQ",
            output,
            0,
            TRANSPORT_MAGIC,
            TRANSPORT_VERSION,
            TRANSPORT_HEADER_SIZE,
            self.output_capacity,
            0,
            pid,
        )
        output.flush()
        self._write_record(
            {
                "t": "process",
                "v": EVIDENCE_VERSION,
                "run": self.run_id,
                "pid": pid,
                "worker": self.worker,
                "python": sys.version.split()[0],
                "executable": sys.executable,
                "argv": sys.argv,
            }
        )
        context = self.context.get()
        identity = self.identities.get(context)
        if context and identity is not None:
            # A forked child inherits the active ContextVar and identity table,
            # but its new transport needs a local declaration before its first
            # hit can reference that context.
            self._write_record({"t": "phase", "ctx": context, "at": _now_ms(), **identity})

    def _close_output(self, flush: bool) -> None:
        output = self.output
        descriptor = self.output_descriptor
        self.output = None
        self.output_descriptor = None
        self.output_path = None
        self.output_capacity = 0
        self.output_cursor = TRANSPORT_HEADER_SIZE
        self.output_pid = None
        if output is not None:
            if flush:
                output.flush()
            output.close()
        if descriptor is not None:
            os.close(descriptor)

    def _ensure_process_output(self) -> None:
        if self.output_pid == os.getpid() and self.output is not None:
            return
        # A fork inherits the parent's Python objects and mapping. It must not
        # append through the parent's transport: rotate to a child-owned file
        # on the first post-fork record instead.
        if self.output is not None or self.output_descriptor is not None:
            self._close_output(flush=False)
        self._open_output()

    @staticmethod
    def _checksum(payload: bytes) -> int:
        value = 0x811C9DC5
        for byte in payload:
            value ^= byte
            value = (value * 0x01000193) & 0xFFFFFFFF
        return value

    def _grow_output(self, required: int) -> bool:
        if self.output is None or self.output_descriptor is None:
            return False
        capacity = self.output_capacity
        while capacity < required and capacity < TRANSPORT_MAX_CAPACITY:
            capacity = min(capacity * 2, TRANSPORT_MAX_CAPACITY)
        if capacity < required:
            return False
        self.output.flush()
        self.output.close()
        os.ftruncate(self.output_descriptor, capacity)
        self.output = mmap.mmap(self.output_descriptor, capacity, access=mmap.ACCESS_WRITE)
        self.output_capacity = capacity
        struct.pack_into("<Q", self.output, 16, capacity)
        return True

    def _write_record(self, record: dict) -> None:
        if self.output is None:
            raise RuntimeError("Python evidence transport is not open")
        payload = json.dumps(record, separators=(",", ":"), sort_keys=True).encode("utf-8")
        if len(payload) > TRANSPORT_MAX_RECORD_SIZE:
            self._drop_record()
            return
        end = self.output_cursor + TRANSPORT_RECORD_HEADER_SIZE + len(payload)
        next_cursor = (end + 7) & ~7
        if next_cursor > self.output_capacity and not self._grow_output(next_cursor):
            self._drop_record()
            return
        output = self.output
        cursor = self.output_cursor
        output[cursor + TRANSPORT_RECORD_HEADER_SIZE : end] = payload
        if next_cursor > end:
            output[end:next_cursor] = b"\0" * (next_cursor - end)
        struct.pack_into("<II", output, cursor + 4, len(payload), self._checksum(payload))
        # The single-byte commit is deliberately last. A killed process can
        # leave bytes in an uncommitted frame, which the Rust reader ignores;
        # it cannot expose a committed record with a missing payload.
        output[cursor] = 1
        self.output_cursor = next_cursor

    def _drop_record(self) -> None:
        self.dropped_records += 1
        if self.output is not None:
            struct.pack_into("<Q", self.output, 24, self.dropped_records)

    def _record(self, record: dict) -> None:
        with self.lock:
            self._ensure_process_output()
            self._write_record(record)

    def flush(self) -> None:
        # mmap writes are visible through the kernel page cache immediately;
        # forcing every test phase through msync would add latency without
        # improving SIGKILL survival. `close` flushes once on an ordinary exit.
        return

    def limitation(self, identifier: str, reason: str, file: str | None = None, obligation: str | None = None) -> None:
        key = (identifier, file, obligation)
        with self.lock:
            if key in self.reported_limitations:
                return
            self.reported_limitations.add(key)
            record = {"t": "limitation", "id": identifier, "reason": reason}
            if file is not None:
                record["file"] = file
            if obligation is not None:
                record["obligation"] = obligation
            self._record(record)

    # -- identity -----------------------------------------------------------

    def set_worker(self, worker: str) -> None:
        with self.lock:
            if worker != self.worker:
                self.worker = worker
                self._record({"t": "worker", "worker": worker})

    def switch(self, identity: dict | None) -> int:
        """Enter a test phase (or background when `identity` is None).

        Allocates a process-local context id, records the identity it stands
        for, and re-arms every event location that earlier phases disabled so
        first sightings are observed again for this phase.
        """
        began = time.perf_counter() if TIMING else 0.0
        try:
            return self._switch(identity)
        finally:
            if TIMING:
                _timing["switch"][0] += 1
                _timing["switch"][1] += time.perf_counter() - began

    def _switch(self, identity: dict | None) -> int:
        with self.lock:
            if identity is None:
                context = 0
            else:
                context = self.next_context
                self.next_context += 1
                stored = {
                    "worker": identity.get("worker", self.worker),
                    "test": identity["test"],
                    "retry": int(identity.get("retry", 0)),
                    "phase": identity["phase"],
                }
                self.identities[context] = stored
                self._record({"t": "phase", "ctx": context, "at": _now_ms(), **stored})
            self.context.set(context)
            self.flush()
            touched = self.touched
            self.touched = []
        # Re-arm only the measured code objects the previous phase executed:
        # re-setting a code object's local events clears the locations it
        # disabled, so their first sightings are observed again in this phase.
        # Everything else (pytest, the standard library) stays disabled.
        if self.tool_id is not None:
            for info in touched:
                info.touched = False
                _monitoring.set_local_events(self.tool_id, info.code, 0)
                _monitoring.set_local_events(self.tool_id, info.code, info.events)
        return context

    def current_identity(self) -> dict | None:
        return self.identities.get(self.context.get())

    def child_environment(self) -> dict:
        """Environment additions that carry the current phase into a child
        interpreter. `PYTHONPATH` and the plan variables already inherit."""
        identity = self.current_identity()
        if identity is None:
            return {}
        return {CONTEXT_ENV: json.dumps(identity, separators=(",", ":"), sort_keys=True)}

    def outcome(self, worker: str, test: str, retry: int, phase: str, outcome: str, xfail: bool, runner: str = "pytest") -> None:
        with self.lock:
            self._record(
                {
                    "t": "outcome",
                    "worker": worker,
                    "test": test,
                    "retry": int(retry),
                    "phase": phase,
                    "outcome": outcome,
                    "xfail": bool(xfail),
                    "runner": runner,
                }
            )

    # -- observation --------------------------------------------------------

    def _hit(self, context: int, obligation: str) -> None:
        key = (context, obligation)
        if key in self.seen_hits:
            return
        with self.lock:
            if key in self.seen_hits:
                return
            self.seen_hits.add(key)
            self._record({"t": "hit", "ctx": context, "id": obligation})

    def _vector(self, context: int, decision: _Decision, values: list, outcome: bool) -> None:
        digits = "".join("0" if value is None else ("2" if value else "1") for value in values)
        key = (context, decision.id, digits)
        with self.lock:
            if key in self.seen_vectors:
                return
            self.seen_vectors.add(key)
            count_key = (context, decision.id)
            self.vector_counts[count_key] = self.vector_counts.get(count_key, 0) + 1
            self._record({"t": "dec", "ctx": context, "id": decision.id, "v": digits, "o": 1 if outcome else 0})
        self._hit(context, decision.outcome_true if outcome else decision.outcome_false)
        for logical in decision.logical:
            previous_reached = any(values[index] is not None for index in logical["previousLeaves"])
            operand_reached = any(values[index] is not None for index in logical["operandLeaves"])
            if operand_reached:
                self._hit(context, logical["evaluated"])
            elif previous_reached:
                self._hit(context, logical["shortCircuit"])

    # -- code object mapping ------------------------------------------------

    def _relative_path(self, filename: str) -> str | None:
        cached = self.path_cache.get(filename, self)
        if cached is not self:
            return cached
        relative = None
        if filename and not filename.startswith("<"):
            candidates = [filename]
            if not os.path.isabs(filename):
                candidates.append(os.path.join(os.getcwd(), filename))
            for candidate in candidates:
                try:
                    real = os.path.realpath(candidate)
                except OSError:
                    continue
                if real == self.root or real.startswith(self.root + os.sep):
                    relative = real[len(self.root) + 1 :].replace(os.sep, "/")
                    if relative in self.files:
                        break
                    relative = None
        self.path_cache[filename] = relative
        return relative

    def _branch_table(self, code, instructions):
        """offset -> (fallthrough, target) for every conditional branch.

        On 3.14 `co_branches()` names both successors; the not-taken
        successor is the one that is not the jump target, which skips the
        NOT_TAKEN glue instruction exactly as the BRANCH events do. Older
        interpreters have no glue, so the next instruction is the fall-through.
        """
        table = {}
        targets = {}
        for instruction in instructions:
            if instruction.opname in _BRANCH_OPNAMES:
                target = getattr(instruction, "jump_target", None)
                if target is None:
                    target = instruction.argval
                targets[instruction.offset] = target
        if hasattr(code, "co_branches"):
            for source, left, right in code.co_branches():
                target = targets.get(source, right)
                fallthrough = left if left != target else right
                table[source] = (fallthrough, target)
        else:
            for index, instruction in enumerate(instructions):
                if instruction.offset in targets:
                    following = instructions[index + 1].offset if index + 1 < len(instructions) else None
                    table[instruction.offset] = (following, targets[instruction.offset])
        return table

    def _build_code_info(self, code) -> _CodeInfo | None:
        relative = self._relative_path(code.co_filename)
        if relative is None:
            return None
        file_plan = self.files[relative]
        info = _CodeInfo(code, file_plan)
        instructions = list(dis.get_instructions(code))
        candidates = file_plan.functions.get((code.co_firstlineno, code.co_name), [])
        if len(candidates) == 1:
            info.function_id = candidates[0]["id"]
        elif candidates:
            # Several lambdas on one line: the code object's first positioned
            # instruction lies inside exactly one of their spans.
            for instruction in instructions:
                position = instruction.positions
                if position is None or position.lineno is None or position.col_offset is None:
                    continue
                if instruction.opname == "RESUME":
                    continue
                for candidate in candidates:
                    (start_line, start_col), (end_line, end_col) = candidate["span"]
                    inside = (start_line, start_col) <= (position.lineno, position.col_offset) and (
                        position.lineno,
                        position.col_offset,
                    ) <= (end_line, end_col)
                    if inside:
                        info.function_id = candidate["id"]
                        break
                break
        if not instructions:
            return info
        offsets = [instruction.offset for instruction in instructions]
        offset_indexes = {offset: index for index, offset in enumerate(offsets)}
        instructions_by_offset = {instruction.offset: instruction for instruction in instructions}
        opnames = {instruction.offset: instruction.opname for instruction in instructions}
        positions = {}
        for instruction in instructions:
            position = instruction.positions
            if position is None or position.lineno is None or position.col_offset is None:
                continue
            positions[instruction.offset] = (
                position.lineno,
                position.end_lineno if position.end_lineno is not None else position.lineno,
                position.col_offset,
                position.end_col_offset if position.end_col_offset is not None else position.col_offset,
            )
        if not positions:
            self.limitation(
                "python-code-positions-unavailable",
                "the interpreter compiled this code without column positions (PYTHONNODEBUGRANGES or -X no_debug_ranges)",
                relative,
            )
            return info
        info.positions = positions
        branches = self._branch_table(code, instructions)
        branch_offsets = sorted(branches)
        fallthrough = {offset: successors[0] for offset, successors in branches.items()}
        min_line = min(position[0] for position in positions.values())
        max_line = max(position[1] for position in positions.values())
        consumers = info.consumers

        def add_consumer(offset, consumer):
            consumers.setdefault(offset, []).append(consumer)

        claimed = set()
        # Decisions: leaf regions by position containment.
        for decision_plan, logical in file_plan.decisions:
            span = decision_plan["span"]
            if span[1][0] < min_line or span[0][0] > max_line:
                continue
            decision = _Decision(decision_plan, logical)
            mapped_leaves = 0
            last_branch_by_leaf = [None] * decision.width
            leaf_branches_by_index = [[] for _ in range(decision.width)]
            comprehension = decision_plan.get("comprehension")
            for offset in ([] if comprehension is not None else branch_offsets):
                position = positions.get(offset)
                if position is None:
                    continue
                for index, condition in enumerate(decision.conditions):
                    if _span_contains(condition["span"], *position):
                        last_branch_by_leaf[index] = offset
            for index, condition in enumerate(decision.conditions):
                last_branch = last_branch_by_leaf[index]
                if last_branch is None:
                    continue
                mapped_leaves += 1
                for offset, position in positions.items():
                    if offset <= last_branch and _span_contains(condition["span"], *position):
                        decision.regions[index].add(offset)
                leaf_branches = [
                    offset
                    for offset in branch_offsets
                    if offset in decision.regions[index]
                    and _jump_sense(opnames[offset], condition) is not None
                ]
                leaf_branches_by_index[index] = leaf_branches
            if comprehension is not None:
                # CPython 3.13+ stamps inlined-comprehension filter jumps with
                # the element's position, so positions cannot be trusted here.
                # Filter jumps follow the loop's FOR_ITER and precede the
                # element's own jumps in offset order, so the first `width`
                # unclaimed conditional jumps inside the comprehension are its
                # conditions in source order.
                candidates = [
                    offset
                    for offset in branch_offsets
                    if offset not in claimed
                    and offset in positions
                    and _line_in_span(comprehension, positions[offset][0])
                    and opnames[offset] != "FOR_ITER"
                ]
                candidates = candidates[: decision.width]
                if len(candidates) == decision.width:
                    previous = -1
                    for index, offset in enumerate(candidates):
                        jumps_true = _jump_sense(opnames[offset], decision.conditions[index])
                        if jumps_true is None:
                            break
                        decision.regions[index] = {o for o in offsets if previous < o <= offset}
                        previous = offset
                        leaf_branches_by_index[index] = [offset]
                        mapped_leaves += 1
            if mapped_leaves == 0:
                continue
            if mapped_leaves < decision.width:
                self.limitation(
                    "python-decision-partially-mapped",
                    "some conditions of this decision produce no conditional jump in the compiled bytecode (constant folding or unsupported shape); the decision is not measured",
                    relative,
                    decision.id,
                )
                for offset in list(consumers):
                    consumers[offset] = [c for c in consumers[offset] if not (c[0] == "leaf" and c[1] is decision)]
                    if not consumers[offset]:
                        del consumers[offset]
                continue
            decision.region_union = set().union(*decision.regions)
            leaf_indexes = {
                offset: index
                for index, leaf_branches in enumerate(leaf_branches_by_index)
                for offset in leaf_branches
            }

            def next_leaf(start):
                """Follow straight-line/unconditional flow until another leaf
                jump or the decision region ends. CPython 3.14 branch events
                land on positioned NOT_TAKEN glue, so destination containment
                alone cannot tell whether a compound leaf continues."""
                cursor = start
                visited = set()
                while cursor is not None and cursor not in visited:
                    visited.add(cursor)
                    if cursor in leaf_indexes:
                        return (leaf_indexes[cursor], cursor)
                    instruction = instructions_by_offset.get(cursor)
                    if instruction is None:
                        return None
                    if cursor not in decision.region_union and instruction.opname != "NOT_TAKEN":
                        return None
                    if instruction.opname == "SEND":
                        # The successful await path jumps directly to
                        # END_SEND; the fall-through suspends and loops.
                        cursor = instruction.argval
                        continue
                    if instruction.opname.startswith("JUMP"):
                        target = getattr(instruction, "jump_target", None)
                        cursor = target if target is not None else instruction.argval
                        continue
                    position = offset_indexes[cursor]
                    cursor = offsets[position + 1] if position + 1 < len(offsets) else None
                return None

            for index, leaf_branches in enumerate(leaf_branches_by_index):
                condition = decision.conditions[index]
                single_jump = len(leaf_branches) == 1
                for offset in leaf_branches:
                    jumps_true = _jump_sense(opnames[offset], condition)
                    if jumps_true is None:
                        continue
                    branch_fallthrough, branch_target = branches[offset]
                    def advancing_leaf(start):
                        following = next_leaf(start)
                        if following is None:
                            return None
                        following_index, following_offset = following
                        if following_index > index or (
                            following_index == index and following_offset > offset
                        ):
                            return following_index
                        # A comprehension filter can jump back through
                        # FOR_ITER to this or an earlier leaf. That starts the
                        # next element's evaluation; it does not continue the
                        # current vector.
                        return None

                    claimed.add(offset)
                    add_consumer(
                        offset,
                        (
                            "leaf",
                            decision,
                            index,
                            jumps_true,
                            condition["not"] % 2 == 1,
                            branch_fallthrough,
                            advancing_leaf(branch_fallthrough),
                            advancing_leaf(branch_target),
                            single_jump,
                        ),
                    )
        # Loops: FOR_ITER positioned at the iterable.
        for loop in file_plan.loops:
            for offset in branch_offsets:
                position = positions.get(offset)
                if position is None or offset in claimed:
                    continue
                if opnames[offset] == "FOR_ITER" and _span_contains(loop["iter"], *position):
                    claimed.add(offset)
                    add_consumer(offset, ("loop", loop, fallthrough[offset]))
                    break
        # Value-context logical operators: jumps stamped with the BoolOp span,
        # in operand order.
        by_boolop = {}
        for logical in file_plan.value_logical:
            span = tuple(map(tuple, logical["boolop"]))
            by_boolop.setdefault(span, []).append(logical)
        for span, logicals in by_boolop.items():
            matching = [
                offset
                for offset in branch_offsets
                if offset in positions and offset not in claimed and _span_equals(span, *positions[offset])
            ]
            logicals.sort(key=lambda item: item["operand"])
            for logical in logicals:
                index = logical["operand"] - 1
                if index < len(matching):
                    claimed.add(matching[index])
                    add_consumer(matching[index], ("logical", logical, fallthrough[matching[index]]))
        # Match cases: pattern and guard jumps. A taken jump that does not land
        # in the case body is a failed test (the failure path runs through a
        # POP_TOP still positioned at the pattern); the last refutable case
        # failing means no case matched.
        for match in file_plan.matches:
            cases = match["cases"]
            for case_index, case in enumerate(cases):
                is_last = case_index == len(cases) - 1
                for offset in branch_offsets:
                    position = positions.get(offset)
                    if position is None or offset in claimed:
                        continue
                    if _span_contains(case["test"], *position):
                        claimed.add(offset)
                        add_consumer(offset, ("case", match, case, fallthrough[offset], is_last))
        # Statements that share a line: the first instruction stamped with the
        # statement's own start position proves it.
        for span, statement_id in file_plan.exact_statements:
            # The first instruction stamped inside the statement's own span:
            # `return "a"` starts at `"a"`, `y = 2` at `2`.
            for offset in offsets:
                position = positions.get(offset)
                if position is not None and _span_contains(span, *position):
                    info.instructions.setdefault(offset, []).append(("stmt", statement_id, False))
                    break
        for try_plan in file_plan.tries:
            self._map_try(info, try_plan, instructions, offsets, positions, opnames, branches, fallthrough, claimed, add_consumer)
        # A DISABLE returned from the LINE callback also drops the INSTRUCTION
        # event of that same instruction for the current pass, so lines that
        # carry an instruction consumer keep their LINE event live.
        info.live_lines = frozenset(positions[o][0] for o in info.instructions if o in positions)
        # With INSTRUCTION instrumentation active, CPython 3.14 reports a
        # not-taken branch from the NOT_TAKEN glue instruction's offset rather
        # than the jump's; alias the glue back to its jump.
        for offset in branch_offsets:
            index = offsets.index(offset)
            if index + 1 < len(offsets) and opnames[offsets[index + 1]] == "NOT_TAKEN":
                info.aliases[offsets[index + 1]] = offset
        info.has_branches = bool(consumers)
        return info

    def _map_try(self, info, try_plan, instructions, offsets, positions, opnames, branches, fallthrough, claimed, add_consumer):
        """Exception flow from structure alone: the body's normal completion,
        handler type tests, handler entry and `finally` copies all have fixed
        instruction positions, so no global exception event is needed."""
        body = try_plan["body"]
        orelse = try_plan.get("orelse")
        finalbody = try_plan.get("finalbody")
        in_body = [offset for offset in offsets if offset in positions and _span_contains(body, *positions[offset])]
        if not in_body:
            return
        body_first, body_last = in_body[0], in_body[-1]
        normal_region = set(in_body)
        if orelse is not None:
            normal_region.update(o for o in offsets if o in positions and _span_contains(orelse, *positions[o]))
        handler_offsets = set()
        for handler in try_plan["handlers"]:
            header = handler["header"]
            body_start, body_end = handler["bodyLines"]
            for offset in offsets:
                position = positions.get(offset)
                if position is None:
                    continue
                if _span_contains_start(header, *position) or body_start <= position[0] <= body_end:
                    handler_offsets.add(offset)
            if handler["bare"]:
                continue
            for offset in sorted(branches):
                position = positions.get(offset)
                if position is None or offset in claimed or not _span_contains_start(header, *position):
                    continue
                claimed.add(offset)
                add_consumer(offset, ("handler", handler, fallthrough[offset]))
        # Returns and breaks that leave the body without an exception.
        for offset in in_body:
            opname = opnames[offset]
            if opname in ("RETURN_VALUE", "RETURN_CONST"):
                info.returns[offset] = try_plan
            elif opname.startswith("JUMP") and not opname.startswith("JUMP_BACKWARD_NO"):
                target = getattr(instructions[offsets.index(offset)], "jump_target", None)
                if target is None:
                    target = instructions[offsets.index(offset)].argval
                if target not in normal_region:
                    info.jumps.setdefault(offset, []).append(("body_exit", try_plan))
        if finalbody is not None:
            # Every copy of the finally body is a separate run of instructions.
            # A copy preceded by body/else code is a success path; one that
            # ends in RERAISE (preceded by PUSH_EXC_INFO) is the exceptional
            # path; handler-path copies carry nothing new.
            copies = []
            current = []
            for offset in offsets:
                position = positions.get(offset)
                inside = position is not None and _span_contains(finalbody, *position)
                if inside:
                    current.append(offset)
                elif current:
                    copies.append(current)
                    current = []
            if current:
                copies.append(current)
            for copy in copies:
                first = copy[0]
                index = offsets.index(first)
                preceding = offsets[index - 1] if index > 0 else None
                exceptional = any(opnames[o] == "RERAISE" for o in copy) or (
                    preceding is not None and opnames[preceding] == "PUSH_EXC_INFO"
                )
                if exceptional:
                    info.instructions.setdefault(first, []).append(("try_raised", try_plan, False))
                elif preceding in normal_region:
                    info.instructions.setdefault(first, []).append(("try_success", try_plan, False))
            return
        # Without finally, normal completion falls into the instruction after
        # the body (or else block). Handlers usually jump back to that same
        # merge point, so the merge stays live and each handler exit is
        # subtracted before an arrival counts as a success.
        merge = next((o for o in offsets if o > max(normal_region)), None)
        if merge is None:
            return
        handler_exits = []
        for offset in sorted(handler_offsets):
            opname = opnames[offset]
            if opname.startswith("JUMP"):
                instruction = instructions[offsets.index(offset)]
                target = getattr(instruction, "jump_target", None)
                if target is None:
                    target = instruction.argval
                if target == merge:
                    handler_exits.append(offset)
        if handler_exits:
            for offset in handler_exits:
                info.jumps.setdefault(offset, []).append(("handler_exit", try_plan))
            info.instructions.setdefault(merge, []).append(("try_merge", try_plan, True))
        else:
            info.instructions.setdefault(merge, []).append(("try_success", try_plan, False))
        del body_first, body_last

    def _cached_code_info(self, code):
        """Cache lookup keyed by id(code), guarded against id reuse: pytest
        collection creates and frees many code objects, so a dead object's id
        can come back on a live one. The weak reference proves identity."""
        entry = self.code_cache.get(id(code))
        if entry is None:
            return None
        reference, info = entry
        if reference() is code:
            return entry
        del self.code_cache[id(code)]
        return None

    def code_info(self, code) -> _CodeInfo | None:
        entry = self._cached_code_info(code)
        if entry is not None:
            return entry[1]
        try:
            info = self._build_code_info(code)
        except Exception as error:  # noqa: BLE001 - measurement must never break the run
            self.limitation(
                "python-code-mapping-failed",
                f"could not map {code.co_name} in {code.co_filename}: {error!r}",
                self._relative_path(code.co_filename),
            )
            info = None
        self.code_cache[id(code)] = (weakref.ref(code), info)
        return info

    # -- sys.monitoring callbacks -------------------------------------------

    def _on_start(self, code, offset):
        # PY_START only discovers code objects: each fires once per process
        # and is then disabled for good. Function entry is proven by the
        # first LINE event inside the code object, which the per-phase re-arm
        # keeps exact without re-firing PY_START for every unrelated frame.
        info = self.code_info(code)
        if info is not None and not info.armed:
            info.armed = True
            events = _monitoring.events.LINE
            if info.has_branches:
                if self.branch_pairs:
                    events |= _monitoring.events.BRANCH_LEFT | _monitoring.events.BRANCH_RIGHT
                else:
                    events |= _monitoring.events.BRANCH
            if info.instructions:
                events |= _monitoring.events.INSTRUCTION
            if info.jumps:
                events |= _monitoring.events.JUMP
            if info.returns:
                events |= _monitoring.events.PY_RETURN
            info.events = events
            _monitoring.set_local_events(self.tool_id, code, events)
        return _monitoring.DISABLE

    def _on_instruction(self, code, offset):
        entry = self._cached_code_info(code)
        info = entry[1] if entry is not None else None
        if info is None:
            return _monitoring.DISABLE
        consumers = info.instructions.get(offset)
        if not consumers:
            # A DISABLE here on a conditional jump also silences its BRANCH
            # event, so instructions that carry branch consumers stay live.
            return None if offset in info.consumers else _monitoring.DISABLE
        context = self.context.get()
        if not info.touched:
            self._touch(info)
        live = offset in info.consumers
        for kind, target, keep in consumers:
            if kind == "stmt":
                self._hit(context, target)
            elif kind == "try_success":
                self._hit(context, target["success"])
            elif kind == "try_raised":
                self._hit(context, target["raised"])
            elif kind == "try_merge":
                key = (context, target["id"])
                pending = self.pending_handler_exits.get(key, 0)
                if pending > 0:
                    self.pending_handler_exits[key] = pending - 1
                else:
                    self._hit(context, target["success"])
            live = live or keep
        return None if live else _monitoring.DISABLE

    def _on_jump(self, code, offset, destination):
        entry = self._cached_code_info(code)
        info = entry[1] if entry is not None else None
        if info is None:
            return _monitoring.DISABLE
        consumers = info.jumps.get(offset)
        if not consumers:
            return _monitoring.DISABLE
        context = self.context.get()
        if not info.touched:
            self._touch(info)
        live = False
        for kind, try_plan in consumers:
            if kind == "body_exit":
                self._hit(context, try_plan["success"])
            elif kind == "handler_exit":
                key = (context, try_plan["id"])
                self.pending_handler_exits[key] = self.pending_handler_exits.get(key, 0) + 1
                live = True
        return None if live else _monitoring.DISABLE

    def _on_return(self, code, offset, value):
        entry = self._cached_code_info(code)
        info = entry[1] if entry is not None else None
        if info is None:
            return _monitoring.DISABLE
        try_plan = info.returns.get(offset)
        if try_plan is not None:
            if not info.touched:
                self._touch(info)
            self._hit(self.context.get(), try_plan["success"])
        return _monitoring.DISABLE

    def _touch(self, info):
        if not info.touched:
            info.touched = True
            self.touched.append(info)

    def _on_line(self, code, line):
        entry = self._cached_code_info(code)
        info = entry[1] if entry is not None else None
        if info is None:
            return _monitoring.DISABLE
        context = self.context.get()
        if not info.touched:
            self._touch(info)
        if info.function_id is not None:
            self._hit(context, info.function_id)
        statement = info.statements.get(line)
        if statement is not None:
            self._hit(context, statement)
        alternatives = info.line_alternatives.get(line)
        if alternatives:
            for alternative in alternatives:
                self._hit(context, alternative)
        return None if line in info.live_lines else _monitoring.DISABLE

    def _on_branch(self, code, offset, destination):
        entry = self._cached_code_info(code)
        info = entry[1] if entry is not None else None
        if info is None:
            return _monitoring.DISABLE
        consumers = info.consumers.get(offset)
        if not consumers:
            alias = info.aliases.get(offset)
            consumers = info.consumers.get(alias) if alias is not None else None
            if not consumers:
                return _monitoring.DISABLE
        context = self.context.get()
        if not info.touched:
            self._touch(info)
        seen_hits = self.seen_hits
        exhausted = True
        for consumer in consumers:
            kind = consumer[0]
            if kind == "leaf":
                if self._leaf_event(context, consumer, destination) == "quiet":
                    continue
                decision = consumer[1]
                if decision.possible is None or self.vector_counts.get((context, decision.id), 0) < decision.possible:
                    exhausted = False
            elif kind == "loop":
                _, loop, fallthrough = consumer
                key = (context, loop["id"])
                if destination == fallthrough:
                    self.loop_entered[key] = True
                    self._hit(context, loop["entered"])
                    # FOR_ITER fires on every iteration through this direction.
                    # With per-direction events (3.14+) only the body direction
                    # goes quiet; the exit direction below re-arms the code
                    # object so the next execution of the loop is observed
                    # afresh and a zero-iteration run is not missed.
                    if self.branch_pairs:
                        return _monitoring.DISABLE
                    exhausted = False
                else:
                    if not self.loop_entered.get(key):
                        self._hit(context, loop["zero"])
                    self.loop_entered[key] = False
                    if self.branch_pairs:
                        rearms = self.loop_rearms.get(key, 0)
                        if rearms < MAX_LOOP_REARMS:
                            self.loop_rearms[key] = rearms + 1
                            _monitoring.set_local_events(self.tool_id, code, 0)
                            _monitoring.set_local_events(self.tool_id, code, info.events)
                            return None
                        # Beyond the cap this loop stops reporting for the
                        # phase; the coverage model declares the bound.
                        return _monitoring.DISABLE
                    if (context, loop["entered"]) not in seen_hits or (context, loop["zero"]) not in seen_hits:
                        exhausted = False  # single BRANCH event: stay live for exactness
            elif kind == "logical":
                _, logical, fallthrough = consumer
                self._hit(context, logical["evaluated"] if destination == fallthrough else logical["shortCircuit"])
                if (context, logical["evaluated"]) not in seen_hits or (context, logical["shortCircuit"]) not in seen_hits:
                    exhausted = False
            elif kind == "handler":
                _, handler, fallthrough = consumer
                if destination != fallthrough:
                    position = info.positions.get(destination)
                    body_start, body_end = handler["bodyLines"]
                    if position is None or not (body_start <= position[0] <= body_end):
                        self._hit(context, handler["missed"])
                if (context, handler["missed"]) not in seen_hits:
                    exhausted = False
            elif kind == "case":
                _, match, case, fallthrough, is_last = consumer
                if destination != fallthrough:
                    position = info.positions.get(destination)
                    body_start, body_end = case["bodyLines"]
                    if position is None or not (body_start <= position[0] <= body_end):
                        self._hit(context, case["missed"])
                        no_case = match.get("noCase")
                        if no_case is not None and is_last:
                            self._hit(context, no_case["unmatched"])
                if (context, case["missed"]) not in seen_hits:
                    exhausted = False
        # Every observation this location can still contribute in the current
        # phase has been made; the next phase switch re-arms it.
        return _monitoring.DISABLE if exhausted else None

    def _leaf_event(self, context, consumer, destination):
        (
            _,
            decision,
            index,
            jumps_if_true,
            inverted,
            fallthrough,
            fallthrough_leaf,
            target_leaf,
            single_jump,
        ) = consumer
        taken = destination != fallthrough
        next_leaf = target_leaf if taken else fallthrough_leaf
        if next_leaf == index:
            # This jump selects another operand inside one source condition,
            # such as the `a` jump in `b if a else c`. It is not itself the
            # condition's value; the later jump supplies that value.
            return "quiet"
        value = (jumps_if_true == taken) != inverted
        prefix = decision.prefixes[index]
        if prefix is not None and self.branch_pairs and single_jump:
            # Every evaluation reaching this leaf took the same earlier values,
            # so this one event determines the whole vector when it leaves the
            # decision, and carries nothing new when it continues. Either way
            # the direction can go quiet until the next phase re-arms it.
            if next_leaf is None:
                values = [None] * decision.width
                for earlier, earlier_value in prefix.items():
                    values[earlier] = earlier_value
                values[index] = value
                reached = set()
                outcome = _evaluate_tree(decision.tree, values, reached)
                if outcome is None or reached != {i for i, v in enumerate(values) if v is not None}:
                    self.limitation(
                        "python-decision-vector-inconsistent",
                        "observed conditional jumps do not form a short-circuit evaluation of the source decision",
                        decision_file(self, decision),
                        decision.id,
                    )
                else:
                    self._vector(context, decision, values, outcome)
            return "quiet"
        key = (context, decision.id)
        stack = self.open_evaluations.get(key)
        if stack is None:
            stack = self.open_evaluations[key] = []
        if not stack or index < stack[-1].last or (index == stack[-1].last and stack[-1].exited):
            if len(stack) >= MAX_OPEN_EVALUATIONS:
                del stack[0]
            stack.append(_Evaluation(decision.width))
        evaluation = stack[-1]
        evaluation.last = index
        evaluation.values[index] = value
        if DEBUG:
            sys.stderr.write(
                f"[supercov:debug] leaf {decision.id} index={index} taken={taken} value={value} "
                f"dest={destination} fallthrough={fallthrough} single_jump={single_jump} "
                f"region={sorted(decision.regions[index])} union={sorted(decision.region_union)} "
                f"values={evaluation.values} depth={len(stack)}\n"
            )
        # The destination decides everything: CPython may lay a chain out with
        # the taken jump continuing to the next condition and the fall-through
        # leaving (inside loops), or the other way round. Landing inside any
        # condition's region means the evaluation goes on; anything else
        # (a then/else block, a chained-comparison cleanup, a loop exit) ends
        # it. Earlier jumps inside one leaf (chained comparisons, a ternary
        # used as a condition) stay inside that leaf's region.
        evaluation.exited = next_leaf is None
        if next_leaf is not None:
            return
        stack.pop()
        reached = set()
        outcome = _evaluate_tree(decision.tree, evaluation.values, reached)
        observed = {i for i, v in enumerate(evaluation.values) if v is not None}
        if outcome is None or reached != observed:
            if DEBUG:
                sys.stderr.write(
                    f"[supercov:debug] inconsistent {decision.id} values={evaluation.values} "
                    f"outcome={outcome} reached={sorted(reached)} observed={sorted(observed)} tree={decision.tree}\n"
                )
            self.limitation(
                "python-decision-vector-inconsistent",
                "observed conditional jumps do not form a short-circuit evaluation of the source decision",
                decision_file(self, decision),
                decision.id,
            )
            return
        self._vector(context, decision, evaluation.values, outcome)

    # -- installation -------------------------------------------------------

    def install(self) -> None:
        with self.lock:
            if self.tool_id is not None:
                return
            for candidate in (3, 4, 1):
                if _monitoring.get_tool(candidate) is None:
                    _monitoring.use_tool_id(candidate, "supercov")
                    self.tool_id = candidate
                    break
            if self.tool_id is None:
                raise RuntimeError("no free sys.monitoring tool id for Supercov")
            self._open_output()
        events = _monitoring.events
        on_start = _timed("start", self._on_start)
        on_line = _timed("line", self._on_line)
        on_branch = _timed("branch", self._on_branch)
        _monitoring.register_callback(self.tool_id, events.PY_START, on_start)
        _monitoring.register_callback(self.tool_id, events.LINE, on_line)
        _monitoring.register_callback(self.tool_id, events.INSTRUCTION, _timed("instruction", self._on_instruction))
        _monitoring.register_callback(self.tool_id, events.JUMP, _timed("jump", self._on_jump))
        _monitoring.register_callback(self.tool_id, events.PY_RETURN, _timed("return", self._on_return))
        if self.branch_pairs:
            _monitoring.register_callback(self.tool_id, events.BRANCH_LEFT, on_branch)
            _monitoring.register_callback(self.tool_id, events.BRANCH_RIGHT, on_branch)
        else:
            _monitoring.register_callback(self.tool_id, events.BRANCH, on_branch)
        _monitoring.set_events(self.tool_id, events.PY_START)
        inherited = os.environ.get(CONTEXT_ENV)
        if inherited:
            try:
                self.switch(json.loads(inherited))
            except (ValueError, KeyError, TypeError):
                self.limitation("python-inherited-context-invalid", "SUPERCOV_CONTEXT was not a valid Supercov identity")
        atexit.register(self.close)
        _install_propagation(self)
        try:
            import supercov_unittest

            supercov_unittest.install(self)
        except Exception as error:  # noqa: BLE001 - the adapter must never break the interpreter
            self.limitation("python-unittest-adapter-unavailable", f"unittest adapter failed to install: {error!r}")

    def close(self) -> None:
        with self.lock:
            if self.closed:
                return
            self.closed = True
            self._record({"t": "exit", "at": _now_ms()})
            self._close_output(flush=True)
            unmatched = (
                self.worker == "main"
                and bool(self.path_cache)
                and all(relative is None for relative in self.path_cache.values())
            )
        if unmatched:
            # Every executed code object lay outside the measured tree. That is
            # what a run reports as zero coverage without a word of explanation
            # -- on Windows the root once carried a `\\?\` prefix its files did
            # not -- so name the root and one file that missed it.
            sample = next(
                (name for name in self.path_cache if not name.startswith("<")),
                next(iter(self.path_cache)),
            )
            sys.stderr.write(
                f"[supercov] none of the {len(self.path_cache)} executed files lay under "
                f"the measured root {self.root}; for example {sample}\n"
            )
        if TIMING:
            sys.stderr.write(
                "[supercov:timing] "
                + " ".join(f"{name}={count} calls/{seconds * 1000:.0f}ms" for name, (count, seconds) in _timing.items())
                + f" code_objects={len(self.code_cache)}\n"
            )


def decision_file(runtime: Runtime, decision: _Decision) -> str | None:
    for path, file_plan in runtime.files.items():
        for plan, _ in file_plan.decisions:
            if plan["id"] == decision.id:
                return path
    return None


# -- causal context propagation ----------------------------------------------


def _install_propagation(runtime: Runtime) -> None:
    import concurrent.futures
    import multiprocessing.process
    import subprocess

    if not getattr(threading.Thread, "_supercov_patched", False):
        original_start = threading.Thread.start

        def start_with_context(thread, *args, **kwargs):
            if not hasattr(thread, "_supercov_original_run"):
                context = contextvars.copy_context()
                original_run = thread.run
                thread._supercov_original_run = original_run

                def run_with_context():
                    return context.run(original_run)

                thread.run = run_with_context
            return original_start(thread, *args, **kwargs)

        threading.Thread.start = start_with_context
        threading.Thread._supercov_patched = True

        original_submit = concurrent.futures.ThreadPoolExecutor.submit

        def submit_with_context(executor, function, /, *args, **kwargs):
            context = contextvars.copy_context()
            return original_submit(executor, context.run, function, *args, **kwargs)

        concurrent.futures.ThreadPoolExecutor.submit = submit_with_context

    if not getattr(subprocess.Popen, "_supercov_patched", False):
        original_init = subprocess.Popen.__init__

        def init_with_context(process, *args, **kwargs):
            additions = runtime.child_environment()
            if additions:
                environment = dict(os.environ if kwargs.get("env") is None else kwargs["env"])
                environment.update(additions)
                kwargs["env"] = environment
            original_init(process, *args, **kwargs)

        subprocess.Popen.__init__ = init_with_context
        subprocess.Popen._supercov_patched = True

    process_type = multiprocessing.process.BaseProcess
    if not getattr(process_type, "_supercov_patched", False):
        original_process_start = process_type.start
        environment_lock = threading.Lock()

        def process_start_with_context(process, *args, **kwargs):
            additions = runtime.child_environment()
            if not additions:
                return original_process_start(process, *args, **kwargs)
            with environment_lock:
                previous = {key: os.environ.get(key) for key in additions}
                os.environ.update(additions)
                try:
                    return original_process_start(process, *args, **kwargs)
                finally:
                    for key, value in previous.items():
                        if value is None:
                            os.environ.pop(key, None)
                        else:
                            os.environ[key] = value

        process_type.start = process_start_with_context
        process_type._supercov_patched = True


# -- module-level singleton --------------------------------------------------

_RUNTIME: Runtime | None = None
_INSTALL_ERROR: str | None = None


def runtime() -> Runtime | None:
    return _RUNTIME


def install() -> Runtime | None:
    global _RUNTIME, _INSTALL_ERROR
    if _RUNTIME is not None or _INSTALL_ERROR is not None:
        return _RUNTIME
    plan_path = os.environ.get(PLAN_ENV)
    evidence_dir = os.environ.get(EVIDENCE_DIR_ENV)
    run_id = os.environ.get(RUN_ID_ENV)
    if not plan_path or not evidence_dir or not run_id:
        return None
    if sys.version_info < (3, 12):
        _INSTALL_ERROR = f"Supercov requires CPython 3.12 or newer, found {sys.version.split()[0]}"
        sys.stderr.write(f"[supercov] {_INSTALL_ERROR}\n")
        return None
    try:
        instance = Runtime(plan_path, evidence_dir, run_id, os.environ.get(WORKER_ENV, "main"))
        instance.install()
    except Exception as error:  # noqa: BLE001 - never break the user's interpreter
        _INSTALL_ERROR = repr(error)
        sys.stderr.write(f"[supercov] Python runtime disabled: {_INSTALL_ERROR}\n")
        return None
    _RUNTIME = instance
    return instance
