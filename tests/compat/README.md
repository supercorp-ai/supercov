# Backward compatibility corpus

A project's run store outlives the Supercov that wrote it. People upgrade and
still read yesterday's run, diff against last week's, merge shards that other
machines measured with an older release, and export any of them. This
directory holds runs that published releases recorded. The current build must
keep reading all of them.

## Layout

- `fixtures/<fixture>/`: a small project. `compat.json` holds the command that
  runs its suite, one command per shard, and the file and test that the
  queries name.
- `runs/<fixture>/<release>/runs/{full,shard-1,shard-2}/`: what that release
  stored for the whole suite and for each of its two shards, exactly as it
  wrote it, query index included.
- `runs/<fixture>/<release>/answers/`: what that release itself printed about
  those runs:
  - `summary.txt` for each run;
  - `files`, `gaps`, `file` and `test` for the whole suite;
  - the `merge` of the two shards.

## What the suite checks

`crates/supercov-cli/tests/compat.rs` installs each recording once into a fresh
checkout of its fixture and checks five things against it:

- **Numbers:** every run is listed, and its lines, branches and MC/DC read
  exactly as the recording release reported them.
- **Every query answers:** `files`, `gaps` (text, JSON and JSONL), `file`,
  `line`, `decision`, `test`, `kinds`, `runners`, `scope`, `source`,
  `minimize`, `tests affected`, `tests without-evidence`, `assertions`,
  `--filter`, every `report` format and `diff`. Each must exit 0 with no
  error.
- **Same answers across releases:** the run of each release answers `files`,
  `gaps --format jsonl`, `file`, `test`, `minimize` and the LCOV export exactly
  as the newest release's run of the same fixture does.
- **Merge:** the two shards merge into a run that covers what the whole suite
  covers, and diffs against it with nothing gained.
- **Clean:** `runs clean` removes every recorded run.

The oldest and newest recording of each JavaScript fixture are also given
`supercov runs <id> mutants`, as a run any release before mutants existed
will be: its mutants are listed from the checkout's sources and kept beside
it (`mutants-listed.json`), with the same answers whichever release measured
it; every query still answers with the list there, and its shards still
merge. A strong pass (`mutants test`) then either saves a verdict for each
mutant into the run, or is refused with its reason. These fixtures' tests
run under `node:test`, which no strong driver runs yet, so it is refused.

Linux checks every recording. Windows and macOS, where starting a process
costs far more, check the releases on either side of each change in what runs
hold, plus the oldest and the newest. A further test fails when a minor release
is missing from the corpus, the one this checkout carries included once
`CHANGELOG.md` has its section.

## The shape of what runs hold

The corpus finds a change in what runs hold once a release that made it is
recorded and read. `store-shapes.txt` finds it as it is made. It lists each
file a run keeps (`run.json`, each kind of entry in `evidence.raw.gz`,
`test-executions.json`, `statement-spans.json`, `source-manifest.cache.json`,
`assessment-input.json.gz`, `mutants.json` and `mutants-listed.json`,
`strong-mutation.json`, the query index's identity) with the version that
names its shape, and every type in it, as written and as read. The test in
`crates/supercov-engine/src/store_shapes.rs` works the shapes out from the
types themselves (`schemars`, which reads serde's renames, defaults, skipped
fields and wire types) and fails when they differ from the file. Its message
says what the change owes the runs already written: a version bump with a
refusal or a conversion, or reading the old shape as well; a recording in the
old shape here; and then the new snapshot:

```sh
SUPERCOV_UPDATE_STORE_SHAPES=1 cargo test -p supercov-engine --lib store_shapes
```

The file is plain text, so a review sees the change: 4.24.0's rename of a
decision record's `meta` to `id` would have read

```text
  type ServerRecord, written = closed {
-   meta?: RecordedDecision | null
+   id?: string | null
```

## Recording a release

Recording needs Docker. Each release runs in a Linux container at a fixed path
and a fixed commit, so the corpus holds no machine's paths:

```sh
scripts/compat-record.sh javascript-node-test 4.29.0
scripts/compat-record.sh python-pytest 4.29.0
```

A release commit records itself before it is published, so that the corpus
test passes on the release branch. `checkout` in place of a version records
this checkout at the version its `package.json` carries: it is packed as npm
would publish it, and its Linux binary is built in a Rust container (the
release profile without whole-program optimisation, cached in the
`supercov-compat-cargo` volume; the first build takes about ten minutes) or
taken from `SUPERCOV_COMPAT_BINARY`:

```sh
scripts/compat-record.sh javascript-node-test checkout
scripts/compat-record.sh python-pytest checkout
```

Once npm has the release, `publish.yml` records it again as published
(`.github/workflows/compat-record.yml`): each fixture in a job of its own,
then a pull request against the default branch from `compat/record-<version>`
with the recordings, replacing the checkout's where the release commit made
one. It never pushes to the default branch. A pull request opened with the
workflow's own token starts no checks until someone pushes to it;
`COMPAT_PR_TOKEN`, a token with contents and pull-requests write, opens it so
that they start. Run the workflow by hand for a release published without it.

A release that deliberately stops reading older runs must refuse them with a
message that says so. The suite then states that refusal for those
recordings; it never deletes them.
