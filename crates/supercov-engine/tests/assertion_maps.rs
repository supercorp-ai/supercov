use serde_json::json;
use std::collections::BTreeSet;
use supercov_engine::{assertion_map::*, assertion_store::assess, coverage_report::*};

fn anchor(file: &str, text: &str, needle: &str) -> Anchor {
    let start = text.find(needle).unwrap();
    Anchor::new(file, text, start, start + needle.len())
}
fn fixture() -> (Inputs, AssertionMap, State) {
    let test = "import assert from 'node:assert/strict';\nassert.equal(result, 1);\n";
    let source = "return 1;\n";
    let at = anchor("test.js", test, "assert.equal(result, 1)");
    let inputs = Inputs {
        schema_version: 1,
        language: "javascript".into(),
        context_digest: "context".into(),
        files: Files::from([
            ("test.js".into(), test.into()),
            ("src/a.js".into(), source.into()),
            ("src/b.js".into(), source.into()),
            ("src/helper.js".into(), "helper();\n".into()),
        ]),
        assertions: vec![InventorySite {
            at,
            operation: "assert.equal".into(),
        }],
        limitations: vec![],
    };
    let (mut map, mut state) = seed(&inputs, "archive");
    let a = &mut map.assertions[0];
    a.analysis = Analysis::Mapped;
    a.observes = vec!["result equals one".into()];
    a.flows = ["a", "b"]
        .into_iter()
        .map(|id| Flow {
            id: id.into(),
            explanation: format!("Author judgement for {id}"),
            applies_to: vec![],
            nodes: vec![Node {
                id: "return".into(),
                at: anchor(&format!("src/{id}.js"), source, "return 1;"),
                role: "value".into(),
                meaning: String::new(),
            }],
            edges: vec![],
            counts_as_asserted: vec!["return".into()],
            watch: vec![
                Watch::File {
                    file: format!("src/{id}.js"),
                },
                Watch::File {
                    file: "test.js".into(),
                },
            ],
        })
        .collect();
    review(&map, &mut state, &inputs, &BTreeSet::new(), true, false).unwrap();
    (inputs, map, state)
}
fn current(map: &AssertionMap, state: &State, inputs: &Inputs) -> usize {
    map.assertions
        .iter()
        .flat_map(|a| a.flows.iter().map(move |f| (a, f)))
        .filter(|(a, f)| reasons(a, f, state, inputs).is_empty())
        .count()
}

