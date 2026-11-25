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

/// How much of its tests' measured time a first tier holds at least, in
/// milliseconds: it ends at `FIRST_TIER` candidates or once its candidates
/// take this long, whichever comes first. Of 3,972 kills on a large
/// application's e2e suite, each placed in its mutant's order with the time
/// of the candidates up to its killer, 90% came within 26 s and 95% within
/// 61 s; a tier of 32 candidates or 60 s holds 93.4% of them where 32
/// candidates alone hold 93.7%, and takes 34 s of a mutant's tests on
/// average where 32 candidates take 40 s. Where a test takes 10 s, it is 6
/// candidates, not 32.
pub const FIRST_TIER_MS: u64 = 60_000;

/// How many candidates a first tier holds at least, however long they take:
/// a slow test does not leave a mutant with one candidate tried.
pub const FIRST_TIER_LEAST: usize = 4;

/// A run's first tier: it ends at `candidates`, or once its candidates'
/// measured time reaches `ms` (and it holds `FIRST_TIER_LEAST`), whichever
/// comes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    pub candidates: usize,
    pub ms: u64,
}

/// A test's measured time, as a first tier counts it.
fn time_of(test: &PlannedTest) -> u64 {
    test.duration_ms.unwrap_or(UNTIMED_TEST_MS)
}

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

/// The tests that killed mutants of each file in a run, by file, each with
/// how many it killed there (`killers`).
pub type Killers = BTreeMap<String, BTreeMap<usize, usize>>;

/// Each test the run's verdicts say killed a mutant, by the mutant's file,
/// with how many it killed there: a test that killed one mutant of a file
/// exercises that file, and likely kills its other mutants. On a large
/// application's e2e suite, of 255 mutants killed past a first tier of 32,
/// 58% had their killer among the next 32 candidates with this order (from
/// the kills within the first tiers alone), and none with the order before.
pub fn killers(plan: &StrongPlan, verdicts: &BTreeMap<String, Verdict>) -> Killers {
    let index = plan
        .tests
        .iter()
        .enumerate()
        .map(|(at, test)| (test.id.as_str(), at))
        .collect::<std::collections::HashMap<_, _>>();
    let mut killers = Killers::new();
    for mutant in &plan.mutants {
        let Some(killer) = verdicts
            .get(&mutant.id)
            .filter(|verdict| verdict.status == "killed")
            .and_then(|verdict| verdict.killed_by.as_deref())
            .and_then(|killer| index.get(killer))
        else {
            continue;
        };
        *killers
            .entry(mutant.file.clone())
            .or_default()
            .entry(*killer)
            .or_default() += 1;
    }
    killers
}

