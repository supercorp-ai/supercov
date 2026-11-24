//! A run's saved strong verdicts (`strong-mutation.json`): the ones its plan
//! decides without running anything, how they are written, and what a merge
//! of runs that each hold some of them may claim for the merged run's suite
//! (`docs/design/mutants.md`, Merged runs).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::strong_plan::{Decided, PlannedMutant, Progress, StrongPlan, StrongVerdicts, Verdict};

/// The verdict a plan gives a mutant without running it: survived where no
/// test infected it, no coverage where none ran it, equivalent where a
/// static rule shows no input can tell it apart.
pub fn decided_verdict(mutant: &PlannedMutant) -> Option<Verdict> {
    let status = match mutant.decided? {
        Decided::Survived => "survived",
        Decided::NoCoverage => "no-coverage",
        Decided::Equivalent => "equivalent",
    };
    Some(Verdict {
        status: status.into(),
        killed_by: None,
        tests: 0,
        duration_ms: 0,
        reason: mutant.reason.clone(),
    })
}

/// Writes `document` to `path` whole, then renames it into place: a pass
/// killed while it saves leaves the last whole save.
pub fn save(path: &Path, document: &StrongVerdicts) -> Result<(), String> {
    let partial = path.with_extension("json.partial");
    // fault-site: strong-verdicts.save
    crate::fault_fs::write(
        &partial,
        serde_json::to_vec_pretty(document).expect("plain data serializes"),
    )
    .and_then(|()| crate::fault_fs::rename(&partial, path))
    .map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether a mutant's partial progress counts for it: the plan does not
/// decide it, each candidate runs on its own, and a request's own switch
/// reaches it (not a mutant the server must start with, which a candidate's
/// pass with a per-request switch says nothing of). Only then is "this
/// candidate passed with it" a fact a later pass can rest on. A mutant that
/// runs a test file whole (module-level code) runs its candidates one by one
/// only where every one is a Playwright test: each attempt has a page and a
/// request scope of its own, so the file's earlier tests leave nothing the
/// mutant made behind.
pub fn resumes(mutant: &PlannedMutant, tests: &[crate::strong_plan::PlannedTest]) -> bool {
    let alone = mutant.whole_files.is_empty()
        || mutant
            .candidates
            .iter()
            .all(|candidate| tests[candidate.test].runner == PLAYWRIGHT);
    mutant.decided.is_none() && mutant.server_wide.is_none() && alone
}

/// The runner whose tests run one by one whatever the plan says of their
/// files.
const PLAYWRIGHT: &str = "playwright";

/// A mutant some of whose candidates already passed with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resumed {
    pub id: String,
    /// How many of its candidates passed with it before.
    pub passed: usize,
    /// How many candidates it has.
    pub candidates: usize,
}

/// Takes off each mutant's candidates those that already passed with it
/// (`progress`, by test id), and says, for each mutant it took some off,
/// how many of how many. A mutant every candidate of which passed is left
/// with none: it survived. A kill needs one failing candidate, which a
/// candidate that passed is not; a survivor needs every candidate to pass,
/// which the ones taken off did.
pub fn resume(plan: &mut StrongPlan, progress: &Progress) -> Vec<Resumed> {
    let StrongPlan { tests, mutants, .. } = plan;
    let mut resumed = Vec::new();
    for mutant in mutants.iter_mut().filter(|mutant| resumes(mutant, tests)) {
        let Some(passed) = progress.get(&mutant.id) else {
            continue;
        };
        let candidates = mutant.candidates.len();
        mutant
            .candidates
            .retain(|candidate| !passed.contains(&tests[candidate.test].id));
        let left = mutant.candidates.len();
        if left < candidates {
            resumed.push(Resumed {
                id: mutant.id.clone(),
                passed: candidates - left,
                candidates,
            });
        }
    }
    resumed
}

/// Whether a mutant's saved verdict settles it: any but an `error`, which
/// a later pass runs again, and which keeps its progress.
pub fn settles(verdict: &Verdict) -> bool {
    verdict.status != "error"
}

