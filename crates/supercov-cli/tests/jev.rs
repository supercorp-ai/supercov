//! The commands that ask Jev, end to end, against a local server answering
//! the way Jev does: what each sends, what it saves, and what reads that back
//! with no key and no network.

mod common;

use common::{
    Project, answer_no, answer_reviewer, answer_taint, answer_yes, contains_all, gateway,
    gateway_with, through,
};

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
    // The requests are megabytes on a real project; the count and the cost
    // are one line beside them.
    let estimate = project.supercov_with(&["quality", "--dry-run"], &jev);
    assert!(
        estimate
            .stderr()
            .contains("[supercov] quality: 1 requests to 127.0.0.1 (typesafe/jev-1.13), about "),
        "{}",
        estimate.stderr()
    );
    assert!(
        estimate.stderr().contains(") if none is cached"),
        "{}",
        estimate.stderr()
    );

    let first = project.supercov_with(&["quality"], &jev);
    assert!(
        first
            .stderr()
            .contains("[supercov] quality: 100 input tokens this run ($0.0000); 0 more were answered from the cache"),
        "{}",
        first.stderr()
    );
    let assessed = first.succeeds();
    contains_all(
        &assessed,
        &[
            "Quality weak (1.0/10) over 1 file.",
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
        &["1 file with findings", "0.90  complex_conditional"],
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
            "Security: 1 of 1 file flagged",
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
            // Nobody asked the run's tests what they catch, which is not the
            // same as their catching nothing.
            "has no assertion assessment, so whether its tests would catch a change to a flagged line is not reported",
        ],
    );
    assert!(!with_run.contains("judged to catch"), "{with_run}");
    let scope = project.supercov(&["security", "scope"]).succeeds();
    assert!(scope.contains("Roots: src"), "{scope}");
    let file = project
        .supercov(&["security", "file", "src/cart.js"])
        .succeeds();
    assert!(file.contains("injection_sink"), "{file}");
}

