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
TEST262_DIR=/path/to/test262 npm run test:test262
```

The differential suite includes getters, proxies, optional calls and `this`,
computed logical assignments, defaults, `try`/`catch`/`finally`, iterator
closing, switch fallthrough, labeled loops, async functions, and generators.
The compatibility workflow additionally runs Node 22/24/25, Playwright
1.55/current, Vite 5/current, Vitest 2/current, Chromium, Firefox, WebKit, and
modern JavaScript/JSX/TypeScript/TSX syntax fixtures. Test262's module, async,
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

Each run is stored locally as a compressed, immutable report under
`.supercov/runs/<run-id>/`. Its `report.json.gz` and `run.json` keep the
machine report and run metadata together. HTML is not generated during a test
run; agents should use bounded CLI queries instead of loading the complete
report into context.

```sh
# Orient using only a few lines.
npx supercov runs --limit 5
npx supercov runs latest coverage
npx supercov runs latest coverage --filter passed
npx supercov runs latest coverage --filter failed
npx supercov runs latest coverage kinds
npx supercov runs latest coverage runners
npx supercov runs latest coverage --kind e2e
npx supercov runs latest coverage files
npx supercov runs latest coverage gaps --limit 10
npx supercov runs latest coverage gaps --kind e2e --limit 10

# Drill into one target selected from the gap list.
npx supercov runs latest coverage file app/routes/example.ts
npx supercov runs latest coverage decision app/routes/example.ts:42
npx supercov runs latest coverage covers app/routes/example.ts:57

