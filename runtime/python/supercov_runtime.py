"""Supercov's stdlib-only Python runtime.

Rust decides the denominator ahead of the run and ships it as a plan: every
statement, function, decision, loop, match and exception-flow obligation
with a stable id and a source span. `supercov_probes` compiles a probe for
each into the measured modules as they are imported; this module owns what
the probes write into, and turns it into evidence per test phase. It never
changes a file on disk, never computes a coverage verdict, and imports
nothing outside the standard library.

Where a probe writes. Every context -- a test phase, or the background -- is
handed a slot: a small file mapped into memory, one byte per obligation and
a region per decision. A probe is a byte store into the current context's
slot. The kernel owns those pages, so what a process observed outlives the
process however it ends -- `os._exit`, SIGTERM, SIGKILL -- and the reader
collects whatever no harvest reached. A harvest turns set bytes into records
on the evidence transport and clears them; it runs when a phase ends, at a
test's first assertion and at exit. A slot no thread or task holds any more
is harvested once more and handed to the next context: a mapping is never
unmapped while the process measures, because unmapping dirty shared pages
flushes them synchronously (3 ms on macOS, per phase).

Supported interpreters: CPython 3.9 and newer.
"""

from __future__ import annotations

import atexit
import bisect
import contextvars
import itertools
import json
import mmap
import os
import struct
import sys
import threading
import time
import zlib

PLAN_VERSION = 1
EVIDENCE_VERSION = 1
CONTEXT_ENV = "SUPERCOV_CONTEXT"
PLAN_ENV = "SUPERCOV_PYTHON_PLAN"
EVIDENCE_DIR_ENV = "SUPERCOV_PYTHON_EVIDENCE_DIR"
RUN_ID_ENV = "SUPERCOV_RUN_ID"
WORKER_ENV = "SUPERCOV_PYTHON_WORKER"
TIMING = bool(os.environ.get("SUPERCOV_PYTHON_TIMING"))
# Debug only: leave out one piece of the runtime to measure what it costs --
# `harvest` skips turning slots into records, `detector` the unprobed-module
# detector.
STUB = os.environ.get("SUPERCOV_PYTHON_STUB", "")
_timing = {"switch": [0, 0.0], "harvest": [0, 0.0]}

TRANSPORT_MAGIC = b"SCVPYTH1"
TRANSPORT_VERSION = 2
TRANSPORT_HEADER_SIZE = 64
TRANSPORT_RECORD_HEADER_SIZE = 16
TRANSPORT_INITIAL_CAPACITY = 1024 * 1024
TRANSPORT_MAX_CAPACITY = 512 * 1024 * 1024
TRANSPORT_MAX_RECORD_SIZE = 4 * 1024 * 1024
MAX_OPEN_EVALUATIONS = 64
# The slot layout every process of a run shares, written once beside the
# evidence. A slot file names the layout in its header; a zeroed name marks
# a slot its process closed, whose bytes the reader must not count.
LAYOUT_NAME = "layout.json"
SLOT_SUFFIX = ".slot"
# Records of one harvest are chunked under the transport's record bound.
HARVEST_CHUNK = 40000

_monitoring = getattr(sys, "monitoring", None)  # 3.12+: only the unprobed-module detector uses it


def _now_ms() -> int:
    return int(time.time() * 1000)


def _digits(mask: int, width: int) -> str:
    """A base-3 evaluation mask as the vector digits the evidence carries:
    per condition 0 not evaluated, 1 false, 2 true."""
    out = []
    for _ in range(width):
        out.append("012"[mask % 3])
        mask //= 3
    return "".join(out)


