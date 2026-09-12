use std::collections::BTreeSet;

use serde_json::json;
use supercov_engine::{
    asserted_coverage::{ReasonKind, Status, join},
    coverage_report::{CoverageManifest, RawTestResult},
    rust_asserted_coverage::analyze_runtime_assertions,
};

fn manifest() -> CoverageManifest {
    serde_json::from_value(json!({
        "decisions": [], "branches": [], "points": [
            {"id":"f", "kind":"function", "file":"src/lib.rs", "line":1, "column":0, "source":"fn f() {}"},
            {"id":"never", "kind":"function", "file":"src/lib.rs", "line":2, "column":0, "source":"fn never() {}"},
            {"id":"test", "kind":"function", "file":"tests/t.rs", "line":1, "column":0, "source":"fn test() {}"}
        ]
    })).unwrap()
}

fn result() -> RawTestResult {
    serde_json::from_value(json!({
        "test":"t", "testFile":"tests/t.rs", "status":"passed", "phases":[
            {"id":"a", "kind":"assertion", "operation":"Rust assertion at tests/t.rs:2:4",
             "source":"assert_eq!({ f(); 42 }, 42)", "startedAtMs":0, "status":"passed"}
        ], "runtime":[{"hits":["f"], "events":[
            {"type":"hit", "id":"f", "timestampMs":0, "phaseId":"a", "environment":"rust", "statementId":"tests/t.rs:2:4"}
        ]}]
    })).unwrap()
}

fn files() -> BTreeSet<String> {
    BTreeSet::from(["src/lib.rs".into()])
}

#[test]
fn temporal_links_never_become_value_observations() {
    let output = analyze_runtime_assertions(&manifest(), &[result()], &files()).unwrap();
    assert_eq!(
        output.facts.sites.len(),
        2,
        "keeps uncalled functions; excludes explicit out-of-scope files"
    );
    assert_eq!(output.execution_links.len(), 1);
    assert_eq!(
        output.execution_links[0].statement.as_deref(),
        Some("tests/t.rs:2:4")
    );
    assert!(output.facts.tests[0].observations.is_empty());
    let resolutions = join(&output.facts);
    assert_eq!(resolutions[0].status, Status::Unresolved);
    assert_eq!(
        resolutions[0].reason.as_ref().unwrap().kind,
        ReasonKind::LimitOperandShape
    );
    assert_eq!(
        resolutions[1].reason.as_ref().unwrap().kind,
        ReasonKind::GapNotReached
    );
}

#[test]
fn earlier_execution_remains_a_limit_not_a_claim_of_missing_assertions() {
    let mut input = result();
    input.runtime[0].events[0].phase_id = None;
    input.runtime[0].events[0].statement_id = None;
    let output = analyze_runtime_assertions(&manifest(), &[input], &files()).unwrap();
    assert!(output.execution_links.is_empty());
    assert_eq!(
        join(&output.facts)[0].reason.as_ref().unwrap().kind,
        ReasonKind::LimitOperandShape
    );
}

#[test]
fn failed_flaky_setup_and_failed_assertions_cannot_supply_links() {
    for field in [
        "failed-test",
        "flaky",
        "setup",
        "failed-assertion",
        "incomplete-assertion",
    ] {
        let mut input = result();
        match field {
            "failed-test" => input.status = Some("failed".into()),
            "flaky" => input.flaky = true,
            "setup" => input.role = "setup".into(),
            "failed-assertion" => input.phases[0].status = Some("failed".into()),
            _ => input.phases[0].status = None,
        }
        let output = analyze_runtime_assertions(&manifest(), &[input], &files()).unwrap();
        assert!(output.execution_links.is_empty(), "{field}");
    }
}

#[test]
fn retries_and_repeated_assertions_are_not_merged() {
    let input = result();
    let mut repeated = input.clone();
    repeated.phases[0].id = "b".into();
    repeated.runtime[0].events[0].phase_id = Some("b".into());
    let output = analyze_runtime_assertions(&manifest(), &[input, repeated], &files()).unwrap();
    assert_eq!(output.execution_links.len(), 2);
    assert_ne!(
        output.execution_links[0].attempt,
        output.execution_links[1].attempt
    );
    assert_ne!(
        output.execution_links[0].phase,
        output.execution_links[1].phase
    );
}

#[test]
fn invalid_phase_identity_fails_closed() {
    let mut input = result();
    input.runtime[0].events[0].phase_id = Some("unknown".into());
    assert!(analyze_runtime_assertions(&manifest(), &[input], &files()).is_err());
    let mut input = result();
    input.phases.push(input.phases[0].clone());
    assert!(analyze_runtime_assertions(&manifest(), &[input], &files()).is_err());
}

#[test]
fn unmeasured_functions_remain_visible() {
    let mut manifest = manifest();
    manifest.unmeasured.push("never".into());
    let output = analyze_runtime_assertions(&manifest, &[result()], &files()).unwrap();
    assert_eq!(output.unmeasured, ["never"]);
    assert_eq!(output.facts.sites.len(), 2);
}

#[test]
fn incomplete_assertion_binding_is_visible_even_without_dropped_records() {
    let mut input = result();
    input.phases[0].status = None;
    let output = analyze_runtime_assertions(&manifest(), &[input], &files()).unwrap();
    assert_eq!(output.unsettled_assertions.len(), 1);
    assert!(output.unsettled_assertions[0].contains("tests/t.rs:2:4"));
    assert!(output.execution_links.is_empty());
}
