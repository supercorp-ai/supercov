//! Parity between the language-neutral join and the prototype that defined it.
//!
//! The prototype in the supergateway worktree emits `facts.json` (what its analyzers found) beside
//! `resolution.json` (what it concluded). Feeding the facts to `asserted_coverage::join` must reproduce
//! the conclusions site for site: same facts in, same verdicts out. That is the only check that the port
//! carries the rules rather than an approximation of them.
//!
//! The fixtures are large and live outside this repository, so the test is opt-in:
//!
//! ```sh
//! SUPERCOV_ASSERTED_FACTS=/path/to/out/facts.json \
//! SUPERCOV_ASSERTED_RESOLUTION=/path/to/out/resolution.json \
//!   cargo test -p supercov-engine --test asserted_coverage_parity -- --nocapture
//! ```
//!
//! Without those variables it reports that it was skipped and passes, so CI stays green while the
//! prototype still lives elsewhere.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use supercov_engine::asserted_coverage::{Facts, Resolution, Status, Strength, join, summary};

#[derive(Debug, Deserialize)]
struct PrototypeResolutions {
    resolutions: Vec<PrototypeResolution>,
}

/// The prototype's own output shape: camelCase, and statuses including `explained`, which is the pragma
/// escape hatch the join does not own.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrototypeResolution {
    site: String,
    status: String,
    #[serde(default)]
    strength: Option<Strength>,
    #[serde(default)]
    reason: Option<PrototypeReason>,
    #[serde(default)]
    tests: Option<Vec<String>>,
    covered_by: usize,
    #[serde(default)]
    stuck_false_caught: Option<bool>,
    #[serde(default)]
    stuck_true_caught: Option<bool>,
    #[serde(default)]
    absence_needed: Option<bool>,
    #[serde(default)]
    value_observed: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PrototypeReason {
    kind: String,
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Evident => "evident",
        Status::Presence => "presence",
        Status::Partial => "partial",
        Status::Unresolved => "unresolved",
    }
}