class _Decision:
    """What a decision's vectors imply, beyond themselves."""

    __slots__ = ("id", "width", "outcome_true", "outcome_false", "logical")

    def __init__(self, plan: dict, logical: list) -> None:
        self.id = plan["id"]
        self.width = len(plan["conditions"])
        self.outcome_true = plan["outcomeTrue"]
        self.outcome_false = plan["outcomeFalse"]
        self.logical = logical

    def implied(self, digits: str, outcome: bool, into: list) -> None:
        """The outcome hit, and each logical operator's: evaluated when a leaf
        of its right operand was, short-circuited when only earlier ones were."""
        into.append(self.outcome_true if outcome else self.outcome_false)
        for logical in self.logical:
            if any(digits[index] != "0" for index in logical["operandLeaves"]):
                into.append(logical["evaluated"])
            elif any(digits[index] != "0" for index in logical["previousLeaves"]):
                into.append(logical["shortCircuit"])


class _Hits(mmap.mmap):
    """A slot: one small file mapped for the probes, handed to one context
    at a time.

    A subclass so it can carry its context, the evaluations in progress of
    its multi-condition decisions, and the file it maps.
    """

    __slots__ = ("context", "states", "path", "descriptor")


class _Sink(bytearray):
    """Where probes write once the runtime has closed, or when no slot could
    be mapped: nothing it holds is recorded."""

    __slots__ = ("context", "states")


class _DecisionState:
    """One multi-condition decision's evaluation in one context.

    `mask` holds a base-3 digit per condition for the evaluation in progress.
    A nested evaluation of the same decision -- a recursive call inside a
    condition -- saves the outer one and restores it when the inner closes.
    """

    __slots__ = ("mask", "last", "saved")

    def __init__(self) -> None:
        self.mask = 0
        self.last = -1
        self.saved: list = []


