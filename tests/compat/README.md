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

Linux checks every recording. Windows and macOS, where starting a process
costs far more, check the releases on either side of each change in what runs
hold, plus the oldest and the newest. A further test fails when a minor release
is missing from the corpus.

## Recording a release

Recording needs Docker. Each release runs in a Linux container at a fixed path
and a fixed commit, so the corpus holds no machine's paths:

```sh
scripts/compat-record.sh javascript-node-test 4.29.0
scripts/compat-record.sh python-pytest 4.29.0
```

Record each release once it is published, for every fixture. A release that
deliberately stops reading older runs must refuse them with a message that
says so. The suite then states that refusal for those recordings; it never
deletes them.