/// A mutant's candidates in the order its first tier takes them: those whose
/// own evidence ran its code first, then the rest; within each, the tests
/// that killed most of its file's other mutants (`kills`, by test) first,
/// then cheapest to a kill (each test's measured time over how likely it is
/// to kill, one that infected it the likelier), the plan's order between
/// equals.
pub fn ordered(
    mutant: &PlannedMutant,
    tests: &[PlannedTest],
    kills: &BTreeMap<usize, usize>,
) -> Vec<Candidate> {
    let mut ordered = mutant.candidates.clone();
    ordered.sort_by_key(|candidate| {
        let time = time_of(&tests[candidate.test]);
        let likelier = if candidate.reason == CandidateReason::Infected {
            1
        } else {
            INFECTED_LIKELIER
        };
        let killed = kills.get(&candidate.test).copied().unwrap_or(0);
        (
            !own_evidence(candidate.reason),
            std::cmp::Reverse(killed),
            time * likelier,
        )
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

/// A run's first tier: `FIRST_TIER` candidates and `FIRST_TIER_MS`, or more
/// where the run's kills say so. Each kill's place in its mutant's order is
/// counted, with its candidates' time up to and with the killer, and the
/// tier holds `KILLS_HELD` of them by each. A first tier's kills are never
/// past its end, so it only grows, from kills its rest made. The places are
/// read without the run's killers first, whose order puts a kill at the
/// front by its own count.
pub fn sizing(plan: &StrongPlan, verdicts: &BTreeMap<String, Verdict>) -> Sizing {
    let mut places = Vec::new();
    let mut times = Vec::new();
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
        let order = ordered(mutant, &plan.tests, &BTreeMap::new());
        if let Some(place) = order
            .iter()
            .position(|candidate| plan.tests[candidate.test].id == killer)
        {
            places.push(place + 1);
            times.push(
                order[..=place]
                    .iter()
                    .map(|candidate| time_of(&plan.tests[candidate.test]))
                    .sum::<u64>(),
            );
        }
    }
    if places.len() < KILLS_TO_SIZE {
        return Sizing {
            candidates: FIRST_TIER,
            ms: FIRST_TIER_MS,
        };
    }
    places.sort_unstable();
    times.sort_unstable();
    let held = (places.len() as f64 * KILLS_HELD).ceil() as usize - 1;
    Sizing {
        candidates: places[held].max(FIRST_TIER),
        ms: times[held].max(FIRST_TIER_MS),
    }
}

/// The first tier of each mutant whose candidates outlast it: its
/// candidates in `ordered`'s order, with the tests that killed its file's
/// other mutants (`killers`) first, up to `sizing`'s count or time, kept in
/// the plan's order, by mutant id.
pub fn first_tiers(
    mutants: &[PlannedMutant],
    tests: &[PlannedTest],
    sizing: Sizing,
    killers: &Killers,
) -> BTreeMap<String, Vec<Candidate>> {
    let none = BTreeMap::new();
    mutants
        .iter()
        .filter(|mutant| tiered(mutant, tests))
        .filter_map(|mutant| {
            let kills = killers.get(&mutant.file).unwrap_or(&none);
            let mut tier = Vec::new();
            let mut ms = 0;
            for candidate in ordered(mutant, tests, kills) {
                let full = tier.len() >= sizing.candidates
                    || (tier.len() >= FIRST_TIER_LEAST && ms >= sizing.ms);
                if full {
                    break;
                }
                ms += time_of(&tests[candidate.test]);
                tier.push(candidate);
            }
            if tier.len() == mutant.candidates.len() {
                return None;
            }
            tier.sort_by_key(|candidate| {
                mutant
                    .candidates
                    .iter()
                    .position(|planned| planned == candidate)
            });
            Some((mutant.id.clone(), tier))
        })
        .collect()
}

/// What a first tier's `survived` says: every candidate of the tier passed
/// with the mutant, which is its progress, not a verdict. Each such verdict
/// is taken off `verdicts`, and where it counts every test of the tier (a
/// candidate the driver could not run leaves it fewer), the tier goes into
/// `passed`, by test id, and the mutant into `lived`: its progress holds a
/// whole first tier. Any other verdict stands: a kill is any candidate's.
pub fn lived_through(
    verdicts: &mut BTreeMap<String, Verdict>,
    passed: &mut Progress,
    lived: &mut std::collections::BTreeSet<String>,
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
            lived.insert(id.clone());
        }
        verdicts.remove(id);
    }
}

/// Whether an open mutant lived through a first tier: the run marked it so
/// (`lived`, as `lived_through` found it), or it passed as many candidates
/// as the smallest tier by count holds (what 4.29.12 and 4.29.13 kept, with
/// no mark). A pass its budget stopped partway through a tier records the
/// few candidates it passed too (on a large application's settings page, 1
/// to 4 of 135): such a mutant has not met its likeliest killers, and is no
/// more than open.
pub fn lived_a_tier(id: &str, passed: usize, lived: &std::collections::BTreeSet<String>) -> bool {
    passed > 0 && (lived.contains(id) || passed >= FIRST_TIER)
}

