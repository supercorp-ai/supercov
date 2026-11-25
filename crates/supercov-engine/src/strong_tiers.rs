//! A pass's first tier (`docs/design/strong-mutation.md`, "A first tier,
//! then the rest"): a Playwright mutant with many candidate tests runs first
//! against the few most likely to kill it, every such mutant before any runs
//! the rest. One its first tier lets live has `survived so far`, which the
//! run keeps as the candidates it passed (its progress), never as a verdict;
//! the rest run after, in the same pass where it has the time, or in a later
//! one. A kill is any candidate's and a survivor needs every candidate to
//! pass, so the order changes when a verdict comes, never what it is.

use std::collections::BTreeMap;

use crate::strong_plan::{
    Candidate, CandidateReason, PlannedMutant, PlannedTest, Progress, StrongPlan, UNTIMED_TEST_MS,
    Verdict,
};

/// How many candidates a first tier holds at least: about 94% of the kills
/// of a large application's e2e suite came from a mutant's first 32
/// candidates in this order, and 95% from its first 37.
pub const FIRST_TIER: usize = 32;

/// The share of a run's kills a first tier is sized to hold: where more than
/// this share of them came after its candidates, it grows to hold them.
const KILLS_HELD: f64 = 0.95;

/// How many kills a run must have before they size its first tier.
const KILLS_TO_SIZE: usize = 20;

/// How much likelier to kill a mutant a test whose own evidence infected it
/// is taken to be than one that only reached it: what orders a mutant's
/// candidates with their times, as the Playwright driver orders them
/// (`INFECTED_LIKELIER` in `playwrightStrong.mjs`).
const INFECTED_LIKELIER: u64 = 4;

/// The runner whose mutants have a first tier: each candidate runs on its
/// own, with a page and a request scope of its own, so a candidate that
/// passed is a fact a later run of the mutant can rest on.
const PLAYWRIGHT: &str = "playwright";

/// Whether a candidate's own evidence ran the mutant's code: it infected it,
/// could not tell, read its value, or ran it with others. A test that may
/// only have caused the code to run (work no test was credited with, a
/// module its file loaded) is a candidate on another's evidence: of 3,448
/// kills on a large application's e2e suite, 14 came from such a test.
fn own_evidence(reason: CandidateReason) -> bool {
    !matches!(
        reason,
        CandidateReason::Unattributed | CandidateReason::ModuleLoad
    )
}

/// A mutant's candidates in the order its first tier takes them: those whose
/// own evidence ran its code first, then the rest; within each, cheapest to
/// a kill first (each test's measured time over how likely it is to kill,
/// one that infected it the likelier), the plan's order between equals.
pub fn ordered(mutant: &PlannedMutant, tests: &[PlannedTest]) -> Vec<Candidate> {
    let mut ordered = mutant.candidates.clone();
    ordered.sort_by_key(|candidate| {
        let time = tests[candidate.test].duration_ms.unwrap_or(UNTIMED_TEST_MS);
        let likelier = if candidate.reason == CandidateReason::Infected {
            1
        } else {
            INFECTED_LIKELIER
        };
        (!own_evidence(candidate.reason), time * likelier)
    });
    ordered
}

/// Whether a mutant has a first tier: the plan does not decide it, and every
/// candidate is a Playwright test. One a server must start with has one too:
/// a candidate that passed in its round alone passes again in the next.
fn tiered(mutant: &PlannedMutant, tests: &[PlannedTest]) -> bool {
    mutant.decided.is_none()
        && mutant
            .candidates
            .iter()
            .all(|candidate| tests[candidate.test].runner == PLAYWRIGHT)
}

/// Whether a mutant's progress counts for it in a later pass over the same
/// run where `strong_verdicts::resumes` would not have it: one a server must
/// start with, whose progress only its rounds alone record. Another run's
/// plan may not start a server with it, so its progress is not carried or
/// merged.
pub fn resumes_alone(mutant: &PlannedMutant, tests: &[PlannedTest]) -> bool {
    mutant.server_wide.is_some() && tiered(mutant, tests)
}

