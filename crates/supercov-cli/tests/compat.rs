//! Runs that earlier releases recorded, read by this build.
//!
//! A project's run store outlives the release that wrote it: a team upgrades
//! Supercov and still asks about yesterday's run, diffs against last week's,
//! merges shards measured on machines with an older release. Under
//! `tests/compat/runs/<fixture>/<release>/` each published release measured
//! the same fixture three times, the whole suite and its two shards, and kept
//! what it stored and what it answered (`scripts/compat-record.sh`;
//! `tests/compat/README.md`). This build must read every one of those runs,
//! give the numbers the recording release gave, answer every query on it, and
//! answer it as it answers the newest release's run of the same fixture.

mod common;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use common::{Project, Ran};
use serde_json::Value;

const RUNS: [&str; 3] = ["full", "shard-1", "shard-2"];

/// One release's recording of one fixture.
struct Recording {
    fixture: String,
    release: String,
    directory: PathBuf,
}

impl Recording {
    fn spec(&self) -> Value {
        let path = corpus()
            .join("fixtures")
            .join(&self.fixture)
            .join("compat.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    /// What the release itself printed for `name`.
    fn answer(&self, name: &str) -> String {
        fs::read_to_string(self.directory.join("answers").join(name))
            .unwrap_or_else(|error| panic!("{} {name}: {error}", self.label()))
    }

    fn label(&self) -> String {
        format!("{} recorded by {}", self.fixture, self.release)
    }

    /// The fixture, committed, with this recording's runs in its store.
    fn install(&self) -> Installed {
        let project = Project::empty(&format!("compat-{}", self.fixture));
        copy_tree(
            &corpus().join("fixtures").join(&self.fixture),
            &project.root,
        );
        project.git(&["init", "-q"]);
        project.commit("fixture");
        let mut ids = BTreeMap::new();
        for run in RUNS {
            let stored = self.directory.join("runs").join(run);
            let metadata: Value =
                serde_json::from_str(&fs::read_to_string(stored.join("run.json")).unwrap())
                    .unwrap();
            let id = metadata["id"].as_str().unwrap().to_owned();
            copy_tree(&stored, &project.root.join(".supercov/runs").join(&id));
            ids.insert(run, id);
        }
        Installed { project, ids }
    }
}

struct Installed {
    project: Project,
    ids: BTreeMap<&'static str, String>,
}

impl Installed {
    fn id(&self, run: &str) -> &str {
        &self.ids[run]
    }

    fn supercov(&self, args: &[&str]) -> Ran {
        self.project.supercov(args)
    }

    /// Exits 0 and says nothing went wrong: a query that printed an error
    /// and still exited 0 read nothing.
    fn answers(&self, label: &str, args: &[&str]) -> String {
        let ran = self.supercov(args);
        let stderr = ran.stderr();
        assert!(
            !stderr.contains("Failed") && !stderr.contains("error"),
            "{label}: supercov {args:?} printed an error\n{stderr}"
        );
        let stdout = ran.succeeds();
        // Ids are the recording's own; the answer is the same without them.
        self.ids
            .values()
            .fold(stdout, |text, id| text.replace(id.as_str(), "<run>"))
    }
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/compat")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn release_key(release: &str) -> (u32, u32, u32) {
    let mut parts = release.split('.').map(|part| part.parse::<u32>().unwrap());
    (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    )
}

/// Every recording, oldest release first.
static RECORDINGS: LazyLock<Vec<Recording>> = LazyLock::new(|| {
    let mut recordings = Vec::new();
    for fixture in fs::read_dir(corpus().join("runs")).unwrap() {
        let fixture = fixture.unwrap();
        for release in fs::read_dir(fixture.path()).unwrap() {
            let release = release.unwrap();
            recordings.push(Recording {
                fixture: fixture.file_name().to_string_lossy().into_owned(),
                release: release.file_name().to_string_lossy().into_owned(),
                directory: release.path(),
            });
        }
    }
    recordings
        .sort_by_key(|recording| (recording.fixture.clone(), release_key(&recording.release)));
    recordings
});

/// The releases on either side of each change in what runs hold, and the
/// oldest and newest. Reading a run is the same on every platform; Linux
/// reads every recording, and Windows and macOS, where starting a process
/// costs ten times as much, read these.
const ACROSS_PLATFORMS: [&str; 8] = [
    "1.0.1", "1.2.0", "2.0.1", "3.0.5", "4.13.0", "4.14.0", "4.23.0", "4.24.8",
];

fn checked_here(recording: &Recording) -> bool {
    cfg!(target_os = "linux")
        || ACROSS_PLATFORMS.contains(&recording.release.as_str())
        || newest_of(&recording.fixture).release == recording.release
}

fn newest_of(fixture: &str) -> &'static Recording {
    RECORDINGS
        .iter()
        .rev()
        .find(|recording| recording.fixture == fixture)
        .unwrap()
}

/// Runs `check` on every recording checked here at once, and reports every
/// one that failed rather than the first.
fn each_recording(check: impl Fn(&Recording) + Sync) {
    each_of(
        RECORDINGS
            .iter()
            .filter(|recording| checked_here(recording))
            .collect(),
        check,
    );
}

/// Runs `check` on each of `checked` at once, and reports every one that
/// failed rather than the first.
fn each_of(checked: Vec<&'static Recording>, check: impl Fn(&Recording) + Sync) {
    let failures = std::thread::scope(|scope| {
        let handles = checked
            .iter()
            .copied()
            .map(|recording| (recording, scope.spawn(|| check(recording))))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|(recording, handle)| {
                handle.join().err().map(|panic| {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|text| text.to_string()))
                        .unwrap_or_default();
                    format!("== {}\n{message}", recording.label())
                })
            })
            .collect::<Vec<_>>()
    });
    assert!(
        failures.is_empty(),
        "{} of {} recordings failed:\n{}",
        failures.len(),
        checked.len(),
        failures.join("\n\n")
    );
}

