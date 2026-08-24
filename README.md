# supercov

Zero-edit, runner-aware coverage-completeness command for JavaScript test
suites.

```sh
npx supercov -- npm test
```

For local development before publication, a Supercov contributor can expose
the checkout globally. Consumer repositories still remain untouched:

```sh
# In the supercov repository.
npm install
npm link
```

## Verifying the instrumenter

The coverage engine has seven independent release gates:

- semantic differential fixtures execute original and instrumented programs
  in isolated scopes and compare return values, thrown errors, and observable
  side-effect order;
- a deterministic generated corpus exercises 160 nested combinations of
  short-circuiting, ternaries, coercion, and thrown expressions on every run;
- seeded `fast-check` properties exercise another 500 generated nested
  expressions and 300 generated control-flow executions, with shrinking and a
  reproducible seed on failure;
- coverage oracles assert exact decision vectors, MC/DC witnesses, and branch
  alternatives independently of program behavior;
- the same three-condition masking-MC/DC golden cases must report 100% for a
  complete witness set and 33.33% for an incomplete one under both Supercov
  and Clang/LLVM source-based MC/DC;
- release CI shards the pinned TC39 Test262 corpus across 16 workers, runs the
  official Test262 harness on original and instrumented sources, and rejects
  any scenario that passes originally but fails after transformation; and
- checked performance budgets cover transform latency, transactional workspace
  preparation, output expansion, and runtime probe overhead.

```sh
npm test
npm run test:clang-mcdc
npm run benchmark:check

# One-time contributor setup. The corpus stays inside this checkout and is
# ignored by Git because it is a large, reproducible test dependency.
git clone --depth 1 https://github.com/tc39/test262.git .cache/test262

# Uses .cache/test262 by default.
npm run test:test262
```

Test262 is TC39's conformance suite for ECMA-262, the JavaScript language
specification. Supercov executes eligible tests both before and after
instrumentation and rejects any semantic difference. The local clone is not
part of the npm package or a coverage run and can be deleted and cloned again
at any time. Contributors who already keep Test262 elsewhere can override the
default with `TEST262_DIR=/path/to/test262` or `--test262 <path>`.

The differential suite includes getters, proxies, optional calls and `this`,
computed logical assignments, defaults, `try`/`catch`/`finally`, iterator
closing, switch fallthrough, labeled loops, async functions, and generators.
The compatibility workflow additionally runs Node 22/24/25, Playwright
1.55/current, Vite 5/current, Vitest 2/current, Chromium, Firefox, WebKit, and
modern JavaScript/JSX/TypeScript/TSX syntax fixtures. Filesystem publication,
symlink, copy fallback, ENOSPC, failed rename, and forced-termination recovery
also run on Ubuntu, macOS, and Windows. Test262's module, async,
raw, parse/resolution-negative, Annex B sloppy-script extension, and explicit
`Function.prototype.toString`/function-source-coercion tests are intentionally
excluded from the source-rewrite comparison, with reason counts printed for
every shard. Annex B does not apply to the Vite application modules Supercov
instruments; exact source reflection necessarily observes a source transform.
When application code directly coerces or observes a function's source,
Supercov leaves that function body uninstrumented and records a visible
`semantic-safety` completeness blocker. The release corpus covers every other eligible synchronous
script and runtime-negative scenario, while dedicated differential fixtures
cover async functions and generators. Every semantic-equivalence failure
blocks the trusted-publishing workflow.

## Agent query workflow

Each run is stored locally under `.supercov/runs/<run-id>/`. Its immutable
`evidence.raw.gz` archive and `run.json` metadata are the source of truth. The archive
contains the exact coverage denominator manifest plus raw per-worker and
background evidence. The first query lazily reconstructs the complete coverage
model and atomically writes a disposable, integrity-checked `query-index.v1.json.gz`;
later queries reuse it. A changed archive, incompatible Supercov/schema version,
or corrupt index causes automatic reconstruction, so the index can be deleted at
any time without losing coverage data.
Loose evidence is removed only after the whole run directory is atomically
visible. HTML is not generated during a test run; agents should use bounded
CLI queries instead of loading the complete derived model into context.

