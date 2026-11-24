//! A run's saved strong verdicts (`strong-mutation.json`): the ones its plan
//! decides without running anything, how they are written, and what a merge
//! of runs that each hold some of them may claim for the merged run's suite
//! (`docs/design/mutants.md`, Merged runs).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::strong_plan::{Decided, PlannedMutant, StrongPlan, StrongVerdicts, Verdict};

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

/// What one of the merged runs lends the merged run's verdicts.
pub struct Lent<'a> {
    /// Its verdicts, by mutant id, of the merged list's mutant set.
    pub verdicts: &'a BTreeMap<String, Verdict>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong_plan::{Candidate, CandidateReason, PlannedTest};

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
                    tests: &tests,
                    other_bytes: &none,
                },
                Lent {
                    verdicts: &second,
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
                        tests: &unit_tests,
                        other_bytes: &changed,
                    },
                    Lent {
                        verdicts: &e2e,
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