/// Deliberate changes in what a recorded run reads as, each from the release
/// that made it. A recording before such a release reads as later releases
/// read it, not as its own release did: the newest release's run of the same
/// fixture is what it must then match.
mod changes {
    use super::{Recording, release_key};

    /// 3.0.0: branch coverage counts each decision outcome once in every
    /// language. Python counted each `if` twice before (CHANGELOG 3.0.0).
    pub fn branches_counted_once(recording: &Recording) -> bool {
        recording.fixture.starts_with("python") && release_key(&recording.release) < (3, 0, 0)
    }

    /// 3.0.0: runs keep the sources they measured. An earlier run has none,
    /// and `source` says so rather than show a checkout it cannot vouch for.
    pub fn without_sources(recording: &Recording) -> bool {
        release_key(&recording.release) < (3, 0, 0)
    }
}

/// The covered and total counts of lines, branches and MC/DC in a run
/// summary, as every release since 1.0 prints it: `Lines      69.23% (9/13)`.
fn summary_counts(summary: &str) -> Option<[(u64, u64); 3]> {
    let count = |metric: &str| {
        let line = summary
            .lines()
            .find(|line| line.trim_start().starts_with(metric))?;
        let inner = line.rsplit_once('(')?.1.strip_suffix(')')?;
        let (covered, total) = inner.split_once('/')?;
        Some((covered.parse().ok()?, total.parse().ok()?))
    };
    Some([count("Lines ")?, count("Branches ")?, count("MC/DC ")?])
}