```sh
# Orient using only a few lines.
npx supercov runs --limit 5
npx supercov runs latest coverage
npx supercov runs latest coverage --filter passed
npx supercov runs latest coverage --filter failed
npx supercov runs latest coverage kinds
npx supercov runs latest coverage runners
npx supercov runs latest coverage scope
npx supercov runs latest coverage --kind e2e
npx supercov runs latest coverage files
npx supercov runs latest coverage gaps
npx supercov runs latest coverage gaps --metric mcdc
npx supercov runs latest coverage gaps --kind e2e

# Drill into one target selected from the gap list.
npx supercov runs latest coverage file app/routes/example.ts
npx supercov runs latest coverage file app/routes/example.ts --metric mcdc
npx supercov runs latest coverage decision app/routes/example.ts:42
npx supercov runs latest coverage covers app/routes/example.ts:57

# Understand redundancy/contribution and validate a newly written test. Replace
# "latest" with the immutable run ID when an agent continues work later.
npx supercov runs latest coverage test "test title fragment"
npx supercov runs latest coverage minimize --filter passed
npx supercov runs latest coverage minimize --filter passed --metric mcdc --target 80
npx supercov diff <older-run> <newer-run>

# Combine compatible shards without deleting their immutable source runs.
npx supercov merge <first-run-id> <second-run-id>
```

`supercov runs` is metadata-only for uncached history and never reconstructs
coverage for twenty runs merely to list them. Runs whose disposable query index
already exists include their metrics; other rows say `coverage not indexed`.
Selecting a run with `runs <run-id> coverage` materializes its index lazily.

Coverage queries use `--filter all` by default, matching conventional coverage
tools: every executed attempt contributes, including attempts that later fail.
Use `--filter passed` for verified coverage from successful attempts of
ultimately passing tests, or `--filter failed` to inspect only execution from
failed attempts (including failed retries of flaky tests). Evidence records
attempt status and classify each test as passed, failed, flaky, skipped, timed
out, interrupted, or unknown. Passed and failed views are derived from the
same immutable archive rather than duplicated into presentation files.

The run ID is positional because all coverage queries operate on one immutable
run. `latest` is a convenience selector for interactive use. Every query
accepts `--json` and—where the result can be long—`--limit` and `--offset`.
Every collection is paginated at 20 items by default and prints its range plus
a copyable next-page command; generated commands omit the default limit.
Agents targeting one coverage dimension can pass `--metric` to `coverage
files`, `coverage gaps`, or `coverage file`; this ranks and narrows the existing
resource instead of requiring a separate MC/DC-specific command.
Measurement limitations use the same drill-down commands as ordinary gaps.
The coverage summary reports whether the measured denominator is complete,
`coverage files` and `coverage gaps` include per-file limitation counts and
kinds, and `coverage file <path>` returns the bounded source locations, reasons,
and denominator effect. `coverage scope` attaches the same counts to included,
excluded, and ambiguous source entries. A 100% metric with a blocking limitation
is therefore never reported as structurally complete.
The summary also exposes provider-neutral transport counters. If Supercov
supervises remote launches but receives no server records, it emits a
`REMOTE_SERVER_EVIDENCE_MISSING` diagnostic instead of letting an agent assume
that browser-only evidence describes the whole application.
Malformed JSONL transport records do not make the entire run unreadable.
Supercov retains valid records, emits a `CORRUPT_EVIDENCE_RECORDS` error
diagnostic, and marks measurement completeness false until a clean run is
available.
Text output is concise for an interactive agent; JSON is the stable machine
interface that can later back hosted coverage tools without changing the
stored evidence schema. Every JSON response uses contract version 1:
successful responses contain `schemaVersion`, `ok: true`, `command`, `data`,
and, for every bounded collection, one `pagination` object with `offset`,
`limit`, `returned`, `total`, `hasMore`, and `nextOffset`. Failures exit with
status 2 and emit only a parseable `ok: false` envelope containing a stable
error `code`, message, retryability, and bounded details. JSON stdout has a
hard 64 KiB limit; an oversized request returns `RESPONSE_TOO_LARGE` so the
caller can paginate or narrow it instead of flooding an agent context.
`coverage minimize` is an exact branch-and-bound
solver: line, statement, function, and branch obligations use per-test
provenance, while MC/DC obligations retain complete independence-witness pairs
and are recomputed for every candidate subset. Its result is therefore a
proved minimum, not a greedy approximation.
It intentionally refuses a view containing background/unattributed evidence:
there is no honest way to claim an exact test subset when the runner did not
expose test boundaries.