/// How many candidates a run's first tier holds: `FIRST_TIER`, or more where
/// the run's kills say so. Each kill's place in its mutant's order is
/// counted, and the tier holds `KILLS_HELD` of them. A first tier's kills
/// are never past its end, so it only grows, from kills its rest made.
pub fn size(plan: &StrongPlan, verdicts: &BTreeMap<String, Verdict>) -> usize {
    let mut places = Vec::new();
    for mutant in plan
        .mutants
        .iter()
        .filter(|mutant| tiered(mutant, &plan.tests))
    {
        let Some(killer) = verdicts
            .get(&mutant.id)
            .filter(|verdict| verdict.status == "killed")
            .and_then(|verdict| verdict.killed_by.as_deref())
        else {
            continue;
        };
        if let Some(place) = ordered(mutant, &plan.tests)
            .iter()
            .position(|candidate| plan.tests[candidate.test].id == killer)
        {
            places.push(place + 1);
        }
    }
    if places.len() < KILLS_TO_SIZE {
        return FIRST_TIER;
    }
    places.sort_unstable();
    let held = (places.len() as f64 * KILLS_HELD).ceil() as usize;
    places[held - 1].max(FIRST_TIER)
}

/// The first tier of each mutant with more candidates than `size`: its
/// first `size` candidates in `ordered`'s order, kept in the plan's, by
/// mutant id.
pub fn first_tiers(
    mutants: &[PlannedMutant],
    tests: &[PlannedTest],
    size: usize,
) -> BTreeMap<String, Vec<Candidate>> {
    mutants
        .iter()
        .filter(|mutant| mutant.candidates.len() > size && tiered(mutant, tests))
        .map(|mutant| {
            let mut tier = ordered(mutant, tests);
            tier.truncate(size);
            tier.sort_by_key(|candidate| {
                mutant
                    .candidates
                    .iter()
                    .position(|planned| planned == candidate)
            });
            (mutant.id.clone(), tier)
        })
        .collect()
}

/// What a first tier's `survived` says: every candidate of the tier passed
/// with the mutant, which is its progress, not a verdict. Each such verdict
/// is taken off `verdicts`, and where it counts every test of the tier (a
/// candidate the driver could not run leaves it fewer), the tier goes into
/// `passed`, by test id. Any other verdict stands: a kill is any
/// candidate's.
pub fn lived_through(
    verdicts: &mut BTreeMap<String, Verdict>,
    passed: &mut Progress,
    tiers: &BTreeMap<String, Vec<String>>,
) {
    for (id, tier) in tiers {
        let Some(verdict) = verdicts.get(id).filter(|v| v.status == "survived") else {
            continue;
        };
        if verdict.tests == tier.len() as u64 {
            passed
                .entry(id.clone())
                .or_default()
                .extend(tier.iter().cloned());
        }
        verdicts.remove(id);
    }
}

/// The mutants a run left with `survived so far`: no verdict, and candidates
/// that passed with them. Each is said as a strong status of its own,
/// `survived-so-far`, with how many candidates it passed: never a survivor,
/// never in the score.
pub fn survived_so_far(
    verdicts: &BTreeMap<String, Verdict>,
    progress: &Progress,
) -> BTreeMap<String, serde_json::Value> {
    progress
        .iter()
        .filter(|(id, passed)| !passed.is_empty() && !verdicts.contains_key(*id))
        .map(|(id, passed)| {
            (
                id.clone(),
                serde_json::json!({"status": SURVIVED_SO_FAR, "tests": passed.len()}),
            )
        })
        .collect()
}

