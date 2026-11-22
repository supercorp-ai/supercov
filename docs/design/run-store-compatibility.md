# What runs hold, across releases

A project's `.supercov/runs` outlives the Supercov that wrote it. Three
things keep a release reading the runs of the ones before it.

## The corpus

`tests/compat/runs` holds what every minor release from 1.0 on stored of two
small fixtures, and `crates/supercov-cli/tests/compat.rs` reads all of it with
the current build (`tests/compat/README.md`). It found that 4.24.0 stopped
reading every earlier run: the field naming a decision in evidence went from
`meta` to `id`, and the evidence schema version stayed 3. It found it only
after the fact, once the release before was recorded and read.

## The shape guard

`crates/supercov-engine/src/store_shapes.rs` finds such a change as it is
made. It lists every file a run keeps that a type writes or reads:

| File | Type | Version |
| --- | --- | --- |
| `run.json` | `RunMetadata` | `RUN_INTEGRITY_SCHEMA_VERSION` |
| `evidence.raw.gz`: each entry's header | `EntryHeader` | `EVIDENCE_ARCHIVE_SCHEMA_VERSION` |
| `frontend.json` | `FrontendRunDeclaration` | `LANGUAGE_FRONTEND_PROTOCOL_VERSION` |
| `coverage-model.json` | `PersistedCoverageModel` | `COVERAGE_MODEL_SCHEMA_VERSION` |
| `manifest.json` | `CoverageManifest` | evidence |
| `mcdc.json`, `*.mcdc.jsonl` | `RawTestResult` | evidence |
| `server/**/*.jsonl` | `ServerRecord` | evidence |
| `execution.*.jsonl` | `ExecutionTraceEvent` (read only) | evidence |
| `source-inputs.json` | `InputManifest` | `MANIFEST_SCHEMA` |
| `statement-exclusions.json` | JSON values | evidence |
| `support-reach.json` | `SupportReach` | evidence |
| `mutants.json`, `mutants-listed.json` | `MutantsFile` | `MUTANTS_SCHEMA_VERSION` |
| `source-manifest.cache.json` | `ManifestCache` | `MANIFEST_SCHEMA` |
| `test-executions.json` | `Executions` | none |
| `statement-spans.json` | `StatementSpan` | none |
| `assessment-input.json.gz` | `AssessmentInput` | none |
| `strong-mutation.json` | `StrongVerdicts` | `STRONG_VERDICTS_SCHEMA_VERSION` |
| `query-index.v1.bin` header | `QueryIndexIdentity` | `QUERY_INDEX_SCHEMA_VERSION` |

Each type derives `schemars::JsonSchema` in test builds only (`cfg_attr(test,
...)` in the engine, the `store-shapes` feature in the contracts crate, which
only the engine's tests turn on), so nothing ships with it. `schemars` reads
serde's own attributes, so the shape is the one serde writes and reads:
renames, `default`, `skip_serializing_if`, internally tagged enums, and the
wire types a stored type converts from (`try_from = "ServerRecordWire"`). Each
shape is taken twice, as serialized and as deserialized, since those differ
exactly where old runs are read: a field read with a default, a field only an
old release wrote.

The test renders the shapes as plain text, one field a line, and compares the
text with `tests/compat/store-shapes.txt`. A change fails it with the changed
lines under the type they are in, and says what the change owes: a version
bump with a refusal or a conversion, or reading the old shape as well; a
recording of the old shape in the corpus; then the new snapshot, written with
`SUPERCOV_UPDATE_STORE_SHAPES=1`. Doc comments are left out of the text, so
only a change in shape fails it.

Two files were written as ad hoc JSON and are now typed so the guard sees
them: `statement-spans.json` (`StatementSpan`; the bytes are unchanged) and
`strong-mutation.json` (`StrongVerdicts`, now in `strong_plan`, read whole
rather than by its `mutants` field). The query index is binary; its
identity's fields are what the snapshot holds, beside its version.

Not covered: the files `assertions assess` saves into a run
(`assertion-coverage.json`, `assertion-impact.json`,
`assertion-sources.json.gz`), which the CLI crate writes, and
`assertions.json` and `assertions.state.json`, which only releases before 3.0
wrote.

## Old runs and mutants

Every release before 4.29.0 measured without listing mutants. Given
`supercov runs <id> mutants`, such a run has its mutants listed from the
checkout's sources, kept beside it in `mutants-listed.json`. The compat suite
does this to the oldest (1.0.1) and newest recording of the JavaScript
fixture: each of its three runs lists its mutants, the whole suite's run the
same 29 with the same weak verdicts as the newest release's run, every query still answers once the list is part
of the query index's identity, and the shards merge into a run that lists the
whole suite's mutants.

The fixture's tests run under `node:test`. A strong pass over such a run used
to start the command, find no driver for the runner, and save an `error`
verdict for every mutant it meant to run, reported as a mutation score of 0%.
`mutants test` now refuses a pass with mutants to run and no test of a runner
a strong driver drives (`strong_plan::STRONG_RUNNERS`: Vitest, Playwright, and Rust's libtest, nextest and rustdoc),
naming the runners the run's tests used, before it runs anything or saves
anything. The suite accepts either outcome, a verdict for every mutant or that
refusal, so that a driver for `node:test` changes nothing here but the
outcome. No recording in the corpus is a Vitest or Playwright run, so the
strong pass over an old run of those runners is not checked here.

## Recording each release

`scripts/compat-record.sh <fixture> checkout` records the checkout itself at
its `package.json` version: `npm pack` of the checkout, installed without its
native packages, with `SUPERCOV_RUST_BINARY` pointing at a Linux binary built
in `rust:1.95-bookworm` (profile `measure`, cached in a Docker volume) or given
as `SUPERCOV_COMPAT_BINARY`. A release commit records itself this way, and the
corpus test now requires the checkout's own minor release once `CHANGELOG.md`
has its section.

After `publish.yml` publishes to npm, its `compat-record` job calls
`.github/workflows/compat-record.yml`: it waits until npm serves the version,
records each fixture in a job of its own, and opens a pull request from
`compat/record-<version>` against the default branch with the recordings. It
pushes only that branch.