`merge` accepts only runs with identical source, test, dependency,
configuration, instrumenter, schema, and denominator fingerprints. It rewrites
the run scope inside every evidence record, namespaces shard paths, publishes a
new immutable run atomically, and leaves all input runs untouched. This is the
distributed/multi-host primitive; incompatible shards fail clearly instead of
producing a plausible but invalid aggregate.

For a JavaScript or TypeScript project, the CLI:

1. refreshes a stable isolated source namespace under
   `.supercov/cache/instrumented-workspace/<project>/`, links the existing
   dependency tree, and creates generated runner configuration and build output
   only there; file data uses copy-on-write reflinks where the filesystem
   supports them, and falls back to copying where it does not; the stable path
   lets VM/container snapshot systems reuse a coverage build without touching
   the application's ordinary build; when the complete source/config/toolchain
   fingerprint is unchanged, the prior instrumented output and manifest are
   carried into the refreshed source snapshot and the build is skipped;
2. inventories first-party source from package entry points, workspaces,
   conventional source directories, and TypeScript roots. Every candidate is
   retained as included, excluded, or ambiguous; ambiguity blocks a complete
   verdict and is inspectable with `coverage scope`. Set
   `SUPERCOV_SOURCE_ROOTS` for an explicit authoritative scope;
3. instruments through the existing Vite graph when available, or instruments
   only the disposable source copy before the project's unchanged
   Next/Turbopack, Webpack, esbuild, SWC, or other build command. No-build ESM
   and CommonJS projects use the same disposable direct path;
4. runs the exact command following `--`, propagating coverage through every
   Node child process it launches. Generated adapters provide exact test,
   worker, retry, and outcome scopes for Playwright, Vitest, Jest, and
   `node:test` without changing test imports or configs;
5. attributes source hits and decision vectors to individual tests where an
   exact adapter is active,
   automatically wraps Playwright actions and assertions, and records the
   action/assertion phase responsible for each correlated hit; and
6. atomically publishes the exact denominator and raw evidence into one gzip
   archive under `.supercov/`, then
   removes loose evidence and terminal per-run work state, retaining only the
   immutable run and disposable isolated build namespace. The
   ordinary application build is never read as an input, overwritten, or
   rebuilt afterward.

Only `.supercov/` is modified in the user's checkout. A per-project lock
rejects overlapping runs before either can build. Run state is durably written
through preparing/building/testing/publishing phases; SIGINT, SIGTERM,
and SIGHUP are forwarded to the entire child process group. If the process is
killed without a cleanup opportunity, the next invocation marks the dead PID's
run abandoned and refreshes the isolated namespace before using it. Cache
refresh is transactional: a new sibling generation is prepared completely,
the stable name is switched only at publication, and the prior complete
generation is retained until that switch succeeds. The next invocation
discards orphan staging trees or restores the prior generation if a host crash
landed between the two same-filesystem renames. Evidence archive, metadata, and
state writes use sibling-temp files, fsync, and atomic rename; lock acquisition
uses exclusive creation and fsync. Published `run.json` is the durable terminal
record, so terminal work state is not retained.