class Runtime:
    def __init__(self, plan_path: str, evidence_dir: str, run_id: str, worker: str) -> None:
        import supercov_probes

        root, index = supercov_probes.load_plan(plan_path, PLAN_VERSION)
        self.root = os.path.realpath(root)
        self.evidence_dir = evidence_dir
        self.run_id = run_id
        self.worker = worker
        self.tool_id = None
        self.lock = threading.RLock()
        self.path_cache: dict[str, str | None] = {}
        self.under_root = False
        self.context = contextvars.ContextVar("supercov_python_context", default=0)
        self.identities: dict[int, dict] = {}
        self.next_context = 1
        self.asserted: set[int] = set()
        # (context, file, line) of every assertion site a test has reached.
        self.asserted_sites: set[tuple[int, str, int]] = set()
        self.seen_hits: set = set()
        self.seen_vectors: set = set()
        self._id_json: dict = {}
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
        self.registered_events: tuple = ()
        # The plan, numbered into the slot layout; see supercov_probes.
        self.index = index
        self.probe_files = index.files
        self.probe_sites = index.sites
        self.probe_ids = index.ids
        self.probe_decisions = index.decisions
        self.loaded_decisions: dict[int, _Decision] = {}
        self.probe_boolops = index.boolop_groups
        self.regions = index.regions
        self.slot_header = supercov_probes.SLOT_HEADER
        self.slot_bytes = index.slot_bytes
        self.slot_tag = bytes.fromhex(index.digest[:16])
        # Regions in slot order, for the harvest to find a set byte's decision.
        self.region_starts = [start for start, _, _ in index.region_table]
        self.region_decisions = index.region_table
        self.pow3 = [3**i for i in range(index.max_width + 1)]
        self.live_slots: list = []
        self.free_slots: list = []
        self.next_slot = 0
        # How many references `_holders` sees to an object only the slot
        # list holds: taken through the same code, so it is right whatever
        # the interpreter counts along the way.
        self.free_refs = self._holders([object()], 0)
        # Open evaluations of standalone BoolOps by (context, group): stacks,
        # because an operand can re-enter its own group through a recursive
        # call before the outer evaluation has finished.
        self.probe_open_boolops: dict = {}
        self.probing = None
        # Created at install, when the background's slot can be.
        self.hits_var = None

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
                "frontend": "probes",
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
        # CRC-32 in C. FNV-1a in a Python loop cost two microseconds of every
        # record's five; the reader tells the two apart by transport version.
        return zlib.crc32(payload)

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
        self._write_payload(json.dumps(record, separators=(",", ":"), sort_keys=True).encode("utf-8"))

    def _json_id(self, identifier: str) -> bytes:
        # The JSON spelling of an obligation or decision id, once. Every test
        # that reaches it writes it again, and encoding a dict for each was a
        # microsecond of a five-microsecond record.
        encoded = self._id_json.get(identifier)
        if encoded is None:
            encoded = self._id_json[identifier] = json.dumps(identifier).encode("utf-8")
        return encoded

    def _write_hit(self, context: int, obligation: str) -> None:
        # Byte for byte what `_write_record` produces for the same dict: keys
        # sorted, no spaces. The reader does not know the difference.
        self._write_payload(b'{"ctx":%d,"id":%s,"t":"hit"}' % (context, self._json_id(obligation)))

    def _write_vector(self, context: int, decision_id: str, digits: str, outcome: bool) -> None:
        self._write_payload(
            b'{"ctx":%d,"id":%s,"o":%d,"t":"dec","v":"%s"}'
            % (context, self._json_id(decision_id), 1 if outcome else 0, digits.encode("ascii"))
        )

    def _write_payload(self, payload: bytes) -> None:
        if self.output is None:
            raise RuntimeError("Python evidence transport is not open")
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
            # Close stops observing before it closes the transport, so nothing
            # new is scheduled after it. A callback already running on another
            # thread when close began waits on this lock and arrives here once
            # the transport is gone. Reopening it would rebuild the very path
            # close left on disk -- same worker, same pid, the same token -- and
            # O_EXCL refuses that, which surfaced as a traceback in a suite
            # whose daemon threads outlived its tests. What a thread runs after
            # every atexit handler belongs to no test, so it is not recorded.
            if self.closed:
                return
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
        for, harvests the slot of the phase it leaves, and hands the new phase
        a slot of its own.
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
            leaving = self.hits_var.get()
            if isinstance(leaving, _Hits):
                self._harvest(leaving)
            self.hits_var.set(self._new_hits(context))
            # Usually the last reference: the slot is free again below.
            del leaving
            self._reclaim()
            self.context.set(context)
        return context

    def current_identity(self) -> dict | None:
        return self.identities.get(self.context.get())

    def assertion(self) -> bool:
        """The first assertion of a call phase.

        Every first sighting the phase recorded so far already carries its
        context; the marker says they ran before an assertion, and the report
        links them to it once the phase passes. Returns whether this call
        wrote the marker: later assertions of the phase cost one set lookup.
        """
        context = self.context.get()
        if context == 0 or context in self.asserted:
            return False
        with self.lock:
            if context in self.asserted:
                return False
            identity = self.identities.get(context)
            if identity is None or identity.get("phase") != "call":
                return False
            self.asserted.add(context)
            # What the phase observed so far goes out ahead of the marker:
            # the reader credits the assertion with the records before it.
            array = self.hits_var.get()
            if isinstance(array, _Hits) and array.context == context:
                self._harvest(array)
            self._record({"t": "assert", "ctx": context})
        return True

    def child_environment(self) -> dict:
        """Environment additions that carry the current phase into a child
        interpreter. `PYTHONPATH` and the plan variables already inherit."""
        identity = self.current_identity()
        if identity is None:
            return {}
        return {CONTEXT_ENV: json.dumps(identity, separators=(",", ":"), sort_keys=True)}

    def outcome(self, worker: str, test: str, retry: int, phase: str, outcome: str, xfail: bool, runner: str = "pytest", file: "str | None" = None) -> None:
        """`file` is where the runner says the test is defined.

        An assertion map selects tests by source file and name, and a runner
        identity alone -- a dotted module path, a pytest node id -- is not a
        path. An adapter that cannot name the file leaves it None and the
        report falls back to deriving one from the identity.
        """
        with self.lock:
            entry = {
                "t": "outcome",
                "worker": worker,
                "test": test,
                "retry": int(retry),
                "phase": phase,
                "outcome": outcome,
                "xfail": bool(xfail),
                "runner": runner,
            }
            if file:
                entry["file"] = file
            self._record(entry)

    def assertion_site(self, file: "str | None", line: "int | None") -> None:
        """Where in the test an assertion ran, so a map can tell sites apart.

        The record is a file and a line; the report resolves the column
        against the syntax inventory Supercov captured before the run, and a
        frame naming no inventoried site matches nothing rather than inventing
        a witness. Recorded once per site per test: later sightings cost one
        set lookup. Only the call phase is reported, because setup and
        teardown assertions witness no test.
        """
        if not file or not line:
            return
        context = self.context.get()
        if context == 0:
            return
        # One spelling per site, whoever reports it: a site probe names the
        # project-relative path and a frame names whatever the interpreter
        # loaded. The reader keys a site on the text it is given, and two
        # spellings of one site became two assertion phases with one id.
        file = self._relative_path(file)
        if file is None:
            return
        key = (context, file, line)
        if key in self.asserted_sites:
            return
        with self.lock:
            if key in self.asserted_sites:
                return
            identity = self.identities.get(context)
            if identity is None or identity.get("phase") != "call":
                return
            self.asserted_sites.add(key)
            self._record({"t": "asite", "ctx": context, "f": file, "l": int(line)})

    # -- observation --------------------------------------------------------

    def _hit(self, context: int, obligation: str) -> None:
        """A hit a probe cannot store: a standalone BoolOp's operands, and
        what a wide decision's vector implies. Written at once, so it is
        exactly as durable as a slot byte."""
        key = (context, obligation)
        if key in self.seen_hits:
            return
        with self.lock:
            if key in self.seen_hits:
                return
            self.seen_hits.add(key)
            # Same guard against a record after close as `_record`.
            if self.closed:
                return
            self._ensure_process_output()
            self._write_hit(context, obligation)

    def _wide_vector(self, context: int, d: int, mask: int, outcome: bool) -> None:
        """A vector of a decision too wide for a region: recorded at once."""
        decision = self._decision(d)
        digits = _digits(mask, decision.width)
        key = (context, decision.id, digits)
        if key in self.seen_vectors:
            return
        with self.lock:
            if key in self.seen_vectors:
                return
            self.seen_vectors.add(key)
            if self.closed:
                return
            self._ensure_process_output()
            self._write_vector(context, decision.id, digits, outcome)
        implied: list = []
        decision.implied(digits, outcome, implied)
        for identifier in implied:
            self._hit(context, identifier)

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
                    self.under_root = True
                    relative = real[len(self.root) + 1 :].replace(os.sep, "/")
                    if relative in self.probe_files or relative in self.probe_sites:
                        break
                    relative = None
        self.path_cache[filename] = relative
        return relative

    # -- slots ----------------------------------------------------------------

    def _decision(self, d: int) -> _Decision:
        if d not in self.loaded_decisions:
            self.loaded_decisions[d] = _Decision(*self.probe_decisions[d])
        return self.loaded_decisions[d]

    def _write_layout(self) -> None:
        """The slot layout, once per run: every process numbers the same plan
        the same way, so the first to get here writes it for all of them."""
        path = os.path.join(self.evidence_dir, LAYOUT_NAME)
        if os.path.exists(path):
            return
        partial = f"{path}.{os.getpid()}.{self.output_token}.partial"
        with open(partial, "w", encoding="utf-8") as stream:
            json.dump(self.index.layout(), stream, separators=(",", ":"))
        try:
            os.replace(partial, path)
        except OSError:
            # Another process published the same layout first.
            if not os.path.exists(path):
                raise
            try:
                os.unlink(partial)
            except OSError:
                pass

    def _create_slot(self) -> _Hits:
        # A slot is named after its process's transport, which the reader
        # reads first: the phases a slot's context stands for are declared there.
        self._ensure_process_output()
        path = f"{self.output_path[: -len('.mmap')]}.{self.next_slot}{SLOT_SUFFIX}"
        self.next_slot += 1
        descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0), 0o600)
        try:
            os.ftruncate(descriptor, self.slot_bytes)
            slot = _Hits(descriptor, self.slot_bytes, access=mmap.ACCESS_WRITE)
        except Exception:
            os.close(descriptor)
            raise
        slot[8:16] = self.slot_tag
        slot.path = path
        slot.descriptor = descriptor
        return slot

    def _sink(self, context: int) -> _Sink:
        sink = _Sink(self.slot_bytes)
        sink.context = context
        sink.states = [None] * len(self.probe_decisions)
        return sink

    def _new_hits(self, context: int):
        if self.closed:
            return self._sink(context)
        try:
            slot = self.free_slots.pop() if self.free_slots else self._create_slot()
        except Exception as error:  # noqa: BLE001 - measurement must never break the run
            self.limitation(
                "python-slot-unavailable",
                f"could not map a slot for a test phase ({error!r}); what it executed was not observed",
            )
            return self._sink(context)
        slot.context = context
        slot.states = [None] * len(self.probe_decisions)
        struct.pack_into("<Q", slot, 0, context)
        self.live_slots.append(slot)
        return slot

    @staticmethod
    def _holders(items: list, index: int) -> int:
        return sys.getrefcount(items[index])

    def _reclaim(self) -> None:
        """Free every slot nothing but this list holds any more -- no
        context, thread or suspended task -- after reading what was written
        into it since its last harvest."""
        live = self.live_slots
        index = 0
        while index < len(live):
            if self._holders(live, index) <= self.free_refs:
                slot = live.pop(index)
                self._harvest(slot)
                self.free_slots.append(slot)
            else:
                index += 1

    def _after_fork_in_child(self) -> None:
        # The inherited slots are the parent's pages: harvesting or reusing
        # them here would clear or overwrite what the parent observed. The
        # child starts its own transport and gives the forking context a slot
        # of its own. A lock some other parent thread held cannot be released
        # in a child that has no such thread.
        self.lock = threading.RLock()
        self.live_slots = []
        self.free_slots = []
        if self.closed or self.hits_var is None:
            return
        current = self.hits_var.get()
        try:
            self.hits_var.set(self._new_hits(current.context))
        except Exception:  # noqa: BLE001 - never break the child
            pass

    def _harvest(self, slot: _Hits) -> None:
        """Turn a slot's set bytes into records, clearing them as they are read.

        `find` walks the slot in C. Hits go out as one `hits` record per
        harvest and vectors as one `decs` record, each chunked under the
        transport's record bound: a record per first hit was most of a
        parser suite's remaining overhead.
        """
        if STUB == "harvest":
            return
        began = time.perf_counter() if TIMING else 0.0
        view = slot
        header = self.slot_header
        index = view.find(b"\x01", header)
        if index == -1:
            return
        context = slot.context
        ids = self.probe_ids
        id_end = header + len(ids)
        starts = self.region_starts
        regions = self.region_decisions
        hit_ids: list = []
        decs: list = []
        with self.lock:
            while index != -1:
                view[index] = 0
                if index < id_end:
                    hit_ids.append(ids[index - header])
                else:
                    start, width, d = regions[bisect.bisect_right(starts, index) - 1]
                    decision = self._decision(d)
                    offset = index - start
                    if width == 1:
                        outcome = offset == 1
                        digits = "2" if outcome else "1"
                    else:
                        outcome = bool(offset & 1)
                        digits = _digits(offset >> 1, width)
                    key = (context, decision.id, digits)
                    if key not in self.seen_vectors:
                        self.seen_vectors.add(key)
                        decs.append((decision.id, digits, 1 if outcome else 0))
                        decision.implied(digits, outcome, hit_ids)
                index = view.find(b"\x01", index + 1)
            if not self.closed:
                self._ensure_process_output()
                prefix = b'{"ctx":%d,"ids":[' % context
                for start in range(0, len(hit_ids), HARVEST_CHUNK):
                    chunk = hit_ids[start : start + HARVEST_CHUNK]
                    seen: set = set()
                    unique = [identifier for identifier in chunk if not (identifier in seen or seen.add(identifier))]
                    self._write_payload(prefix + b",".join(self._json_id(identifier) for identifier in unique) + b'],"t":"hits"}')
                for start in range(0, len(decs), HARVEST_CHUNK):
                    body = b",".join(
                        b'[%s,"%s",%d]' % (self._json_id(identifier), digits.encode("ascii"), outcome)
                        for identifier, digits, outcome in decs[start : start + HARVEST_CHUNK]
                    )
                    self._write_payload(b'{"ctx":%d,"t":"decs","v":[' % context + body + b"]}")
        if TIMING:
            _timing["harvest"][0] += 1
            _timing["harvest"][1] += time.perf_counter() - began

    # -- what the probes call -------------------------------------------------

    def _value_probe(self, k: int, value):
        """A lambda's entry: record and hand the value back unchanged."""
        self.hits_var.get()[k] = 1
        return value

    def _single_probe(self, start: int, value) -> bool:
        """A one-condition decision: its truth is its vector, one byte each."""
        truth = bool(value)
        self.hits_var.get()[start + truth] = 1
        return truth

    def _condition_probe(self, d: int, index: int, value, inverted: bool) -> bool:
        """One condition of a multi-condition decision: record its truth.

        Returns the operand's truth rather than the value: inside a decision
        only the truth is used, so the interpreter's own test runs on this
        bool and an object's `__bool__` is consulted exactly once, by us. The
        recorded value is the condition as written, `not` included; the
        source's own `not` then applies to what is returned, once.
        """
        truth = bool(value)
        states = self.hits_var.get().states
        state = states[d]
        if state is None:
            state = states[d] = _DecisionState()
        # Conditions arrive in index order within one evaluation; an index at
        # or before the last one seen is a nested evaluation of the decision.
        if index <= state.last:
            state.saved.append((state.mask, state.last))
            state.mask = 0
        state.last = index
        state.mask += (2 if truth != inverted else 1) * self.pow3[index]
        return truth

    def _decision_probe(self, d: int, value) -> bool:
        """A multi-condition decision's test evaluated: its vector is a byte."""
        outcome = bool(value)
        hits = self.hits_var.get()
        state = hits.states[d]
        if state is None:
            return outcome
        mask = state.mask
        if state.saved:
            state.mask, state.last = state.saved.pop()
        else:
            state.mask = 0
            state.last = -1
        start = self.regions[d]
        if start >= 0:
            hits[start + 2 * mask + outcome] = 1
        else:
            self._wide_vector(hits.context, d, mask, outcome)
        return outcome

    def _operand_probe(self, g: int, index: int, value):
        """A standalone BoolOp's right operand was evaluated."""
        key = (self.context.get(), g)
        stack = self.probe_open_boolops.get(key)
        if stack is None:
            stack = self.probe_open_boolops[key] = []
        if not stack or index <= stack[-1][-1]:
            if len(stack) >= MAX_OPEN_EVALUATIONS:
                del stack[0]
            stack.append([0])
        stack[-1].append(index)
        return value

    def _boolop_probe(self, g: int, value):
        """A standalone BoolOp finished: operands not evaluated short-circuited."""
        context = self.context.get()
        stack = self.probe_open_boolops.get((context, g))
        evaluated = set(stack.pop()) if stack else {0}
        for logical in self.probe_boolops[g]:
            operand = logical["operand"]
            self._hit(context, logical["evaluated"] if operand in evaluated else logical["shortCircuit"])
        return value

    def _iter_probe(self, entered: int, zero: int, iterable):
        """A comprehension's loop: `entered` on the first item, `zero` on an
        empty exhaustion, per evaluation. The rest of the iteration runs in C.

        The array is fetched where it is stored, never held in a local: an
        iterable that raises leaves this frame in a traceback, and a frame
        holding a slot pins it until the cycle collector runs.
        """
        iterator = iter(iterable)
        try:
            first = next(iterator)
        except StopIteration:
            self.hits_var.get()[zero] = 1
            return iter(())
        self.hits_var.get()[entered] = 1
        return itertools.chain((first,), iterator)

    async def _aiter_probe(self, entered: int, zero: int, iterable):
        iterator = iterable.__aiter__()
        try:
            first = await iterator.__anext__()
        except StopAsyncIteration:
            self.hits_var.get()[zero] = 1
            return
        self.hits_var.get()[entered] = 1
        yield first
        async for item in iterator:
            yield item

    def _site_probe(self, file: str, line: int) -> None:
        self.assertion_site(file, line)
        self.assertion()

    # -- installation ---------------------------------------------------------

    def _detect_unprobed(self, code, offset):
        """PY_START as a detector only: a planned file's code object the probe
        loader did not produce reached the interpreter another way. Every
        code object fires once and is then disabled."""
        probing = self.probing
        if probing is not None and id(code) not in probing.code_objects:
            relative = self._relative_path(code.co_filename)
            if relative is not None and relative in self.probe_files:
                self.limitation(
                    "python-probes-unobserved-module",
                    "measured code reached the interpreter by a path the import hook did not see, so its execution was not observed",
                    relative,
                )
        return _monitoring.DISABLE

    def _install_probes(self) -> None:
        import supercov_probes

        self.probing = supercov_probes.install(
            self.probe_files,
            self._relative_path,
            os.path.join(self.root, ".supercov", "cache", "python"),
            {
                "hits_get": self.hits_var.get,
                "value_probe": self._value_probe,
                "condition_probe": self._condition_probe,
                "decision_probe": self._decision_probe,
                "single_probe": self._single_probe,
                "operand_probe": self._operand_probe,
                "boolop_probe": self._boolop_probe,
                "iter_probe": self._iter_probe,
                "aiter_probe": self._aiter_probe,
                "site_probe": self._site_probe,
                "limitation": self.limitation,
            },
            self.probe_sites,
        )
        if _monitoring is not None and STUB != "detector":
            # The detector (3.12+): one PY_START per code object, then off.
            for candidate in (3, 4, 1):
                if _monitoring.get_tool(candidate) is None:
                    _monitoring.use_tool_id(candidate, "supercov")
                    self.tool_id = candidate
                    break
            if self.tool_id is not None:
                _monitoring.register_callback(self.tool_id, _monitoring.events.PY_START, self._detect_unprobed)
                self.registered_events = (_monitoring.events.PY_START,)
                _monitoring.set_events(self.tool_id, _monitoring.events.PY_START)

    def install(self) -> None:
        with self.lock:
            if self.output is not None:
                return
            self._open_output()
            self._write_layout()
            # The background's slot is the default, so a thread started past
            # the propagation patch still has somewhere to write.
            self.hits_var = contextvars.ContextVar("supercov_hits", default=self._new_hits(0))
        self._install_probes()
        # A measured module imported before this point -- by a `.pth` file,
        # say -- ran without probes and will not be imported again. Named on
        # every interpreter, where the detector covers 3.12+ only for code
        # compiled past the import system later.
        for module in list(sys.modules.values()):
            filename = getattr(module, "__file__", None)
            relative = self._relative_path(filename) if isinstance(filename, str) else None
            if relative is not None and relative in self.probe_files:
                self.limitation(
                    "python-probes-unobserved-module",
                    "measured code was imported before Supercov installed, so its execution was not observed",
                    relative,
                )
        # CPython compiles an entry script in C, bypassing both the import
        # loader and builtins.compile. Detect this on 3.9–3.11 too, where
        # sys.monitoring cannot report the missed code object later.
        entry = sys.argv[0] if sys.argv else ""
        if entry and not entry.startswith("-"):
            relative = self._relative_path(entry)
            if relative is not None and relative in self.probe_files:
                self.limitation(
                    "python-probes-unobserved-module",
                    "CPython compiled the entry script without import probes; its execution was not observed",
                    relative,
                )
        if hasattr(os, "register_at_fork"):
            os.register_at_fork(after_in_child=self._after_fork_in_child)
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

    def _stop_observing(self) -> None:
        # The detector's global event off and its callback withdrawn: nothing
        # is worth reporting about code first run during shutdown.
        if self.tool_id is None:
            return
        try:
            _monitoring.set_events(self.tool_id, 0)
            for event in self.registered_events:
                _monitoring.register_callback(self.tool_id, event, None)
        except Exception:  # noqa: BLE001 - shutdown must never raise into the interpreter
            pass

    def close(self) -> None:
        with self.lock:
            if self.closed:
                return
            self._stop_observing()
            for slot in self.live_slots:
                self._harvest(slot)
            # The exit marker is the transport's last record, so it is written
            # while the transport is open -- before `closed` turns every later
            # record away, this one included.
            self._record({"t": "exit", "at": _now_ms()})
            self.closed = True
            self._close_output(flush=True)
            slots = self.live_slots + self.free_slots
            self.live_slots = []
            self.free_slots = []
            unmatched = self.worker == "main" and bool(self.path_cache) and not self.under_root
        for slot in slots:
            # Everything a slot held is in the transport now. A daemon thread
            # can still write into it after this point, so it stays mapped,
            # and a zeroed layout tag tells the reader not to count what it
            # finds. The file stays too: a process that outlives the test
            # command -- multiprocessing's resource tracker -- closes while the
            # reader is listing the evidence, and a slot removed between the
            # listing and the read failed the whole run.
            try:
                slot[8:16] = b"\0" * 8
                os.close(slot.descriptor)
            except (OSError, ValueError):
                pass
        if unmatched and TIMING:
            # Every file the interpreter imported lay outside the measured
            # tree. That is what a run reports as zero coverage without a word
            # of explanation -- on Windows the root once carried a `\\?\`
            # prefix its files did not -- so name the root and one file that
            # missed it. This is diagnostic output only: a healthy child may
            # deliberately run nothing but stdlib code, and its stderr is
            # part of the program's observable behavior.
            sample = next(
                (name for name in self.path_cache if not name.startswith("<")),
                next(iter(self.path_cache)),
            )
            sys.stderr.write(
                f"[supercov] none of the {len(self.path_cache)} imported files lay under "
                f"the measured root {self.root}; for example {sample}\n"
            )
        if TIMING:
            sys.stderr.write(
                "[supercov:timing] "
                + " ".join(f"{name}={count} calls/{seconds * 1000:.0f}ms" for name, (count, seconds) in _timing.items())
                + f" slots={self.next_slot}\n"
            )


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
                    try:
                        return context.run(original_run)
                    finally:
                        # The thread holds this function and this function
                        # the thread: a cycle only the collector would free,
                        # keeping the context -- and the test phase's slot --
                        # alive long after the thread ended.
                        thread.__dict__.pop("run", None)
                        thread.__dict__.pop("_supercov_original_run", None)

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
                # env is Popen's eleventh positional parameter on every
                # supported CPython. Preserve that calling convention too.
                supplied = args[10] if len(args) > 10 else kwargs.get("env")
                environment = dict(os.environ if supplied is None else supplied)
                # An explicitly isolated environment cannot load this run's
                # observer. Do not add a stray context variable to it: callers
                # use env= to define exactly what their child receives.
                text_plan = environment.get(PLAN_ENV)
                bytes_plan = environment.get(PLAN_ENV.encode())
                if text_plan or bytes_plan:
                    for key, value in additions.items():
                        environment.pop(key, None)
                        environment.pop(key.encode(), None)
                        if bytes_plan and not text_plan:
                            environment[key.encode()] = value.encode()
                        else:
                            environment[key] = value
                    if len(args) > 10:
                        args = (*args[:10], environment, *args[11:])
                    else:
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
    if sys.version_info < (3, 9):
        _INSTALL_ERROR = f"Supercov requires CPython 3.9 or newer, found {sys.version.split()[0]}"
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