#[test]
fn the_join_reproduces_the_prototype_verdicts() {
    let (Ok(facts_path), Ok(resolution_path)) = (
        std::env::var("SUPERCOV_ASSERTED_FACTS"),
        std::env::var("SUPERCOV_ASSERTED_RESOLUTION"),
    ) else {
        eprintln!(
            "skipped: set SUPERCOV_ASSERTED_FACTS and SUPERCOV_ASSERTED_RESOLUTION to compare \
             against the prototype"
        );
        return;
    };
    let facts: Facts = serde_json::from_str(
        &std::fs::read_to_string(&facts_path).expect("facts.json is readable"),
    )
    .expect("facts.json parses as the join's input schema");
    let expected: PrototypeResolutions = serde_json::from_str(
        &std::fs::read_to_string(&resolution_path).expect("resolution.json is readable"),
    )
    .expect("resolution.json parses");

    let ours = join(&facts);
    let expected_ids: BTreeSet<_> = expected.resolutions.iter().map(|r| &r.site).collect();
    let actual_ids: BTreeSet<_> = ours.iter().map(|r| &r.site).collect();
    assert_eq!(
        expected_ids.len(),
        expected.resolutions.len(),
        "duplicate prototype site ids"
    );
    assert_eq!(actual_ids.len(), ours.len(), "duplicate engine site ids");
    assert_eq!(
        actual_ids, expected_ids,
        "the complete site sets must agree"
    );
    let by_id: BTreeMap<&str, &Resolution> = ours.iter().map(|r| (r.site.as_str(), r)).collect();
    let summary = summary(&facts.sites, &ours);
    eprintln!(
        "{} sites, {} tests; contractual {} → evident {}, partial {}, presence {}, unresolved {} \
         ({} gaps, {} limits)",
        facts.sites.len(),
        facts.tests.len(),
        summary.contractual,
        summary.evident,
        summary.partial,
        summary.presence,
        summary.unresolved,
        summary.gaps,
        summary.limits,
    );

    let mut status_mismatch = Vec::new();
    let mut strength_mismatch = Vec::new();
    let mut reason_mismatch = Vec::new();
    let mut tests_mismatch = Vec::new();
    let mut compared = 0;
    for want in &expected.resolutions {
        // `explained` is the pragma path, which the frontend applies after the join
        if want.status == "explained" {
            continue;
        }
        let Some(got) = by_id.get(want.site.as_str()) else {
            status_mismatch.push(format!("{}: missing from the join's output", want.site));
            continue;
        };
        compared += 1;
        assert_eq!(
            got.covered_by, want.covered_by,
            "{}: covering-test count",
            want.site
        );
        assert_eq!(
            got.stuck_false_caught, want.stuck_false_caught,
            "{}: stuck-false verdict",
            want.site
        );
        assert_eq!(
            got.stuck_true_caught, want.stuck_true_caught,
            "{}: stuck-true verdict",
            want.site
        );
        assert_eq!(
            got.absence_needed, want.absence_needed,
            "{}: absence requirement",
            want.site
        );
        assert_eq!(
            got.value_observed, want.value_observed,
            "{}: value observation",
            want.site
        );
        if status_name(got.status) != want.status {
            status_mismatch.push(format!(
                "{}: prototype {}, join {}",
                want.site,
                want.status,
                status_name(got.status)
            ));
        }
        if got.strength != want.strength {
            strength_mismatch.push(format!(
                "{}: prototype {:?}, join {:?}",
                want.site, want.strength, got.strength
            ));
        }
        let want_reason = want.reason.as_ref().map(|r| r.kind.as_str());
        let got_reason = got
            .reason
            .as_ref()
            .map(|r| serde_json::to_value(r.kind).expect("a reason kind serializes"));
        let got_reason = got_reason
            .as_ref()
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        if want_reason.map(str::to_owned) != got_reason {
            reason_mismatch.push(format!(
                "{}: prototype {:?}, join {:?}",
                want.site, want_reason, got_reason
            ));
        }
        // the tests behind a verdict matter: the decision rules restrict evidence by outcome
        if let Some(want_tests) = &want.tests {
            let got_tests: Vec<&str> = got.tests.iter().map(String::as_str).collect();
            if want_tests.iter().map(String::as_str).collect::<Vec<_>>() != got_tests {
                tests_mismatch.push(format!(
                    "{}: prototype {:?}, join {:?}",
                    want.site, want_tests, got_tests
                ));
            }
        }
    }
    let report = |label: &str, items: &[String]| {
        if !items.is_empty() {
            eprintln!("\n{} {label}:", items.len());
            for line in items.iter().take(25) {
                eprintln!("  {line}");
            }
        }
    };
    report("status mismatches", &status_mismatch);
    report("strength mismatches", &strength_mismatch);
    report("reason mismatches", &reason_mismatch);
    report("test-set mismatches", &tests_mismatch);
    eprintln!("\ncompared {compared} sites");
    assert!(
        status_mismatch.is_empty()
            && strength_mismatch.is_empty()
            && reason_mismatch.is_empty()
            && tests_mismatch.is_empty(),
        "the join disagrees with the prototype on {} statuses, {} strengths, {} reasons and {} test sets",
        status_mismatch.len(),
        strength_mismatch.len(),
        reason_mismatch.len(),
        tests_mismatch.len(),
    );
    // Optional current comparison: the old facts remain the oracle for the engine
    // port, but stricter source analysis is deliberately NOT verdict-equivalent.
    if let Ok(path) = std::env::var("SUPERCOV_ASSERTED_CURRENT_FACTS") {
        let current: Facts = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let current_ids: BTreeSet<_> = current.sites.iter().map(|site| &site.id).collect();
        assert_eq!(current_ids, expected_ids, "v2 must retain the denominator");
        let current_results = join(&current);
        let current_summary =
            supercov_engine::asserted_coverage::summary(&current.sites, &current_results);
        let mut without_issues = current.clone();
        for test in &mut without_issues.tests {
            test.witness_issues.clear();
        }
        let previous_results = join(&without_issues);
        let mut changes = Vec::new();
        for (before, after) in previous_results.iter().zip(&current_results) {
            let mut projected = after.clone();
            projected.reason = before.reason.clone();
            projected.witness_issues.clear();
            assert_eq!(
                &projected, before,
                "witness reporting must not change credit or coverage"
            );
            if before.reason != after.reason {
                assert!(
                    before
                        .reason
                        .as_ref()
                        .is_some_and(|reason| !reason.kind.is_limit())
                );
                assert_eq!(
                    after.reason.as_ref().unwrap().kind,
                    supercov_engine::asserted_coverage::ReasonKind::LimitAssertionWitness
                );
                assert!(!after.witness_issues.is_empty());
                changes.push(serde_json::json!({"site": after.site, "before": before.reason, "after": after.reason}));
            }
        }
        let comparison = serde_json::json!({
            "sites": current.sites.len(), "tests": current.tests.len(),
            "before": supercov_engine::asserted_coverage::summary(&without_issues.sites, &previous_results),
            "after": current_summary, "changedReasons": changes,
            "sitesWithWitnessIssues": current_results.iter().filter(|r| !r.witness_issues.is_empty()).count(),
            "unchangedCreditAndCoverage": true,
        });
        if let Ok(path) = std::env::var("SUPERCOV_ASSERTED_COMPARISON_OUTPUT") {
            std::fs::write(path, serde_json::to_vec_pretty(&comparison).unwrap()).unwrap();
        }
        eprintln!(
            "witness-reporting: {} reason changes; credit and coverage unchanged",
            changes.len()
        );
        eprintln!(
            "current-analyzer-candidates: {}",
            serde_json::to_string(&current_summary).unwrap()
        );
    }
}