Retention is deterministic because UTC run IDs sort chronologically:

```sh
npx supercov prune --keep 20
npx supercov prune --keep 20 --dry-run
npx supercov clean --keep 20   # also removes the shared build cache
```

Neither operation runs automatically. `prune` removes explicit history beyond
the requested retention and orphan/terminal transient data while preserving
the shared cache. `clean` also removes that cache. Both acquire the same lock
as a coverage run, refuse to race an active run, and never touch files outside
`.supercov/`.

The complete ownership, crash-recovery, symlink, and future copy-free design is
documented in [Workspace isolation](docs/workspace-isolation.md).

Every run prints and stores monotonic phase timings for initialization,
workspace preparation, adapter setup, the instrumented build, the unchanged
test command, and evidence publication. They are available in
`.supercov/runs/<run-id>/run.json` and in the JSON form of `supercov runs`.
These phase timings do not pretend to be end-to-end overhead: that percentage
requires an explicit comparison with the same command run without Supercov,
which Supercov never executes automatically because an arbitrary test command
may have side effects or external cost. See
[Performance and storage](docs/performance.md) for the comparison methodology,
strategy trade-offs, and a measured real-suite reference.

The automatic exact-attribution adapters support standard Playwright suites
(ESM and CommonJS specs in arbitrary project directories), project-owned
Playwright fixture packages, Vitest, Jest—including concurrent and
parameterized tests—and `node:test`. A single command can collect several
runners into one run. Unsupported runners such as AVA or Mocha still receive
aggregate first-party structural coverage through inherited process
instrumentation, but their hits remain background/unattributed rather than
being guessed onto tests. Browser component runners without a recognized
adapter have the same explicit boundary.

Remote execution discovery is structural rather than provider-specific. The
preload and narrowly gated ESM transform observe exports for a static
`build(options)` capability,
activate only when those options contain a host-to-guest mount that includes
the isolated project, scopes an existing cache/snapshot identity to the run's
source fingerprint, and follows the opaque returned object graph. A method
whose options contain `argv`, `cmd`, or `command` receives guest-translated
Supercov paths and a guest-valid Node preload. The execution log records this
process/capability graph but hashes long or multiline arguments so embedded
shell bodies and credentials are never persisted.

This zero-edit mechanism has explicit boundaries. It follows Node child
processes, not arbitrary non-Node supervisors or a remote control plane that
never exposes launches to the local process. CommonJS and pure-ESM executor
SDKs, object-shaped and positional execution APIs, and opaque returned object
graphs are covered when a discoverable build capability exposes the workspace
mount and an execution capability accepts an environment. Providers that hide
all launch state behind an out-of-process RPC still need an adapter. Supercov
reports missing evidence rather than claiming those paths are covered.

The public regression suite includes provider-neutral CommonJS and pure-ESM
opaque executors. Each exposes only a static build capability, a host-to-guest mount,
an existing snapshot key, an opaque image/pool/machine chain, and an
argv-shaped execution method. CI requires Supercov to discover that structure,
scope the cache identity, translate paths and the Node preload into the guest,
run nested Vitest and Playwright commands, parse every concurrent trace shard,
and produce 100% fixture coverage. A separate clean-room gate packs the npm
tarball and invokes it through `npx` in a project with no build step, asserting
that no source or configuration file changes.

Before the isolated build, Supercov also compares the invoked npm/pnpm/yarn/bun
script with explicit string-valued `process.env` mode checks in the project's
build config. A semantic match such as `test:preview` and
`process.env.TEST_PREVIEW === "true"` activates that build-only flag and is
printed before the build. It never guesses values for unrelated environment
variables.

Each test carries two independent provenance fields:

- `runner`: the process that executed it, such as `playwright` or `vitest`;
- `kind`: its semantic level, such as `e2e`, `integration`, `component`, or
  `unit`.