# Understand redundancy/contribution and validate a newly written test. Replace
# "latest" with the immutable run ID when an agent continues work later.
npx supercov runs latest coverage test "test title fragment"
npx supercov diff <older-run> <newer-run>
```

Coverage queries use `--filter all` by default, matching conventional coverage
tools: every executed attempt contributes, including attempts that later fail.
Use `--filter passed` for verified coverage from successful attempts of
ultimately passing tests, or `--filter failed` to inspect only execution from
failed attempts (including failed retries of flaky tests). Reports record
attempt status and classify each test as passed, failed, flaky, skipped, timed
out, interrupted, or unknown. The passed and failed subsets are stored inside
the same compressed report rather than duplicated into presentation files.

The run ID is positional because all coverage queries operate on one immutable
run. `latest` is a convenience selector for interactive use. Every query
accepts `--json` and—where the result can be long—`--limit` and `--offset`.
Every collection is paginated at 20 items by default and prints its range plus
a copyable next-page command; generated commands omit the default limit.
Text output is concise for an interactive agent; JSON is the stable machine
interface that can later back hosted coverage tools without changing the
stored schema.

For a conventional Vite project, or a Node project with no build step, the
CLI:

1. refreshes a stable isolated source namespace under
   `.supercov/cache/instrumented-workspace/<project>/`, links the existing
   dependency tree, and creates all Vite, Vitest, Playwright, and build output
   only there; file data uses copy-on-write reflinks where the filesystem
   supports them, and falls back to copying where it does not; the stable path
   lets VM/container snapshot systems reuse a coverage build without touching
   the application's ordinary build;
2. inventories every `app/**/*.ts(x)` and `src/**/*.ts(x)` file for the
   denominator, then instruments modules loaded by Vite without changing the
   project's config; when no build script exists, it instruments only the
   disposable source copy and supplies a module-format-neutral runtime through
   the inherited Node preload;
3. discovers the Playwright-compatible fixture provider, test export, and
   additional named exports from the suite's existing imports, then redirects
   that provider at module-load time; Vitest setup is injected through the
   child runner's generated config;
4. runs the exact command following `--`, propagating coverage through every
   Node child process it launches;
5. attributes every source hit and decision vector to its individual test,
   automatically wraps Playwright actions and assertions, and records the
   action/assertion phase responsible for each correlated hit; then merges
   server and browser evidence into one compressed JSON report; and
6. atomically publishes the evidence/report into `.supercov/` and retains only
   the disposable isolated build namespace as a provider snapshot cache. The
   ordinary application build is never read as an input, overwritten, or
   rebuilt afterward.

Only `.supercov/` is modified in the user's checkout. A per-project lock
rejects overlapping runs before either can build. Run state is durably written
through preparing/building/testing/reporting/terminal phases; SIGINT, SIGTERM,
and SIGHUP are forwarded to the entire child process group. If the process is
killed without a cleanup opportunity, the next invocation marks the dead PID's
run abandoned and refreshes the isolated namespace before using it. Cache
refresh is transactional: a new sibling generation is prepared completely,
the stable name is switched only at publication, and the prior complete
generation is retained until that switch succeeds. The next invocation
discards orphan staging trees or restores the prior generation if a host crash
landed between the two same-filesystem renames. Report, evidence, and state
writes use sibling-temp files, fsync, and atomic rename; lock acquisition uses
exclusive creation and fsync.

Retention is deterministic because UTC run IDs sort chronologically:

```sh
npx supercov clean --keep 20
npx supercov clean --keep 20 --dry-run
```

The cleanup command acquires the same project lock as a coverage run, so it
cannot race cache publication and refuses to run while coverage is active. It
never touches files outside `.supercov/`.

The complete ownership, crash-recovery, symlink, and future copy-free design is
documented in [Workspace isolation](docs/workspace-isolation.md).

Every run prints and stores monotonic phase timings for initialization,
workspace preparation, adapter setup, the instrumented build, the unchanged
test command, and report preparation. They are available in
`.supercov/runs/<run-id>/run.json` and in the JSON form of `supercov runs`.
These phase timings do not pretend to be end-to-end overhead: that percentage
requires an explicit comparison with the same command run without Supercov,
which Supercov never executes automatically because an arbitrary test command
may have side effects or external cost. See
[Performance and storage](docs/performance.md) for the comparison methodology,
strategy trade-offs, and a measured real-suite reference.

The automatic adapters currently support standard Playwright suites (ESM and
CommonJS specs in arbitrary project directories), project-owned Playwright
fixture packages, Vitest, no-build Node commands, and Node coordinators that
launch tests inside a mounted VM/container workspace. A single command such as
`supercov -- npm test` can collect Vitest and Playwright evidence into the same
run. No-build Node execution is retained as background/unattributed evidence
until a recognized test-runner adapter supplies exact test boundaries. Jest,
exact `node:test` attribution, non-Vite application builds, browser component
runners, and distributed multi-host merging still require adapters; they are
not silently reported as per-test coverage.

Remote execution discovery is structural rather than provider-specific. The
preload observes CommonJS exports for a static `build(options)` capability,
activates only when those options contain a host-to-guest mount that includes
the isolated project, scopes an existing cache/snapshot identity to the run's
source fingerprint, and follows the opaque returned object graph. A method
whose options contain `argv`, `cmd`, or `command` receives guest-translated
Supercov paths and a guest-valid Node preload. The execution log records this
process/capability graph but hashes long or multiline arguments so embedded
shell bodies and credentials are never persisted.

This first zero-edit mechanism has explicit boundaries. It follows Node child
processes, not arbitrary non-Node supervisors or a remote control plane that
never exposes launches to the local process. The remote SDK must currently be
visible through CommonJS loading, its build options must expose the workspace
mount, and its execution call must accept an environment. Pure-ESM executor
SDKs, positional-only remote exec APIs, and providers that hide all launch
state behind an RPC need additional interception layers. Supercov reports
missing evidence rather than claiming those paths are covered.

The public regression suite includes a provider-neutral opaque executor. Its
CommonJS SDK exposes only a static build capability, a host-to-guest mount,
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

The JSON report contains both per-test and per-test-file coverage data. MC/DC
stores vector-level provenance rather than only a decision-level test list, so
a later suite minimizer can recompute valid independence pairs for any proposed
subset. This matters because the two vectors in a witness pair may come from
different tests.

The same report also contains an action/assertion trace without requiring spec
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
scope. It is visible in the all-attempt report and excluded from passed-only
per-test coverage.

The Playwright adapter covers the page and request fixtures, API request
contexts, user-created browser contexts/pages, popups and all their frames,
dedicated/service workers, WebSocket handshake headers, and test-spawned child
processes. A two-worker generic fixture exercises these surfaces without
changing its test imports or Playwright config.

Every run stores SHA-256 fingerprints for source, tests, dependency lockfiles,
test/build configuration, and the instrumenter, plus its report schema and Git
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
code is discovered the report records its exact location as a completeness
blocker instead of allowing a misleading 100% verdict.