/// What one of the merged runs lends the merged run's verdicts.
pub struct Lent<'a> {
    /// Its verdicts, by mutant id, of the merged list's mutant set.
    pub verdicts: &'a BTreeMap<String, Verdict>,
    /// Its partial progress: the candidates each mutant it left without a
    /// verdict passed with.
    pub progress: &'a Progress,
    /// The ids of the tests it measured.
    pub tests: &'a BTreeSet<String>,
    /// The files it recorded with other bytes than the run whose mutants of
    /// them the merge listed: a verdict there is of other code.
    pub other_bytes: &'a BTreeSet<String>,
}

/// The merged run's verdicts, by mutant id, from those of the runs merged
/// (`docs/design/mutants.md`, Merged runs), and how many of them each run
/// lent: a verdict two runs hold alike is counted for the first, in the
/// order given. Only the mutants of the merged plan have one. A mutant the
/// plan decides takes the plan's verdict, lent by no run. Of the rest, a
/// kill in any run is a kill (killed before timeout), and a mutant that did
/// not compile in one did not in any. Survived stands only where every test
/// of the merged suite that could kill it (its candidates, and the tests of
/// the files it runs whole) is one a run that let it survive measured: a
/// test only another run had, which reached it with no verdict for it,
/// leaves it undecided, and a later pass runs it. An error stands only where
/// no run holds anything else.
pub fn merged_verdicts(
    plan: &StrongPlan,
    lent: &[Lent<'_>],
) -> (BTreeMap<String, Verdict>, Vec<usize>) {
    let mut merged = BTreeMap::new();
    let mut taken = vec![0; lent.len()];
    for mutant in &plan.mutants {
        if let Some(verdict) = decided_verdict(mutant) {
            merged.insert(mutant.id.clone(), verdict);
            continue;
        }
        let held = lent
            .iter()
            .enumerate()
            .filter(|(_, run)| !run.other_bytes.contains(&mutant.file))
            .filter_map(|(index, run)| Some((index, run, run.verdicts.get(&mutant.id)?)))
            .collect::<Vec<_>>();
        let first_of = |status: &str| {
            held.iter()
                .find(|(.., verdict)| verdict.status == status)
                .map(|(index, _, verdict)| (*index, (*verdict).clone()))
        };
        let survived_in = held
            .iter()
            .filter(|(.., verdict)| verdict.status == "survived")
            .map(|(_, run, _)| run.tests)
            .collect::<Vec<_>>();
        let kept = if let Some(definite) = ["killed", "timeout", "unviable"]
            .into_iter()
            .find_map(first_of)
        {
            Some(definite)
        } else if survived_in.is_empty() {
            first_of("error")
        } else {
            let whole = plan.tests.iter().filter(|test| {
                test.file
                    .as_ref()
                    .is_some_and(|file| mutant.whole_files.contains(file))
            });
            let could_kill = mutant
                .candidates
                .iter()
                .map(|candidate| &plan.tests[candidate.test])
                .chain(whole)
                .collect::<Vec<_>>();
            let all_measured = could_kill
                .iter()
                .all(|test| survived_in.iter().any(|tests| tests.contains(&test.id)));
            all_measured.then(|| first_of("survived")).flatten()
        };
        if let Some((index, verdict)) = kept {
            taken[index] += 1;
            merged.insert(mutant.id.clone(), verdict);
        }
    }
    (merged, taken)
}

/// The merged run's partial progress, from the runs merged: for each
/// mutant the merged verdicts leave unsettled (no verdict, or an `error`),
/// every candidate any run says passed with it -- shards of one pass each
/// tried some candidates, and a pass is a pass whichever ran it. A run lends
/// a candidate only where it measured that test and recorded the mutant's
/// file with the bytes the merged run lists, as a survival is lent.
pub fn merged_progress(
    plan: &StrongPlan,
    lent: &[Lent<'_>],
    merged: &BTreeMap<String, Verdict>,
) -> Progress {
    let mut progress = Progress::new();
    for mutant in plan
        .mutants
        .iter()
        .filter(|mutant| resumes(mutant, &plan.tests))
    {
        if merged.get(&mutant.id).is_some_and(settles) {
            continue;
        }
        let candidates = mutant
            .candidates
            .iter()
            .map(|candidate| plan.tests[candidate.test].id.as_str())
            .collect::<BTreeSet<_>>();
        let mut passed = BTreeSet::new();
        for run in lent
            .iter()
            .filter(|run| !run.other_bytes.contains(&mutant.file))
        {
            passed.extend(
                run.progress
                    .get(&mutant.id)
                    .into_iter()
                    .flatten()
                    .filter(|test| run.tests.contains(*test) && candidates.contains(test.as_str()))
                    .cloned(),
            );
        }
        if !passed.is_empty() {
            progress.insert(mutant.id.clone(), passed);
        }
    }
    progress
}

/// What a merge says of the progress it kept, after its verdicts: nothing
/// where it kept none.
pub fn progress_note(progress: &Progress) -> String {
    if progress.is_empty() {
        return String::new();
    }
    format!(
        "; and the progress of {} mutant(s) with no verdict yet, {} candidate(s) they passed",
        progress.len(),
        progress.values().map(BTreeSet::len).sum::<usize>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong_plan::{Candidate, CandidateReason, PlannedTest, ServerWide};

    fn test(id: &str, file: &str) -> PlannedTest {
        PlannedTest {
            id: id.into(),
            file: Some(file.into()),
            name: id.into(),
            title: None,
            runner: "vitest".into(),
            duration_ms: None,
            span: None,
            runnable: true,
        }
    }

    fn mutant(id: &str, file: &str, candidates: &[usize], whole: &[&str]) -> PlannedMutant {
        PlannedMutant {
            id: id.into(),
            file: file.into(),
            language: "javascript".into(),
            index: 0,
            mutator: "BooleanLiteral".into(),
            weak: "infected",
            decided: None,
            reason: None,
            candidates: candidates
                .iter()
                .map(|test| Candidate {
                    test: *test,
                    reason: CandidateReason::Infected,
                })
                .collect(),
            whole_files: whole.iter().map(|file| (*file).to_owned()).collect(),
            server_wide: None,
            covering: candidates.len(),
            statement: None,
            function: None,
            function_hit_by: BTreeMap::new(),
            narrowed: None,
        }
    }

    fn verdict(status: &str) -> Verdict {
        Verdict {
            status: status.into(),
            killed_by: (status == "killed").then(|| "a test".into()),
            tests: 1,
            duration_ms: 3,
            reason: None,
        }
    }

    fn verdicts(of: &[(&str, &str)]) -> BTreeMap<String, Verdict> {
        of.iter()
            .map(|(id, status)| ((*id).to_owned(), verdict(status)))
            .collect()
    }

    fn ids(of: &[&str]) -> BTreeSet<String> {
        of.iter().map(|id| (*id).to_owned()).collect()
    }

    fn progress(of: &[(&str, &[&str])]) -> Progress {
        of.iter()
            .map(|(id, tests)| ((*id).to_owned(), ids(tests)))
            .collect()
    }

    /// Candidates that passed with a mutant come off its list, and a mutant
    /// every candidate of which passed has none left. A mutant the plan
    /// decides, one that runs a test file whole and one a server must start
    /// with keep theirs; an error does not settle a mutant, and keeps its
    /// progress.
    #[test]
    fn a_mutant_resumes_from_the_candidates_it_passed() {
        let mut plan = StrongPlan {
            mutant_set: "set".into(),
            tests: vec![
                test("t1", "t/a.spec.js"),
                test("t2", "t/a.spec.js"),
                test("t3", "t/a.spec.js"),
            ],
            mutants: vec![
                mutant("partway", "src/a.js", &[0, 1, 2], &[]),
                mutant("done", "src/a.js", &[0, 1], &[]),
                mutant("untouched", "src/a.js", &[0, 1], &[]),
                mutant("whole", "src/a.js", &[0], &["t/a.spec.js"]),
                mutant("server", "src/a.js", &[0], &[]),
                mutant("decided", "src/a.js", &[0], &[]),
            ],
        };
        plan.mutants[4].server_wide = Some(ServerWide::Background);
        plan.mutants[5].decided = Some(Decided::Survived);
        // Module-level code under Playwright: each candidate runs alone.
        plan.tests.push(PlannedTest {
            runner: "playwright".into(),
            ..test("p1", "e2e/a.spec.js")
        });
        plan.tests.push(PlannedTest {
            runner: "playwright".into(),
            ..test("p2", "e2e/a.spec.js")
        });
        plan.mutants
            .push(mutant("page", "src/a.js", &[3, 4], &["e2e/a.spec.js"]));
        let passed = progress(&[
            ("partway", &["t1", "t3"]),
            ("done", &["t1", "t2"]),
            ("whole", &["t1"]),
            ("server", &["t1"]),
            ("decided", &["t1"]),
            ("gone", &["t1"]),
            ("page", &["p1"]),
            // A test that is no candidate of it now: nothing comes off.
            ("untouched", &["t9"]),
        ]);
        assert_eq!(
            resume(&mut plan, &passed),
            [
                Resumed {
                    id: "partway".into(),
                    passed: 2,
                    candidates: 3
                },
                Resumed {
                    id: "done".into(),
                    passed: 2,
                    candidates: 2
                },
                Resumed {
                    id: "page".into(),
                    passed: 1,
                    candidates: 2
                },
            ]
        );
        let left = |id: &str| {
            plan.mutants
                .iter()
                .find(|mutant| mutant.id == id)
                .unwrap()
                .candidates
                .iter()
                .map(|candidate| candidate.test)
                .collect::<Vec<_>>()
        };
        assert_eq!(left("partway"), [1]);
        assert!(left("done").is_empty());
        assert_eq!(left("untouched"), [0, 1]);
        for kept in ["whole", "server", "decided"] {
            assert_eq!(left(kept), [0], "{kept}");
        }
        assert!(settles(&verdict("killed")) && settles(&verdict("survived")));
        assert!(!settles(&verdict("error")));
        assert_eq!(left("page"), [4]);
    }

    /// Shards' progress merges to its union, for the mutants the merged
    /// verdicts leave unsettled. A run lends only tests it measured, of the
    /// mutant's candidates, and only where it recorded the mutant's file as
    /// the merged run lists it; a mutant whose progress does not count
    /// (a file it runs whole) has none.
    #[test]
    fn shards_progress_merges_to_its_union() {
        let plan = StrongPlan {
            mutant_set: "set".into(),
            tests: vec![
                test("t1", "t/a.spec.js"),
                test("t2", "t/a.spec.js"),
                test("t3", "t/b.spec.js"),
            ],
            mutants: vec![
                mutant("open", "src/a.js", &[0, 1, 2], &[]),
                mutant("erred", "src/a.js", &[0, 1], &[]),
                mutant("killed", "src/a.js", &[0, 1], &[]),
                mutant("changed", "src/b.js", &[0, 1], &[]),
                mutant("whole", "src/a.js", &[0], &["t/b.spec.js"]),
                // Every run recorded its file with other bytes: none lends.
                mutant("elsewhere", "src/c.js", &[0, 1], &[]),
            ],
        };
        let first_progress = progress(&[
            ("open", &["t1", "t9"]),
            ("erred", &["t1"]),
            ("killed", &["t1"]),
            ("changed", &["t1"]),
            ("whole", &["t1"]),
            ("elsewhere", &["t1"]),
        ]);
        let second_progress = progress(&[
            ("open", &["t2", "t3"]),
            ("changed", &["t2"]),
            ("elsewhere", &["t2"]),
        ]);
        let first = verdicts(&[("erred", "error")]);
        let second = verdicts(&[("killed", "killed")]);
        let (first_tests, second_tests) = (ids(&["t1", "t2", "t3", "t9"]), ids(&["t1", "t2"]));
        let (first_changed, changed) = (ids(&["src/c.js"]), ids(&["src/b.js", "src/c.js"]));
        let lent = [
            Lent {
                verdicts: &first,
                progress: &first_progress,
                tests: &first_tests,
                other_bytes: &first_changed,
            },
            Lent {
                verdicts: &second,
                progress: &second_progress,
                tests: &second_tests,
                other_bytes: &changed,
            },
        ];
        let (merged, _) = merged_verdicts(&plan, &lent);
        let held = merged_progress(&plan, &lent, &merged);
        assert_eq!(
            held,
            progress(&[
                ("open", &["t1", "t2"]),
                ("erred", &["t1"]),
                ("changed", &["t1"]),
            ])
        );
        assert_eq!(
            progress_note(&held),
            "; and the progress of 3 mutant(s) with no verdict yet, 4 candidate(s) they passed"
        );
        assert_eq!(progress_note(&Progress::new()), "");
    }

    fn statuses(merged: &BTreeMap<String, Verdict>) -> Vec<(&str, &str)> {
        merged
            .iter()
            .map(|(id, verdict)| (id.as_str(), verdict.status.as_str()))
            .collect()
    }

    /// The plan decides what it decides; the rest are each said once, and
    /// a decided mutant takes no run's verdict.
    #[test]
    fn a_decided_mutant_takes_the_plan_s_verdict() {
        let mut plan = StrongPlan {
            mutant_set: "set".into(),
            tests: vec![test("t1", "t/a.test.js")],
            mutants: Vec::new(),
        };
        for (id, decided) in [
            ("survived", Decided::Survived),
            ("no-coverage", Decided::NoCoverage),
            ("equivalent", Decided::Equivalent),
        ] {
            let mut decided_mutant = mutant(id, "src/a.js", &[], &[]);
            decided_mutant.decided = Some(decided);
            decided_mutant.reason = Some("why".into());
            plan.mutants.push(decided_mutant);
        }
        let killed = verdicts(&[("survived", "killed")]);
        let tests = ids(&["t1"]);
        let none = BTreeSet::new();
        let (merged, taken) = merged_verdicts(
            &plan,
            &[Lent {
                verdicts: &killed,
                progress: &Progress::new(),
                tests: &tests,
                other_bytes: &none,
            }],
        );
        assert_eq!(
            statuses(&merged),
            [
                ("equivalent", "equivalent"),
                ("no-coverage", "no-coverage"),
                ("survived", "survived")
            ]
        );
        // The plan's verdicts are lent by no run.
        assert_eq!(taken, [0]);
        assert!(merged.values().all(|verdict| verdict.tests == 0
            && verdict.duration_ms == 0
            && verdict.killed_by.is_none()
            && verdict.reason.as_deref() == Some("why")));
        assert_eq!(
            decided_verdict(&mutant("open", "src/a.js", &[0], &[])),
            None
        );
        // A plan of no mutant keeps nothing.
        plan.mutants.clear();
        assert_eq!(merged_verdicts(&plan, &[]), (BTreeMap::new(), Vec::new()));
    }

    /// Two shards of one suite, each with half the verdicts, merge to all
    /// of them; a kill outranks a survival, and a survival an error; the
    /// survival of one shard stands where the other has none, as each
    /// measured every test.
    #[test]
    fn disjoint_shards_of_one_suite_merge_to_the_union() {
        let plan = StrongPlan {
            mutant_set: "set".into(),
            tests: vec![test("t1", "t/a.test.js"), test("t2", "t/b.test.js")],
            mutants: ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
                .iter()
                .map(|id| mutant(id, "src/a.js", &[0, 1], &[]))
                .collect(),
        };
        let first = verdicts(&[
            ("a", "killed"),
            ("b", "survived"),
            ("d", "survived"),
            ("e", "error"),
            ("f", "error"),
            ("g", "unviable"),
            ("gone", "killed"),
            ("i", "killed"),
        ]);
        let second = verdicts(&[
            ("c", "timeout"),
            ("d", "killed"),
            ("e", "survived"),
            ("g", "survived"),
            ("h", "no-coverage"),
            ("i", "killed"),
        ]);
        let tests = ids(&["t1", "t2"]);
        let none = BTreeSet::new();
        let (merged, taken) = merged_verdicts(
            &plan,
            &[
                Lent {
                    verdicts: &first,
                    progress: &Progress::new(),
                    tests: &tests,
                    other_bytes: &none,
                },
                Lent {
                    verdicts: &second,
                    progress: &Progress::new(),
                    tests: &tests,
                    other_bytes: &none,
                },
            ],
        );
        assert_eq!(
            statuses(&merged),
            [
                ("a", "killed"),
                ("b", "survived"),
                ("c", "timeout"),
                ("d", "killed"),
                ("e", "survived"),
                ("f", "error"),
                ("g", "unviable"),
                ("i", "killed"),
            ]
        );
        assert_eq!(merged["a"].killed_by.as_deref(), Some("a test"));
        // Each verdict is counted for the run it was taken from: a, b, f
        // and g the first's, c, d and e the second's, and i, which both
        // hold alike, the first's.
        assert_eq!(taken, [5, 3]);
    }

    /// A unit run and an integration run: a survival in one stands only
    /// where the other's tests could not kill the mutant; where they could,
    /// and gave no verdict, it is undecided, and so where a test of a file
    /// the mutant runs whole is the other's. A kill in either is a kill. A
    /// run that recorded the mutant's file with other bytes lends nothing
    /// there.
    #[test]
    fn a_survival_stands_only_where_no_other_run_s_test_could_kill() {
        let plan = StrongPlan {
            mutant_set: "set".into(),
            tests: vec![
                test("unit", "t/unit.test.js"),
                test("e2e", "e2e/app.spec.js"),
                test("fileless", "t/unit.test.js"),
            ],
            mutants: vec![
                // Only the unit test could kill it.
                mutant("unit-only", "src/a.js", &[0], &[]),
                // The e2e test could too.
                mutant("both", "src/a.js", &[0, 1], &[]),
                // A file it runs whole holds a test only e2e had.
                mutant("whole", "src/a.js", &[0], &["e2e/app.spec.js"]),
                // A file it runs whole that the unit run measured.
                mutant("whole-unit", "src/a.js", &[0], &["t/unit.test.js"]),
                mutant("killed-e2e", "src/a.js", &[0, 1], &[]),
                mutant("changed", "src/b.js", &[0], &[]),
            ],
        };
        let mut no_file = plan.clone();
        no_file.tests[2].file = None;
        let unit = verdicts(&[
            ("unit-only", "survived"),
            ("both", "survived"),
            ("whole", "survived"),
            ("whole-unit", "survived"),
            ("killed-e2e", "survived"),
            ("changed", "killed"),
        ]);
        let e2e = verdicts(&[("killed-e2e", "killed")]);
        let (unit_tests, e2e_tests) = (ids(&["unit", "fileless"]), ids(&["e2e"]));
        let (same, changed) = (BTreeSet::new(), ids(&["src/b.js"]));
        for plan in [&plan, &no_file] {
            let (merged, taken) = merged_verdicts(
                plan,
                &[
                    Lent {
                        verdicts: &unit,
                        progress: &Progress::new(),
                        tests: &unit_tests,
                        other_bytes: &changed,
                    },
                    Lent {
                        verdicts: &e2e,
                        progress: &Progress::new(),
                        tests: &e2e_tests,
                        other_bytes: &same,
                    },
                ],
            );
            assert_eq!(
                statuses(&merged),
                [
                    ("killed-e2e", "killed"),
                    ("unit-only", "survived"),
                    ("whole-unit", "survived"),
                ]
            );
            assert_eq!(taken, [2, 1]);
        }
    }

    /// Verdicts are written whole and renamed into place; a path that
    /// cannot be written says where.
    #[test]
    fn verdicts_are_saved_whole_and_a_failed_save_says_where() {
        let root = crate::test_directory::TestDirectory::adopt(std::env::temp_dir().join(format!(
            "supercov-strong-verdicts-save-{}",
            std::process::id()
        )));
        std::fs::create_dir_all(&*root).unwrap();
        let path = root.join("strong-mutation.json");
        let document = StrongVerdicts {
            schema_version: crate::strong_plan::STRONG_VERDICTS_SCHEMA_VERSION,
            run: "r1".into(),
            mutant_set: "set".into(),
            mutants: verdicts(&[("a", "killed")]),
            progress: Progress::new(),
        };
        save(&path, &document).unwrap();
        let read = serde_json::from_slice::<StrongVerdicts>(&std::fs::read(&path).unwrap());
        assert_eq!(read.unwrap(), document);
        assert!(!path.with_extension("json.partial").exists());
        let unwritable = root.join("missing/strong-mutation.json");
        let error = save(&unwritable, &document).unwrap_err();
        assert!(
            error.starts_with(&unwritable.display().to_string()),
            "{error}"
        );
    }
}
