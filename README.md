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

The coverage engine has three independent test layers:

- semantic differential fixtures execute original and instrumented programs
  in isolated scopes and compare return values, thrown errors, and observable
  side-effect order;
- a deterministic generated corpus exercises 160 nested combinations of
  short-circuiting, ternaries, coercion, and thrown expressions on every run;
- coverage oracles assert exact decision vectors, MC/DC witnesses, and branch
  alternatives independently of program behavior.

```sh
npm test
```

The differential suite includes getters, proxies, optional calls and `this`,
computed logical assignments, defaults, `try`/`catch`/`finally`, iterator
closing, switch fallthrough, labeled loops, async functions, and generators.
Every generated failure prints its reproducible seed and expression.

## Agent query workflow

Each run is stored locally as a compressed, immutable report under
`.supercov/runs/<run-id>/`. Its `report.json.gz`, `run.json`, and
`report.html` keep the machine report, run metadata, and human report together.
Agents should use bounded CLI queries instead of loading the complete report
into context.

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
out, interrupted, or unknown. The HTML equivalents are `report.html`,
`report-passed.html`, and `report-failed.html`.

The run ID is positional because all coverage queries operate on one immutable
run. `latest` is a convenience selector for interactive use. Every query
accepts `--json` and—where the result can be long—`--limit` and `--offset`.
Every collection is paginated at 20 items by default and prints its range plus
a copyable next-page command; generated commands omit the default limit.
Text output is concise for an interactive agent; JSON is the stable machine
interface that can later back hosted coverage tools without changing the
stored schema.

For a conventional Vite project, the CLI:

1. creates ignored Vite, Vitest, and Playwright overlays under
   `.supercov/`;
2. inventories every `app/**/*.ts(x)` and `src/**/*.ts(x)` file for the
   denominator, then
   instruments modules loaded by Vite without changing the project's config;
3. redirects existing `@playwright/test` imports at module-load time and injects
   a Vitest setup through the child runner's generated config, without changing
   specs, imports, package scripts, or checked-in configs;
4. runs the exact command following `--`;
5. attributes every source hit and decision vector to its individual test,
   automatically wraps Playwright actions and assertions, and records the
   action/assertion phase responsible for each correlated hit; then merges
   server and browser evidence into HTML and JSON reports; and
6. restores an ordinary application build even after a failed test command.

The automatic adapters currently support standard Playwright suites (ESM and
CommonJS specs in arbitrary project directories), Vitest, and the Essential
Apps isolated Playwright VM runner. A single command such as
`supercov -- npm test` can collect Vitest and Playwright evidence into
the same run. The application build must currently be Vite-based. Jest,
`node:test`, non-Vite build systems, browser component runners, and distributed
multi-host merging still require adapters; they are not silently reported as
covered.

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
assertion” queryable in JSON and visible in HTML.

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