#[test]
fn unchanged_and_blank_line_moves_reuse_review() {
    let (old, map, state) = fixture();
    let mut new = old.clone();
    for text in new.files.values_mut() {
        *text = format!("\n\n{text}");
    }
    for site in &mut new.assertions {
        site.at.line += 2;
    }
    let (carried, state) = carry(&map, &state, &old, &new, "new", false).unwrap();
    assert_eq!(current(&carried, &state, &new), 2);
    assert!(state.scope_review.is_empty());
    assert_eq!(map.assertions[0].id, carried.assertions[0].id);
    assert_eq!(carried.assertions[0].at.line, 4);
}
#[test]
fn changed_flow_stays_dirty_through_carries_and_partial_review() {
    let (old, map, state) = fixture();
    let mut new = old.clone();
    new.files.insert("src/a.js".into(), "return 2;\n".into());
    let (mut map, state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(current(&map, &state, &new), 1);
    let (next, mut state) = carry(&map, &state, &new, &new, "three", false).unwrap();
    map = next;
    assert_eq!(current(&map, &state, &new), 1);
    // Acknowledging only the unaffected sibling never clears the dirty one.
    let selected = BTreeSet::from([flow_key(&map.assertions[0], &map.assertions[0].flows[1])]);
    review(&map, &mut state, &new, &selected, false, false).unwrap();
    assert_eq!(current(&map, &state, &new), 1);
    assert!(review(&map, &mut state, &new, &BTreeSet::new(), true, false).is_err());
    map.assertions[0].flows[0].nodes[0].at.text = "return 2;".into();
    review(&map, &mut state, &new, &BTreeSet::new(), true, false).unwrap();
    assert_eq!(current(&map, &state, &new), 2);
}

#[test]
fn changed_assertion_keeps_its_explanation_as_a_dirty_identity_suggestion() {
    let (old, map, state) = fixture();
    let mut new = old.clone();
    new.files
        .get_mut("test.js")
        .unwrap()
        .replace_range(.., &old.files["test.js"].replace("result, 1", "result, 2"));
    new.assertions[0].at.text = "assert.equal(result, 2)".into();
    let (next, state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(next.assertions.len(), 1);
    assert_eq!(next.assertions[0].id, map.assertions[0].id);
    assert_eq!(
        next.assertions[0].flows[0].explanation,
        map.assertions[0].flows[0].explanation
    );
    assert_eq!(current(&next, &state, &new), 0);
}

#[test]
fn newly_declared_watch_missing_in_old_snapshot_does_not_panic_on_carry() {
    let (old, mut map, state) = fixture();
    let mut new = old.clone();
    map.assertions[0].flows[0].watch.push(Watch::File {
        file: "new.js".into(),
    });
    new.files.insert("new.js".into(), "new();".into());
    let (next, state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(current(&next, &state, &new), 1);
}
#[test]
fn revert_does_not_implicitly_acknowledge_review() {
    let (old, map, state) = fixture();
    let mut new = old.clone();
    new.files.insert("src/a.js".into(), "return 2;\n".into());
    let (map, state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    let (map, state) = carry(&map, &state, &new, &old, "three", false).unwrap();
    assert_eq!(current(&map, &state, &old), 1);
}
#[test]
fn map_edits_require_acknowledgement_even_with_unchanged_sources() {
    let (inputs, mut map, state) = fixture();
    map.assertions[0].flows[0].explanation.push_str(" revised");
    assert_eq!(current(&map, &state, &inputs), 1);
    let (map, state) = carry(&map, &state, &inputs, &inputs, "two", false).unwrap();
    assert_eq!(current(&map, &state, &inputs), 1);
}
#[test]
fn assertion_meaning_edit_invalidates_every_flow() {
    let (inputs, mut map, state) = fixture();
    map.assertions[0].observes.push("another property".into());
    assert_eq!(current(&map, &state, &inputs), 0);
}
#[test]
fn deleted_or_changed_assertions_keep_their_explanations() {
    let (old, map, state) = fixture();
    let mut new = old.clone();
    new.assertions.clear();
    new.files.insert("test.js".into(), String::new());
    let (next, _) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert!(next.assertions.is_empty());
    assert_eq!(next.retired_assertions[0].assertion, map.assertions[0]);
}
#[test]
fn new_assertions_are_unmapped_and_do_not_steal_old_ids() {
    let (old, map, state) = fixture();
    let mut new = old.clone();
    new.files
        .get_mut("test.js")
        .unwrap()
        .push_str("assert.equal(other, 2);\n");
    new.assertions.push(InventorySite {
        at: anchor("test.js", &new.files["test.js"], "assert.equal(other, 2)"),
        operation: "assert.equal".into(),
    });
    let (next, _) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(next.assertions.len(), 2);
    assert_eq!(next.assertions[0].id, map.assertions[0].id);
    assert_eq!(next.assertions[1].analysis, Analysis::Unmapped);
}
#[test]
fn ambiguous_duplicate_is_never_matched_by_distance() {
    let old = Files::from([("a.js".into(), "before(); x(); after();".into())]);
    let at = anchor("a.js", &old["a.js"], "x()");
    let new = Files::from([("a.js".into(), "changed(); x(); x(); after();".into())]);
    assert!(relocate(&at, &old, &new).is_none());
}
#[test]
fn unique_file_rename_rebases_and_ambiguous_rename_dirties() {
    let (mut old, map, state) = fixture();
    old.files
        .insert("src/a.js".into(), "return 1;\n// unique".into());
    let mut state = state;
    review(&map, &mut state, &old, &BTreeSet::new(), true, false).unwrap_err();
    state.inputs_digest = digest(&old);
    review(&map, &mut state, &old, &BTreeSet::new(), true, false).unwrap();
    let mut new = old.clone();
    let source = new.files.remove("src/a.js").unwrap();
    new.files.insert("src/renamed.js".into(), source.clone());
    let (next, next_state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(current(&next, &next_state, &new), 2);
    assert!(next_state.scope_review.is_empty());
    new.files.insert("src/other.js".into(), source);
    let (next, next_state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(current(&next, &next_state, &new), 1);
}
#[test]
fn unwatched_changes_and_new_files_queue_scope_review() {
    let (old, map, state) = fixture();
    let mut new = old.clone();
    new.files
        .insert("src/helper.js".into(), "changed();\n".into());
    new.files.insert("src/new.js".into(), "newThing();".into());
    let (map, mut state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(current(&map, &state, &new), 2);
    assert_eq!(state.scope_review.len(), 2);
    review(&map, &mut state, &new, &BTreeSet::new(), false, true).unwrap();
    assert!(state.scope_review.is_empty());
}
#[test]
fn context_change_dirties_all_without_new_tracing() {
    let (inputs, map, state) = fixture();
    let (map, state) = carry(&map, &state, &inputs, &inputs, "two", true).unwrap();
    assert_eq!(current(&map, &state, &inputs), 0);
}
#[test]
fn span_watch_does_not_hide_unwatched_setup_edit_in_same_file() {
    let (mut old, mut map, mut state) = fixture();
    old.files
        .insert("src/a.js".into(), "setup();\nreturn 1;\n".into());
    map.assertions[0].flows[0].nodes[0].at.line = 2;
    map.assertions[0].flows[0].watch[0] = Watch::Span {
        at: map.assertions[0].flows[0].nodes[0].at.clone(),
    };
    state.inputs_digest = digest(&old);
    review(&map, &mut state, &old, &BTreeSet::new(), true, false).unwrap();
    let mut new = old.clone();
    new.files
        .insert("src/a.js".into(), "setupChanged();\nreturn 1;\n".into());
    let (map, state) = carry(&map, &state, &old, &new, "two", false).unwrap();
    assert_eq!(current(&map, &state, &new), 2);
    assert!(state.scope_review.contains("src/a.js"));
}
#[test]
fn unicode_and_multiline_anchors_use_utf8_byte_columns() {
    let source = "🙂; assert(\n  true\n);";
    let at = anchor("test.js", source, "assert(\n  true\n)");
    assert_eq!(at.column, 7);
    assert_eq!(
        at.offset(&Files::from([("test.js".into(), source.into())])),
        Some(6)
    );
    let mut invalid = at;
    invalid.column = 2;
    assert!(
        invalid
            .offset(&Files::from([("test.js".into(), source.into())]))
            .is_none()
    );
}
#[test]
fn validation_rejects_dangling_edges_unknown_credit_duplicate_ids_and_bad_paths() {
    let (inputs, map, _) = fixture();
    for kind in 0..4 {
        let mut map = map.clone();
        let f = &mut map.assertions[0].flows[0];
        match kind {
            0 => f.edges.push(Edge {
                from: "absent".into(),
                to: "return".into(),
                kind: "agent-defined".into(),
                basis: String::new(),
            }),
            1 => f.counts_as_asserted.push("absent".into()),
            2 => f.nodes.push(f.nodes[0].clone()),
            _ => f.nodes[0].at.file = "../outside".into(),
        };
        assert!(!validate(&map, &inputs).is_empty());
    }
}
fn report_fixture(hits: &[&str], assertion_passed: bool) -> CoverageReport {
    let phase = json!({"id":"phase","kind":"assertion","operation":"assert.equal","source":"test.js:2:1","startedAtMs":0,"endedAtMs":1,"status":if assertion_passed {"passed"}else{"failed"}});
    let raw = json!({"testId":"test","test":"test","testFile":"test.js","retry":0,"status":"passed","flaky":false,"provenance":{"runner":"node:test","kind":"unit","source":"test"},"role":"test","phases":[phase],"runtime":[{"hits":hits}],"browser":[],"server":[]});
    let request: CoverageReportRequest=serde_json::from_value(json!({"runId":"run","generatedAt":"now","testExitCode":0,
        "manifest":{"decisions":[],"branches":[],"points":[
            {"id":"a","kind":"statement","file":"src/a.js","line":1,"column":1,"source":"return 1;"},
            {"id":"b","kind":"statement","file":"src/b.js","line":1,"column":1,"source":"return 1;"}]},"rawResults":[raw]})).unwrap();
    analyze_coverage_results(&request).unwrap()
}
#[test]
fn score_requires_explicit_credit_current_review_and_passing_site() {
    let (inputs, mut map, state) = fixture();
    let coverage = report_fixture(&["a", "b"], true);
    assert_eq!(
        assess(&map, &state, &inputs, &coverage, true)["summary"]["lines"]["percentage"],
        100.0
    );
    let failed_site = report_fixture(&["a", "b"], false);
    assert_eq!(
        assess(&map, &state, &inputs, &failed_site, true)["summary"]["lines"]["asserted"],
        0
    );
    map.assertions[0].flows[0].counts_as_asserted.clear();
    assert_eq!(
        assess(&map, &state, &inputs, &coverage, true)["summary"]["lines"]["asserted"],
        1
    );
    assert_eq!(
        assess(&map, &state, &inputs, &coverage, false)["summary"]["lines"]["asserted"],
        0
    );
}
#[test]
fn absence_claims_do_not_credit_unexecuted_code_and_scope_blocks_credit() {
    let (inputs, map, mut state) = fixture();
    let coverage = report_fixture(&["a"], true);
    assert_eq!(
        assess(&map, &state, &inputs, &coverage, true)["summary"]["lines"]["percentage"],
        50.0
    );
    state.scope_review.insert("unknown helper".into());
    assert_eq!(
        assess(&map, &state, &inputs, &coverage, true)["summary"]["lines"]["asserted"],
        0
    );
}
#[test]
fn unsupported_runtime_identity_does_not_borrow_test_level_assertions() {
    let (inputs, map, state) = fixture();
    let mut coverage = report_fixture(&["a", "b"], true);
    coverage.filters.passed.phases[0].phase.source = Some("test.js::whole_test".into());
    assert_eq!(
        assess(&map, &state, &inputs, &coverage, true)["summary"]["lines"]["asserted"],
        0
    );
}

#[test]
fn unmeasured_statements_never_enter_the_primary_denominator_or_numerator() {
    let (inputs, map, state) = fixture();
    let mut coverage = report_fixture(&["a", "b"], true);
    coverage.filters.passed.points[1].measured = false;
    let report = assess(&map, &state, &inputs, &coverage, true);
    assert_eq!(report["summary"]["statements"]["total"], 1);
    assert_eq!(report["summary"]["statements"]["asserted"], 1);
    assert_eq!(report["summary"]["statements"]["percentage"], 100.0);
}
#[test]
fn execution_from_another_test_cannot_supply_credit() {
    let (inputs, map, state) = fixture();
    let mut coverage = report_fixture(&["a", "b"], true);
    for p in &mut coverage.filters.passed.points {
        p.tests = vec!["different test".into()];
    }
    assert_eq!(
        assess(&map, &state, &inputs, &coverage, true)["summary"]["lines"]["asserted"],
        0
    );
}
#[test]
fn frozen_inputs_accept_absolute_relative_and_windows_verbatim_paths_once() {
    let temporary = std::env::temp_dir().join(format!("supercov-map-paths-{}", std::process::id()));
    let directory = temporary.join("project");
    std::fs::create_dir_all(&directory).unwrap();
    let canonical = std::fs::canonicalize(&directory).unwrap();
    // Discovery supplies ordinary absolute paths, while canonicalize on Windows
    // returns a verbatim path. Both must identify the same frozen source file.
    #[cfg(windows)]
    let root = std::path::PathBuf::from(canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    #[cfg(not(windows))]
    let root = canonical.clone();
    let source = "import assert from 'node:assert/strict'; assert.equal(1, 1);";
    std::fs::write(root.join("test.js"), source).unwrap();
    let inputs = supercov_engine::assertion_inputs::capture(
        &root,
        "javascript",
        [
            "test.js".into(),
            root.join("test.js"),
            canonical.join("test.js"),
        ],
    )
    .unwrap();
    assert_eq!(inputs.files.len(), 1);
    assert_eq!(inputs.assertions.len(), 1);
    assert_eq!(inputs.assertions[0].at.file, "test.js");
    assert_eq!(inputs.files["test.js"], source);
    let outside = root.parent().unwrap().join("outside.js");
    std::fs::write(&outside, source).unwrap();
    assert!(
        supercov_engine::assertion_inputs::capture(&root, "javascript", [outside])
            .unwrap_err()
            .contains("assertion input outside project")
    );
    std::fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn syntax_inventory_spans_work_across_all_language_adapters() {
    let root = std::env::temp_dir().join(format!("supercov-map-languages-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for (file, source, expected) in [
        (
            "test.js",
            "import assert from 'node:assert/strict'; assert.equal(1, 1);",
            1,
        ),
        ("test.py", "assert True\nself.assertEqual(1, 1)\n", 2),
        ("test.rb", "assert_equal 1, 1\nexpect(1).to eq(1)\n", 2),
        ("test.rs", "fn main() { assert_eq!(1, 1); }", 1),
    ] {
        std::fs::write(root.join(file), source).unwrap();
        let inputs =
            supercov_engine::assertion_inputs::capture(&root, "fixture", [file.into()]).unwrap();
        assert_eq!(inputs.assertions.len(), expected, "{file}");
        assert!(
            inputs
                .assertions
                .iter()
                .all(|s| s.at.offset(&inputs.files).is_some())
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn syntax_diagnostics_locate_nested_errors_and_reject_ambiguous_json() {
    let (_, map, _) = fixture();
    let mut value = serde_json::to_value(map).unwrap();
    value["assertions"][0]["flows"][0]["countsAsAsserted"] = json!(true);
    let error = parse(&serde_json::to_vec_pretty(&value).unwrap()).unwrap_err();
    assert_eq!(error.pointer, "/assertions/0/flows/0/countsAsAsserted");
    assert!(error.line > 1);
    for text in [
        r#"{"assertions":[],"assertions":[]}"#,
        r#"{"assertions":[],"unknown":1}"#,
        r#"{"assertions":[],"schemaVersion":2}"#,
        r#"{"assertions":[]} {}"#,
    ] {
        assert!(parse(text.as_bytes()).is_err(), "{text}");
    }
    assert!(parse(br#"{"assertions":[]}"#).is_ok());
}

#[test]
fn javascript_inventory_includes_async_operands_and_fixture_modules() {
    use supercov_engine::js_instrumenter::{
        assertion_ranges_with_expect_modules, instrument_node_assertion_phases_with_expect_modules,
    };
    let source = "import { expect as check } from '@acme/fixtures';\nasync function test() { check(await value()).toBe(1); check(value()).toBe(1); }\n";
    let modules = vec!["@acme/fixtures".into()];
    let inventory = assertion_ranges_with_expect_modules("test.ts", source, &modules).unwrap();
    assert_eq!(inventory.len(), 2, "await sites must stay inventoried");
    assert!(source[inventory[0].0..inventory[0].1].contains("await"));
    let transformed =
        instrument_node_assertion_phases_with_expect_modules(source, "test.ts", &modules).unwrap();
    assert_eq!(
        transformed.assertions, 2,
        "callee binding preserves await syntax"
    );
    let cjs = "const { expect: check } = require('@jest/globals'); check(1).toBe(1); function f(check) { check(2).toBe(2); }";
    assert_eq!(
        assertion_ranges_with_expect_modules("test.cjs", cjs, &[])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_coordinate_cannot_substitute_for_exact_assertion_identity() {
    let (inputs, mut map, mut state) = fixture();
    let coverage = report_fixture(&["a", "b"], true);
    map.assertions[0].at.text.push(';');
    assert!(
        validate(&map, &inputs).is_empty(),
        "source exists but it is a different anchor"
    );
    review(&map, &mut state, &inputs, &BTreeSet::new(), true, false).unwrap();
    let report = assess(&map, &state, &inputs, &coverage, true);
    assert_eq!(report["summary"]["statements"]["asserted"], 0);
    assert_eq!(report["summary"]["unobservedAssertions"], 1);
}

#[test]
fn statement_view_exposes_exact_anchors_and_flow_credit() {
    let (inputs, map, state) = fixture();
    let report = assess(&map, &state, &inputs, &report_fixture(&["a"], true), true);
    let rows = report["statements"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["at"]["text"], "return 1;");
    assert_eq!(
        rows[0]["flows"][0],
        flow_key(&map.assertions[0], &map.assertions[0].flows[0])
    );
    assert_eq!(rows[1]["declared"], true);
    assert_eq!(rows[1]["asserted"], false);
}

#[test]
fn crediting_a_control_statement_does_not_credit_its_nested_body() {
    let (mut inputs, mut map, mut state) = fixture();
    let source = "if (true) { return 1; }\n";
    inputs.files.insert("src/a.js".into(), source.into());
    map.assertions[0].flows[0].nodes[0].at = anchor("src/a.js", source, source.trim());
    state.inputs_digest = digest(&inputs);
    review(&map, &mut state, &inputs, &BTreeSet::new(), true, false).unwrap();
    let mut coverage = report_fixture(&["a", "b"], true);
    let mut inner = coverage.filters.passed.points[0].clone();
    inner.meta.id = "inner".into();
    inner.meta.column = 13;
    coverage.filters.passed.points[0].meta.source = source.trim().into();
    coverage.filters.passed.points.push(inner);
    let report = assess(&map, &state, &inputs, &coverage, true);
    assert_eq!(report["summary"]["statements"]["asserted"], 2);
    assert_eq!(report["summary"]["statements"]["total"], 3);
    assert_eq!(report["statements"][2]["asserted"], false);
}
