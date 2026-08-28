# Supported suites

Supercov wraps a test command and instruments the processes it launches.
Runner support differs by attribution level: exact per-test attribution or
aggregate coverage.

## Attribution by runner

| Runner | Attribution | Notes |
| --- | --- | --- |
| Playwright | Exact per test | Test, worker, retry and outcome scopes. ESM and CommonJS specs in arbitrary directories, plus project-owned fixture packages. |
| Vitest | Exact per test | Module-import and setup execution is kept as a separate setup scope. |
| Jest | Exact per test | Including concurrent and parameterized tests. |
| `node:test` | Exact per test | Through the generated adapter. |
| AVA, Mocha, other runners | Aggregate only | First-party structural coverage through inherited process instrumentation. Hits are recorded as background rather than guessed onto tests. |
| Browser component runners without an adapter | Aggregate only | Same boundary, made explicit in the report. |

Adapters are generated into the isolated workspace. Your test imports, reporter
list and runner configuration are not modified.

A single command may collect several runners into one run. Each test is then
labelled with the runner that executed it and the semantic kind it belongs to.

## Builds

| Project shape | How instrumentation is applied |
| --- | --- |
| Vite or Vitest | Through the existing Vite graph |
| Next, Turbopack, Webpack, esbuild, SWC, other build commands | Applied to the disposable source copy, then your unchanged build command runs against it |
| No build step (ESM or CommonJS) | Direct instrumentation of the disposable source copy |

The ordinary application build is never read as an input, overwritten, or
rebuilt afterwards.

When the complete source, configuration and toolchain fingerprint is unchanged
between runs, the previous instrumented output and manifest are carried into the
refreshed workspace and the build is skipped entirely.

### Build-only environment flags

Before the isolated build, Supercov compares the invoked npm, pnpm, yarn or bun
script with explicit string-valued `process.env` checks in the project's build
configuration. A semantic match — a `test:preview` script and a
`process.env.TEST_PREVIEW === "true"` check, for example — activates that
build-only flag, and the decision is printed before the build. Values are never
guessed for unrelated environment variables.

## Browsers

The compatibility workflow exercises Chromium, Firefox and WebKit, along with
Node 22, 24 and 25, Playwright 1.55 and current, Vite 5 and current, Vitest 2
and current, and modern JavaScript, JSX, TypeScript and TSX syntax fixtures.

The Playwright adapter covers the `page` and `request` fixtures, API request
contexts, user-created browser contexts and pages, popups and all of their
frames, dedicated and service workers, WebSocket handshake headers, and
test-spawned child processes.

For Chromium documents exposed through the page target, a pre-document probe
installs the action phase before application JavaScript starts. A newly created
cross-origin iframe may run in a separate target that cannot be safely paused
during navigation; its earliest probes use a timing fallback until the frame is
live. This affects action-level causal precision only — never structural
coverage or test attribution.

## Servers, background work and child processes

Server-side coverage is safe when Playwright runs multiple workers against one
application server. Every routed request carries a run, worker, test and retry
scope; Node async context retains that scope and its current phase across
awaited work; and each worker writes to a distinct attempt path that only its
own collecting fixture will accept.

Detached work is never dropped silently or guessed onto whichever test is
active:

- HTTP callbacks inherit the carrier automatically.
- Child processes inherit it through their environment.
- Exported queue helpers cover BullMQ, Bee-Queue, pg-boss, Agenda and
  in-process schedulers.
- Anything that still arrives without a carrier is persisted under the
  background scope.

## Remote and containerised execution

Discovery is structural rather than provider-specific. The preload and a
narrowly gated ESM transform look for a static `build(options)` capability,
activate only when those options contain a host-to-guest mount that includes the
isolated project, scope any existing cache or snapshot identity to the run's
source fingerprint, and follow the returned object graph. A method whose options
contain `argv`, `cmd` or `command` receives guest-translated Supercov paths and
a guest-valid Node preload.

The execution log records this process and capability graph, but hashes long or
multiline arguments so embedded shell bodies and credentials are never
persisted.

The boundary is explicit: Supercov follows Node child processes, not arbitrary
non-Node supervisors, and not a remote control plane that never exposes its
launches to the local process. CommonJS and pure-ESM executor SDKs,
object-shaped and positional execution APIs, and opaque returned object graphs
are all covered when a discoverable build capability exposes the workspace mount
and an execution capability accepts an environment. Anything that hides all
launch state behind an out-of-process RPC needs a dedicated adapter, and
Supercov reports missing evidence rather than claiming those paths are covered.

The public regression suite includes provider-neutral CommonJS and pure-ESM
opaque executors. CI requires Supercov to discover that structure, scope the
cache identity, translate paths and the Node preload into the guest, run nested
Vitest and Playwright commands, parse every concurrent trace shard, and produce
100% fixture coverage.

## Distributed runs

Each shard produces its own immutable run. `supercov merge` combines runs whose
source, test, dependency, configuration, instrumenter, schema and denominator
fingerprints match exactly, publishes a new immutable run atomically, and leaves
every input untouched. Incompatible shards fail with a clear reason instead of
being averaged together.
