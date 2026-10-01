//! The commands that ask Jev, end to end, against a local server answering
//! the way Jev does: what each sends, what it saves, and what reads that back
//! with no key and no network.

mod common;

use common::{Project, answer_no, answer_yes, contains_all, gateway, through};

const MODEL: &str = "typesafe/jev-1.13-20260917";

#[test]
fn quality_assesses_once_and_reads_back_without_the_network() {
    let project = Project::cart("quality");
    let (base, seen) = gateway(MODEL, answer_yes);
    let jev = through(&base);

    let dry = project
        .supercov_with(&["quality", "--dry-run"], &jev)
        .succeeds();
    contains_all(
        &dry,
        &[
            "\"path\": \"src/cart.js\"",
            "god_class",
            "/api/v1/systemone",
        ],
    );
    assert!(seen.lock().unwrap().is_empty(), "a dry run sends nothing");

    let assessed = project.supercov_with(&["quality"], &jev).succeeds();
    contains_all(
        &assessed,
        &[
            "Quality weak (1.0/10) over 1 files.",
            "weak  src/cart.js",
            "model typesafe/jev-1.13",
        ],
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    project.supercov_with(&["quality"], &jev).succeeds();
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the repeat is answered from cache"
    );

    // Reading needs neither key nor network.
    let gaps = project.supercov(&["quality", "gaps"]).succeeds();
    contains_all(
        &gaps,
        &["1 files with findings", "0.90  complex_conditional"],
    );
    let file = project
        .supercov(&["quality", "file", "src/cart.js"])
        .succeeds();
    contains_all(
        &file,
        &[
            "src/cart.js  quality weak (1.0/10)",
            "yes  0.90  magic_values",
        ],
    );
    let scope = project.supercov(&["quality", "scope"]).succeeds();
    contains_all(
        &scope,
        &["1 included", "excluded   test/cart.test.js  (test)"],
    );
    let snapshots = project.supercov(&["quality", "snapshots"]).succeeds();
    assert!(snapshots.contains("2 quality snapshots"), "{snapshots}");
    assert!(!snapshots.contains("null"), "{snapshots}");
    let shown = project.supercov(&["quality", "show", "--json"]).json();
    assert!(shown.to_string().contains("src/cart.js"), "{shown}");
    let listed = project.supercov(&["quality", "snapshots", "--json"]).json();
    let first = listed["snapshots"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(listed["snapshots"][0]["counts"]["errors"], 0, "{listed}");

    // A later assessment that finds less reads as an improvement.
    let (clean_base, _) = gateway(MODEL, answer_no);
    project.edit("src/cart.js", "return 0;", "return 0; // free");
    let later = project
        .supercov_with(&["quality", "--json"], &through(&clean_base))
        .json();
    let second = later["id"].as_str().unwrap().to_owned();
    let diff = project
        .supercov(&["quality", "diff", &first, &second])
        .succeeds();
    contains_all(&diff, &["Improved:", "src/cart.js"]);
    let backwards = project
        .supercov(&["quality", "diff", &second, &first])
        .succeeds();
    assert!(backwards.contains("src/cart.js"), "{backwards}");

    let dry_clean = project
        .supercov(&["quality", "clean", "--dry-run"])
        .succeeds();
    contains_all(
        &dry_clean,
        &[
            "Would remove 3 saved quality assessment(s), keeping 0.",
            &first,
        ],
    );
    let kept = project
        .supercov(&["quality", "clean", "--keep", "1"])
        .succeeds();
    assert!(
        kept.contains("Removed 2 saved quality assessment(s), keeping 1."),
        "{kept}"
    );
    let left = project.supercov(&["quality", "snapshots"]).succeeds();
    assert!(left.contains(&second) && !left.contains(&first), "{left}");
    project.supercov(&["quality", "clean"]).succeeds();
    let none = project.supercov(&["quality", "snapshots"]).succeeds();
    assert!(none.contains("No quality snapshots here yet"), "{none}");
}

#[test]
fn assessing_needs_a_key_and_reading_does_not() {
    let project = Project::cart("keyless");
    let refused = project.supercov(&["quality"]);
    assert_ne!(refused.code(), 0);
    assert!(
        refused.stderr().contains("TYPESAFE_API_KEY"),
        "{}",
        refused.stderr()
    );
    let run = project.measure(&[]);
    let assess = project.supercov(&["runs", &run, "assertions", "assess"]);
    assert_ne!(assess.code(), 0);
    assert!(
        assess.stderr().contains("set TYPESAFE_API_KEY"),
        "{}",
        assess.stderr()
    );
    let estimate = project
        .supercov(&["runs", &run, "assertions", "assess", "--dry-run"])
        .succeeds();
    contains_all(
        &estimate,
        &[
            "8 executed statements, 0 answered from the cache",
            "First round: 2 requests",
        ],
    );
}

#[test]
fn quality_patch_reviews_only_what_a_change_introduced() {
    let project = Project::cart("quality-patch");
    let (base, seen) = gateway(MODEL, answer_yes);
    let jev = through(&base);

    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let review = project
        .supercov_with(&["quality", "patch"], &jev)
        .succeeds();
    contains_all(
        &review,
        &[
            "Reviewing unstaged changes.",
            "src/cart.js",
            "0.90  weakens_tests",
            "0.90  debug_leftovers",
        ],
    );
    let annotated = project
        .supercov_with(&["quality", "patch", "--annotate", "github"], &jev)
        .succeeds();
    assert!(
        annotated.contains("::warning file=src/cart.js,line="),
        "{annotated}"
    );
    let requests = seen.lock().unwrap().len();
    assert!(requests >= 1);

    project.git(&["add", "src/cart.js"]);
    let staged = project
        .supercov_with(&["quality", "patch", "--staged"], &jev)
        .succeeds();
    assert!(staged.contains("src/cart.js"), "{staged}");

    // Answers are kept by content: a change Jev finds nothing in says so.
    project.commit("express");
    project.edit("src/cart.js", "return 0;", "return 0.0;");
    project.commit("zero");
    let (quiet, _) = gateway(MODEL, answer_no);
    let committed = project
        .supercov_with(&["quality", "patch", "--base", "HEAD~1"], &through(&quiet))
        .succeeds();
    assert!(!committed.contains("0.90"), "{committed}");
    assert!(committed.contains("introduce"), "{committed}");

    let security = project
        .supercov_with(&["security", "patch", "--base", "HEAD~1"], &jev)
        .succeeds();
    contains_all(&security, &["src/cart.js", "0.90  injection_sink"]);
    assert!(!security.contains("god_class"), "{security}");
}

#[test]
fn security_reads_a_coverage_run_beside_its_findings() {
    let project = Project::cart("security");
    let (base, _) = gateway(MODEL, answer_yes);
    let jev = through(&base);
    project.measure(&[]);

    let flagged = project.supercov_with(&["security"], &jev).succeeds();
    contains_all(
        &flagged,
        &[
            "Security: 1 of 1 files flagged",
            "0.90  secret_in_source  (file-level only)",
        ],
    );
    let gaps = project.supercov(&["security", "gaps"]).succeeds();
    assert!(gaps.contains("weak_cryptography"), "{gaps}");
    let with_run = project
        .supercov_with(&["security", "--run", "latest"], &jev)
        .succeeds();
    contains_all(
        &with_run,
        &[
            "have lines no test in run",
            "3 of its measured lines are not covered by the run",
        ],
    );
    let scope = project.supercov(&["security", "scope"]).succeeds();
    assert!(scope.contains("Roots: src"), "{scope}");
    let file = project
        .supercov(&["security", "file", "src/cart.js"])
        .succeeds();
    assert!(file.contains("injection_sink"), "{file}");
}

#[test]
fn an_assessment_is_saved_reused_and_drives_tests_affected() {
    let project = Project::cart("assess");
    let (base, seen) = gateway(MODEL, answer_yes);
    let jev = through(&base);
    let run = project.measure(&[]);

    let assessed = project
        .supercov_with(&["runs", &run, "assertions", "assess"], &jev)
        .succeeds();
    contains_all(
        &assessed,
        &[
            "100% asserted (8 of 8 executed statements)",
            "Assessed by Jev: 2 requests",
        ],
    );
    let asked = seen.lock().unwrap().len();
    assert_eq!(asked, 2);

    let saved = project.supercov(&["runs", &run, "assertions"]).succeeds();
    assert!(saved.contains("100% asserted (8 of 8"), "{saved}");
    let all = project
        .supercov(&["runs", &run, "assertions", "--all", "--json"])
        .json();
    assert_eq!(all["ok"], true, "{all}");
    let summary = project.supercov(&["runs", &run]).succeeds();
    assert!(
        summary.contains("Assertions   100% (8/8 executed statements a test is judged to catch)"),
        "{summary}"
    );

    // A second run of the same code asks nothing new.
    let again = project.measure(&[]);
    let reused = project
        .supercov_with(&["runs", &again, "assertions", "assess"], &jev)
        .succeeds();
    assert!(reused.contains("100% asserted"), "{reused}");
    assert_eq!(seen.lock().unwrap().len(), asked, "every answer was reused");

    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let changed = project
        .supercov_with(&["runs", &again, "assertions", "assess", "--changed"], &jev)
        .succeeds();
    assert!(
        changed.contains("2 changed statement(s) asked of every test that ran them"),
        "{changed}"
    );
    let affected = project
        .supercov(&["runs", &again, "tests", "affected"])
        .succeeds();
    contains_all(
        &affected,
        &[
            "2 of 2 changed statements the run assessed are asserted by a test",
            "catches a change to src/cart.js:13 (0.90)",
        ],
    );
    let asserting = project
        .supercov(&[
            "runs",
            &again,
            "tests",
            "affected",
            "--asserting",
            "--names",
        ])
        .succeeds();
    assert_eq!(asserting.trim(), "shipping is free over fifty");

    let html = project
        .supercov(&["runs", "report", "--format", "html", "--output", "report"])
        .exits(0);
    assert!(!html.is_empty());
}

#[test]
fn tests_judged_not_to_catch_a_change_are_left_out_when_asked() {
    let project = Project::cart("not-asserted");
    let (base, _) = gateway(MODEL, answer_no);
    let run = project.measure(&[]);
    let assessed = project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    assert!(assessed.contains("0% asserted (0 of 8"), "{assessed}");
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    project
        .supercov_with(
            &["runs", &run, "assertions", "assess", "--changed"],
            &through(&base),
        )
        .succeeds();
    let affected = project
        .supercov(&["runs", &run, "tests", "affected", "--names"])
        .succeeds();
    assert_eq!(affected.trim(), "shipping is free over fifty");
    let asserting = project
        .supercov(&["runs", &run, "tests", "affected", "--asserting", "--names"])
        .succeeds();
    assert_eq!(
        asserting.trim(),
        "",
        "no affected test is judged to catch it"
    );
}