Kind is resolved in descending confidence from an explicit
`SUPERCOV_TEST_KIND`, Playwright project name, test path, then runner
default (Playwright is E2E; Vitest is unit). The report preserves how the label
was established, so an inferred kind is never presented as user-declared.
Vitest module-import/setup execution is retained as a separate setup scope,
not mislabeled as a test case.

Filtered queries recompute every obligation from the selected tests. MC/DC is
especially important: the command recomputes independence witness pairs rather
than filtering an already-computed percentage. Therefore a witness assembled
from one unit vector and one E2E vector counts for the combined suite but not
for either filtered subset. With `--kind e2e`, gap and file queries also
distinguish obligations covered only by other test levels from obligations
uncovered everywhere.

The query model reconstructed from the archive contains both per-test and
per-test-file coverage data. MC/DC stores vector-level provenance rather than
only a decision-level test list, so the exact minimizer recomputes valid
independence pairs for every proposed subset. This matters because the two
vectors in a witness pair may come from different tests.

The reconstructed query model also contains an action/assertion trace without requiring spec
changes. Calls such as `page.goto()`, `locator.click()`, and `locator.fill()`
open action phases; Playwright `expect()` matchers open assertion phases. The
phase travels on browser requests into automatically wrapped Remix loaders,
actions, and the server document renderer. Node async context preserves that
ID through awaited helpers. An assertion also retains the preceding action ID,
making chains such as “click -> application lines/decisions -> visible
assertion” queryable in JSON.

Server evidence is safe when Playwright uses multiple workers against one
application server. Every routed request carries a run/worker/test/retry scope;
Node async context retains that scope and its current phase across awaited
work. The server writes to a distinct attempt path, and the collecting fixture
accepts only records bearing that attempt ID. No worker deletes, reads, or
attributes another worker's live evidence file.

Detached work is never silently dropped or guessed onto the currently active
test. HTTP callbacks inherit the carrier automatically; child processes inherit
it through their environment; and exported queue helpers support BullMQ,
Bee-Queue, pg-boss, Agenda, and in-process schedulers. Evidence that arrives
without a carrier is persisted under a first-class `background/unattributed`
scope. It is visible in the all-attempt view and excluded from passed-only
per-test coverage.

The Playwright adapter covers the page and request fixtures, API request
contexts, user-created browser contexts/pages, popups and all their frames,
dedicated/service workers, WebSocket handshake headers, and test-spawned child
processes. A two-worker generic fixture exercises these surfaces without
changing its test imports or Playwright config.

Every run stores SHA-256 fingerprints for source, tests, dependency lockfiles,
test/build configuration, and the instrumenter, plus its evidence schema and Git
revision/dirty state. Queries compare the stored fingerprint with the current
workspace, visibly mark stale runs, and reject evidence carrying a different
run scope.

For Chromium documents exposed through the page target, a pre-document probe
also installs the phase before application JavaScript starts. Chromium may run
a newly created cross-origin iframe in a separate target that cannot be safely
paused and attached during navigation; its earliest browser probes use the
timing fallback until the frame is live. This affects only action-level causal
precision, not structural coverage or exact test-case provenance.

Code reached outside a recognized Playwright action, such as setup work or a
project-specific helper that performs HTTP requests directly, still has exact
test-case attribution but may not have an explicit action-phase ID. The report
labels explicit browser/server events separately from events assigned by the
isolated VM's timing fallback. Only explicit phases can raise confidence to
`asserted`; a timing-correlated event remains execution-only. Each line, point,
branch alternative, vector, and MC/DC condition therefore distinguishes
unexecuted, executed, action-linked, and assertion-linked evidence, as well as
unit-only versus E2E coverage.

The v2 denominator additionally measures optional-chain short-circuiting,
logical assignments, parameter/destructuring defaults, try versus catch,
zero versus entered `for-in`/`for-of`, and implicit switch no-match. Direct
`eval`/`Function` source cannot receive a stable pre-run denominator; when such
code is discovered the evidence records its exact location as a completeness
blocker instead of allowing a misleading 100% verdict.