#[test]
fn an_assessment_sends_no_more_than_its_estimate_says_it_can() {
    // Three journeys that share most of the steps they run, as end-to-end
    // tests do. A statement goes to one test per round of requests, so a test
    // is asked again about what the others did not settle; the estimate
    // counted one request a test, said "at most 3" here, and the pass sent 5.
    let project = Project::empty("assess-estimate");
    project.write(
        "package.json",
        r#"{ "name": "journeys", "private": true, "type": "module" }"#,
    );
    let steps: String = (0..12)
        .map(|i| format!("export function step{i}(x) {{\n  return x + {i};\n}}\n"))
        .collect();
    project.write("src/steps.js", &steps);
    let journey = |name: usize, steps: &[usize]| {
        let calls: Vec<String> = steps
            .iter()
            .map(|step| format!("steps.step{step}({name})"))
            .collect();
        format!(
            "test(\"journey {name}\", () => {{ {}; assert.ok(true); }});\n",
            calls.join("; ")
        )
    };
    project.write(
        "test/journeys.test.js",
        &format!(
            "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport * as steps from \"../src/steps.js\";\n\n{}{}{}",
            journey(0, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
            journey(1, &[4, 5, 6, 7, 8, 9, 10, 11]),
            journey(2, &[8, 9, 10, 11, 0, 1]),
        ),
    );
    project.git(&["init", "-q"]);
    project.commit("journeys");
    let run = project.measure(&["test/journeys.test.js"]);

    let estimate = project
        .supercov(&["runs", &run, "assertions", "assess", "--dry-run", "--json"])
        .json();
    let most = estimate["data"]["atMost"]["requests"].as_u64().unwrap();
    assert!(
        estimate["data"]["firstRound"]["requests"].as_u64().unwrap() < most,
        "{estimate}"
    );

    // No test is judged to catch anything: the most the pass can ask.
    let (base, seen) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    assert_eq!(seen.lock().unwrap().len() as u64, most, "{estimate}");
    assert_eq!(most, 5, "{estimate}");

    // And every test catches everything: the least, inside the estimate.
    let (base, seen) = gateway(MODEL, answer_yes);
    project
        .supercov_with(
            &["runs", &run, "assertions", "assess", "--refresh"],
            &through(&base),
        )
        .succeeds();
    assert_eq!(
        seen.lock().unwrap().len() as u64,
        estimate["data"]["firstRound"]["requests"].as_u64().unwrap()
    );
}

#[test]
fn a_statement_no_asked_test_catches_says_which_test_files_were_not_asked() {
    // One function forty tests run, in four files. Eighteen are asked at
    // most, so some of the four may never be asked: the statement's page said
    // "18 of 40 asked" and not where the other 22 are.
    let project = Project::empty("not-asked");
    project.write(
        "package.json",
        r#"{ "name": "shared", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/rate.js",
        "export function rate(amount) {\n  return amount * 2;\n}\n",
    );
    let files = ["alpha", "beta", "gamma", "delta"];
    for file in files {
        let tests: String = (0..10)
            .map(|i| format!("test(\"{file} {i}\", () => {{ rate({i}); assert.ok(true); }});\n"))
            .collect();
        project.write(
            &format!("test/{file}.test.js"),
            &format!("import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ rate }} from \"../src/rate.js\";\n\n{tests}"),
        );
    }
    project.git(&["init", "-q"]);
    project.commit("shared");
    let run = project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();

    let view = project
        .supercov(&["runs", &run, "assertions", "src/rate.js:2", "--json"])
        .json();
    let statement = &view["data"]["statements"][0];
    assert_eq!(statement["asserted"], false, "{view}");
    assert_eq!(statement["testsRunningIt"], 40);
    let asked = statement["testsAsked"].as_u64().unwrap();
    assert_eq!(statement["notAsked"]["tests"], 40 - asked, "{statement}");
    let asked_files: std::collections::BTreeSet<&str> = statement["answers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|answer| answer["file"].as_str().unwrap())
        .collect();
    let untouched = statement["notAsked"]["files"].as_array().unwrap();
    // Every test file is either one a test was asked from, or named as not.
    assert_eq!(
        asked_files.len() + untouched.len(),
        files.len(),
        "{statement}"
    );
    for file in untouched {
        assert!(
            !asked_files.contains(file["file"].as_str().unwrap()),
            "{statement}"
        );
        assert_eq!(file["tests"], 10);
    }
    let text = project
        .supercov(&["runs", &run, "assertions", "src/rate.js:2"])
        .succeeds();
    assert_eq!(
        text.contains("No test was asked from"),
        !untouched.is_empty(),
        "{text}"
    );
    assert!(
        text.contains(&format!("Not asked: {} test(s).", 40 - asked)) || untouched.is_empty(),
        "{text}"
    );
}

#[test]
fn a_merged_run_is_assessed_like_the_runs_it_was_made_from() {
    // Suites that cannot run as one command are measured apart and merged.
    // The merged run was refused for holding "multiple source manifests":
    // the same manifest, once per run.
    let project = Project::cart("assess-merged");
    project.write(
        "integration/describe.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { describe } from \"../src/cart.js\";\n\ntest(\"describe names the items\", () => {\n  assert.equal(describe([{ name: \"a\" }, { name: \"b\" }]), \"a, b\");\n});\n",
    );
    project.commit("an integration suite");
    let measure = |kind: &str, file: &str| {
        project
            .supercov_with(
                &["--", "node", "--test", file],
                &[("SUPERCOV_TEST_KIND", kind)],
            )
            .succeeds();
        project.latest()
    };
    let unit = measure("unit", "test/cart.test.js");
    let integration = measure("integration", "integration/describe.test.js");
    project.supercov(&["merge", &unit, &integration]).succeeds();
    let merged = project.latest();
    assert!(merged != unit && merged != integration);

    // What the run's own page says to do next has to work.
    let unassessed = project
        .supercov(&["runs", &merged, "assertions"])
        .succeeds();
    let hint = format!("runs {merged} assertions assess");
    assert!(unassessed.contains(&hint), "{unassessed}");
    project
        .supercov(&["runs", &merged, "assertions", "assess", "--dry-run"])
        .succeeds();

    let (base, _) = gateway(MODEL, answer_yes);
    let assessed = project
        .supercov_with(&["runs", &merged, "assertions", "assess"], &through(&base))
        .succeeds();
    // The unit suite runs 8 statements and the integration suite one more.
    assert!(
        assessed.contains("100% asserted (9 of 9 executed statements)"),
        "{assessed}"
    );
    let by_test = project
        .supercov(&[
            "runs",
            &merged,
            "assertions",
            "--test",
            "describe names the items",
        ])
        .succeeds();
    assert!(by_test.contains("src/cart.js"), "{by_test}");

    // Merging computes nothing a query has not asked for: what each test ran
    // stays with the runs that recorded it, and a merge is answered from them.
    for derived in ["source-manifest.cache.json", "test-executions.json"] {
        assert!(
            !project
                .root
                .join(".supercov/runs")
                .join(&merged)
                .join(derived)
                .exists(),
            "{derived} was written by the merge"
        );
    }
    project.edit("src/cart.js", "join(\", \")", "join(\"; \")");
    let affected = project
        .supercov(&["runs", &merged, "tests", "affected", "--json"])
        .json()["data"]
        .clone();
    assert_eq!(
        affected["mergedFrom"],
        serde_json::json!([unit, integration]),
        "{affected}"
    );
    let names = affected["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|test| test["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, ["describe names the items"], "{affected}");
    assert_eq!(affected["summary"]["tests"], 4, "{affected}");
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
    // 3.0.2 printed this heading's count as a literal `{MORE_TESTS}`.
    assert!(
        saved.contains("Up to 18 of the tests that run a statement are asked"),
        "{saved}"
    );
    assert!(!saved.contains('{'), "{saved}");
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

    // Nor does a run after a dependency or configuration change: no question
    // shows either, so each is the same question. 4.17 keyed every answer on
    // both fingerprints and asked the whole suite again.
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test", "lint": "true" }, "devDependencies": { "left-pad": "1.3.0" } }"#,
    );
    project.write(
        "package-lock.json",
        "{\"lockfileVersion\":3,\"packages\":{}}\n",
    );
    let reconfigured = project.measure(&[]);
    let carried = project
        .supercov_with(&["runs", &reconfigured, "assertions", "assess"], &jev)
        .succeeds();
    assert!(carried.contains("100% asserted"), "{carried}");
    assert_eq!(seen.lock().unwrap().len(), asked, "{carried}");
    // Back as it was: what follows asks which tests a change affects, and a
    // configuration change affects them all.
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" } }"#,
    );
    std::fs::remove_file(project.root.join("package-lock.json")).unwrap();

    // `--refresh` asks every question again instead, for an answer shown to
    // be wrong, and keeps what it gets.
    let refreshed = project
        .supercov_with(&["runs", &again, "assertions", "assess", "--refresh"], &jev)
        .succeeds();
    assert!(refreshed.contains("; 0 answers reused"), "{refreshed}");
    assert_eq!(
        seen.lock().unwrap().len(),
        asked * 2,
        "every question was asked again"
    );

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

const HANDLER: &str = r#"import { exec } from "node:child_process";
import { readFile } from "node:fs/promises";
import { findUser } from "./users.js";

const adminPassword = "correct-horse-battery";

export async function handler(req, res, db) {
  const user = findUser(req.query.id);
  const rows = await db.query("SELECT * FROM orders WHERE user = " + req.query.id);
  exec("convert " + req.query.file + " out.png");
  const page = await readFile("/srv/pages/" + req.params.name, "utf8");
  res.send("<h1>" + req.query.title + "</h1>" + page + rows.length + user.name);
}
"#;

fn shop(label: &str) -> Project {
    let project = Project::empty(label);
    project.write("package.json", r#"{"name":"shop","type":"module"}"#);
    project.write("src/handler.js", HANDLER);
    project.write(
        "src/users.js",
        "export function findUser(id) {\n  return { name: String(id) };\n}\n",
    );
    project
}

#[test]
fn security_confirms_each_finding_at_the_line_that_holds_it() {
    let project = shop("security-lines");
    let (base, _) = gateway(MODEL, answer_reviewer);
    let report = project
        .supercov_with(&["security", "src/handler.js"], &through(&base))
        .succeeds();
    contains_all(
        &report,
        &[
            "Security: 1 of 1 file flagged, 0 clean; 1 confirmed at a line.",
            "line 11  0.90  readFile(\"/srv/pages/\" + req.params.name, \"utf8\")",
            "line 12  0.90  res.send(",
        ],
    );
    // The query and the command are one injection to fix: one finding per
    // check per ten lines, the first of equally strong lines shown.
    assert!(report.contains("line 9  0.90  db.query("), "{report}");
    assert!(!report.contains("line 10  0.90  exec("), "{report}");
    assert!(
        report.contains("0.90  weak_cryptography  (file-level only)"),
        "{report}"
    );
}

#[test]
fn a_file_too_large_for_one_request_is_read_in_windows() {
    let project = Project::empty("windows");
    project.write("package.json", r#"{"name":"big","type":"module"}"#);
    let mut source = String::new();
    for i in 0..1600 {
        source.push_str(&format!(
            "export function f{i}(a, b) {{\n  if (a > {i} && b < {i}) {{\n    return a * b + {i};\n  }}\n  return eval(\"a\" + b);\n}}\n\n"
        ));
    }
    project.write("src/big.js", &source);
    let (base, seen) = gateway(MODEL, answer_yes);
    for command in ["quality", "security"] {
        let report = project
            .supercov_with(&[command, "src/big.js"], &through(&base))
            .succeeds();
        assert!(report.contains("src/big.js"), "{command}: {report}");
    }
    let seen = seen.lock().unwrap();
    let largest = seen
        .iter()
        .filter_map(|(_, _, request)| request["state"]["file"]["source"].as_str())
        .map(str::len)
        .max()
        .unwrap();
    assert!(seen.len() > 10, "{} requests", seen.len());
    assert!(
        largest < source.len() / 2,
        "no request carried the whole {}-byte file ({largest})",
        source.len()
    );
}

#[test]
fn a_busy_gateway_is_waited_for_and_a_broken_one_is_named() {
    let project = shop("faults");
    let quality = |faults: common::Faults| {
        let (base, seen) = gateway_with(MODEL, answer_no, faults);
        let ran = project.supercov_with(&["quality", "src/users.js", "--refresh"], &through(&base));
        let requests = seen.lock().unwrap().len();
        (ran, requests)
    };

    let (busy, requests) = quality(|n| (n == 0).then_some((429, "{}")));
    assert!(busy.succeeds().contains("Quality good"));
    assert_eq!(requests, 2, "asked once more after the 429");

    let (down, requests) = quality(|_| Some((500, "{}")));
    assert!(
        down.exits(2)
            .contains("error  src/users.js  (127.0.0.1 HTTP 500")
    );
    assert_eq!(requests, 4, "three retries, then the failure");

    for (faults, message) in [
        (
            (|_| Some((401, r#"{"error":"bad key"}"#))) as common::Faults,
            "HTTP 401: the key was refused; check TYPESAFE_API_KEY",
        ),
        (
            |_| Some((200, "not json")),
            "returned an invalid response schema",
        ),
        (
            |_| {
                Some((
                    200,
                    r#"{"model":"typesafe/jev-1.13-20260917","answers":{},"usage":{"input_tokens":1,"output_tokens":0}}"#,
                ))
            },
            "response question IDs do not match request",
        ),
    ] {
        let (ran, _) = quality(faults);
        let output = ran.exits(2);
        assert!(output.contains(message), "{message}: {output}");
    }

    // Assessing assertion coverage waits the same way and gives up the same way.
    let cart = Project::cart("assess-faults");
    let run = cart.measure(&[]);
    let (base, _) = gateway_with(MODEL, answer_no, |_| Some((500, "{}")));
    let stopped = cart
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .exits(2);
    assert!(
        stopped.contains("failed requests: 127.0.0.1 HTTP 500"),
        "{stopped}"
    );
    let (base, _) = gateway_with(MODEL, answer_no, |n| (n == 0).then_some((429, "{}")));
    let assessed = cart
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    assert!(assessed.contains("0% asserted (0 of 8"), "{assessed}");
}

#[test]
fn quality_names_a_mistake_before_asking_anything() {
    let project = Project::cart("quality-mistakes");
    project.write("notes.txt", "not source\n");
    let key = [("TYPESAFE_API_KEY", "sk-unused")];
    for (args, message) in [
        (
            vec!["quality", "patch", "--annotate", "gitlab"],
            "--annotate does not know gitlab; use github",
        ),
        (
            vec!["quality", "patch", "--limit", "x"],
            "--limit needs a number",
        ),
        (
            vec!["quality", "clean", "--keep", "1", "--keep", "2"],
            "--keep may only be specified once",
        ),
        (
            vec!["quality", "clean", "--bogus"],
            "unknown quality clean option: --bogus",
        ),
        (
            vec!["quality", "diff"],
            "quality diff requires two snapshots to compare",
        ),
        (
            vec!["quality", "diff", "q_1"],
            "quality diff requires a second snapshot to compare",
        ),
        (vec!["quality", "file"], "quality file requires a file path"),
        (
            vec!["quality", "file", "src/cart.js", "--limit", "3"],
            "quality file shows one thing and takes no --limit",
        ),
        (
            vec!["quality", "gaps", "--bogus"],
            "unknown quality gaps option: --bogus",
        ),
        (
            vec!["quality", "snapshots", "--limit", "x"],
            "--limit requires a row count",
        ),
        (
            vec!["quality", "nosuchdir"],
            // The operating system's own words for a missing path.
            if cfg!(windows) {
                "nosuchdir: The system cannot find the file specified"
            } else {
                "nosuchdir: No such file or directory"
            },
        ),
        (
            vec!["quality", "notes.txt"],
            "unsupported source extension: notes.txt",
        ),
        (vec!["quality", "show"], "no quality snapshot here yet"),
    ] {
        let output = project.supercov_with(&args, &key).exits(2);
        assert!(output.contains(message), "{args:?}: {output}");
    }
}

#[test]
fn a_corrupt_saved_answer_is_named_and_refresh_replaces_it() {
    let project = Project::cart("quality-cache");
    let (base, seen) = gateway(MODEL, answer_no);
    let jev = through(&base);
    project
        .supercov_with(&["quality", "src/cart.js"], &jev)
        .succeeds();
    let requests = project.root.join(".supercov/quality/requests");
    let saved = std::fs::read_dir(&requests)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(saved.len(), 1, "{saved:?}");
    std::fs::write(&saved[0], "{ not json").unwrap();

    let refused = project
        .supercov_with(&["quality", "src/cart.js"], &jev)
        .exits(2);
    assert!(
        refused.contains("invalid quality cache; use --refresh"),
        "{refused}"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "nothing is asked over a corrupt answer"
    );

    let refreshed = project
        .supercov_with(&["quality", "src/cart.js", "--refresh"], &jev)
        .succeeds();
    assert!(refreshed.contains("Quality good"), "{refreshed}");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(std::fs::read_to_string(&saved[0]).unwrap().starts_with('{'));
}

const SHAPES: &str = "#[derive(Debug, Clone, Copy, PartialEq)]\npub enum Size {\n    Small,\n    Large,\n}\n\n#[derive(Debug, PartialEq)]\npub struct Box2 {\n    pub width: u32,\n    pub height: u32,\n}\n\npub const LIMIT: u32 = 100;\n\npub fn size(area: u32) -> Size {\n    if area > LIMIT {\n        return Size::Large;\n    }\n    Size::Small\n}\n\npub fn parse(text: &str) -> Result<u32, String> {\n    let value = text.trim().parse::<u32>().map_err(|e| e.to_string())?;\n    Ok(value)\n}\n\npub fn first_even(values: &[u32]) -> Option<u32> {\n    values.iter().copied().find(|v| v % 2 == 0)\n}\n\npub fn grow(b: &Box2) -> Box2 {\n    Box2 { width: b.width * 2, height: b.height }\n}\n\npub fn tagged(name: &str) -> (bool, String) {\n    let label = name.to_uppercase();\n    (label.is_empty(), label)\n}\n\npub fn scale() -> impl Fn(u32) -> u32 {\n    |x| x * 3\n}\n";

const SHAPES_TESTS: &str = "use shapes::*;\n\n#[test]\nfn sizes() {\n    assert_eq!(size(200), Size::Large);\n    assert_eq!(size(10), Size::Small);\n}\n\n#[test]\nfn parses() {\n    assert_eq!(parse(\" 7 \"), Ok(7));\n    assert!(parse(\"x\").is_err());\n}\n\n#[test]\nfn evens_and_boxes() {\n    assert_eq!(first_even(&[1, 4]), Some(4));\n    assert_eq!(grow(&Box2 { width: 1, height: 2 }), Box2 { width: 2, height: 2 });\n    assert_eq!(tagged(\"ab\"), (false, \"AB\".to_string()));\n    assert_eq!(scale()(2), 6);\n}\n";

#[test]
fn rust_statements_are_asked_as_changes_of_their_own_type() {
    let project = Project::empty("rust-changes");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write("src/lib.rs", SHAPES);
    project.write("tests/shapes.rs", SHAPES_TESTS);
    project.git(&["init", "-q"]);
    project.supercov(&["--", "cargo", "test", "-q"]).succeeds();
    let (base, seen) = gateway(MODEL, answer_no);
    let assessed = project
        .supercov_with(&["runs", "latest", "assertions", "assess"], &through(&base))
        .succeeds();
    assert!(
        assessed.contains("0% asserted (0 of 10 executed statements)"),
        "{assessed}"
    );
    let asked = seen
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, _, request)| {
            request["questions"]
                .as_object()
                .unwrap()
                .values()
                .filter_map(|question| question["instructions"]["task"].as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for change in [
        "`return Size::Large;` becomes `return Size::Small;`",
        "`Size::Small` becomes `Size::Large`",
        "`Ok(value)` becomes `Err(String::new())`",
        "`values.iter().copied().find(|v| v % 2 == 0)` becomes `None`",
        "`Box2 { width: b.width * 2, height: b.height }` becomes `Box2 { width: 0, height: b.height }`",
        "`(label.is_empty(), label)` becomes `(!(label.is_empty()), label)`",
        // A closure returned as `impl Fn(u32) -> u32` returns another u32.
        "`|x| x * 3` becomes `|x| 0`",
        "whenever the condition `area > LIMIT` is true it is treated as false",
    ] {
        assert!(asked.contains(change), "{change} not asked:\n{asked}");
    }
}

#[test]
fn security_follows_outside_data_across_an_import() {
    let project = Project::empty("security-paths");
    project.write("package.json", r#"{"name":"orders","type":"module"}"#);
    project.write(
        "src/handler.js",
        "import { findOrders } from \"./db.js\";\n\nexport async function handler(req, db) {\n  const rows = await findOrders(db, req.query.customer);\n  return { count: rows.length };\n}\n",
    );
    project.write(
        "src/db.js",
        "export function findOrders(db, customer) {\n  return db.query(\"SELECT * FROM orders WHERE customer = '\" + customer + \"'\");\n}\n",
    );
    // `route` is handed `findOrders` rather than importing it: only a test
    // that runs both shows the path.
    project.write(
        "src/router.js",
        "export async function route(req, deps) {\n  return deps.findOrders(deps.db, req.params.id);\n}\n",
    );
    project.write(
        "test/router.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { route } from \"../src/router.js\";\nimport { findOrders } from \"../src/db.js\";\n\ntest(\"routes to the orders query\", async () => {\n  const db = { query: async () => [1] };\n  assert.deepEqual(await route({ params: { id: \"7\" } }, { db, findOrders }), [1]);\n});\n",
    );
    project.write(
        "test/handler.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { handler } from \"../src/handler.js\";\n\ntest(\"counts a customer's orders\", async () => {\n  const db = { query: async () => [1, 2] };\n  assert.deepEqual(await handler({ query: { customer: \"ann\" } }, db), { count: 2 });\n});\n",
    );
    project.git(&["init", "-q"]);
    project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_taint);
    let report = project
        .supercov_with(&["security", "src/", "--run", "latest"], &through(&base))
        .succeeds();
    contains_all(
        &report,
        &[
            "Cross-file paths confirmed: 2",
            "injection_sink  src/handler.js:4 handler -> src/db.js:1 findOrders",
            "injection_sink (observed in a test)  src/router.js:1 route -> src/db.js:1 findOrders",
        ],
    );
    let asked_path = seen
        .lock()
        .unwrap()
        .iter()
        .any(|(_, _, request)| request["questions"].get("path").is_some());
    assert!(asked_path, "the pair was confirmed with one question");
}

#[test]
fn a_patch_review_reads_only_changed_source_and_says_what_it_skipped() {
    let project = Project::cart("patch-edges");
    project.write("src/extra.js", "export const e = (a) => a + 1;\n");
    project.write("src/old.js", "export const o = 1;\n");
    project.commit("more");
    let (base, _) = gateway(MODEL, answer_no);
    let jev = through(&base);

    let nothing = project
        .supercov_with(&["quality", "patch"], &jev)
        .succeeds();
    assert!(
        nothing.contains("No changed source files to review."),
        "{nothing}"
    );
    // An assessment never shows up as a change of its own.
    assert_eq!(
        project.git(&["status", "--porcelain"]),
        "",
        "the store is ignored"
    );

    project.git(&["rm", "-q", "src/old.js"]);
    project.git(&["mv", "src/extra.js", "src/renamed.js"]);
    project.write("README.md", "# cart\n");
    project.write("dist/bundle.js", "var x=1;\n");
    project.edit("test/cart.test.js", "20);", "20 );");
    project.edit("src/cart.js", "return 0;", "return 0 ;");
    let unstaged = project
        .supercov_with(&["quality", "patch"], &jev)
        .succeeds();
    contains_all(
        &unstaged,
        &[
            "Reviewing unstaged changes.",
            "Nothing introduced across 1 changed files.",
            "3 changed files not reviewed.",
        ],
    );

    project.commit("changes");
    let reviewed = project
        .supercov_with(&["quality", "patch", "--base", "HEAD~1", "--json"], &jev)
        .json();
    let files = reviewed["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(files, ["src/cart.js", "src/renamed.js"]);
    let skipped = reviewed["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().to_owned(),
                file["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        skipped,
        [
            ("README.md", "not a source file"),
            ("dist/bundle.js", "unclassified first-party source"),
            ("src/extra.js", "a deletion cannot introduce anything"),
            ("src/old.js", "a deletion cannot introduce anything"),
            ("test/cart.test.js", "test"),
        ]
        .map(|(path, reason)| (path.to_owned(), reason.to_owned()))
    );
}

/// Jev judges every test to catch every change but one to `let sum = 0`.
fn answer_all_but_the_sum(id: &str, question: &serde_json::Value) -> serde_json::Value {
    if question["instructions"]["task"]
        .as_str()
        .is_some_and(|task| task.contains("let sum = 0"))
    {
        answer_no(id, question)
    } else {
        answer_yes(id, question)
    }
}

#[test]
fn a_saved_assessment_reads_from_its_files_down_to_one_statement_and_one_test() {
    let project = Project::cart("assertion-views");
    let (base, _) = gateway(MODEL, answer_all_but_the_sum);
    let run = project.measure(&[]);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();

    // From the run: the files, with the command for the next step.
    let files = project.supercov(&["runs", &run, "assertions"]).succeeds();
    contains_all(
        &files,
        &[
            " NOT ASSERTED  ASSERTED  FILE",
            "            1       7/8  src/cart.js",
            "showing 1-1 of 1",
            "assertions 'src/cart.js'",
        ],
    );
    // A file: what is not asserted in it, and the test that came closest.
    let file = project
        .supercov(&["runs", &run, "assertions", "src/cart.js"])
        .succeeds();
    contains_all(
        &file,
        &[
            "src/cart.js: 87.5% asserted (7 of 8 statements)",
            "    2  let sum = 0;  (value becomes undefined)",
            "assertions 'src/cart.js:2'",
        ],
    );
    // One statement: every asked test's answer.
    let statement = project
        .supercov(&["runs", &run, "assertions", "src/cart.js:2"])
        .succeeds();
    contains_all(
        &statement,
        &["Not asserted:", "would pass   total adds prices"],
    );
    assert!(!statement.contains("would fail"), "{statement}");
    // One test: what it was asked about, caught or not. A statement one test
    // catches is not asked of the rest, so this one was asked only the sum.
    let test = project
        .supercov(&["runs", &run, "assertions", "--test", "total adds prices"])
        .succeeds();
    contains_all(
        &test,
        &[
            "judged to catch a change to 0 of the 1 statements it was asked about",
            "Would not catch\n  src/cart.js:2  let sum = 0;",
        ],
    );
    // Paged in JSON as in text.
    let page = project
        .supercov(&["runs", &run, "assertions", "--limit", "1", "--json"])
        .json();
    assert_eq!(page["data"]["page"]["returned"], 1, "{page}");
    assert_eq!(page["data"]["page"]["nextOffset"], serde_json::Value::Null);
    // The coverage views say the same beside each file's and line's coverage.
    let file_view = project
        .supercov(&["runs", &run, "file", "src/cart.js"])
        .succeeds();
    contains_all(
        &file_view,
        &[
            "Assertions  87.5% (7/8 statements a test is judged to catch)",
            "  Not asserted on line(s) 2\n",
            "assertions 'src/cart.js'",
        ],
    );
    let line_view = project
        .supercov(&["runs", &run, "line", "src/cart.js:2"])
        .succeeds();
    contains_all(
        &line_view,
        &[
            "  Not asserted: `let sum = 0;`",
            "assertions 'src/cart.js:2'",
        ],
    );
    // Names that match nothing, or several, say so.
    let missing = project
        .supercov(&["runs", &run, "assertions", "src/none.js"])
        .exits(2);
    assert!(
        missing.contains("no assessed statement in src/none.js"),
        "{missing}"
    );
    let several = project
        .supercov(&["runs", &run, "assertions", "--test", "total"])
        .exits(2);
    assert!(several.contains("assessed tests match total"), "{several}");
}

/// A probability above one, which no model can mean.
fn answer_beyond_one(id: &str, question: &serde_json::Value) -> serde_json::Value {
    if question["type"] == "noul" {
        serde_json::json!({ "type": "noul", "noul": 1.5 })
    } else {
        answer_no(id, question)
    }
}

/// A choice whose probabilities do not add up to one.
fn answer_lopsided(id: &str, question: &serde_json::Value) -> serde_json::Value {
    if question["type"] == "choice" {
        let options = question["criteria"].as_object().unwrap();
        let probabilities = options
            .keys()
            .map(|option| (option.clone(), serde_json::json!(0.9)))
            .collect::<serde_json::Map<_, _>>();
        let chosen = options.keys().next().unwrap().clone();
        serde_json::json!({ "type": "choice", "choice": chosen, "probabilities": probabilities, "confidence": 1.0 })
    } else {
        answer_no(id, question)
    }
}

#[test]
fn a_large_assessment_reads_its_tree_and_names_every_failure() {
    let project = Project::empty("many");
    project.write("package.json", r#"{"name":"many","type":"module"}"#);
    for i in 0..12 {
        project.write(
            &format!("src/m{i}.js"),
            &format!("export const v{i} = (a) => a + {i};\n"),
        );
    }
    project.write("tools/gen.js", "export const g = 1;\n");
    project.write("scripts/build.js", "export const b = 2;\n");
    project.git(&["init", "-q"]);

    // A file under no source root is put to the model, by path only, before
    // anything is assessed; the one it reads as shipping is assessed too.
    let (base, seen) = gateway(MODEL, answer_yes);
    let assessed = project
        .supercov_with(&["quality"], &through(&base))
        .succeeds();
    contains_all(
        &assessed,
        &[
            "Quality weak (1.0/10) over 13 files.",
            "1 files under no source root were read by the model from the tree: 1 ship, 0 do not.",
        ],
    );
    let first = seen.lock().unwrap()[0].2.clone();
    assert_eq!(first["questions"].as_object().unwrap().len(), 1);
    assert!(
        first["questions"]["f0"]["instructions"]["task"]
            .as_str()
            .unwrap()
            .contains("tools/gen.js")
    );

    let (down, _) = gateway_with(MODEL, answer_no, |_| Some((500, "{}")));
    let failed = project
        .supercov_with(&["quality", "src/", "--refresh"], &through(&down))
        .exits(2);
    contains_all(
        &failed,
        &[
            "error  src/m0.js  (127.0.0.1 HTTP 500",
            "... and 7 more errors",
        ],
    );

    let (refused, _) = gateway_with(MODEL, answer_no, |_| Some((413, "{}")));
    let large = project
        .supercov_with(&["quality", "src/m1.js", "--refresh"], &through(&refused))
        .exits(2);
    assert!(large.contains("HTTP 413: the request was refused, most often because it is larger than the model accepts"), "{large}");

    // Security asks choices as well as yes/no questions; both are checked.
    for answerer in [answer_beyond_one as common::Answerer, answer_lopsided] {
        let (base, _) = gateway(MODEL, answerer);
        let invalid = project
            .supercov_with(&["security", "src/m2.js", "--refresh"], &through(&base))
            .exits(2);
        assert!(invalid.contains("invalid answer for "), "{invalid}");
    }
}

#[test]
fn security_beside_an_assessed_run_says_which_flagged_lines_a_test_catches() {
    let project = shop("security-asserted");
    // The page read fails, so the test reaches every line but the reply.
    project.write(
        "test/handler.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { handler } from \"../src/handler.js\";\n\ntest(\"a missing page is refused\", async () => {\n  const db = { query: async () => [] };\n  const req = { query: { id: \"1\", file: \"a\", title: \"t\" }, params: { name: \"no-such-page\" } };\n  await assert.rejects(handler(req, { send() {} }, db), /ENOENT/);\n});\n",
    );
    project.write(
        "scripts/deploy.js",
        "import { exec } from \"node:child_process\";\nexec(\"deploy \" + process.argv[2]);\n",
    );
    project.git(&["init", "-q"]);
    let run = project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();

    let (base, _) = gateway(MODEL, answer_reviewer);
    let report = project
        .supercov_with(
            &[
                "security",
                "src/handler.js",
                "scripts/deploy.js",
                "--run",
                "latest",
            ],
            &through(&base),
        )
        .succeeds();
    contains_all(
        &report,
        &[
            "scripts/deploy.js",
            "the run did not measure this file",
            // The run left only the reply uncovered.
            "and 1 of its measured lines are not covered by the run",
            "lines are asserted (a test is judged to catch a change to them)",
            "line 9: asserted; a test is judged to catch a change to it",
            "line 11: asserted; a test is judged to catch a change to it",
        ],
    );
    assert!(!report.contains("line 12: asserted"), "{report}");
}

const MORE_SHAPES: &str = r#"use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn unit() -> Point {
        Point::origin().shifted(1)
    }

    pub fn origin() -> Self {
        Self { x: 0, y: 0 }
    }

    pub fn shifted(self, by: i32) -> Self {
        Self { x: self.x + by, ..self }
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

pub fn raw(value: &mut u8) -> *mut u8 {
    value as *mut u8
}

pub fn raw_const(value: &u8) -> *const u8 {
    value as *const u8
}

pub fn order(a: f64, b: f64) -> Option<Ordering> {
    a.partial_cmp(&b)
}

pub fn initial(name: &str) -> char {
    name.chars().next().unwrap_or('?')
}

pub fn pair(a: u8, b: bool) -> (u8, bool) {
    (a + 1, !b)
}

pub fn filled(n: usize) -> Vec<u8> {
    vec![7; n]
}

pub fn label(n: u32) -> String {
    let word = match n {
        0 => "zero",
        1 => "one",
        _ => "many",
    };
    word.to_string()
}

pub fn pick(flag: bool) -> u16 {
    let value = if flag { 10 } else { 20 };
    value
}

pub fn lookup(map: &std::collections::HashMap<String, u32>, key: &str) -> Option<u32> {
    map.get(key).copied()
}
"#;

const MORE_SHAPES_TESTS: &str = r#"use more::*;

#[test]
fn points() {
    assert_eq!(Point::origin().shifted(2), Point { x: 2, y: 0 });
    assert_eq!(Point::unit(), Point { x: 1, y: 0 });
    assert_eq!(Point { x: 1, y: 2 }.to_string(), "(1, 2)");
}

#[test]
fn pointers_and_values() {
    let mut v = 3u8;
    assert!(!raw(&mut v).is_null());
    assert!(!raw_const(&v).is_null());
    assert_eq!(order(1.0, 2.0), Some(std::cmp::Ordering::Less));
    assert_eq!(initial("ann"), 'a');
    assert_eq!(pair(1, true), (2, false));
    assert_eq!(filled(2), vec![7, 7]);
    assert_eq!(label(1), "one");
    assert_eq!(pick(true), 10);
    let map = std::collections::HashMap::from([("a".to_string(), 1)]);
    assert_eq!(lookup(&map, "a"), Some(1));
}
"#;

#[test]
fn rust_values_of_every_shape_are_asked_as_changes_of_their_own_type() {
    let project = Project::empty("rust-more-changes");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"more\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write("src/lib.rs", MORE_SHAPES);
    project.write("tests/more.rs", MORE_SHAPES_TESTS);
    project.git(&["init", "-q"]);
    project.supercov(&["--", "cargo", "test", "-q"]).succeeds();
    let (base, seen) = gateway(MODEL, answer_no);
    let assessed = project
        .supercov_with(&["runs", "latest", "assertions", "assess"], &through(&base))
        .succeeds();
    let asked = seen
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, _, request)| {
            request["questions"]
                .as_object()
                .unwrap()
                .values()
                .filter_map(|question| question["instructions"]["task"].as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        assessed.contains("0% asserted (0 of 15 executed statements)"),
        "{assessed}"
    );
    // `unit` is never asked as returning `Point::unit()`, a call to itself,
    // and `Self { .. }` changes a field as `Point { .. }` would.
    for change in [
        "`Point::origin().shifted(1)` becomes `Point::origin()`",
        "`Self { x: 0, y: 0 }` becomes `Point { x: 1, y: 0 }`",
        "`Self { x: self.x + by, ..self }` becomes `Point { x: 0, ..self }`",
        "`write!(f, \"({}, {})\", self.x, self.y)` becomes `Err(std::fmt::Error)`",
        "`value as *mut u8` becomes `std::ptr::null_mut()`",
        "`value as *const u8` becomes `std::ptr::null()`",
        "`a.partial_cmp(&b)` becomes `None`",
        "`name.chars().next().unwrap_or('?')` becomes `'\\0'`",
        "`(a + 1, !b)` becomes `(0, !b)`",
        "`vec![7; n]` becomes `Vec::new()`",
        "`let word = match n {` becomes `let word = \"\";`",
        "`word.to_string()` becomes `String::new()`",
        "`10` becomes `0`",
        "`value` becomes `0`",
        "`map.get(key).copied()` becomes `None`",
    ] {
        assert!(asked.contains(change), "{change} not asked:\n{asked}");
    }
}

#[test]
fn tests_affected_and_assertions_name_a_mistake_and_estimate_a_change() {
    let project = Project::cart("affected-mistakes");
    let run = project.measure(&[]);
    let on_run = |args: &[&str]| {
        let mut all = vec!["runs", run.as_str()];
        all.extend_from_slice(args);
        project.supercov(&all)
    };
    let help = on_run(&["tests", "affected", "--help"]).succeeds();
    assert!(
        help.starts_with("Usage: supercov runs <run> tests affected"),
        "{help}"
    );
    for (args, message) in [
        (
            vec!["tests", "affected", "--json", "--names"],
            "Choose one of --json, --names or --files",
        ),
        (
            vec!["tests", "affected", "--asserting"],
            "--asserting and --ran-changed narrow --names or --files",
        ),
        (
            vec![
                "tests",
                "affected",
                "--ran-changed",
                "--asserting",
                "--names",
            ],
            "Choose one of --ran-changed or --asserting",
        ),
        (
            vec!["tests", "affected", "--names", "--names"],
            "Duplicate option: --names",
        ),
        (
            vec!["assertions", "--changed"],
            "--changed belongs to `assertions assess`",
        ),
        (
            vec!["assertions", "assess", "--frob"],
            "Unknown option: --frob",
        ),
        // A change is assessed against sources an assessment kept.
        (
            vec!["assertions", "assess", "--changed", "--dry-run"],
            "this run's sources were not kept; assess the run first",
        ),
    ] {
        let refused = on_run(&args).exits(2);
        assert!(refused.contains(message), "{args:?}: {refused}");
    }
    let unchanged = on_run(&["tests", "affected"]).succeeds();
    contains_all(
        &unchanged,
        &[
            "0 of 3 tests affected",
            "No captured file has changed.",
            "Working tree: matches the run.",
        ],
    );
    let json = on_run(&["tests", "affected", "--json"]).json();
    assert_eq!(json["data"]["summary"]["unaffected"], 3, "{json}");

    let (base, _) = gateway(MODEL, answer_yes);
    let mut assess = vec!["runs", run.as_str(), "assertions", "assess"];
    project.supercov_with(&assess, &through(&base)).succeeds();
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    assess.extend(["--changed", "--dry-run"]);
    let estimate = project.supercov(&assess).succeeds();
    assert!(
        estimate.contains("2 changed statement(s), 0 answers in the cache. 1 requests"),
        "{estimate}"
    );
    assess.pop();
    let keyless = project.supercov(&assess).exits(2);
    assert!(
        keyless.contains("set TYPESAFE_API_KEY to ask Jev"),
        "{keyless}"
    );
}

#[test]
fn a_statement_no_asked_test_catches_is_asked_of_the_tests_named_for_it() {
    // Forty-six tests run the return. Fifteen are asked in the usual order,
    // one per file and describe block; the test whose name shares a word with
    // the line ("sum") is the last of its block and is not among them.
    let project = Project::empty("last-tests");
    project.write("package.json", r#"{"name":"last","type":"module"}"#);
    project.write(
        "src/fee.js",
        "export function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\n",
    );
    for file in ["fee", "checkout", "refund"] {
        let mut body = String::from(
            "import { describe, test } from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { fee } from \"../src/fee.js\";\n\n",
        );
        for group in ["small", "large", "edge"] {
            body.push_str(&format!("describe(\"{group}\", () => {{\n"));
            for n in 0..5 {
                body.push_str(&format!(
                    "  test(\"{file} {group} {n}\", () => assert.equal(fee({}), {}));\n",
                    n * 50,
                    if n * 50 > 100 { 0 } else { 5 }
                ));
            }
            if file == "refund" && group == "edge" {
                body.push_str(
                    "  test(\"zero for a sum over the limit\", () => assert.equal(fee(500), 0));\n",
                );
            }
            body.push_str("});\n");
        }
        project.write(&format!("test/{file}.test.js"), &body);
    }
    project.git(&["init", "-q"]);
    let run = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let names = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, _, request)| request["state"]["test"]["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 18, "fifteen, then three more: {names:?}");
    // By the usual order it would be the last of the forty-six.
    assert!(
        names[15..]
            .iter()
            .any(|name| name == "edge > zero for a sum over the limit"),
        "the test named for the line is among the last three: {names:?}"
    );
    let statement = project
        .supercov(&["runs", &run, "assertions", "src/fee.js:2"])
        .succeeds();
    assert!(
        statement.contains("Not asserted: 46 test(s) run it, 18 asked"),
        "{statement}"
    );
}

#[test]
fn a_change_more_tests_ran_than_are_asked_says_so() {
    let project = Project::empty("many-tests");
    project.write("package.json", r#"{"name":"many","type":"module"}"#);
    project.write(
        "src/fee.js",
        "export function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\n",
    );
    for file in ["fee", "checkout", "refund"] {
        let mut body = String::from(
            "import { describe, test } from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { fee } from \"../src/fee.js\";\n\n",
        );
        for group in ["small", "large", "edge"] {
            body.push_str(&format!("describe(\"{group}\", () => {{\n"));
            for n in 0..5 {
                body.push_str(&format!(
                    "  test(\"{file} {group} {n}\", () => assert.equal(fee({}), {}));\n",
                    n * 50,
                    if n * 50 > 100 { 0 } else { 5 }
                ));
            }
            body.push_str("});\n");
        }
        project.write(&format!("test/{file}.test.js"), &body);
    }
    project.git(&["init", "-q"]);
    let run = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let before = seen.lock().unwrap().len();
    project.edit("src/fee.js", "? 0 : 5", "? 0 : 6");
    let changed = project
        .supercov_with(
            &["runs", &run, "assertions", "assess", "--changed"],
            &through(&base),
        )
        .succeeds();
    assert!(
        changed.contains("asked of the tests that ran them, 40 at most each (1 ran under more)"),
        "{changed}"
    );
    // Forty of the 45 are asked, every test file and describe block among them.
    let asked = seen.lock().unwrap()[before..]
        .iter()
        .filter_map(|(_, _, request)| request["state"]["test"]["file"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(
        asked.len(),
        39,
        "one of the forty was answered before the change"
    );
    for file in [
        "test/fee.test.js",
        "test/checkout.test.js",
        "test/refund.test.js",
    ] {
        assert!(
            asked.iter().any(|asked| asked == file),
            "{file} not asked: {asked:?}"
        );
    }
    let names = seen.lock().unwrap()[before..]
        .iter()
        .filter_map(|(_, _, request)| request["state"]["test"]["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    // node:test names carry their describe block, so each block of each file
    // has a test among the forty.
    for file in ["fee", "checkout", "refund"] {
        for group in ["small", "large", "edge"] {
            let prefix = format!("{group} > {file} {group} ");
            assert!(
                names.iter().any(|name| name.starts_with(&prefix)),
                "no {prefix:?} test asked: {names:?}"
            );
        }
    }
}

#[test]
fn a_javascript_request_shows_the_function_a_statement_is_in_and_no_helper() {
    // What a request held was mostly the code the test ran, all of it in
    // every file a question was about, and the files the test imports. Cut to
    // the function each asked statement is in, with no helper, a request is
    // 28 to 81% of the tokens and as accurate (eight projects, 2026-10-13).
    let project = Project::empty("within-the-function");
    project.write("package.json", r#"{ "name": "cart", "type": "module" }"#);
    project.write(
        "src/cart.js",
        "export function total(items) {\n  const sum = items.reduce((all, item) => all + item.price, 0);\n  return sum;\n}\n\nexport function shipping(weight) {\n  const rate = weight > 10 ? 9 : 4;\n  return rate;\n}\n\nexport function describe(order) {\n  const lines = order.items.map((item) => item.name);\n  const head = `order ${order.id}`;\n  const body = lines.join(\", \");\n  const tail = `to ${order.city}`;\n  return [head, body, tail].join(\"; \");\n}\n",
    );
    project.write(
        "test/fixtures.js",
        "export const items = [{ price: 2, name: \"tea\" }, { price: 3, name: \"cup\" }]; // the fixture\n",
    );
    // The second test runs more, so it is asked first and about `shipping`;
    // the first is then asked about `total` alone, though it ran both.
    project.write(
        "test/cart.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total, shipping, describe } from \"../src/cart.js\";\nimport { items } from \"./fixtures.js\";\n\ntest(\"totals and ships\", () => {\n  assert.equal(total(items), 5);\n  assert.equal(shipping(12), 9);\n});\n\ntest(\"ships and describes\", () => {\n  assert.equal(shipping(2), 4);\n  assert.equal(describe({ id: 7, city: \"Kaunas\", items }), \"order 7; tea, cup; to Kaunas\");\n});\n",
    );
    project.git(&["init", "-q"]);
    let run = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let seen = seen.lock().unwrap();
    let request = seen
        .iter()
        .find(|(_, _, request)| request["state"]["test"]["name"] == "totals and ships")
        .map(|(_, _, request)| request.clone())
        .expect("the first test was asked about");
    let state = &request["state"];
    let shown = state["code_run"]["src/cart.js"]
        .as_str()
        .expect("the file the questions are about");
    assert!(shown.contains("const sum = items.reduce"), "{shown}");
    // It ran `shipping` too, and is asked nothing about it.
    assert!(!shown.contains("const rate"), "{shown}");
    assert!(shown.ends_with("lines not shown"), "{shown}");
    assert!(
        request["questions"]
            .as_object()
            .unwrap()
            .values()
            .all(|question| question["instructions"]["task"]
                .as_str()
                .is_some_and(
                    |task| task.starts_with("src/cart.js:2:") || task.starts_with("src/cart.js:3:")
                )),
        "{request}"
    );
    // Nor is the file the test imports sent with it.
    assert!(state.get("helpers").is_none(), "{state}");
    assert!(
        state["test_code"]
            .as_str()
            .is_some_and(|code| code.contains("totals and ships")),
        "{state}"
    );
}

/// A body larger than any answer Supercov accepts.
fn oversized(_: usize) -> Option<(u16, &'static str)> {
    static BODY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    Some((200, BODY.get_or_init(|| "x".repeat(1_048_577)).as_str()))
}

#[test]
fn what_quality_cannot_read_or_was_asked_twice_is_named() {
    let project = Project::cart("quality-limits");
    let key = [("TYPESAFE_API_KEY", "sk-unused")];
    for (args, message) in [
        (
            vec!["quality", "snapshots", "--limit", "1", "--limit", "2"],
            "--limit may only be specified once",
        ),
        (
            vec![
                "quality",
                "src/cart.js",
                "--context",
                "src/cart.js",
                "--context",
                "src/cart.js",
            ],
            "--context may only be specified once",
        ),
    ] {
        let output = project.supercov_with(&args, &key).exits(2);
        assert!(output.contains(message), "{args:?}: {output}");
    }

    // A source over the size limit is left out, and says why.
    project.write(
        "src/huge.js",
        &format!("export const big = {:?};\n", "x".repeat(4 << 20)),
    );
    let (base, seen) = gateway(MODEL, answer_no);
    let huge = project
        .supercov_with(&["quality", "src/huge.js"], &through(&base))
        .exits(2);
    assert!(
        huge.contains("larger than 4194304 bytes and is not assessed"),
        "{huge}"
    );
    assert_eq!(seen.lock().unwrap().len(), 0, "nothing is sent for it");

    // An answer over the response limit is refused.
    let (base, _) = gateway_with(MODEL, answer_no, oversized);
    let flooded = project
        .supercov_with(&["quality", "src/cart.js", "--refresh"], &through(&base))
        .exits(2);
    assert!(
        flooded.contains("127.0.0.1 response exceeds size limit"),
        "{flooded}"
    );

    // A gateway at an IPv6 address is named by it, brackets and all.
    let unreachable = project
        .supercov_with(
            &["quality", "src/cart.js", "--refresh"],
            &[
                ("TYPESAFE_BASE_URL", "http://[::1]:1/api"),
                ("TYPESAFE_DEFAULT_MODEL", "typesafe/jev-1.13"),
                ("TYPESAFE_API_KEY", "sk-gateway"),
            ],
        )
        .exits(2);
    assert!(unreachable.contains("[::1]"), "{unreachable}");
}

#[test]
fn a_saved_answer_to_another_question_is_not_used() {
    let project = Project::cart("quality-stale-cache");
    let (base, seen) = gateway(MODEL, answer_no);
    let jev = through(&base);
    project
        .supercov_with(&["quality", "src/cart.js"], &jev)
        .succeeds();
    let requests = project.root.join(".supercov/quality/requests");
    let saved = std::fs::read_dir(&requests)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    let mut entry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&saved[0]).unwrap()).unwrap();
    entry["request_hash"] = "another question".into();
    std::fs::write(&saved[0], serde_json::to_vec(&entry).unwrap()).unwrap();
    let refused = project
        .supercov_with(&["quality", "src/cart.js"], &jev)
        .exits(2);
    assert!(
        refused.contains("quality cache hash mismatch; use --refresh"),
        "{refused}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_patch_review_needs_a_repository() {
    let project = Project::empty("quality-no-repository");
    project.write("src/a.js", "export const a = 1;\n");
    for lane in ["quality", "security"] {
        let output = project
            .supercov_with(&[lane, "patch"], &[("TYPESAFE_API_KEY", "sk-unused")])
            .exits(2);
        assert!(
            output.contains(&format!(
                "{lane} patch needs a Git repository; run it inside one"
            )),
            "{output}"
        );
    }
}

#[test]
fn security_lists_five_failures_and_counts_the_rest() {
    let project = Project::empty("security-failures");
    project.write(
        "package.json",
        r#"{ "name": "many", "private": true, "type": "module" }"#,
    );
    for index in 0..7 {
        project.write(
            &format!("src/handler{index}.js"),
            &format!(
                "export function handle{index}(request) {{\n  return request.query.name;\n}}\n"
            ),
        );
    }
    project.git(&["init", "-q"]);
    project.commit("handlers");
    let (base, _) = gateway_with(MODEL, answer_taint, |_| Some((500, "{}")));
    let output = project
        .supercov_with(&["security", "src"], &through(&base))
        .exits(2);
    assert!(output.contains("... and 2 more errors"), "{output}");
}

#[test]
fn generated_files_are_left_out_unless_all_are_asked_for() {
    let project = Project::cart("quality-generated");
    for (file, text) in [
        ("src/types.d.mts", "export declare const a: number;\n"),
        ("src/types.d.cts", "export declare const b: number;\n"),
        ("src/message.pb.cc", "int message() { return 1; }\n"),
        ("src/message.pb.h", "int message();\n"),
        ("src/service_pb2_grpc.py", "def call():\n    return 1\n"),
        ("src/Form.g.cs", "class Form { int Size() { return 1; } }\n"),
        (
            "src/client.js",
            "// Code generated by openapi-generator. DO NOT EDIT.\nexport function fetchUser(id) {\n  return id ? { id } : null;\n}\n",
        ),
    ] {
        project.write(file, text);
    }
    let (base, seen) = gateway(MODEL, answer_no);
    let jev = through(&base);
    let default = project.supercov_with(&["quality", "src"], &jev).succeeds();
    let asked_by_default = seen.lock().unwrap().len();
    assert!(!default.contains("client.js"), "{default}");
    let all = project
        .supercov_with(&["quality", "src", "--all", "--refresh"], &jev)
        .succeeds();
    assert!(
        seen.lock().unwrap().len() > asked_by_default * 2,
        "--all also asks about the generated client\n{all}"
    );
}

const KINDS: &str = r#"use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroU32;

pub fn order(a: u32, b: u32) -> Ordering {
    a.cmp(&b)
}

pub fn least() -> Ordering {
    Ordering::Less
}

pub fn maybe_order(a: f64, b: f64) -> Option<Ordering> {
    a.partial_cmp(&b)
}

pub fn greeting(name: &str) -> String {
    if name.is_empty() {
        String::from("hello")
    } else if name == "admin" {
        format!("welcome {name}")
    } else {
        name.to_string()
    }
}

pub fn owned(text: &str) -> String {
    text.to_owned()
}

pub fn empty_label() -> &'static str {
    ""
}

pub fn items(n: usize) -> Vec<u32> {
    if n == 0 {
        Vec::new()
    } else {
        vec![1; n]
    }
}

pub fn size(values: &[u32]) -> usize {
    values.len()
}

pub fn odd_count(values: &[u32]) -> usize {
    values.iter().filter(|v| *v % 2 == 1).count()
}

pub fn leading(x: u32) -> u32 {
    x.leading_zeros()
}

pub fn width() -> usize {
    std::mem::size_of::<u64>()
}

pub fn cleaned(text: &str) -> &str {
    text.trim()
}

pub fn suffixless(text: &str) -> &str {
    text.trim_end_matches('!')
}

pub fn first_char(text: &str) -> char {
    text.chars().next().unwrap()
}

pub fn last_char(text: &str) -> char {
    text.chars().next_back().unwrap()
}

pub fn nonzero(x: u32) -> Option<NonZeroU32> {
    NonZeroU32::new(x)
}

pub fn parsed(text: &str) -> Result<u32, std::num::ParseIntError> {
    let value = text.parse::<u32>()?;
    Ok(value + 1)
}

pub fn first_even(values: &[u32]) -> Option<u32> {
    let Some(first) = values.iter().copied().find(|v| v % 2 == 0) else {
        return None;
    };
    Some(first)
}

pub fn tail(values: &[u32]) -> &[u32] {
    &values[1..]
}

pub fn pair(x: u32) -> (u32, bool) {
    (x * 2, x > 3)
}

pub fn grid() -> [u8; 3] {
    [0; 3]
}

pub fn negated(flag: bool) -> bool {
    !flag
}

pub fn ratio() -> f64 {
    0.5
}

pub fn sign(n: i32) -> &'static str {
    match n.cmp(&0) {
        Ordering::Less => "negative",
        Ordering::Equal => "zero",
        Ordering::Greater => "positive",
    }
}

pub struct Celsius(pub f64);

impl fmt::Display for Celsius {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}C", self.0)
    }
}

pub trait Scale {
    type Output;
    fn scale(&self) -> Self::Output;
}

impl Scale for Celsius {
    type Output = f64;
    fn scale(&self) -> f64 {
        self.0 * 2.0
    }
}

impl Celsius {
    pub fn warmer(&self, by: f64) -> Self {
        Celsius(self.0 + by)
    }
}

pub fn counter() -> impl FnMut() -> u32 {
    let mut count = 0;
    move || {
        count += 1;
        count
    }
}

pub fn address(values: &[u32]) -> *const u32 {
    values.as_ptr()
}

pub fn blank() -> String {
    String::new()
}

pub fn none_yet() -> Vec<u32> {
    vec![]
}
"#;

const KINDS_TESTS: &str = r#"use kinds::*;
use std::cmp::Ordering;

#[test]
fn everything_runs() {
    assert_eq!(order(1, 2), Ordering::Less);
    assert_eq!(least(), Ordering::Less);
    assert_eq!(maybe_order(1.0, 2.0), Some(Ordering::Less));
    assert_eq!(greeting(""), "hello");
    assert_eq!(greeting("admin"), "welcome admin");
    assert_eq!(greeting("ann"), "ann");
    assert_eq!(owned("x"), "x");
    assert_eq!(empty_label(), "");
    assert!(items(0).is_empty());
    assert_eq!(items(2), vec![1, 1]);
    assert_eq!(size(&[1, 2]), 2);
    assert_eq!(odd_count(&[1, 2, 3]), 2);
    assert_eq!(leading(1), 31);
    assert_eq!(width(), 8);
    assert_eq!(cleaned(" a "), "a");
    assert_eq!(suffixless("a!!"), "a");
    assert_eq!(first_char("ab"), 'a');
    assert_eq!(last_char("ab"), 'b');
    assert!(nonzero(0).is_none());
    assert_eq!(parsed("1"), Ok(2));
    assert_eq!(first_even(&[1, 4]), Some(4));
    assert_eq!(first_even(&[1]), None);
    assert_eq!(tail(&[1, 2]), &[2]);
    assert_eq!(pair(4), (8, true));
    assert_eq!(grid(), [0, 0, 0]);
    assert!(negated(false));
    assert_eq!(ratio(), 0.5);
    assert_eq!(sign(-2), "negative");
    assert_eq!(sign(0), "zero");
    assert_eq!(sign(3), "positive");
    assert_eq!(Celsius(1.0).to_string(), "1C");
    assert_eq!(Celsius(2.0).scale(), 4.0);
    assert_eq!(Celsius(1.0).warmer(1.0).0, 2.0);
    let mut next = counter();
    assert_eq!(next(), 1);
    assert!(!address(&[1]).is_null());
    assert!(blank().is_empty());
    assert!(none_yet().is_empty());
}
"#;

#[test]
fn every_rust_return_shape_is_asked_as_a_change_of_its_type() {
    let project = Project::empty("rust-kinds");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"kinds\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write("src/lib.rs", KINDS);
    project.write("tests/kinds.rs", KINDS_TESTS);
    project.git(&["init", "-q"]);
    project.supercov(&["--", "cargo", "test", "-q"]).succeeds();
    let (base, seen) = gateway(MODEL, answer_no);
    let assessed = project
        .supercov_with(&["runs", "latest", "assertions", "assess"], &through(&base))
        .succeeds();
    assert!(assessed.contains("0% asserted (0 of 43"), "{assessed}");
    let asked = seen
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, _, request)| {
            request["questions"]
                .as_object()
                .unwrap()
                .values()
                .filter_map(|question| question["instructions"]["task"].as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for change in [
        "`a.cmp(&b)` becomes `(a.cmp(&b)).reverse()`",
        "`Ordering::Less` becomes `Ordering::Greater`",
        "`a.partial_cmp(&b)` becomes `None`",
        "`String::from(\"hello\")` becomes `String::new()`",
        "`\"\"` becomes `\"supercov\"`",
        "`vec![1; n]` becomes `Vec::new()`",
        "`values.len()` becomes `0`",
        "`text.trim()` becomes `\"\"`",
        "`text.chars().next().unwrap()` becomes `'\\0'`",
        "`NonZeroU32::new(x)` becomes `None`",
        "`write!(f, \"{}C\", self.0)` becomes `Err(std::fmt::Error)`",
        "`&values[1..]` becomes `&values[..0]`",
        "`values.as_ptr()` becomes `std::ptr::null()`",
        "`let Some(first) = values.iter().copied().find(|v| v % 2 == 0) else {` becomes `let Some(first) = None;`",
        // An empty value changes to a non-empty one: the same empty value is
        // no change, and a question no test could answer yes to.
        "`Vec::new()` becomes `vec![Default::default()]`",
        "`vec![]` becomes `vec![Default::default()]`",
        "`String::new()` becomes `String::from(\"supercov\")`",
    ] {
        assert!(asked.contains(change), "{change} not asked:\n{asked}");
    }
    assert!(
        !asked.contains("`Vec::new()` becomes `Vec::new()`"),
        "{asked}"
    );
}

/// A file's unasserted statements page like every other listing: the next
/// page's command, run as printed, shows the next page.
#[test]
fn unasserted_statements_page_with_a_next_page_command_that_shows_it() {
    let project = Project::cart("assertion-pages");
    let (base, _) = gateway(MODEL, answer_no);
    let run = project.measure(&[]);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let first = project
        .supercov(&["runs", &run, "assertions", "src/cart.js", "--limit", "2"])
        .succeeds();
    assert!(first.contains("showing 1-2 of "), "{first}");
    let next = first
        .lines()
        .find_map(|line| line.strip_prefix("next page: supercov "))
        .unwrap_or_else(|| panic!("no next page in\n{first}"));
    let words = next
        .split(' ')
        .map(|word| word.trim_matches('\''))
        .collect::<Vec<_>>();
    assert!(
        words.ends_with(&["--offset", "2", "--limit", "2"]),
        "{next}"
    );
    let second = project.supercov(&words).succeeds();
    assert!(second.contains("showing 3-4 of "), "{next}\n{second}");
}

/// `assess` and `assertions` take the filters every query takes: assessing
/// some tests asks only them about what they ran and saves apart from the
/// whole run; reading with a filter shows what those tests assert.
#[test]
fn assertions_read_and_assess_some_of_the_tests() {
    let project = Project::cart("assertion-filters");
    let (base, seen) = gateway(MODEL, answer_yes);
    let run = project.measure(&[]);
    let estimate = |extra: &[&str]| {
        let mut args = vec![
            "runs",
            run.as_str(),
            "assertions",
            "assess",
            "--dry-run",
            "--json",
        ];
        args.extend(extra);
        project.supercov_with(&args, &through(&base)).json()
    };
    let whole = estimate(&[]);
    let some = estimate(&["--title", "shipping"]);
    assert!(
        some["data"]["statements"].as_u64() < whole["data"]["statements"].as_u64(),
        "{some}\n{whole}"
    );
    assert_eq!(some["data"]["filter"], "--title shipping");
    assert!(seen.lock().unwrap().is_empty(), "a dry run sends nothing");

    // Judged to catch nothing, so the reading names a file to inspect.
    let (refusing, refused) = gateway(MODEL, answer_no);
    let assessed = project
        .supercov_with(
            &["runs", &run, "assertions", "assess", "--title", "shipping"],
            &through(&refusing),
        )
        .succeeds();
    assert!(
        assessed.contains("by the tests --title shipping picks"),
        "{assessed}"
    );
    // Only the shipping test was asked.
    let asked = refused
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, request)| {
            request["state"]["test"]["name"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        asked.into_iter().collect::<Vec<_>>(),
        ["shipping is free over fifty"]
    );
    // The whole run is not assessed; the filtered reading is.
    let unfiltered = project.supercov(&["runs", &run, "assertions"]).succeeds();
    assert!(unfiltered.contains("assertions assess"), "{unfiltered}");
    let read = project
        .supercov(&["runs", &run, "assertions", "--title", "shipping"])
        .succeeds();
    contains_all(
        &read,
        &["by the tests --title shipping picks", "--title 'shipping'"],
    );

    // After the whole run is assessed, a filter with no assessment of its
    // own reads the whole one for its tests.
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let whole_read = project
        .supercov(&["runs", &run, "assertions", "--json"])
        .json();
    let total_read = project
        .supercov(&["runs", &run, "assertions", "--title", "total", "--json"])
        .json();
    assert_eq!(
        total_read["data"]["filter"], "--title total",
        "{total_read}"
    );
    assert!(
        total_read["data"]["summary"]["statements"].as_u64()
            < whole_read["data"]["summary"]["statements"].as_u64(),
        "{total_read}\n{whole_read}"
    );
    let none = project
        .supercov(&["runs", &run, "assertions", "--runner", "jest"])
        .exits(2);
    assert!(none.contains("No tests match --runner jest"), "{none}");
}

/// Assessing reads the files its questions quote, not every file the run
/// recorded: a file CI generates and a checkout lacks does not stop it, while
/// a changed file it does quote still does, by name.
#[test]
fn assessing_checks_only_the_files_its_questions_quote() {
    let project = Project::cart("assertion-needed-files");
    project.write("src/generated.js", "export const stack = 'isolated';\n");
    project.commit("generated");
    let (base, _) = gateway(MODEL, answer_yes);
    let run = project.measure(&[]);
    std::fs::remove_file(project.root.join("src/generated.js")).unwrap();
    let assessed = project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    assert!(assessed.contains("asserted"), "{assessed}");
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let refused = project
        .supercov_with(
            &["runs", &run, "assertions", "assess", "--refresh"],
            &through(&base),
        )
        .exits(2);
    assert!(refused.contains("src/cart.js"), "{refused}");
}

#[test]
fn a_test_file_whose_tests_were_never_asked_is_not_read_as_asserting_nothing() {
    // Two files run the same statements. The assessment asks one test per
    // statement first, and stops at a catch: the other file's tests were
    // never asked, and reading them out of it said 0% asserted.
    let project = Project::empty("assertions-unasked");
    project.write(
        "package.json",
        r#"{ "name": "f", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/cart.js",
        "export function total(prices) {\n  let sum = 0;\n  for (const price of prices) {\n    sum += price;\n  }\n  return sum;\n}\n\nexport function label(sum) {\n  if (sum > 100) {\n    return \"big\";\n  }\n  return \"small\";\n}\n",
    );
    project.write(
        "test/total.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\ntest(\"total adds\", () => {\n  assert.equal(total([1, 2]), 3);\n});\n",
    );
    project.write(
        "test/label.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { label, total } from \"../src/cart.js\";\ntest(\"label says big for a big total\", () => {\n  assert.equal(label(total([150, 50])), \"big\");\n});\n",
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", "node", "--test"]).succeeds();
    let (base, _) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", "latest", "assertions", "assess"], &through(&base))
        .succeeds();
    let reading = project
        .supercov(&[
            "runs",
            "latest",
            "assertions",
            "--test-file",
            "test/total.test.js",
        ])
        .succeeds();
    assert!(!reading.contains("0% asserted"), "{reading}");
    contains_all(
        &reading,
        &[
            "not assessed for the tests --test-file test/total.test.js picks",
            "assertions assess --test-file 'test/total.test.js'",
        ],
    );
    let json = project
        .supercov(&[
            "runs",
            "latest",
            "assertions",
            "--test-file",
            "test/total.test.js",
            "--json",
        ])
        .json();
    assert_eq!(json["data"]["summary"]["unasked"], 4, "{json}");
    // Asked, they are what they assert.
    project
        .supercov_with(
            &[
                "runs",
                "latest",
                "assertions",
                "assess",
                "--test-file",
                "test/total.test.js",
            ],
            &through(&base),
        )
        .succeeds();
    let asked = project
        .supercov(&[
            "runs",
            "latest",
            "assertions",
            "--test-file",
            "test/total.test.js",
        ])
        .succeeds();
    assert!(
        asked.contains("100% asserted (4 of 4 executed statements)"),
        "{asked}"
    );
}

#[test]
fn assertion_views_page_name_assessed_tests_and_refuse_what_matches_nothing() {
    let project = Project::cart("assertion-view-edges");
    let (base, _) = gateway(MODEL, answer_all_but_the_sum);
    let run = project.measure(&[]);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let said = |args: &[&str]| {
        let mut full = vec!["runs", run.as_str(), "assertions"];
        full.extend(args);
        project.supercov(&full)
    };
    contains_all(
        &said(&["--weak"]).succeeds(),
        &["0 of 7 asserted statements rest on an answer under 75%"],
    );
    contains_all(
        &said(&["src/cart.js:9"]).succeeds(),
        &[
            "Change: returns undefined",
            "would fail   total applies a coupon",
        ],
    );
    contains_all(
        &said(&["--test", "shipping is free over fifty"]).succeeds(),
        &[
            "judged to catch a change to 2 of the 2 statements",
            "Catches",
        ],
    );
    // A page past the end says so instead of leaving the table out.
    contains_all(
        &said(&["--offset", "5"]).succeeds(),
        &["showing none of 1: --offset 5 is past the end"],
    );
    // A name that matches no assessed test lists the ones there are.
    contains_all(
        &said(&["--test", "nobody"]).exits(2),
        &[
            "no assessed test is named nobody; the assessed tests are:",
            "  total adds prices (test/cart.test.js)",
        ],
    );
    contains_all(
        &said(&["src/cart.js:99"]).exits(2),
        &["no assessed statement at src/cart.js:99"],
    );
    contains_all(
        &said(&["src/cart.js", "--test", "total adds prices"]).exits(2),
        &["give a file, a file:line or --test, not more than one"],
    );
}

#[test]
fn filtered_assertions_read_the_share_of_the_tests_a_filter_picks() {
    let project = Project::cart("filtered-assertions");
    let (base, _) = gateway(MODEL, answer_all_but_the_sum);
    let run = project.measure(&[]);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let said = |args: &[&str]| {
        let mut full = vec!["runs", run.as_str(), "assertions"];
        full.extend(args);
        project.supercov(&full).succeeds()
    };
    contains_all(
        &said(&["--title", "shipping"]),
        &["100% asserted (2 of 2 executed statements), by the tests --title shipping picks"],
    );
    // Statements other tests answered for first are named, with how to ask these.
    contains_all(
        &said(&["--title", "total adds"]),
        &[
            "0% asserted (0 of 1 executed statements)",
            "4 other statement(s) these tests run were not asked of them",
            "assertions assess --title 'total adds'",
            "inspect a file: ",
        ],
    );
    contains_all(
        &said(&["src/cart.js", "--title", "coupon"]),
        &[
            "src/cart.js: 83.3% asserted (5 of 6 statements)",
            "assertions --title 'coupon' 'src/cart.js:2'",
        ],
    );
    contains_all(
        &said(&["src/cart.js", "--weak"]),
        &["src/cart.js: 0 asserted statement(s) rest on an answer under 75%"],
    );
}

/// An assessment where nothing fires says so in every view: the summary
/// pages its files, gaps has nothing to list, a file shows every check as
/// a judgment, and scope names what was left out under a heading.
#[test]
fn an_assessment_where_nothing_fires_reads_as_such_in_every_view() {
    let project = Project::cart("quality-quiet");
    for name in ["a", "b", "c"] {
        project.write(
            &format!("src/{name}.js"),
            &format!("export const {name} = (x) => x ? 1 : 2;\n"),
        );
    }
    project.commit("more");
    let (base, _) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["quality"], &through(&base))
        .succeeds();
    let shown = project
        .supercov(&["quality", "show", "--limit", "1"])
        .succeeds();
    contains_all(&shown, &["over 4 files.", "... and 3 more"]);
    let gaps = project
        .supercov(&["quality", "gaps", "--limit", "1"])
        .succeeds();
    assert!(
        gaps.contains("Nothing fired on any assessed file."),
        "{gaps}"
    );
    let file = project
        .supercov(&["quality", "file", "src/a.js"])
        .succeeds();
    contains_all(
        &file,
        &["no  0.10  complex_conditional", "a model judgment"],
    );
    let scope = project.supercov(&["quality", "scope"]).succeeds();
    contains_all(
        &scope,
        &[
            "Files left out, by reason:\n      1  test\n",
            "excluded   test/cart.test.js  (test)",
        ],
    );
}

/// A diff of two assessments compares the files both assessed, lists what
/// declined, improved, was added and removed in either direction, says when
/// an unchanged file moved by the model's own variation, and names a
/// snapshot asked twice, missing or unknown.
#[test]
fn a_quality_diff_reads_in_either_direction_and_names_its_mistakes() {
    let project = Project::cart("quality-diff");
    let (no, _) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["quality"], &through(&no))
        .succeeds();
    project.write("src/extra.js", "export const extra = (x) => x ? 1 : 2;\n");
    project.commit("extra");
    let (yes, _) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["quality", "--refresh"], &through(&yes))
        .succeeds();
    let list: serde_json::Value = serde_json::from_str(
        &project
            .supercov(&["quality", "snapshots", "--json"])
            .stdout(),
    )
    .unwrap();
    let ids = list["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|snapshot| snapshot["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let (newer, older) = (&ids[0], &ids[1]);
    let declined = project
        .supercov(&["quality", "diff", older, newer])
        .succeeds();
    contains_all(
        &declined,
        &[
            "Health good (9.0) to weak (1.0).",
            "compare the 1 file both assessed",
            "1 declined, 0 improved, 0 unchanged, 1 added, 0 removed.",
            "12 properties appeared that were not there before.",
            "complex_conditional  0.10 → 0.90, appeared",
            "the file did not change, so this is the model's own variation",
            "weak  src/extra.js",
        ],
    );
    let improved = project
        .supercov(&["quality", "diff", newer, older])
        .succeeds();
    contains_all(
        &improved,
        &[
            "0 declined, 1 improved, 0 unchanged, 0 added, 1 removed.",
            "0.90 → 0.10, no longer present",
            "Removed:\n  src/extra.js",
        ],
    );
    for (args, message) in [
        (
            vec!["quality", "diff", newer.as_str(), newer.as_str()],
            "is the same snapshot twice",
        ),
        (
            vec!["quality", "diff", newer.as_str()],
            "requires a second snapshot",
        ),
        (
            vec!["quality", "diff", newer.as_str(), "q_nope"],
            "not a snapshot id: q_nope",
        ),
    ] {
        let refused = project.supercov(&args).exits(2);
        assert!(refused.contains(message), "{args:?}: {refused}");
    }
}
