# Python Tier-A frontend spike

Status: private correctness spike. This does not enable Python in the public
CLI and does not change evidence archive v2.

## Decision

Supercov will import Python line and arc facts from coverage.py through its
documented Python API. It will not read the `.coverage` SQLite schema and it
will not delegate verdicts, persistence, queries, confidence, or MC/DC to
coverage.py.

The unavoidable generated Python shim has two narrow jobs:

1. a pytest plugin assigns a stable run/worker/test/retry/phase context before
   setup, call, and teardown execute, and records pytest's phase outcomes;
2. an exporter reads coverage.py through `Coverage`, `analysis2`,
   `branch_stats`, and `CoverageData`, then emits deterministic observations
   for Rust to validate and normalize.

Rust remains responsible for project isolation, process supervision, source
path validation, the complete obligation manifest, per-attempt merging,
coverage analysis, limitations, evidence archives, queries, and agent output.

## Accuracy boundary

coverage.py is an independent oracle for executable Python statements and
branch arcs. Its measured contexts can attribute lines and arcs to exact
pytest setup/call/teardown phases and, with one data file per worker, to exact
workers and tests.

It does **not** expose condition vectors or masking MC/DC witnesses. A Python
native-import run must therefore carry a blocking `python-mcdc-unavailable`
structural limitation. Zero imported decisions must never be presented as
proof of 100% MC/DC.

pytest assertion rewriting is also not a callback around every successful
assertion. A passed call phase proves that the phase completed, but cannot
identify which obligations caused or were checked by a particular assertion.
The frontend therefore declares assertion attribution unavailable until an
owned assertion probe is independently proven. Action attribution is also
unavailable.

Background or unparseable contexts remain explicit background/unattributed
evidence and are excluded from passed-only per-test confidence. They are not
dropped.

## Proven pytest outcomes

The checked-in `pytest-outcomes.json` golden is produced by a real pytest run
containing an ordinary pass, ordinary failure, skip, expected failure, setup
failure, and teardown failure. Collection begins when the generated plugin is
imported, before fixture/conftest imports, rather than waiting until
`pytest_configure`. The xdist controller stops and discards that early
collector once its role is known.

The importer reproduces coverage.py's 11/12 executable lines and 7/8 branch
arcs exactly. Passed-only coverage contains only the terminal ordinary pass;
xfail, failures, skips, and background imports cannot verify it. Setup-only
confidence is derived from the actual setup phase rather than a guessed test
role.

Retries are proven against pytest-rerunfailures 16.6 in serial pytest and
under two-worker xdist. Its observed lifecycle assigns `item.execution_count`
before the ordinary phase hooks and copies the zero-based attempt onto
`report.rerun`. The producer records that identity directly. An intermediate
`rerun` phase becomes a failed attempt; only the terminal pass contributes to
passed-only coverage; the logical test is flaky. Crashed workers, path/package
matrices, and low-level execution surfaces remain explicit open gates.

## Proven causal concurrency

A checked-in 14-test matrix proves exact source attribution for ordinary and
overlapping asyncio tasks, `threading.Thread`, reused `ThreadPoolExecutor`
workers, `subprocess.Popen`, and multiprocessing `spawn`. Work deliberately
released by the following test remains owned by the test that created or
submitted it. The collector uses a coverage.py dynamic-context plugin only at
measured-source frames, avoiding the long-lived pytest-frame scope that would
otherwise pin setup context across an entire run. `ContextVar` carries causal
identity through asyncio and explicit thread/task submission adapters; child
Python processes receive the exact context through a per-launch environment
and coverage.py's documented process-startup configuration.

The matrix also found that exporting `COVERAGE_PROCESS_START` at plugin import
causes xdist workers themselves to auto-start with the controller's `main`
identity. Subprocess activation now happens only after `pytest_configure` has
provided the authoritative worker ID.

This is not yet universal Python concurrency support. Raw `_thread` and
native-extension-created threads, plus low-level `os.system`, spawn, exec,
fork, and forkserver launch paths, are carried as blocking structural
limitations. They must receive independent adapters and crash matrices before
those limitations can be removed.

## Public-API basis

- [`Coverage.analysis2`](https://coverage.readthedocs.io/en/7.13.5/api_coverage.html#coverage.Coverage.analysis2)
  supplies executable and excluded statement lines.
- [`Coverage.branch_stats`](https://coverage.readthedocs.io/en/7.13.5/api_coverage.html#coverage.Coverage.branch_stats)
  supplies the number of possible and taken exits per branch line.
- [`CoverageData`](https://coverage.readthedocs.io/en/7.13.5/api_coveragedata.html)
  supplies measured files, contexts, context-filtered lines, and arcs. Its
  documentation explicitly says the database schema can change, so direct
  SQL is forbidden.
- [coverage.py measurement contexts](https://coverage.readthedocs.io/en/7.12.0/contexts.html)
  establish the supported per-test attribution mechanism.
- [pytest hooks](https://docs.pytest.org/en/stable/reference/reference.html#hooks)
  establish setup/call/teardown and outcome lifecycle boundaries.

## Gates before public enablement

- Freeze and strictly validate an importer schema, including producer version,
  source roots, branch mode, files, contexts, outcomes, and limitations.
- Prove totals and every executed/missing line and arc against coverage.py on a
  checked-in fixture.
- Test pass, fail, skip, setup failure, teardown failure, retry, xdist workers,
  subprocesses, multiprocessing, threads, async tests, namespace packages,
  generated code, and path aliases.
- Add an explicit archive schema migration that persists the frontend
  declaration and a language-specific coverage-model declaration. Evidence v2
  remains frozen until that migration is specified and dual-read tested.
- Never label the Python run measurement-complete while MC/DC is unavailable.