/// The mutants a run left with `survived so far`: no verdict, and a first
/// tier passed (`lived_a_tier`). Each is said as a strong status of its own,
/// `survived-so-far`, with how many candidates it passed: never a survivor,
/// never in the score.
pub fn survived_so_far(
    verdicts: &BTreeMap<String, Verdict>,
    progress: &Progress,
    lived: &std::collections::BTreeSet<String>,
) -> BTreeMap<String, serde_json::Value> {
    progress
        .iter()
        .filter(|(id, passed)| lived_a_tier(id, passed.len(), lived) && !verdicts.contains_key(*id))
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

    /// A tier sized by its count alone.
    fn count(candidates: usize) -> Sizing {
        Sizing {
            candidates,
            ms: u64::MAX,
        }
    }

    const DEFAULT: Sizing = Sizing {
        candidates: FIRST_TIER,
        ms: FIRST_TIER_MS,
    };

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
            ids(&tests, &ordered(&m, &tests, &BTreeMap::new())),
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
        let tiers = first_tiers(&mutants, &tests, count(2), &Killers::new());
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

    /// The tests that killed a file's mutants come first in its other
    /// mutants' tiers, the one with more kills before, whatever their time;
    /// a kill by a test the plan does not have, a survivor and another
    /// file's kills count for nothing.
    #[test]
    fn a_file_s_killers_come_first_in_its_mutants_tiers() {
        let tests = vec![
            test("render", "playwright", Some(100)),
            test("layout", "playwright", Some(200)),
            test("page", "playwright", Some(9_000)),
            test("page-again", "playwright", Some(8_000)),
        ];
        let all = [(0, Infected), (1, Infected), (2, Infected), (3, Infected)];
        let mut elsewhere = mutant("elsewhere", &all);
        elsewhere.file = "src/b.js".into();
        let plan = StrongPlan {
            mutant_set: "set".into(),
            tests: tests.clone(),
            mutants: vec![
                mutant("k1", &all),
                mutant("k2", &all),
                mutant("k3", &all),
                mutant("lost", &all),
                mutant("lived", &all),
                mutant("open", &all),
                elsewhere,
            ],
        };
        let verdicts = BTreeMap::from([
            ("k1".to_owned(), verdict("killed", Some("page"), 1)),
            ("k2".to_owned(), verdict("killed", Some("page"), 1)),
            ("k3".to_owned(), verdict("killed", Some("page-again"), 1)),
            ("lost".to_owned(), verdict("killed", Some("gone"), 1)),
            ("lived".to_owned(), verdict("survived", None, 4)),
            ("elsewhere".to_owned(), verdict("killed", Some("render"), 1)),
        ]);
        let empty = StrongPlan {
            mutants: Vec::new(),
            ..plan.clone()
        };
        assert!(killers(&empty, &verdicts).is_empty());
        let killers = killers(&plan, &verdicts);
        assert_eq!(
            killers,
            Killers::from([
                ("src/a.js".to_owned(), BTreeMap::from([(2, 2), (3, 1)])),
                ("src/b.js".to_owned(), BTreeMap::from([(0, 1)])),
            ])
        );
        let tiers = first_tiers(&plan.mutants[5..6], &tests, count(2), &killers);
        assert_eq!(ids(&tests, &tiers["open"]), ["page", "page-again"]);
        // Another file's killers change nothing of it.
        let mut other = plan.mutants[5].clone();
        other.file = "src/c.js".into();
        let tiers = first_tiers(&[other], &tests, count(2), &killers);
        assert_eq!(ids(&tests, &tiers["open"]), ["render", "layout"]);
        assert_eq!(
            ids(
                &tests,
                &ordered(&plan.mutants[5], &tests, &killers["src/a.js"])
            ),
            ["page", "page-again", "render", "layout"]
        );
    }

    /// A first tier ends at its count or once its candidates' time reaches
    /// its time, whichever comes first, but holds four candidates however
    /// slow they are; a mutant its tier would hold whole has none. The time
    /// grows to hold 95% of the run's kills by their candidates' time, never
    /// below 60 s.
    #[test]
    fn a_first_tier_ends_at_its_count_or_its_time() {
        let tests = (0..10)
            .map(|at| test(&format!("t{at}"), "playwright", Some(20_000 + at)))
            .chain([test("quick", "playwright", Some(10))])
            .collect::<Vec<_>>();
        let slow = (0..10).map(|at| (at, Infected)).collect::<Vec<_>>();
        let mutants = [
            mutant("slow", &slow),
            mutant("four", &slow[..4]),
            mutant("quick", &[(10, Infected), (0, Infected), (1, Infected)]),
            mutant("none", &[]),
        ];
        let tiers = first_tiers(&mutants, &tests, DEFAULT, &Killers::new());
        // 20 s a test: four, the least a tier holds, pass 60 s.
        assert_eq!(ids(&tests, &tiers["slow"]), ["t0", "t1", "t2", "t3"]);
        for whole in ["four", "quick", "none"] {
            assert!(!tiers.contains_key(whole), "{whole}");
        }
        let tiers = first_tiers(
            &mutants,
            &tests,
            Sizing {
                candidates: 32,
                ms: 70_000,
            },
            &Killers::new(),
        );
        assert_eq!(tiers["slow"].len(), 4);
        let tiers = first_tiers(
            &mutants,
            &tests,
            Sizing {
                candidates: 32,
                ms: 90_000,
            },
            &Killers::new(),
        );
        assert_eq!(tiers["slow"].len(), 5);
        // Twenty kills, each at its mutant's tenth candidate, 200 s in.
        let plan = StrongPlan {
            mutant_set: "set".into(),
            tests,
            mutants: (0..20).map(|at| mutant(&format!("m{at}"), &slow)).collect(),
        };
        let verdicts = (0..20)
            .map(|at| (format!("m{at}"), verdict("killed", Some("t9"), 1)))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            sizing(&plan, &verdicts),
            Sizing {
                candidates: FIRST_TIER,
                ms: 200_045
            }
        );
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
        assert_eq!(sizing(&plan, &verdicts), DEFAULT);
        // A twentieth, at the 50th: 95% of them are still at the first.
        verdicts.insert("m19".into(), verdict("killed", Some("t49"), 1));
        assert_eq!(sizing(&plan, &verdicts), DEFAULT);
        // A twenty-first there too: 95% now needs the 50th.
        verdicts.insert("m20".into(), verdict("killed", Some("t49"), 1));
        assert_eq!(
            sizing(&plan, &verdicts),
            Sizing {
                candidates: 50,
                ms: FIRST_TIER_MS
            }
        );
        // A tier that would be smaller stays 32.
        for at in 0..21 {
            verdicts.insert(format!("m{at}"), verdict("killed", Some("t3"), 1));
        }
        assert_eq!(sizing(&plan, &verdicts), DEFAULT);
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
        let mut lived = BTreeSet::from(["before".to_owned()]);
        lived_through(&mut verdicts, &mut passed, &mut lived, &tiers);
        assert_eq!(
            lived,
            BTreeSet::from(["all".to_owned(), "before".to_owned()])
        );
        assert_eq!(verdicts.keys().collect::<Vec<_>>(), ["killed", "untiered"]);
        assert_eq!(
            passed,
            Progress::from([(
                "all".to_owned(),
                BTreeSet::from(["a".to_owned(), "b".to_owned(), "z".to_owned()])
            )])
        );
        // What a run shows of them: those with no verdict that passed a
        // first tier's worth of candidates; a few passed before a stop are
        // not one.
        let tier = (0..FIRST_TIER)
            .map(|at| format!("t{at}"))
            .collect::<BTreeSet<_>>();
        let two = BTreeSet::from(["a".to_owned(), "b".to_owned()]);
        let progress = Progress::from([
            ("all".to_owned(), tier.clone()),
            ("killed".to_owned(), tier),
            ("few".to_owned(), two.clone()),
            ("timed".to_owned(), two),
            ("empty".to_owned(), BTreeSet::new()),
        ]);
        // A tier ended by its time is marked; 32 candidates with no mark
        // (4.29.12 and 4.29.13 kept none) is one too.
        let marked = BTreeSet::from(["timed".to_owned(), "empty".to_owned()]);
        assert_eq!(
            survived_so_far(&verdicts, &progress, &marked),
            BTreeMap::from([
                (
                    "all".to_owned(),
                    serde_json::json!({"status": "survived-so-far", "tests": 32})
                ),
                (
                    "timed".to_owned(),
                    serde_json::json!({"status": "survived-so-far", "tests": 2})
                ),
            ])
        );
    }
}