#[test]
fn the_corpus_holds_runs_of_every_minor_release() {
    let changelog = fs::read_to_string(corpus().join("../../CHANGELOG.md")).unwrap();
    let package: Value =
        serde_json::from_str(&fs::read_to_string(corpus().join("../../package.json")).unwrap())
            .unwrap();
    let current = release_key(package["version"].as_str().unwrap());
    // Each minor release the changelog has, up to the one this checkout
    // carries: a release commit, which gives its version a section, records
    // itself before it is published (`scripts/compat-record.sh <fixture>
    // checkout`).
    let mut minors = changelog
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .filter(|heading| heading.split('.').count() == 3)
        .map(release_key)
        .filter(|release| *release <= current && release.0 >= 1)
        .map(|(major, minor, _)| (major, minor))
        .collect::<Vec<_>>();
    minors.dedup();
    assert!(minors.len() > 20, "{minors:?}");
    let fixtures = fs::read_dir(corpus().join("fixtures"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut missing = Vec::new();
    for fixture in &fixtures {
        let spec: Value = serde_json::from_str(
            &fs::read_to_string(corpus().join("fixtures").join(fixture).join("compat.json"))
                .unwrap(),
        )
        .unwrap();
        // The first release that measured the fixture's language.
        let (since_major, since_minor, _) = release_key(spec["since"].as_str().unwrap());
        for (major, minor) in &minors {
            if (*major, *minor) < (since_major, since_minor) {
                continue;
            }
            let recorded = RECORDINGS.iter().any(|recording| {
                let (recorded_major, recorded_minor, _) = release_key(&recording.release);
                &recording.fixture == fixture
                    && recorded_major == *major
                    && recorded_minor == *minor
            });
            if !recorded {
                missing.push(format!("{fixture} {major}.{minor}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "record them with scripts/compat-record.sh <fixture> <version>, and this checkout's \
         own release with scripts/compat-record.sh <fixture> checkout: {missing:?}"
    );
}

/// Every run is listed, and reads with the lines, branches and MC/DC its
/// release gave it.
fn reads_with_the_numbers_its_release_gave(recording: &Recording, installed: &Installed) {
    {
        let listed = installed.answers("runs", &["runs", "--json"]);
        let listed: Value = serde_json::from_str(&listed).unwrap();
        assert_eq!(
            listed["data"]["runs"].as_array().map(Vec::len),
            Some(3),
            "every run is listed: {listed:#}"
        );
        for run in RUNS {
            let expected = summary_counts(&recording.answer(&format!("{run}/summary.txt")))
                .unwrap_or_else(|| panic!("{run}: the release's own summary has no counts"));
            let summary = installed.answers(run, &["runs", installed.id(run)]);
            let mut read =
                summary_counts(&summary).unwrap_or_else(|| panic!("{run}: no counts\n{summary}"));
            if changes::branches_counted_once(recording) {
                // Read once each; answers_as_the_newest_release_s_run
                // checks the count.
                assert!(read[1].1 < expected[1].1, "{run}: {read:?} {expected:?}");
                read[1] = expected[1];
            }
            assert_eq!(
                read, expected,
                "{run}: lines, branches and MC/DC as the release gave them\n{summary}"
            );
        }
    }
}

/// Every query and report format answers on the run.
fn every_query_answers(recording: &Recording, installed: &Installed) {
    {
        let spec = recording.spec();
        let file = spec["file"].as_str().unwrap();
        let test = spec["test"].as_str().unwrap();
        let id = installed.id("full").to_owned();
        let line = format!("{file}:3");
        let output = installed.project.root.join("exported");
        let queries: Vec<Vec<&str>> = vec![
            vec!["runs", &id, "files"],
            vec!["runs", &id, "files", "--json"],
            vec!["runs", &id, "gaps"],
            vec!["runs", &id, "gaps", "--json"],
            vec!["runs", &id, "gaps", "--format", "jsonl"],
            vec!["runs", &id, "file", file],
            vec!["runs", &id, "file", file, "--json"],
            vec!["runs", &id, "line", &line],
            vec!["runs", &id, "decision", &line],
            vec!["runs", &id, "test", test],
            vec!["runs", &id, "kinds"],
            vec!["runs", &id, "runners"],
            vec!["runs", &id, "scope"],
            vec!["runs", &id, "minimize", "--kind", "unit"],
            vec!["runs", &id, "tests", "without-evidence"],
            vec!["runs", &id, "assertions"],
            vec!["runs", &id, "--filter", "passed"],
        ];
        for query in &queries {
            installed.answers(&recording.label(), query);
        }
        let affected = installed.supercov(&["runs", &id, "tests", "affected"]);
        if changes::without_sources(recording) {
            let refused = affected.exits(2);
            assert!(
                refused.contains("This run has no source manifest"),
                "{refused}"
            );
        } else {
            let unchanged = affected.succeeds();
            assert!(
                unchanged.contains("0 test(s)") || unchanged.contains("No "),
                "{unchanged}"
            );
        }
        let source = installed.supercov(&["runs", &id, "source", file]);
        if changes::without_sources(recording) {
            let refused = source.exits(2);
            assert!(
                refused.contains("This run has no source manifest"),
                "{refused}"
            );
        } else {
            let listed = source.succeeds();
            assert!(
                listed.contains("Math.max") || listed.contains("max(total, 0)"),
                "{listed}"
            );
        }
        for format in [
            "lcov",
            "cobertura",
            "istanbul",
            "istanbul-summary",
            "clover",
            "jacoco",
            "sonar",
        ] {
            let path = output.join(format);
            let path = path.to_str().unwrap();
            installed.answers(
                format,
                &["runs", &id, "report", "--format", format, "--output", path],
            );
            assert!(fs::metadata(path).unwrap().len() > 0, "{format} is empty");
        }
        let html = output.join("html");
        installed.answers(
            "html",
            &[
                "runs",
                &id,
                "report",
                "--format",
                "html",
                "--output",
                html.to_str().unwrap(),
            ],
        );
        let shard = installed.id("shard-1").to_owned();
        installed.answers("diff", &["diff", &shard, &id]);
    }
}

/// What a run answers, the run's own ids taken out: what the fixture's
/// newest recording must answer too.
fn comparable_answers(recording: &Recording, installed: &Installed) -> BTreeMap<String, String> {
    let spec = recording.spec();
    let file = spec["file"].as_str().unwrap();
    let test = spec["test"].as_str().unwrap();
    let id = installed.id("full").to_owned();
    let mut answers = BTreeMap::new();
    for query in [
        vec!["runs", &id, "files"],
        vec!["runs", &id, "gaps", "--format", "jsonl"],
        vec!["runs", &id, "file", file],
        vec!["runs", &id, "test", test],
        vec!["runs", &id, "minimize", "--kind", "unit"],
    ] {
        let mut answer = installed.answers(&recording.label(), &query);
        if query[2] == "test" {
            // A test's phases are what its release recorded of it, and
            // releases recorded pytest's assertions and their order
            // differently; what the test covered is the same.
            answer = answer
                .lines()
                .filter(|line| {
                    !["setup:", "test:", "assertion:", "teardown:"]
                        .iter()
                        .any(|phase| line.starts_with(phase))
                })
                .map(|line| {
                    line.split(", ")
                        .filter(|part| !part.ends_with("phases"))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        answers.insert(query[2..].join(" "), answer);
    }
    let lcov = installed.project.root.join("lcov.info");
    installed.answers(
        "lcov",
        &[
            "runs",
            &id,
            "report",
            "--format",
            "lcov",
            "--output",
            lcov.to_str().unwrap(),
        ],
    );
    answers.insert("lcov".into(), fs::read_to_string(lcov).unwrap());
    answers
}

/// The run answers as the newest release's run of the same fixture does.
fn answers_as_the_newest_release_s_run(
    recording: &Recording,
    installed: &Installed,
    expected: &BTreeMap<String, String>,
) {
    let answers = comparable_answers(recording, installed);
    for (query, answer) in &answers {
        assert_eq!(
            answer, &expected[query],
            "{query}: answered otherwise than for the newest release's run"
        );
    }
}

/// The two shards merge into a run that covers what the whole suite does.
fn shards_merge_into_the_whole_suite(installed: &Installed) {
    {
        let merged = installed.answers(
            "merge",
            &["merge", installed.id("shard-1"), installed.id("shard-2")],
        );
        let merged_id = merged
            .split_whitespace()
            .find(|word| word.starts_with("run_"))
            .unwrap_or_else(|| panic!("no merged run named\n{merged}"))
            .to_owned();
        let summary = installed.answers("merged", &["runs", &merged_id]);
        let whole = installed.answers("full", &["runs", installed.id("full")]);
        assert_eq!(
            summary_counts(&summary),
            summary_counts(&whole),
            "the shards merged cover what the whole suite covers\n{summary}"
        );
        // The merge is a run like any other: it lists, and it diffs against
        // the whole suite with nothing gained or lost.
        let diff = installed.answers("diff", &["diff", installed.id("full"), &merged_id]);
        assert!(
            diff.contains("gained: 0 lines, 0 branches, 0 MC/DC conditions"),
            "{diff}"
        );
    }
}

/// `runs clean` removes the recorded runs.
fn runs_clean_removes_them(installed: &Installed) {
    {
        installed.answers("clean", &["runs", "clean"]);
        let listed: Value =
            serde_json::from_str(&installed.answers("runs", &["runs", "--json"])).unwrap();
        assert_eq!(
            listed["data"]["runs"].as_array().map(Vec::len),
            Some(0),
            "{listed:#}"
        );
    }
}

/// A run diffs against the newest release's run of the same fixture with
/// nothing changed, and is refused, with its reason, in a merge with it.
fn diffs_with_the_newest_and_merges_only_with_its_own_release(
    recording: &Recording,
    installed: &Installed,
    newest: &Recording,
) {
    {
        if newest.release == recording.release {
            return;
        }
        let stored = newest.directory.join("runs/full");
        let metadata: Value =
            serde_json::from_str(&fs::read_to_string(stored.join("run.json")).unwrap()).unwrap();
        let newest_id = metadata["id"].as_str().unwrap();
        copy_tree(
            &stored,
            &installed
                .project
                .root
                .join(".supercov/runs")
                .join(newest_id),
        );

        // The same suite of the same checkout: whichever release measured
        // it, nothing differs.
        let diff = installed.answers("diff", &["diff", installed.id("full"), newest_id]);
        assert!(diff.contains("Nothing was gained or lost."), "{diff}");

        // Releases instrument differently, so their runs do not merge; the
        // refusal names the release and what to do.
        let refused = installed
            .supercov(&["merge", installed.id("shard-1"), newest_id])
            .exits(2);
        assert!(
            refused.contains("instruments differently") && refused.contains("Measure it again"),
            "{refused}"
        );
    }
}

/// Each recording is installed once and every check reads that one copy, in
/// an order where each leaves the store as the next expects: reads, then the
/// newest release's run beside it, then merges, then `runs clean`.
#[test]
fn every_run_any_release_recorded_still_reads() {
    let newest_answers = RECORDINGS
        .iter()
        .map(|recording| recording.fixture.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|fixture| {
            let newest = newest_of(fixture);
            let installed = newest.install();
            (fixture, comparable_answers(newest, &installed))
        })
        .collect::<BTreeMap<_, _>>();
    each_recording(|recording| {
        let installed = recording.install();
        reads_with_the_numbers_its_release_gave(recording, &installed);
        every_query_answers(recording, &installed);
        answers_as_the_newest_release_s_run(
            recording,
            &installed,
            &newest_answers[recording.fixture.as_str()],
        );
        diffs_with_the_newest_and_merges_only_with_its_own_release(
            recording,
            &installed,
            newest_of(&recording.fixture),
        );
        shards_merge_into_the_whole_suite(&installed);
        runs_clean_removes_them(&installed);
    });
}

/// The oldest and the newest recording of each JavaScript fixture: the
/// releases furthest from this one in what a run holds, and the one nearest.
fn oldest_and_newest_javascript() -> Vec<&'static Recording> {
    let fixtures = RECORDINGS
        .iter()
        .map(|recording| recording.fixture.as_str())
        .filter(|fixture| fixture.starts_with("javascript"))
        .collect::<std::collections::BTreeSet<_>>();
    let mut chosen = Vec::new();
    for fixture in fixtures {
        let oldest = RECORDINGS
            .iter()
            .find(|recording| recording.fixture == fixture)
            .unwrap();
        chosen.push(oldest);
        let newest = newest_of(fixture);
        if newest.release != oldest.release {
            chosen.push(newest);
        }
    }
    chosen
}

/// A run any release recorded, then given `supercov runs <id> mutants`: its
/// mutants are listed from the checkout's sources (it was measured without
/// them, as every release before 4.29.0 measured) and kept beside it, with
/// the same answers whichever release measured it. Every query still
/// answers with the list beside the run, and its shards still merge, the
/// merge listing what the whole suite does. A strong pass then runs and
/// saves a verdict for each mutant into the run, or is refused with the
/// reason where no driver runs the fixtures' runner.
#[test]
fn old_runs_list_their_mutants_and_answer_a_strong_pass() {
    let summary = |installed: &Installed, id: &str| {
        let reply: Value = serde_json::from_str(&installed.answers(
            "mutants",
            &["runs", id, "mutants", "--json", "--limit", "0"],
        ))
        .unwrap();
        reply["data"]["summary"].clone()
    };
    let newest_summaries = oldest_and_newest_javascript()
        .into_iter()
        .map(|recording| newest_of(&recording.fixture))
        .map(|newest| {
            let installed = newest.install();
            (
                newest.fixture.as_str(),
                summary(&installed, installed.id("full")),
            )
        })
        .collect::<BTreeMap<_, _>>();
    each_of(oldest_and_newest_javascript(), |recording| {
        let installed = recording.install();
        let file = recording.spec()["file"].as_str().unwrap().to_owned();
        for run in RUNS {
            let id = installed.id(run);
            let listed = summary(&installed, id);
            assert!(
                listed["mutants"].as_u64().unwrap_or(0) > 0,
                "{run}: no mutants listed: {listed}"
            );
            assert!(
                installed
                    .project
                    .root
                    .join(".supercov/runs")
                    .join(id)
                    .join("mutants-listed.json")
                    .is_file(),
                "{run}: the list is kept beside the run"
            );
        }
        let id = installed.id("full").to_owned();
        assert_eq!(
            summary(&installed, &id),
            newest_summaries[recording.fixture.as_str()],
            "the mutants and their weak verdicts are those of the newest release's run"
        );
        installed.answers("mutants of a file", &["runs", &id, "mutants", &file]);
        installed.answers(
            "mutants as markdown",
            &["runs", &id, "mutants", "--format", "markdown"],
        );
        // The list is part of what the query index is built from: the
        // queries answer from an index built again with it.
        installed.answers("files", &["runs", &id, "files"]);
        installed.answers("file", &["runs", &id, "file", &file]);
        let html = installed.project.root.join("html");
        installed.answers(
            "html",
            &[
                "runs",
                &id,
                "report",
                "--format",
                "html",
                "--output",
                html.to_str().unwrap(),
            ],
        );

        let saved = installed
            .project
            .root
            .join(".supercov/runs")
            .join(&id)
            .join("strong-mutation.json");
        let strong = installed.supercov(&["runs", &id, "mutants", "test", "--json"]);
        if strong.code() == 0 {
            let verdicts: Value = serde_json::from_slice(&fs::read(&saved).unwrap()).unwrap();
            let verdicts = verdicts["mutants"].as_object().unwrap();
            assert_eq!(
                verdicts.len() as u64,
                summary(&installed, &id)["mutants"].as_u64().unwrap(),
                "a verdict for every mutant"
            );
            assert!(
                verdicts
                    .values()
                    .all(|verdict| verdict["status"] != "error"),
                "{verdicts:?}"
            );
        } else {
            let refused = strong.exits(2);
            assert!(
                refused.contains("The strong pass runs tests under vitest, playwright")
                    && refused.contains("ran under node:test")
                    && refused.contains("cannot run their mutants"),
                "{refused}"
            );
            assert!(!saved.exists(), "a refused pass saves nothing");
        }

        let merged = installed.answers(
            "merge",
            &["merge", installed.id("shard-1"), installed.id("shard-2")],
        );
        let merged_id = merged
            .split_whitespace()
            .find(|word| word.starts_with("run_"))
            .unwrap_or_else(|| panic!("no merged run named\n{merged}"))
            .to_owned();
        // The strong verdicts are the whole run's, which the merge of its
        // shards does not have: everything else is the same.
        let without_score = |mut summary: Value| {
            summary["mutationScore"] = Value::Null;
            summary
        };
        assert_eq!(
            summary(&installed, &merged_id),
            without_score(summary(&installed, &id)),
            "the shards merged list the whole suite's mutants"
        );
    });
}