/// The strong status of a mutant some candidates let live and the rest have
/// not run with yet.
pub const SURVIVED_SO_FAR: &str = "survived-so-far";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong_plan::ServerWide;
    use std::collections::BTreeSet;

    fn test(id: &str, runner: &str, duration_ms: Option<u64>) -> PlannedTest {
        PlannedTest {
            id: id.into(),
            file: Some(format!("e2e/{id}.spec.js")),
            name: id.into(),
            title: None,
            runner: runner.into(),
            duration_ms,
            span: None,
            runnable: true,
            failed: false,
        }
    }

    fn mutant(id: &str, candidates: &[(usize, CandidateReason)]) -> PlannedMutant {
        PlannedMutant {
            id: id.into(),
            file: "src/a.js".into(),
            language: "javascript".into(),
            index: 0,
            mutator: "BooleanLiteral".into(),
            weak: "infected",
            decided: None,
            reason: None,
            candidates: candidates
                .iter()
                .map(|(test, reason)| Candidate {
                    test: *test,
                    reason: *reason,
                })
                .collect(),
            whole_files: Vec::new(),
            server_wide: None,
            server_ran: false,
            passed_before: 0,
            covering: candidates.len(),
            statement: None,
            function: None,
            function_hit_by: BTreeMap::new(),
            narrowed: None,
        }
    }

    fn verdict(status: &str, killed_by: Option<&str>, tests: u64) -> Verdict {
        Verdict {
            status: status.into(),
            killed_by: killed_by.map(str::to_owned),
            tests,
            duration_ms: 0,
            reason: None,
        }
    }

    fn ids(tests: &[PlannedTest], order: &[Candidate]) -> Vec<String> {
        order
            .iter()
            .map(|candidate| tests[candidate.test].id.clone())
            .collect()
    }

    use CandidateReason::{Infected, ModuleLoad, Read, Unattributed, Unknown};

    /// Own evidence first, whatever the time; within each, a test that
    /// infected it before one four times faster that did not, a test with
    /// no time as a second, and the plan's order between equals.
    #[test]
    fn candidates_with_their_own_evidence_come_first_cheapest_to_a_kill_first() {
        let tests = vec![
            test("cheap-shared", "playwright", Some(10)),
            test("slow-infected", "playwright", Some(5_000)),
            test("unknown", "playwright", Some(1_000)),
            test("untimed-infected", "playwright", None),
            test("read", "playwright", Some(300)),
            test("module", "playwright", Some(20)),
            test("tie-infected", "playwright", Some(4_000)),
        ];
        let m = mutant(
            "m",
            &[
                (0, Unattributed),
                (1, Infected),
                (2, Unknown),
                (3, Infected),
                (4, Read),
                (5, ModuleLoad),
                (6, Infected),
            ],
        );
        assert_eq!(
            ids(&tests, &ordered(&m, &tests)),
            [
                "untimed-infected",
                "read",
                "unknown",
                "tie-infected",
                "slow-infected",
                "cheap-shared",
                "module"
            ]
        );
    }

    /// A first tier is a Playwright mutant's first candidates in that
    /// order, kept in the plan's; a mutant with no more candidates than the
    /// tier holds, one the plan decides and one with a test of another
    /// runner have none. One a server must start with has one, and its
    /// progress counts in the same run.
    #[test]
    fn a_first_tier_is_the_likeliest_candidates_of_a_playwright_mutant() {
        let tests = vec![
            test("a", "playwright", Some(900)),
            test("b", "playwright", Some(100)),
            test("c", "playwright", Some(50)),
            test("unit", "vitest", Some(1)),
        ];
        let three = [(0, Infected), (1, Infected), (2, Unattributed)];
        let mut server = mutant("server", &three);
        server.server_wide = Some(ServerWide::ModuleLevel);
        let mut decided = mutant("decided", &three);
        decided.decided = Some(crate::strong_plan::Decided::Survived);
        let mutants = [
            mutant("many", &three),
            mutant("two", &[(0, Infected), (1, Infected)]),
            mutant("mixed", &[(0, Infected), (1, Infected), (3, Infected)]),
            decided,
            server.clone(),
        ];
        let tiers = first_tiers(&mutants, &tests, 2);
        assert_eq!(
            tiers
                .iter()
                .map(|(id, tier)| (id.as_str(), ids(&tests, tier)))
                .collect::<Vec<_>>(),
            [
                ("many", vec!["a".to_owned(), "b".to_owned()]),
                ("server", vec!["a".to_owned(), "b".to_owned()]),
            ]
        );
        assert!(resumes_alone(&server, &tests));
        assert!(!resumes_alone(&mutants[0], &tests));
        assert!(!resumes_alone(&mutants[2], &tests));
    }

    /// The tier holds 95% of the run's kills, never fewer than 32
    /// candidates; fewer than 20 kills, a kill whose test is no candidate
    /// and a verdict of another mutant size nothing.
    #[test]
    fn a_first_tier_grows_to_hold_the_run_s_kills() {
        let tests = (0..60)
            .map(|at| test(&format!("t{at}"), "playwright", Some(100 + at)))
            .collect::<Vec<_>>();
        let all = (0..60).map(|at| (at, Infected)).collect::<Vec<_>>();
        let mutants = (0..21)
            .map(|at| mutant(&format!("m{at}"), &all))
            .collect::<Vec<_>>();
        let plan = StrongPlan {
            mutant_set: "set".into(),
            tests,
            mutants,
        };
        // Nineteen kills, at the first candidate: too few to size it.
        let mut verdicts = (0..19)
            .map(|at| (format!("m{at}"), verdict("killed", Some("t0"), 1)))
            .collect::<BTreeMap<_, _>>();
        verdicts.insert("m19".into(), verdict("killed", Some("elsewhere"), 1));
        verdicts.insert("m20".into(), verdict("survived", None, 60));
        verdicts.insert("other".into(), verdict("killed", Some("t59"), 1));
        assert_eq!(size(&plan, &verdicts), FIRST_TIER);
        // A twentieth, at the 50th: 95% of them are still at the first.
        verdicts.insert("m19".into(), verdict("killed", Some("t49"), 1));
        assert_eq!(size(&plan, &verdicts), FIRST_TIER);
        // A twenty-first there too: 95% now needs the 50th.
        verdicts.insert("m20".into(), verdict("killed", Some("t49"), 1));
        assert_eq!(size(&plan, &verdicts), 50);
        // A tier that would be smaller stays 32.
        for at in 0..21 {
            verdicts.insert(format!("m{at}"), verdict("killed", Some("t3"), 1));
        }
        assert_eq!(size(&plan, &verdicts), FIRST_TIER);
    }

    /// A first tier's survivor is progress: every test of the tier where
    /// the verdict counts them all, none where the driver ran fewer. Kills
    /// stand, and a mutant with no tier keeps its survivor.
    #[test]
    fn a_first_tier_s_survivor_is_progress_not_a_verdict() {
        let tiers = BTreeMap::from([
            ("all".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
            ("fewer".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
            ("killed".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
            ("open".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
        ]);
        let mut verdicts = BTreeMap::from([
            ("all".to_owned(), verdict("survived", None, 2)),
            ("fewer".to_owned(), verdict("survived", None, 1)),
            ("killed".to_owned(), verdict("killed", Some("b"), 1)),
            ("untiered".to_owned(), verdict("survived", None, 5)),
        ]);
        let mut passed = Progress::from([("all".to_owned(), BTreeSet::from(["z".to_owned()]))]);
        lived_through(&mut verdicts, &mut passed, &tiers);
        assert_eq!(verdicts.keys().collect::<Vec<_>>(), ["killed", "untiered"]);
        assert_eq!(
            passed,
            Progress::from([(
                "all".to_owned(),
                BTreeSet::from(["a".to_owned(), "b".to_owned(), "z".to_owned()])
            )])
        );
        // What a run shows of them: those with progress and no verdict.
        let progress = Progress::from([
            ("all".to_owned(), BTreeSet::from(["a".to_owned()])),
            ("killed".to_owned(), BTreeSet::from(["a".to_owned()])),
            ("empty".to_owned(), BTreeSet::new()),
        ]);
        assert_eq!(
            survived_so_far(&verdicts, &progress),
            BTreeMap::from([(
                "all".to_owned(),
                serde_json::json!({"status": "survived-so-far", "tests": 1})
            )])
        );
    }
}
