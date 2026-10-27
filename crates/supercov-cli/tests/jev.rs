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
            "1 flagged file has lines no test in run",
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

/// An assessment of each kind, once for each place in the code that makes a
/// file operation, with that place's first call failing: it may stop, but
/// says why and never panics, and the one no fault reaches succeeds.
#[test]
fn an_assessment_whose_file_operation_fails_says_so_and_never_panics() {
    let project = Project::cart("assess-faults");
    let run = project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_yes);
    for command in [
        &["runs", &run, "assertions", "assess", "--refresh"][..],
        &["quality"],
        &["security"],
        &["security", "--run", "latest"],
    ] {
        for place in 1.. {
            let fault = format!("fs#{place}");
            let mut environment = through(&base).to_vec();
            environment.push(("SUPERCOV_TEST_FAULT", fault.as_str()));
            let ran = project.supercov_with(command, &environment);
            let said = ran.stderr();
            assert!(
                !said.contains("panicked at") && ran.code() != 101,
                "{command:?} with {fault}:\n{said}"
            );
            if !said.contains("[supercov] test fault at fs") {
                ran.succeeds();
                assert!(place > 3, "{command:?} reached {place} places");
                break;
            }
        }
    }
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
    // A bug reading a file's values costs only its questions their
    // replacements: the assessment still answers, describing the changes.
    let (base, seen) = gateway(MODEL, answer_no);
    let mut env = through(&base).to_vec();
    env.push(("SUPERCOV_TEST_FAULT", "value-changes-panic"));
    let degraded = project.supercov_with(
        &["runs", "latest", "assertions", "assess", "--refresh"],
        &env,
    );
    assert_eq!(degraded.code(), 0, "{}", degraded.stderr());
    assert!(
        degraded
            .stderr()
            .contains("test fault at value-changes-panic"),
        "{}",
        degraded.stderr()
    );
    assert!(
        degraded
            .stdout()
            .contains("0% asserted (0 of 10 executed statements)"),
        "{}",
        degraded.stdout()
    );
    assert!(!seen.lock().unwrap().is_empty());
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
    // that runs both shows the path. `lookup` would be one too, but no test
    // runs it.
    project.write(
        "src/router.js",
        "export async function route(req, deps) {\n  return deps.findOrders(deps.db, req.params.id);\n}\n\nexport async function lookup(req, deps) {\n  return deps.findOrders(deps.db, req.query.id);\n}\n",
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
    // A path the model does not confirm is not reported.
    fn path_declined(id: &str, question: &serde_json::Value) -> serde_json::Value {
        if id == "path" {
            serde_json::json!({ "type": "noul", "noul": 0.1 })
        } else {
            answer_taint(id, question)
        }
    }
    let (base, seen) = gateway(MODEL, path_declined);
    let report = project
        .supercov_with(
            &["security", "src/", "--run", "latest", "--refresh"],
            &through(&base),
        )
        .succeeds();
    assert!(!report.contains("Cross-file paths"), "{report}");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|(_, _, request)| request["questions"].get("path").is_some())
    );
}

/// A path whose two functions are together larger than one request is
/// named as not asked, by import and as a test ran it, where it was once
/// dropped without a word.
#[test]
fn a_cross_file_path_too_large_to_ask_is_named() {
    let project = Project::empty("security-paths-large");
    project.write("package.json", r#"{"name":"orders","type":"module"}"#);
    // Dense as base64, so each byte is a token: a function fits in a request
    // of its own, two do not.
    let padding = (0..250)
        .map(|i| format!("  // {i:04}{}\n", "Ab0+".repeat(16)))
        .collect::<String>();
    project.write(
        "src/handler.js",
        &format!("import {{ findOrders }} from \"./db.js\";\n\nexport async function handler(req, db) {{\n{padding}  const rows = await findOrders(db, req.query.customer);\n  return {{ count: rows.length }};\n}}\n"),
    );
    project.write(
        "src/db.js",
        &format!("export function findOrders(db, customer) {{\n{padding}  return db.query(\"SELECT * FROM orders WHERE customer = '\" + customer + \"'\");\n}}\n"),
    );
    project.write(
        "src/router.js",
        &format!("export async function route(req, deps) {{\n{padding}  return deps.findOrders(deps.db, req.params.id);\n}}\n"),
    );
    project.write(
        "test/router.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { route } from \"../src/router.js\";\nimport { findOrders } from \"../src/db.js\";\n\ntest(\"routes to the orders query\", async () => {\n  const db = { query: async () => [1] };\n  assert.deepEqual(await route({ params: { id: \"7\" } }, { db, findOrders }), [1]);\n});\n",
    );
    project.git(&["init", "-q"]);
    project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_taint);
    let report = project
        .supercov_with(&["security", "src/", "--run", "latest"], &through(&base))
        .succeeds();
    contains_all(
        &report,
        &[
            "2 cross-file path(s) not asked: the two functions together are larger than one request:",
            "src/handler.js:",
            "src/router.js:1 route -> src/db.js:1 findOrders",
        ],
    );
    assert!(!report.contains("Cross-file paths confirmed"), "{report}");
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
            "Nothing introduced across 1 changed file.",
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
            "1 file under no source root was read by the model from the tree: 1 ships, 0 do not.",
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
    // `tests affected` reads those answers beside the assessment's: the
    // forty asked catch the change, and the five never asked are unassessed,
    // not missing it, since not every test that ran it was asked.
    let affected = project
        .supercov(&["runs", &run, "tests", "affected", "--json"])
        .json()["data"]
        .clone();
    assert_eq!(affected["assertions"]["changedStatements"], 1, "{affected}");
    let tests = &affected["assertions"]["tests"];
    assert_eq!(
        (tests["catches"].as_u64(), tests["unassessed"].as_u64()),
        (Some(40), Some(5)),
        "{tests}"
    );
    assert_eq!(tests["misses"], 0, "{tests}");
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
            "no assessed test is named nobody; a test that ran is asked nothing when other tests catch each statement it runs first. The assessed tests are:",
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

fn answer_weakly(_: &str, _: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "type": "noul", "noul": 0.6 })
}

#[test]
fn every_assertion_view_reads_an_empty_page_and_a_page_past_its_end() {
    let caught = Project::cart("views-caught");
    let (base, _) = gateway(MODEL, answer_yes);
    let run = caught.measure(&[]);
    caught
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let read = |project: &Project, run: &str, args: &[&str]| {
        let mut full = vec!["runs", run, "assertions"];
        full.extend(args);
        project.supercov(&full)
    };
    // Everything asserted, nothing weak: each view says so and offers no
    // next step.
    let weak = read(&caught, &run, &["--weak"]).succeeds();
    assert!(
        weak.contains("0 of 8 asserted statements rest on an answer under 75%"),
        "{weak}"
    );
    assert!(!weak.contains("inspect"), "{weak}");
    let file = read(&caught, &run, &["src/cart.js", "--offset", "50"]).succeeds();
    assert!(
        file.contains("Every statement this file ran is asserted.\n"),
        "{file}"
    );
    assert!(!file.contains("showing"), "{file}");
    let weak_file = read(&caught, &run, &["src/cart.js", "--weak"]).succeeds();
    assert!(
        weak_file.ends_with("0 asserted statement(s) rest on an answer under 75%\n"),
        "{weak_file}"
    );
    // A test that ran but was asked nothing is not among the assessed ones,
    // and the refusal says why that can be.
    let unasked = read(&caught, &run, &["--test", "total adds prices"]).exits(2);
    contains_all(
        &unasked,
        &[
            "a test that ran is asked nothing when other tests catch each statement it runs first",
            "  total applies a coupon (test/cart.test.js)",
        ],
    );
    assert!(!unasked.contains("and 0 more"), "{unasked}");
    // A filter that picks no test is refused before any view is read.
    for args in [
        &["--title", "zzz", "--weak"][..],
        &["src/cart.js", "--title", "zzz"],
    ] {
        assert!(
            read(&caught, &run, args)
                .exits(2)
                .contains("No tests match --title zzz")
        );
    }

    // Nothing asserted: a file's list pages, and a page past its end shows
    // no table, only where the listing ends.
    let missed = Project::cart("views-missed");
    let (base, _) = gateway(MODEL, answer_no);
    let run = missed.measure(&[]);
    missed
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let first = read(&missed, &run, &["src/cart.js", "--limit", "1"]).succeeds();
    contains_all(
        &first,
        &[
            "     2  let sum = 0;  (value becomes undefined)  2 of 2  total applies a coupon 10%",
            "showing 1-1 of 8",
            "assertions 'src/cart.js' --offset 1 --limit 1",
        ],
    );
    let past = read(&missed, &run, &["src/cart.js", "--offset", "50"]).succeeds();
    assert!(
        past.contains("showing none of 8: --offset 50 is past the end"),
        "{past}"
    );
    assert!(!past.contains("LINE  STATEMENT"), "{past}");
    let test = read(
        &missed,
        &run,
        &["--test", "total adds prices", "--offset", "40"],
    )
    .succeeds();
    contains_all(
        &test,
        &[
            "judged to catch a change to 0 of the 5 statements it was asked about",
            "showing none of 5: --offset 40 is past the end",
        ],
    );
    assert!(!test.contains("Would not catch"), "{test}");
    let none_weak = read(&missed, &run, &["--weak"]).succeeds();
    assert!(
        none_weak.contains("0 of 0 asserted statements"),
        "{none_weak}"
    );

    // Every verdict weak: the weak views list and page them.
    let unsure = Project::cart("views-unsure");
    let (base, _) = gateway(MODEL, answer_weakly);
    let run = unsure.measure(&[]);
    unsure
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let files = read(&unsure, &run, &["src/cart.js"]).succeeds();
    contains_all(
        &files,
        &[
            "Every statement this file ran is asserted.",
            "8 of the asserted rest on an answer under 75%",
            "assertions 'src/cart.js' --weak`",
        ],
    );
    contains_all(
        &read(&unsure, &run, &["--weak"]).succeeds(),
        &["     8         8  src/cart.js", "inspect a file: "],
    );
    assert!(
        read(&unsure, &run, &["--weak", "--offset", "5"])
            .succeeds()
            .contains("showing none of 1: --offset 5 is past the end")
    );
    contains_all(
        &read(&unsure, &run, &["src/cart.js", "--weak", "--limit", "1"]).succeeds(),
        &[
            "     2  let sum = 0;  (value becomes undefined)  60%  1 of 2  total applies a coupon",
            "assertions 'src/cart.js' --weak --offset 1 --limit 1",
            "inspect a statement: ",
        ],
    );
    let weak_past = read(&unsure, &run, &["src/cart.js", "--weak", "--offset", "50"]).succeeds();
    assert!(
        weak_past.contains("showing none of 8: --offset 50 is past the end"),
        "{weak_past}"
    );
    assert!(!weak_past.contains("LINE  STATEMENT"), "{weak_past}");
    assert!(
        read(&unsure, &run, &["src/cart.js:2"])
            .succeeds()
            .contains("     60%  would fail   total applies a coupon")
    );
}

#[test]
fn a_statement_many_test_files_run_names_the_first_few_left_unasked() {
    let project = Project::empty("views-many");
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" } }"#,
    );
    project.write("src/cart.js", common::CART);
    for i in 0..25 {
        project.write(
            &format!("test/t{i:02}.test.js"),
            &format!(
                "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ total }} from \"../src/cart.js\";\n\ntest(\"sum {i:02}\", () => {{\n  assert.ok(total([{{ price: {i}, quantity: 1 }}]) >= 0);\n}});\n"
            ),
        );
    }
    project.git(&["init", "-q"]);
    project.commit("many");
    let (base, _) = gateway(MODEL, answer_no);
    let run = project.measure(&[]);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let statement = project
        .supercov(&["runs", &run, "assertions", "src/cart.js:2"])
        .succeeds();
    assert!(
        statement.contains("Not asked: 7 test(s). No test was asked from 7 file(s) that run it: "),
        "{statement}"
    );
    assert!(statement.contains(", and 2 more"), "{statement}");
    let unknown = project
        .supercov(&["runs", &run, "assertions", "--test", "nobody"])
        .exits(2);
    contains_all(&unknown, &["The assessed tests include:", "  and 8 more"]);
}

#[test]
fn each_affected_test_says_how_it_meets_the_change_as_the_change_grows() {
    let project = Project::cart("affected-verdicts");
    let (base, _) = gateway(MODEL, answer_all_but_the_sum);
    let run = project.measure(&[]);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let affected = || {
        project
            .supercov(&["runs", &run, "tests", "affected"])
            .succeeds()
    };
    let names = |narrow: &str| {
        project
            .supercov(&["runs", &run, "tests", "affected", "--names", narrow])
            .succeeds()
    };
    let changed = || {
        project
            .supercov_with(
                &["runs", &run, "assertions", "assess", "--changed"],
                &through(&base),
            )
            .succeeds()
    };

    // A line only the coupon test runs. The other test that runs `total` was
    // never asked about it, until `--changed` asks every test that ran it.
    project.edit(
        "src/cart.js",
        "sum = sum - (sum * coupon.percent) / 100;",
        "sum = sum - (sum * coupon.percent) / 50;",
    );
    contains_all(
        &affected(),
        &[
            "Assertions: 1 of 1 changed statements the run assessed are asserted by a test; 1 affected test(s) catch the change.",
            "    catches a change to src/cart.js:7 (0.90)",
            "  test/cart.test.js › total adds prices\n    src/cart.js: total (line 1) changed (this test ran it)\n    not assessed against the changed code",
        ],
    );
    assert_eq!(
        names("--asserting"),
        "total applies a coupon\ntotal adds prices\n"
    );
    changed();
    assert!(affected().contains(
        "  test/cart.test.js › total adds prices\n    src/cart.js: total (line 1) changed (this test ran it)\n    ran the changed declaration, not the statements that changed"
    ));
    assert_eq!(names("--ran-changed"), "total applies a coupon\n");

    // A line both run, which neither is judged to catch.
    project.edit("src/cart.js", "let sum = 0;", "let sum = 0 + 0;");
    changed();
    contains_all(
        &affected(),
        &[
            "Assertions: 1 of 2 changed statements the run assessed are asserted by a test",
            "  not asserted: src/cart.js:2  let sum = 0;",
            "    runs the changed code; not judged to catch a change to it",
        ],
    );
    assert_eq!(
        names("--ran-changed"),
        "total applies a coupon\ntotal adds prices\n"
    );
    assert_eq!(names("--asserting"), "total applies a coupon\n");

    // A test whose own file changed must run, whatever it was judged.
    project.edit("test/cart.test.js", "free over fifty", "free over 50");
    let json = project
        .supercov(&["runs", &run, "tests", "affected", "--json"])
        .json();
    let verdicts = json["data"]["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|test| test["assertion"]["verdict"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(verdicts, ["must run", "must run", "must run"], "{json}");
    assert_eq!(json["data"]["assertions"]["tests"]["mustRun"], 3);
    assert_eq!(
        names("--asserting"),
        "total applies a coupon\ntotal adds prices\nshipping is free over fifty\n"
    );
}

#[test]
fn a_file_that_cannot_be_assessed_is_named_beside_those_that_were() {
    let project = Project::cart("quality-partial");
    project.write("src/b.js", "export const b = (x) => x * 2;\n");
    project.commit("b");
    let run = project.measure(&[]);
    let ask = |args: &[&str], faults: common::Faults| {
        let (base, _) = gateway_with(MODEL, answer_yes, faults);
        project.supercov_with(args, &through(&base))
    };
    let first_refused: common::Faults = |n| (n == 0).then_some((400, "{}"));

    // One request refused: the other file is scored, the refused one named,
    // and the count said on standard error.
    let partial = ask(&["quality", "--refresh"], first_refused);
    assert_eq!(partial.code(), 0, "{}", partial.stderr());
    contains_all(
        &partial.stdout(),
        &["Quality weak (1.0/10) over 1 file.", "  error  src/"],
    );
    assert!(
        partial.stderr().contains(
            "[supercov] 1 of 2 files could not be assessed; each is listed in the report with its reason"
        ),
        "{}",
        partial.stderr()
    );
    let security = ask(&["security", "--refresh", "--run", &run], first_refused);
    assert_eq!(security.code(), 0, "{}", security.stderr());
    assert!(
        security
            .stderr()
            .contains("1 of 2 files could not be assessed")
    );

    // A server asking for a wait longer than the command keeps: every file
    // fails, and no score stands in for the ones never given.
    let waited = ask(&["quality", "--refresh"], |_| Some((429, "wait")));
    assert_eq!(waited.code(), 2);
    let said = waited.stdout();
    contains_all(
        &said,
        &[
            "Quality not scored: no file was assessed.\n",
            "HTTP 429 and asked for 100000 seconds, longer than this command will wait; try again later or with fewer paths",
        ],
    );
    assert!(!said.contains("/10"), "{said}");
    assert!(
        ask(&["quality", "-h"], |_| None)
            .succeeds()
            .starts_with("Code quality powered by Jev.")
    );

    // A review says how much it found before listing it, pages its files,
    // and names a file it could not review among them.
    project.edit("src/cart.js", "let sum = 0;", "let sum = 0 + 0;");
    project.edit("src/b.js", "x * 2", "x * 3");
    let review = ask(&["quality", "patch", "--limit", "1"], |_| None).succeeds();
    assert!(
        review.starts_with(
            "Reviewing unstaged changes.\n54 checks introduced across 2 changed files. Each is a judgment you can verify against the file.\n\nsrc/b.js\n"
        ),
        "{review}"
    );
    assert!(
        review.ends_with("... and 1 more changed files\n"),
        "{review}"
    );
    assert!(!review.contains("Health"), "{review}");
    let every = ask(&["quality", "patch", "--limit", "0"], |_| None).succeeds();
    assert!(every.contains("\nsrc/cart.js\n") && !every.contains("more changed"));
    let failed = ask(
        &["quality", "patch", "--refresh", "--run", &run],
        first_refused,
    );
    assert_eq!(failed.code(), 0, "{}", failed.stderr());
    contains_all(
        &failed.stdout(),
        &[
            "  failed  src/",
            " of its measured lines are not covered by the run",
        ],
    );
    assert!(
        failed
            .stderr()
            .contains("1 of 2 files could not be assessed")
    );
    let security = ask(&["security", "patch", "--run", &run], |_| None).succeeds();
    assert!(
        security.starts_with(
            "Reviewing unstaged changes.\n24 checks introduced across 2 changed files."
        ),
        "{security}"
    );
}

#[test]
fn an_assessment_read_for_some_tests_says_what_those_tests_ran_and_were_asked() {
    let project = Project::cart("assertions-picked");
    project.write(
        "test/other.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\n\ntest(\"total adds prices\", () => {\n  assert.equal(total([{ price: 1, quantity: 1 }]), 1);\n});\n\ntest(\"noop\", () => { assert.ok(true); });\n",
    );
    project.commit("other");
    let run = project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_no);
    let read = |args: &[&str]| {
        let mut full = vec!["runs", run.as_str(), "assertions"];
        full.extend_from_slice(args);
        project.supercov_with(&full, &through(&base))
    };
    // A test that runs no source has nothing to be asked.
    assert!(
        read(&["assess", "--title", "noop"])
            .exits(2)
            .contains("the tests --title noop picks ran no assessed statement")
    );
    read(&["assess"]).succeeds();
    assert!(read(&["--title", "noop"]).succeeds().starts_with(&format!(
        "Run {run}: the tests --title noop picks ran no assessed statement.\n\n"
    )));
    // One name in two files: the file tells them apart.
    let both = read(&["--test", "total adds prices"]).exits(2);
    contains_all(
        &both,
        &[
            "2 assessed tests match total adds prices; add --test-file with one's file:",
            "  total adds prices (test/other.test.js)",
        ],
    );
    assert!(
        read(&["--test", "total adds prices", "--test-file", "test/other.test.js"])
            .succeeds()
            .contains("total adds prices (test/other.test.js): judged to catch a change to 0 of the 5 statements")
    );
    let several = read(&["--test", "total"]).exits(2);
    assert!(several.contains("give one's full name:"), "{several}");
    // A line that is no number names a file, and the JSON says so too.
    assert!(
        read(&["src/cart.js:abc"])
            .exits(2)
            .contains("no assessed statement in src/cart.js:abc")
    );
    let json: serde_json::Value =
        serde_json::from_str(&read(&["src/none.js", "--json"]).stdout()).unwrap();
    assert_eq!(json["error"]["code"], "INVALID_ARGUMENT");

    // Tests that were never asked, because others answered first.
    let (yes, _) = gateway(MODEL, answer_yes);
    let caught = Project::cart("assertions-unasked");
    let first = caught.measure(&[]);
    caught
        .supercov_with(&["runs", &first, "assertions", "assess"], &through(&yes))
        .succeeds();
    assert!(
        caught
            .supercov(&["runs", &first, "assertions", "--title", "total adds"])
            .succeeds()
            .contains("not assessed for the tests --title total adds picks: the 5 statement(s) they run were asked of other tests, which answered first.")
    );
    // A later run points back at the last one assessed.
    let later = caught.measure(&[]);
    contains_all(
        &caught.supercov(&["runs", &later, "assertions"]).succeeds(),
        &[
            "assertions not assessed for this run.",
            &format!("Last assessed: {first}, 100.0% asserted."),
        ],
    );
}

#[test]
fn source_reads_a_file_as_the_run_saw_it_until_that_file_changes() {
    let project = Project::cart("source-views");
    project.write("src/empty.js", "");
    project.commit("empty");
    project.measure(&[]);
    let source = |args: &[&str]| {
        let mut full = vec!["runs", "latest", "source"];
        full.extend_from_slice(args);
        project.supercov(&full)
    };
    assert!(
        source(&["--help"])
            .succeeds()
            .starts_with("Usage: supercov runs <run> source <path>")
    );
    assert!(
        source(&["src/empty.js"])
            .succeeds()
            .ends_with("\nThe file is empty.\n")
    );
    for limit in ["0", "1001"] {
        assert!(
            source(&["src/cart.js", "--limit", limit])
                .exits(2)
                .contains("limit must be 1..1000")
        );
    }
    assert!(
        source(&["src/cart.js", "--limit", "2", "--limit", "3"])
            .exits(2)
            .contains("Duplicate option: --limit")
    );
    assert!(
        source(&["src/cart.js", "--offset", "500"])
            .succeeds()
            .contains("Showing none of 21: --offset 500 is past the end")
    );
    // Another file changing leaves this one as the run saw it.
    project.edit("test/cart.test.js", "free over fifty", "free over 50");
    assert!(
        source(&["src/cart.js", "--limit", "1"])
            .succeeds()
            .contains(" 1 │ export function total")
    );
    // This one changing does not.
    project.edit("src/cart.js", "let sum = 0;", "let sum = 0 + 0;");
    assert!(
        source(&["src/cart.js"])
            .exits(2)
            .contains("Current source differs from the run or is unavailable: src/cart.js")
    );
    let json: serde_json::Value =
        serde_json::from_str(&source(&["src/cart.js", "--json"]).stdout()).unwrap();
    assert_eq!(json["command"], "coverage.source");

    // One snapshot is one, and a bad id is named.
    let (base, _) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["quality", "src/"], &through(&base))
        .succeeds();
    assert!(
        project
            .supercov(&["quality", "snapshots"])
            .succeeds()
            .starts_with("1 quality snapshot, most recent first (showing 1).")
    );
    assert!(
        project
            .supercov(&["quality", "show", "q_zzzzzzzzzzzzzzzz"])
            .exits(2)
            .contains("not a snapshot id: q_zzzzzzzzzzzzzzzz")
    );
    assert_eq!(
        project.supercov(&["quality", "gaps"]).succeeds(),
        "Nothing fired on any assessed file.\n"
    );
}

/// Labels by a function's name: `take*` reads outside data, `sink*`
/// reaches an injection sink, `clean*` reaches one but sanitises, `quiet*`
/// reaches something that is no sink.
fn answer_by_name(id: &str, question: &serde_json::Value) -> serde_json::Value {
    let task = question["instructions"]["task"]
        .as_str()
        .unwrap_or_default();
    let function = task.split('`').nth(1).unwrap_or_default();
    let yes = serde_json::json!({ "type": "noul", "noul": 0.9 });
    let no = serde_json::json!({ "type": "noul", "noul": 0.1 });
    if id == "path" {
        return yes;
    }
    let Some((_, label)) = id.split_once('_').filter(|_| id.starts_with('f')) else {
        return common::answer_reviewer(id, question);
    };
    let reaches = ["sink", "clean", "quiet"]
        .iter()
        .any(|p| function.starts_with(p));
    match label {
        "takes_outside" => {
            if function.starts_with("take") {
                yes
            } else {
                no
            }
        }
        "reaches_sink" => {
            if reaches {
                yes
            } else {
                no
            }
        }
        "sanitises" => {
            if function.starts_with("clean") {
                yes
            } else {
                no
            }
        }
        "sink_kind" => common::choose(
            question,
            if function.starts_with("sink") || function.starts_with("clean") {
                "injection_sink"
            } else {
                "none"
            },
        ),
        _ => common::answer_reviewer(id, question),
    }
}

#[test]
fn a_cross_file_path_is_followed_only_into_the_function_an_import_names() {
    let project = Project::empty("probe-paths");
    project.write("package.json", r#"{"name":"paths","type":"module"}"#);
    project.write("src/db.js", "export default function sinkDefault(db, v) {\n  return db.query(\"x\" + v);\n}\nexport function sinkNamed(db, v) {\n  return db.query(\"y\" + v);\n}\nexport function cleanQuery(db, v) {\n  return db.query(\"z\", [v]);\n}\nexport function quietLog(db, v) {\n  return console.log(v);\n}\nexport function plain(v) {\n  return v;\n}\n");
    project.write("src/a.js", "import sinkDefault from \"./db.js\";\nexport function takeA(req, db) {\n  return sinkDefault(db, req.q);\n}\n");
    project.write("src/b.js", "import * as store from \"./db.js\";\nexport function takeB(req, db) {\n  return store.sinkNamed(db, req.q);\n}\n");
    project.write("src/c.js", "import { sinkNamed as run } from \"./db.js\";\nexport function takeC(req, db) {\n  return run(db, req.q);\n}\n");
    project.write("src/d.js", "import { cleanQuery, quietLog, plain } from \"./db.js\";\nexport function takeD(req, db) {\n  cleanQuery(db, req.q);\n  quietLog(db, req.q);\n  return plain(req.q);\n}\n");
    project.write("src/e.js", "import { sinkNamed } from \"./db.js\";\nexport function takeE1(req, db) {\n  return sinkNamed(db, req.a);\n}\nexport function takeE2(req, db) {\n  return sinkNamed(db, req.b);\n}\nexport function inner(db) {\n  return sinkNamed(db, 1);\n}\n");
    project.write("src/f.js", "import { sinkNamed } from \"./db.js\";\nexport function takeF(req, db) {\n  return sinkNamed(db, req.f);\n}\n");
    project.write("src/g.js", "import { plain as sinkNamed } from \"./db.js\";\nexport function takeG(req, db) {\n  return sinkNamed(req.g);\n}\n");
    // A default import of a module with no default export names the
    // function called as it is imported; a module of no functions has no
    // callee.
    project.write(
        "src/nodefault.js",
        "export function sinkLocal(db, v) {\n  return db.query(\"w\" + v);\n}\n",
    );
    project.write("src/i.js", "import sinkLocal from \"./nodefault.js\";\nexport function takeI(req, db) {\n  return sinkLocal(db, req.i);\n}\n");
    project.write("src/consts.js", "export const limit = 1;\n");
    project.write("src/j.js", "import { limit } from \"./consts.js\";\nexport function takeJ(req) {\n  return req.j + limit;\n}\n");
    project.git(&["init", "-q"]);
    let (base, _) = gateway(MODEL, answer_by_name);
    let report = project
        .supercov_with(&["security", "src/"], &through(&base))
        .succeeds();
    let paths = report
        .split_once("Cross-file paths confirmed: ")
        .map(|(_, rest)| rest.split("\n\n").next().unwrap_or_default())
        .unwrap_or_default();
    // A default import is the default export alone; a namespace or an alias
    // reaches the function called, and an alias named like another export
    // reaches the one it renames; a callee that sanitises, reaches no sink or
    // is no sink, and a caller reading no outside data, make no path.
    assert_eq!(
        paths,
        "7\n  0.90  injection_sink  src/a.js:3 takeA -> src/db.js:1 sinkDefault\n  0.90  injection_sink  src/b.js:3 takeB -> src/db.js:4 sinkNamed\n  0.90  injection_sink  src/c.js:3 takeC -> src/db.js:4 sinkNamed\n  0.90  injection_sink  src/e.js:3 takeE1 -> src/db.js:4 sinkNamed\n  0.90  injection_sink  src/e.js:6 takeE2 -> src/db.js:4 sinkNamed\n  0.90  injection_sink  src/f.js:3 takeF -> src/db.js:4 sinkNamed\n  0.90  injection_sink  src/i.js:3 takeI -> src/nodefault.js:1 sinkLocal",
        "{report}"
    );
    // Past ten, flagged files and paths are counted rather than listed.
    for i in 1..=11 {
        project.write(&format!("src/h{i}.js"), &format!("import {{ sinkNamed }} from \"./db.js\";\nexport function takeH{i}(req, db) {{\n  return sinkNamed(db, req.q);\n}}\n"));
    }
    let many = project
        .supercov_with(&["security", "src/"], &through(&base))
        .succeeds();
    contains_all(
        &many,
        &[
            "  ... and 13 more\n\nCross-file paths confirmed: 18\n",
            "src/h2.js:3 takeH2 -> src/db.js:4 sinkNamed\n  ... and 8 more\n",
        ],
    );
}

#[test]
fn a_path_seen_in_tests_is_asked_once_three_callers_a_function_and_the_rest_named() {
    let project = Project::empty("probe-observed");
    project.write("package.json", r#"{"name":"observed","type":"module"}"#);
    // A function that reaches nothing dangerous, one no route calls, and a
    // sink no test runs; and a module of no functions the tests run.
    project.write(
        "src/db.js",
        "export function sinkRun(db, v) {\n  return db.query(\"q\" + v);\n}\nexport function quietLog(db, v) {\n  return console.log(v);\n}\nexport function sinkUnused(db, v) {\n  return db.query(\"u\" + v);\n}\n",
    );
    project.write("src/consts.js", "export const limit = 1;\n");
    project.write(
        "src/fmt.js",
        "export function formatRow(v) {\n  return String(v);\n}\n",
    );
    project.write("src/handler.js", "import { sinkRun } from \"./db.js\";\nexport function takeHandler(req, db) {\n  return sinkRun(db, req.q);\n}\n");
    let mut tests = String::from(
        "import test from \"node:test\";\nimport { sinkRun, quietLog } from \"../src/db.js\";\nimport { formatRow } from \"../src/fmt.js\";\nimport { takeHandler } from \"../src/handler.js\";\nimport { limit } from \"../src/consts.js\";\n",
    );
    for i in 1..=9 {
        project.write(&format!("src/route{i}.js"), &format!("export function takeRoute{i}(req, deps) {{\n  return deps.sinkRun(deps.db, req.p);\n}}\n"));
        tests.push_str(&format!(
            "import {{ takeRoute{i} }} from \"../src/route{i}.js\";\n"
        ));
    }
    tests.push_str("const db = { query: () => 1 };\n");
    for i in 1..=9 {
        tests.push_str(&format!(
            "test(\"route {i}\", () => {{ formatRow(1); quietLog(db, 1); takeRoute{i}({{ p: 1 }}, {{ db, sinkRun }}); }});\n"
        ));
    }
    tests.push_str("test(\"route 1 again\", () => { takeRoute1({ p: 2 }, { db, sinkRun }); });\n");
    tests.push_str("test(\"handler\", () => { takeHandler({ q: limit }, db); });\n");
    project.write("test/all.test.js", &tests);
    project.git(&["init", "-q"]);
    project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_by_name);
    let report = project
        .supercov_with(&["security", "src/", "--run", "latest"], &through(&base))
        .succeeds();
    // The handler's path is the import's, not asked again for the test that
    // ran it; route 1, run twice, is one path; the other callers are named,
    // five of them listed; a function reaching no sink, and one taking no
    // outside data, make no path.
    contains_all(
        &report,
        &[
            "Cross-file paths confirmed: 4\n  0.90  injection_sink  src/handler.js:3 takeHandler -> src/db.js:1 sinkRun\n",
            "6 more caller(s) ran with a flagged function in a test and were not asked, three being asked per function:\n",
            " -> src/db.js:1 sinkRun\n  ... and 1 more\n",
        ],
    );
    assert_eq!(
        report.matches("(observed in a test)").count(),
        3,
        "{report}"
    );
    assert_eq!(report.matches("takeRoute1 -> ").count(), 1, "{report}");
    assert_eq!(
        report.matches(" -> src/db.js:1 sinkRun\n").count(),
        9,
        "{report}"
    );
    assert!(
        !report.contains("quietLog") && !report.contains("formatRow"),
        "{report}"
    );
    let json = project
        .supercov_with(
            &["security", "src/", "--run", "latest", "--json"],
            &through(&base),
        )
        .json();
    assert_eq!(
        json["paths_not_asked"].as_array().unwrap().len(),
        6,
        "{}",
        json["paths_not_asked"]
    );
}

#[test]
fn a_handler_run_without_a_guard_is_counted_by_the_tests_that_ran_it() {
    let project = Project::empty("probe-guards");
    project.write("package.json", r#"{"name":"guards","type":"module"}"#);
    project.write(
        "src/guard.js",
        "export function cleanInput(v) {\n  return String(v).replace(/[^a-z]/g, \"\");\n}\n",
    );
    project.write(
        "src/one.js",
        "export function takeOne(req) {\n  return req.q;\n}\n",
    );
    project.write(
        "src/two.js",
        "export function takeTwo(req) {\n  return req.q;\n}\n",
    );
    project.write("test/all.test.js", "import test from \"node:test\";\nimport { cleanInput } from \"../src/guard.js\";\nimport { takeOne } from \"../src/one.js\";\nimport { takeTwo } from \"../src/two.js\";\ntest(\"one bare\", () => { takeOne({ q: 1 }); });\ntest(\"one guarded\", () => { cleanInput(1); takeOne({ q: 1 }); });\ntest(\"two bare\", () => { takeTwo({ q: 1 }); });\ntest(\"two bare again\", () => { takeTwo({ q: 2 }); });\n");
    project.git(&["init", "-q"]);
    project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_by_name);
    let report = project
        .supercov_with(&["security", "src/", "--run", "latest"], &through(&base))
        .succeeds();
    contains_all(
        &report,
        &[
            "    takeOne (line 1) ran under 1 test with no guard running, and under 1 with one\n",
            "    takeTwo (line 1) ran under 2 tests with no guard running\n",
            "  Every line of the flagged files was executed by a test in run ",
        ],
    );
}

fn answer_faint(_: &str, question: &serde_json::Value) -> serde_json::Value {
    if question["type"] == "choice" {
        common::choose(question, "none")
    } else {
        serde_json::json!({ "type": "noul", "noul": 0.2 })
    }
}

fn answer_authorization_only(id: &str, question: &serde_json::Value) -> serde_json::Value {
    let task = question["instructions"]["task"]
        .as_str()
        .unwrap_or_default();
    if question["type"] == "choice" {
        return common::choose(question, "none");
    }
    let yes = id.contains("missing_authorization") || task.contains("authoriz");
    serde_json::json!({ "type": "noul", "noul": if yes { 0.9 } else { 0.1 } })
}

#[test]
fn only_checks_a_file_question_fired_on_are_asked_line_by_line() {
    let project = Project::empty("probe-localise");
    project.write("package.json", r#"{"name":"loc","type":"module"}"#);
    project.write(
        "src/ok.js",
        "export function run(req, db) {\n  return db.query(\"x\" + req.q);\n}\n",
    );
    project.write(
        "src/broken.js",
        "export function run(req, db) {\n  return db.query(\"x\" + req.q;\n}\neval(req.body);\n",
    );
    project.git(&["init", "-q"]);
    let run = |answerer: common::Answerer| {
        let (base, seen) = gateway(MODEL, answerer);
        let report = project
            .supercov_with(&["security", "src/", "--refresh"], &through(&base))
            .succeeds();
        let requests = seen.lock().unwrap().len();
        (report, requests)
    };
    // Under the line pass's threshold: two file questions, nothing more.
    let (faint, requests) = run(answer_faint);
    assert!(
        faint.starts_with("Security: 0 of 2 files flagged, 2 clean.\n"),
        "{faint}"
    );
    assert_eq!(requests, 3, "{faint}");
    // A missing authorization is the absence of a line, never asked as one.
    let (authorization, requests) = run(answer_authorization_only);
    assert!(
        authorization.contains("  missing_authorization 2\n"),
        "{authorization}"
    );
    assert_eq!(requests, 3, "{authorization}");
    // Every check fired: the file that parses is asked line by line, and the
    // one that does not is still reported.
    let (fired, requests) = run(answer_yes);
    assert!(
        fired.contains("  src/broken.js\n") && fired.contains("  src/ok.js\n"),
        "{fired}"
    );
    assert!(requests > 3, "{fired}");
}

#[test]
fn saved_assessments_say_what_a_limit_left_out_and_which_lane_holds_a_snapshot() {
    let project = Project::cart("quality-views");
    project.write("src/b.js", "export const b = (x) => x * 2;\n");
    project.commit("b");
    let (yes, _) = gateway(MODEL, answer_yes);
    let (no, _) = gateway(MODEL, answer_no);
    let assess = |args: &[&str], base: &str| -> String {
        project.supercov_with(args, &through(base)).json()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let calm = assess(&["quality", "src/", "--json"], &no);
    let fired = assess(&["quality", "src/", "--refresh", "--json"], &yes);
    let security = assess(&["security", "src/", "--json"], &yes);
    // One of two files shown, the other counted.
    for lane in ["quality", "security"] {
        let gaps = project.supercov(&[lane, "gaps", "--limit", "1"]).succeeds();
        assert!(
            gaps.starts_with("2 files with findings, 0 clean.")
                && gaps.ends_with("... and 1 more; --limit 0 lists every one\n"),
            "{lane}: {gaps}"
        );
    }
    let diff = project
        .supercov(&["quality", "diff", &calm, &fired, "--limit", "1"])
        .succeeds();
    assert!(
        diff.contains("2 declined, 0 improved")
            && diff.contains("  ... and 1 more; --limit 0 lists every one\n"),
        "{diff}"
    );
    // A security snapshot asked of quality is named as security's.
    for args in [
        vec!["quality", "file", "src/b.js", security.as_str()],
        vec!["quality", "diff", calm.as_str(), security.as_str()],
    ] {
        let wrong = project.supercov(&args).exits(2);
        assert!(
            wrong.contains(&format!(
                "{security} is a security snapshot, not a quality one; read it with `supercov security`"
            )),
            "{wrong}"
        );
    }
}

#[test]
fn assessments_and_reviews_refuse_plainly_and_say_when_nothing_changed() {
    let project = Project::cart("assess-shapes");
    let run = project.measure(&[]);
    let (base, _) = gateway(MODEL, answer_all_but_the_sum);
    let ask = |args: &[&str]| project.supercov_with(args, &through(&base));
    for workers in ["0", "two"] {
        assert!(
            ask(&["runs", &run, "assertions", "assess", "--workers", workers])
                .exits(2)
                .contains("--workers needs a positive number")
        );
    }
    let dry = ask(&["runs", &run, "assertions", "assess", "--dry-run", "--json"]).json();
    assert_eq!(dry["data"]["dryRun"], true);
    assert_eq!(dry["data"]["statements"], 8);
    // --changed reads the sources an assessment kept, so it comes second.
    assert!(
        ask(&["runs", &run, "assertions", "assess", "--changed"])
            .exits(2)
            .contains("this run's sources were not kept; assess the run first")
    );
    ask(&["runs", &run, "assertions", "assess", "--workers", "1"]).succeeds();
    assert_eq!(
        ask(&["runs", &run, "assertions", "assess", "--changed"]).succeeds(),
        format!(
            "Run {run}: no statement the run assessed has changed since it; nothing was asked.\n"
        )
    );
    // Beside an assessed run, where every executed flagged file is caught.
    assert!(
        ask(&["security", "src/", "--run", &run])
            .succeeds()
            .contains(
                "; every flagged file a test executes has a line some test is judged to catch."
            )
    );
    // Change ranges: each is its own, one at a time, and a ref must exist.
    project.edit("src/cart.js", "let sum = 0;", "let sum = 0 + 0;");
    project.git(&["add", "src/cart.js"]);
    assert!(
        ask(&["quality", "patch", "--staged"])
            .succeeds()
            .starts_with("Reviewing staged changes.\n27 checks introduced across 1 changed file.")
    );
    assert!(
        ask(&["quality", "patch", "--unstaged"])
            .succeeds()
            .contains("No changed source files to review.")
    );
    assert!(
        ask(&["quality", "patch", "--staged", "--unstaged"])
            .exits(2)
            .contains("pick one change range: --unstaged, --staged or --base <ref>")
    );
    assert!(
        ask(&["quality", "patch", "--annotate", "gitlab"])
            .exits(2)
            .contains("--annotate does not know gitlab; use github")
    );
    assert!(
        ask(&["quality", "patch", "--base", "nonexistent-ref"])
            .exits(2)
            .contains("make sure nonexistent-ref exists locally")
    );
}

/// A node:test project of these files, committed.
fn node_project(label: &str, files: &[(&str, &str)]) -> Project {
    let project = Project::empty(label);
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" } }"#,
    );
    for (file, contents) in files {
        project.write(file, contents);
    }
    project.git(&["init", "-q"]);
    project.commit(label);
    project
}

/// A file's statements not asserted, at the default page: the next page is
/// named by its offset alone.
#[test]
fn a_file_s_statements_page_at_twenty_by_default() {
    let mut body = String::from("  let a0 = x;\n");
    for i in 1..=24 {
        body.push_str(&format!("  const a{i} = a{} + {i};\n", i - 1));
    }
    let project = node_project(
        "assert-default-page",
        &[
            (
                "src/steps.js",
                &format!("export function steps(x) {{\n{body}  return a24;\n}}\n"),
            ),
            (
                "test/steps.test.js",
                "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { steps } from \"../src/steps.js\";\n\ntest(\"steps\", () => {\n  assert.ok(steps(1) > 0);\n});\n",
            ),
        ],
    );
    let (base, _) = gateway(MODEL, answer_no);
    let run = project.measure(&[]);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let total = project
        .supercov(&["runs", &run, "assertions", "src/steps.js", "--json"])
        .json()["data"]["page"]["total"]
        .as_u64()
        .unwrap();
    assert!(total > 20, "{total}");
    let file = project
        .supercov(&["runs", &run, "assertions", "src/steps.js"])
        .succeeds();
    contains_all(
        &file,
        &[
            &format!("showing 1-20 of {total}"),
            &format!("next page: supercov runs '{run}' assertions 'src/steps.js' --offset 20\n"),
        ],
    );
}

/// An assessment of a run whose tests ran none of its code asks nothing,
/// and every reading says so.
#[test]
fn an_assessment_with_nothing_to_ask_reads_as_such() {
    let project = node_project(
        "assert-nothing",
        &[
            ("src/cart.js", common::CART),
            (
                "test/noop.test.js",
                "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\n\ntest(\"noop\", () => {\n  assert.ok(true);\n});\n",
            ),
        ],
    );
    let (base, seen) = gateway(MODEL, answer_no);
    let run = project.measure(&[]);
    let assessed = project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    assert!(
        assessed.contains(&format!(
            "Run {run}: nothing asserted (0 of 0 executed statements)"
        )),
        "{assessed}"
    );
    assert!(!assessed.contains("showing "), "{assessed}");
    assert!(seen.lock().unwrap().is_empty(), "nothing was asked");
    let unknown = project
        .supercov(&["runs", &run, "assertions", "--test", "foo"])
        .exits(2);
    assert!(
        unknown.contains("no assessed test is named foo; this assessment asked no test"),
        "{unknown}"
    );
    let file = project
        .supercov(&["runs", &run, "file", "src/cart.js", "--json"])
        .json();
    assert_eq!(file["data"]["assertions"]["statements"], 0, "{file}");
    assert!(file["data"]["assertions"]["percentage"].is_null(), "{file}");
    let text = project
        .supercov(&["runs", &run, "file", "src/cart.js"])
        .succeeds();
    assert!(!text.contains("Assertions"), "{text}");
}

/// After an assessment, a test that failed must run whatever changed, and
/// one that ran a changed file but no statement the assessment asked about
/// is unassessed against the change.
#[test]
fn an_assessed_run_says_which_affected_tests_were_never_judged_against_the_change() {
    let project = Project::cart("affected-unassessed");
    project.write(
        "test/broken.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { shipping } from \"../src/cart.js\";\n\ntest(\"broken\", () => {\n  assert.equal(shipping(60, false), 1);\n});\n",
    );
    project.commit("broken");
    project.supercov(&["--", "node", "--test"]).exits(1);
    let run = project.latest();
    let (base, _) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    // A declaration added: the file's shape changed, no statement did.
    let cart = project.read("src/cart.js");
    project.write(
        "src/cart.js",
        &format!("{cart}\nexport function extra() {{\n  return 1;\n}}\n"),
    );
    let data = project
        .supercov(&["runs", &run, "tests", "affected", "--json"])
        .json()["data"]
        .clone();
    assert_eq!(data["assertions"]["changedStatements"], 0, "{data}");
    let verdicts = data["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|test| {
            (
                test["name"].as_str().unwrap().to_owned(),
                test["assertion"]["verdict"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(verdicts[0], ("broken".to_owned(), "must run".to_owned()));
    assert!(
        verdicts[1..]
            .iter()
            .all(|(_, verdict)| verdict == "unassessed"),
        "{verdicts:?}"
    );
    assert_eq!(verdicts.len(), 4, "{verdicts:?}");
    let text = project
        .supercov(&["runs", &run, "tests", "affected"])
        .succeeds();
    assert!(
        text.contains("not assessed against the changed code"),
        "{text}"
    );
    assert!(!text.contains("Assertions:"), "{text}");
}

/// On a terminal, an assessment says as it goes how many requests it has
/// sent and how many statements are still being asked.
#[cfg(unix)]
#[test]
fn an_assessment_on_a_terminal_shows_its_progress() {
    let project = Project::cart("assert-terminal");
    let run = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_no);
    let terminal = common::OnTerminal::start_with(
        &project,
        &["runs", &run, "assertions", "assess"],
        &through(&base),
    );
    let (code, shown) = terminal.finish();
    assert_eq!(code, 0, "{shown}");
    assert!(
        shown.contains("[supercov] assertions: ")
            && shown.contains(" statements still being asked"),
        "{shown}"
    );
    assert!(!seen.lock().unwrap().is_empty());
}

/// A statement asked of an earlier test and then of a later one is judged
/// by the stronger answer, whichever came first.
#[test]
fn a_later_test_s_stronger_answer_decides_a_statement() {
    let shipping = |name: &str| {
        format!(
            "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ shipping }} from \"../src/cart.js\";\n\ntest(\"{name}\", () => {{\n  assert.equal(shipping(60, false), 0);\n}});\n"
        )
    };
    let project = node_project(
        "assert-later",
        &[
            ("src/cart.js", common::CART),
            ("test/a.test.js", &shipping("a ships")),
            ("test/b.test.js", &shipping("b ships")),
        ],
    );
    let run = project.measure(&[]);
    let (no, _) = gateway(MODEL, answer_no);
    project
        .supercov_with(
            &[
                "runs",
                &run,
                "assertions",
                "assess",
                "--test-file",
                "test/a.test.js",
            ],
            &through(&no),
        )
        .succeeds();
    let (yes, _) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&yes))
        .succeeds();
    let statement = project
        .supercov(&["runs", &run, "assertions", "src/cart.js:16"])
        .succeeds();
    let caught = statement
        .find("90%  would fail   b ships (test/b.test.js)")
        .unwrap_or_else(|| panic!("{statement}"));
    let passed = statement
        .find("10%  would pass   a ships (test/a.test.js)")
        .unwrap_or_else(|| panic!("{statement}"));
    assert!(caught < passed, "{statement}");
    assert!(statement.contains("Asserted"), "{statement}");
}

/// Which files no test was asked from: every one when few, and through a
/// filter that picks tests of one name across files, the asked test's own
/// answer told from another file's test of that name.
#[test]
fn files_left_unasked_are_named_in_full_and_through_a_filter() {
    let total = |name: &str| {
        format!(
            "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ total }} from \"../src/cart.js\";\n\ntest(\"{name}\", () => {{\n  assert.ok(total([{{ price: 1, quantity: 1 }}]) >= 0);\n}});\n"
        )
    };
    // Twenty files: eighteen are asked, two are not, and both are named.
    let files = (0..20)
        .map(|i| {
            (
                format!("test/t{i:02}.test.js"),
                total(&format!("sum {i:02}")),
            )
        })
        .chain([("src/cart.js".to_owned(), common::CART.to_owned())])
        .collect::<Vec<_>>();
    let borrowed = files
        .iter()
        .map(|(file, contents)| (file.as_str(), contents.as_str()))
        .collect::<Vec<_>>();
    let few = node_project("unasked-few", &borrowed);
    let (base, _) = gateway(MODEL, answer_no);
    let run = few.measure(&[]);
    few.supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let statement = few
        .supercov(&["runs", &run, "assertions", "src/cart.js:2"])
        .succeeds();
    let named = statement
        .lines()
        .find(|line| line.contains("No test was asked from "))
        .unwrap_or_else(|| panic!("{statement}"));
    assert!(
        named.contains("Not asked: 2 test(s). No test was asked from 2 file(s) that run it: "),
        "{named}"
    );
    assert!(!named.contains(", and "), "{named}");

    // Twenty-five files, twenty of whose tests share a name, read through
    // a filter that picks them all.
    let files = (0..25)
        .map(|i| {
            let name = if i < 20 {
                "sum".to_owned()
            } else {
                format!("sum {i}")
            };
            (format!("test/t{i:02}.test.js"), total(&name))
        })
        .chain([("src/cart.js".to_owned(), common::CART.to_owned())])
        .collect::<Vec<_>>();
    let borrowed = files
        .iter()
        .map(|(file, contents)| (file.as_str(), contents.as_str()))
        .collect::<Vec<_>>();
    let many = node_project("unasked-named", &borrowed);
    let run = many.measure(&[]);
    many.supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let filtered = many
        .supercov(&[
            "runs",
            &run,
            "assertions",
            "src/cart.js:2",
            "--title",
            "sum",
        ])
        .succeeds();
    assert!(
        filtered.contains("Not asked: 7 test(s). No test was asked from 7 file(s) that run it: "),
        "{filtered}"
    );
}

/// A test whose own code is over Jev's limit is never asked: its request is
/// split down to one question, refused, and the other tests are asked in its
/// place. The reading says how many were left unasked, and the statement
/// that only the small tests were asked about says those not asked were not
/// judged.
#[test]
fn a_test_over_jev_s_limit_is_left_unasked_and_counted() {
    let project = Project::cart("assert-over-limit");
    let words = (0..30_000)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    project.write(
        "test/fixture.test.js",
        &format!(
            "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ total, shipping }} from \"../src/cart.js\";\n\ntest(\"reads the fixture\", () => {{\n  const fixture = \"{words}\";\n  assert.ok(fixture.length > 0);\n  assert.equal(total([{{ price: 10, quantity: 2 }}]), 20);\n  assert.equal(shipping(60, false), 0);\n}});\n"
        ),
    );
    project.commit("fixture");
    let run = project.measure(&[]);
    let estimate = project
        .supercov(&["runs", &run, "assertions", "assess", "--dry-run", "--json"])
        .json();
    assert!(
        estimate["data"]["firstRound"]["requests"].as_u64() > Some(0),
        "{estimate}"
    );
    let (base, seen) = gateway(MODEL, answer_no);
    let assessed = project
        .supercov_with(
            &["runs", &run, "assertions", "assess", "--json"],
            &through(&base),
        )
        .json();
    let over = assessed["data"]["summary"]["pairsOverLimit"]
        .as_u64()
        .unwrap();
    assert!(over > 0, "{assessed}");
    let asked = seen.lock().unwrap();
    assert!(!asked.is_empty());
    assert!(
        asked
            .iter()
            .all(|(_, _, request)| request["state"]["test"]["file"] != "test/fixture.test.js"),
        "the test over the limit is never sent"
    );
    let overview = project.supercov(&["runs", &run, "assertions"]).succeeds();
    assert!(
        overview.contains(&format!(
            "Not asked: {over} statement(s) of a test whose request is over Jev's limit even with one question; that test was not judged on them."
        )),
        "{overview}"
    );
    let statement = project
        .supercov(&["runs", &run, "assertions", "src/cart.js:16"])
        .succeeds();
    assert!(
        statement.contains("the tests not asked were not judged"),
        "{statement}"
    );

    // The only test is the one over the limit: no round has anything to
    // send, and the assessment still finishes and counts what it left.
    std::fs::remove_file(project.root.join("test/cart.test.js")).unwrap();
    project.commit("only the fixture");
    let alone = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_no);
    let assessed = project
        .supercov_with(
            &["runs", &alone, "assertions", "assess", "--json"],
            &through(&base),
        )
        .json();
    assert!(
        assessed["data"]["summary"]["pairsOverLimit"].as_u64() > Some(0),
        "{assessed}"
    );
    assert!(seen.lock().unwrap().is_empty(), "nothing is sent");
}

/// More tests to ask than one round sends: forty requests go in the first,
/// the last in the next. Read back by line, a run assessed over two files
/// gives the statement on that line of that file.
#[test]
fn a_round_sends_forty_requests_and_the_next_one_the_rest() {
    let mut functions = String::new();
    let mut files = Vec::new();
    for i in 0..41 {
        functions.push_str(&format!(
            "export function f{i}(x) {{\n  return x + {i};\n}}\n"
        ));
        files.push((
            format!("test/f{i:02}.test.js"),
            format!(
                "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ f{i} }} from \"../src/many.js\";\n\ntest(\"f{i}\", () => {{\n  assert.equal(f{i}(1), {});\n}});\n",
                i + 1
            ),
        ));
    }
    files.push(("src/many.js".to_owned(), functions));
    files.push(("src/cart.js".to_owned(), common::CART.to_owned()));
    files.push((
        "test/cart.test.js".to_owned(),
        common::CART_TESTS.to_owned(),
    ));
    let borrowed = files
        .iter()
        .map(|(file, contents)| (file.as_str(), contents.as_str()))
        .collect::<Vec<_>>();
    let project = node_project("assert-rounds", &borrowed);
    let run = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_yes);
    let assessed = project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let sent = seen.lock().unwrap().len();
    assert!(sent > 40, "{sent}");
    assert!(
        assessed.contains(&format!("Assessed by Jev: {sent} requests")),
        "{assessed}"
    );
    let asked = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, _, request)| request["state"]["test"]["file"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(asked.contains("test/f40.test.js"), "{asked:?}");
    let line = project
        .supercov(&["runs", &run, "line", "src/cart.js:16", "--json"])
        .json();
    let statements = line["data"]["assertions"]["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1, "{line}");
    assert_eq!(statements[0]["text"], "return 0;");
    assert_eq!(statements[0]["asserted"], true);
}

/// A report of a quality assessment alone carries each assessed file's
/// source, and names the ones it cannot: a file that became a directory, and
/// one grown past what a report embeds, before its contents are compared.
#[test]
fn a_quality_report_names_the_sources_it_cannot_embed() {
    let project = Project::cart("quality-report-omitted");
    project.write("src/tax.js", "export const tax = (sum) => sum * 0.2;\n");
    project.commit("tax");
    let (base, _) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["quality"], &through(&base))
        .succeeds();
    let report = |output: &str| {
        project
            .supercov(&["report", "--no-open", "--output", output])
            .succeeds();
        let html = project.read(output);
        // The page carries its data gzipped and base64-encoded.
        let start = html
            .find(r#"<script id="report-data" type="application/octet-stream">"#)
            .unwrap()
            + r#"<script id="report-data" type="application/octet-stream">"#.len();
        let end = start + html[start..].find("</script>").unwrap();
        use base64::Engine as _;
        use std::io::Read as _;
        let compressed = base64::engine::general_purpose::STANDARD
            .decode(&html[start..end])
            .unwrap();
        let mut json = String::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_string(&mut json)
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["qualities"][0].clone()
    };
    let whole = report("whole.html");
    assert!(whole["sources"]["src/cart.js"].is_object(), "{whole}");
    assert_eq!(whole["omittedSources"], serde_json::json!([]));
    std::fs::remove_file(project.root.join("src/cart.js")).unwrap();
    std::fs::create_dir(project.root.join("src/cart.js")).unwrap();
    project.write("src/tax.js", &"// padding\n".repeat(100_000));
    let omitted = report("omitted.html");
    assert_eq!(
        omitted["omittedSources"],
        serde_json::json!(["src/cart.js", "src/tax.js"]),
        "{}",
        omitted["omittedSources"]
    );
}

#[test]
fn a_run_that_cannot_be_read_is_named_before_anything_is_asked() {
    let project = Project::cart("bad-run");
    let (base, seen) = gateway(MODEL, answer_no);
    let jev = through(&base);
    assert!(
        project
            .supercov_with(&["security", "src/", "--run", "latest"], &jev)
            .exits(2)
            .contains("[supercov] no local coverage runs")
    );
    project.measure(&[]);
    project.edit("src/cart.js", "return 0;", "return 0; // free");
    for args in [
        &["quality", "--run", "r_nope"][..],
        &["security", "src/", "--run", "r_nope"],
        &["quality", "patch", "--run", "r_nope"],
    ] {
        assert!(
            project
                .supercov_with(args, &jev)
                .exits(2)
                .contains("[supercov] coverage run not found: r_nope"),
            "{args:?}"
        );
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "a mistyped run costs nothing"
    );
}

#[test]
fn a_snapshot_that_could_not_be_saved_is_said_so_in_every_report() {
    let project = Project::cart("save-fault");
    let (base, _) = gateway(MODEL, answer_no);
    let jev = through(&base);
    // `latest` cannot be replaced: the snapshot is saved, the pointer is not.
    project.write(".supercov/quality/snapshots/latest/held", "");
    let pointed = project.supercov_with(&["quality"], &jev);
    let id = pointed
        .succeeds()
        .split(", snapshot ")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .unwrap()
        .to_owned();
    assert!(
        pointed.stderr().contains(&format!(
            "[supercov] the assessment completed but snapshot {id} was saved, but `latest` does not name it: {}: ",
            project.root.join(".supercov/quality/snapshots/latest").display()
        )),
        "{}",
        pointed.stderr()
    );
    assert!(
        project
            .supercov(&["quality", "snapshots"])
            .succeeds()
            .contains(&id)
    );
    // No directory for snapshots: none is saved, and the listing says why.
    std::fs::remove_dir_all(project.root.join(".supercov/quality/snapshots")).unwrap();
    project.write(".supercov/quality/snapshots", "");
    let json = project.supercov_with(&["quality", "--json"], &jev);
    let warning = json.json()["snapshot_warning"].as_str().unwrap().to_owned();
    assert!(
        warning.starts_with(&format!(
            "the assessment completed but no snapshot was saved: {}: ",
            project.root.join(".supercov/quality/snapshots").display()
        )),
        "{warning}"
    );
    assert!(
        json.stderr().contains(&format!("[supercov] {warning}")),
        "{}",
        json.stderr()
    );
    assert!(
        project
            .supercov(&["quality", "snapshots"])
            .exits(2)
            .contains("[supercov] cannot read quality snapshots: ")
    );
}

#[test]
fn a_damaged_snapshot_store_is_read_for_what_is_intact_and_a_mistyped_id_is_named() {
    let project = Project::cart("store-edges");
    assert_eq!(
        project.supercov(&["quality", "clean"]).succeeds(),
        "Removed 0 saved quality assessment(s), keeping 0.\n"
    );
    assert!(
        project
            .supercov(&["quality", "show", "abcdefghijklmnopqr"])
            .exits(2)
            .contains(
                "not a snapshot id: abcdefghijklmnopqr; list them with: supercov quality snapshots"
            )
    );
    assert!(
        project
            .supercov(&["security", "show", "q_0000000000000000"])
            .exits(2)
            .contains(
                "no security snapshot q_0000000000000000 here; list them with: supercov security snapshots"
            )
    );
    // A snapshot that is a file, and one with no manifest, are not listed.
    project.write(".supercov/quality/snapshots/q_0123456789abcdef", "");
    project.write(
        ".supercov/quality/snapshots/q_fedcba9876543210/files.json",
        "{}",
    );
    assert!(
        project
            .supercov(&["quality", "snapshots"])
            .succeeds()
            .contains("No quality snapshots here yet")
    );
    #[cfg(unix)]
    {
        let made = std::process::Command::new("mkfifo")
            .arg(project.root.join("src/pipe"))
            .status()
            .unwrap();
        assert!(made.success());
        assert!(
            project
                .supercov(&["quality", "--dry-run", "src/pipe"])
                .exits(2)
                .contains("[supercov] not a regular file or directory: src/pipe")
        );
    }
    project.write("jsconfig.json", "{}");
    assert!(
        project
            .supercov(&["security", "--dry-run", "jsconfig.json"])
            .exits(2)
            .contains("[supercov] unsupported source extension: jsconfig.json")
    );
}

#[test]
fn the_scope_reads_manifests_licences_and_checkouts_and_says_what_it_left_out() {
    // A workspace pattern naming nothing, then a manifest that does not parse.
    let project = Project::empty("scope-manifests");
    project.write("package.json", r#"{"workspaces":["packages/*"]}"#);
    project.write("src/a.js", "export const a = 1;\n");
    for manifest in [r#"{"workspaces":["packages/*"]}"#, "{ broken"] {
        project.write("package.json", manifest);
        assert_eq!(
            project.supercov(&["quality", "scope"]).succeeds(),
            "Source scope, automatic mode: 1 included, 0 ambiguous, 0 excluded.\nRoots: src\n",
            "{manifest}"
        );
    }
    // A Cargo path in single quotes is a path, and a directory a workspace
    // excludes is no member.
    let cargo = Project::empty("scope-cargo");
    cargo.write(
        "Cargo.toml",
        "[package]\nname = \"x\"\n[lib]\npath = 'odd/lib.rs'\n",
    );
    cargo.write("odd/lib.rs", "pub fn a() {}\n");
    assert!(
        cargo
            .supercov(&["quality", "scope"])
            .succeeds()
            .starts_with(
                "Source scope, automatic mode: 1 included, 0 ambiguous, 0 excluded.\nRoots: odd\n"
            )
    );
    let excluded = Project::empty("scope-exclude");
    excluded.write(
        "Cargo.toml",
        "[workspace]\nmembers = []\nexclude = [\"sandbox\"]\n# see \"notes\nresolver = \"2\"\n",
    );
    excluded.write("sandbox/x.rs", "pub fn a() {}\n");
    let scope = excluded.supercov(&["quality", "scope"]).succeeds();
    contains_all(
        &scope,
        &[
            "Source scope, automatic mode: 0 included, 1 ambiguous, 0 excluded.\nRoots: (none found)\n",
            "  ambiguous  sandbox/x.rs  (unclassified first-party source)\n",
            "\n1 source file sits under no recognised source root and was not assessed. Read it with `supercov quality scope`, and declare your roots with SUPERCOV_SOURCE_ROOTS=src,app if it is first-party code.\n",
        ],
    );
    // Another checkout, a licence banner, a library name with nothing after
    // it, and a Go file in no module.
    let mixed = Project::empty("scope-mixed");
    mixed.write("package.json", r#"{"name":"p"}"#);
    mixed.write("nested/package.json", r#"{"name":"n"}"#);
    mixed.write("nested/src/n.js", "export const n = 1;\n");
    mixed.write("nested/.git", "gitdir: x\n");
    mixed.write(
        "public/js/w.js",
        "// Licensed under MIT.\nexport const w = 1;\n",
    );
    mixed.write(
        "public/js/v.js",
        "// Licensed under the Apache License 2.0\nexport const v = 1;\n",
    );
    mixed.write("public/jqueryish.js", "export const j = 1;\n");
    mixed.write("misc/main.go", "package main\nfunc main() {}\n");
    assert_eq!(
        mixed.supercov(&["quality", "scope"]).succeeds(),
        "Source scope, automatic mode: 0 included, 3 ambiguous, 2 excluded.\n\
         Roots: (none found)\n\n\
         Files left out, by reason:\n      3  unclassified first-party source\n      2  vendored third-party library\n\n\
         Not assessed:\n\
         \x20 ambiguous  misc/main.go  (unclassified first-party source)\n\
         \x20 ambiguous  nested/src/n.js  (unclassified first-party source)\n\
         \x20 ambiguous  public/jqueryish.js  (unclassified first-party source)\n\
         \x20 excluded   public/js/v.js  (vendored third-party library)\n\
         \x20 excluded   public/js/w.js  (vendored third-party library)\n\n\
         3 source files sit under no recognised source root and were not assessed. Read them with `supercov quality scope`, and declare your roots with SUPERCOV_SOURCE_ROOTS=src,app if they are first-party code.\n"
    );
    assert!(
        mixed
            .supercov(&["quality", "scope", "--limit", "1"])
            .succeeds()
            .contains(
                "  ambiguous  misc/main.go  (unclassified first-party source)\n  ... and 4 more\n"
            )
    );
}

#[test]
fn a_file_under_no_root_is_put_to_the_model_with_the_manifests_and_a_failure_is_said() {
    let project = Project::cart("scope-question");
    project.write("misc/main.go", "package main\nfunc main() {}\n");
    project.write("settings.gradle", "rootProject.name = 'cart'\n");
    project.write("App.sln", "\n");
    project.commit("misc");
    let (base, seen) = gateway(MODEL, answer_no);
    let report = project
        .supercov_with(&["quality"], &through(&base))
        .succeeds();
    assert!(
        report.contains("  1 file under no source root was read by the model from the tree: 0 ship, 1 does not.\n"),
        "{report}"
    );
    let first = seen.lock().unwrap()[0].2.clone();
    assert_eq!(
        first["state"]["manifests"], "App.sln\npackage.json\nsettings.gradle",
        "{first}"
    );
    let json = project
        .supercov_with(&["quality", "--json"], &through(&base))
        .json();
    assert_eq!(
        json["skipped"][0],
        serde_json::json!({"path": "misc/main.go", "reason": "the model reads it as not part of the product", "status": "excluded"}),
    );
    // The scope question is the first request; refused, the file stays out
    // and the report says why.
    let (refusing, _) = gateway_with(MODEL, answer_yes, |n| (n == 0).then_some((401, "{}")));
    let refused = project
        .supercov_with(&["quality", "--refresh"], &through(&refusing))
        .succeeds();
    contains_all(
        &refused,
        &[
            "  The model could not be asked which files under no source root ship: 127.0.0.1 HTTP 401: the key was refused; check TYPESAFE_API_KEY\n",
            "\n1 source file sits under no recognised source root and was not assessed.",
        ],
    );
}

#[test]
fn a_snapshot_that_scored_nothing_reads_as_such_and_diffs_against_one_that_did() {
    let project = Project::empty("generated-only");
    project.write("package.json", r#"{"name":"gen","type":"module"}"#);
    project.write(
        "src/gen.js",
        "// Code generated by x. DO NOT EDIT.\nexport const a = 1;\n",
    );
    project.git(&["init", "-q"]);
    let (no, _) = gateway(MODEL, answer_no);
    let first = project
        .supercov_with(&["quality", "--json"], &through(&no))
        .json();
    let first = first["id"].as_str().unwrap().to_owned();
    assert!(
        project
            .supercov(&["quality", "show"])
            .succeeds()
            .starts_with("Quality not scored: no file was assessed.\n")
    );
    project.write("src/real.js", "export const b = (x) => x + 1;\n");
    let second = project
        .supercov_with(&["quality", "--json"], &through(&no))
        .json();
    let second = second["id"].as_str().unwrap().to_owned();
    let diff = project
        .supercov(&["quality", "diff", &first, &second])
        .succeeds();
    assert!(!diff.contains("Health"), "{diff}");
    contains_all(
        &diff,
        &[
            "The snapshots assessed no file in common.",
            "Added:",
            "src/real.js",
        ],
    );
    let (yes, _) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["quality", "--refresh"], &through(&yes))
        .succeeds();
    let shown = project.supercov(&["quality", "show"]).succeeds();
    assert!(
        shown.contains("   weak  src/real.js\n         complex_conditional, "),
        "{shown}"
    );
    assert!(!shown.contains("... and"), "{shown}");
}

#[test]
fn saved_security_assessments_list_flagged_files_failures_and_what_changed() {
    let project = shop("security-views");
    std::fs::write(project.root.join("src/bad.js"), [0xff, 0xfe]).unwrap();
    project.git(&["init", "-q"]);
    let (no, _) = gateway(MODEL, answer_no);
    let (yes, _) = gateway(MODEL, answer_yes);
    let assess = |base: &str, refresh: bool| -> String {
        let mut args = vec!["security", "src/", "--json"];
        if refresh {
            args.push("--refresh");
        }
        project.supercov_with(&args, &through(base)).json()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let calm = assess(&no, false);
    let shown = project.supercov(&["security", "show"]).succeeds();
    assert!(
        shown.starts_with("Security: 0 of 2 files flagged, 2 clean.\nCatalog "),
        "{shown}"
    );
    let fired = assess(&yes, true);
    let shown = project.supercov(&["security", "show"]).succeeds();
    contains_all(&shown, &["Flagged:", "src/handler.js", "src/users.js"]);
    assert!(!shown.contains("src/bad.js"), "{shown}");
    assert!(
        project
            .supercov(&["security", "show", "--limit", "1"])
            .succeeds()
            .contains("  ... and 2 more files"),
        "{}",
        project
            .supercov(&["security", "show", "--limit", "1"])
            .stdout()
    );
    assert!(
        project
            .supercov(&["security", "gaps"])
            .succeeds()
            .contains("src/bad.js")
    );
    let diff = project
        .supercov(&["security", "diff", &calm, &fired])
        .succeeds();
    assert!(!diff.contains("Health"), "{diff}");
    contains_all(&diff, &["Declined:", "appeared"]);
}

#[test]
fn a_line_classified_without_confidence_or_entered_nowhere_stays_as_the_file_question_left_it() {
    let project = shop("security-confidence");
    fn faint_lines(id: &str, question: &serde_json::Value) -> serde_json::Value {
        let mut answer = common::answer_reviewer(id, question);
        if id.starts_with('n') {
            answer["confidence"] = serde_json::json!(0.2);
        }
        answer
    }
    let (base, _) = gateway(MODEL, faint_lines);
    let report = project
        .supercov_with(&["security", "src/handler.js"], &through(&base))
        .succeeds();
    contains_all(
        &report,
        &[
            "Security: 1 of 1 file flagged, 0 clean; 0 confirmed at a line.",
            "0.90  injection_sink  (file-level only)",
        ],
    );
    assert!(!report.contains("line 9"), "{report}");
    fn entered_nowhere(id: &str, question: &serde_json::Value) -> serde_json::Value {
        if id
            .strip_prefix('s')
            .is_some_and(|n| n.parse::<usize>().is_ok())
        {
            common::choose(question, "same")
        } else {
            common::answer_reviewer(id, question)
        }
    }
    let (base, _) = gateway(MODEL, entered_nowhere);
    let report = project
        .supercov_with(
            &["security", "src/handler.js", "--refresh"],
            &through(&base),
        )
        .succeeds();
    assert!(report.contains("line 9  0.90  db.query("), "{report}");
    assert!(!report.contains("(enters at line"), "{report}");
}

#[test]
fn two_findings_of_one_check_far_apart_are_both_shown() {
    let project = shop("security-far");
    project.edit(
        "src/handler.js",
        "  res.send(",
        "  res.send(\"\");\n  //\n  //\n  //\n  //\n  //\n  //\n  //\n  //\n  //\n  //\n  //\n  const again = await db.query(\"SELECT * FROM t WHERE x = \" + req.query.x);\n  res.send(",
    );
    let (base, _) = gateway(MODEL, answer_reviewer);
    let report = project
        .supercov_with(&["security", "src/handler.js"], &through(&base))
        .succeeds();
    contains_all(
        &report,
        &[
            "line 9  0.90  db.query(\"SELECT * FROM orders",
            "line 24  0.90  db.query(\"SELECT * FROM t WHERE x",
        ],
    );
}

#[test]
fn a_file_every_window_of_which_is_refused_is_one_error() {
    let project = Project::empty("windows-refused");
    project.write("package.json", r#"{"name":"big","type":"module"}"#);
    let mut source = String::new();
    for i in 0..1600 {
        source.push_str(&format!(
            "export function f{i}(a, b) {{\n  if (a > {i} && b < {i}) {{\n    return a * b + {i};\n  }}\n  return eval(\"a\" + b);\n}}\n\n"
        ));
    }
    project.write("src/big.js", &source);
    let (base, _) = gateway_with(MODEL, answer_yes, |_| Some((401, "{}")));
    let report = project
        .supercov_with(&["quality", "src/big.js"], &through(&base))
        .exits(2);
    assert_eq!(
        report.matches("  error  src/big.js  (127.0.0.1 HTTP 401: the key was refused; check TYPESAFE_API_KEY)\n").count(),
        1,
        "{report}"
    );
}

#[test]
fn an_unreadable_cache_entry_in_either_lane_is_named_with_its_place() {
    let project = shop("cache-faults");
    let (base, _) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["security", "src/users.js"], &through(&base))
        .succeeds();
    let requests = project.root.join(".supercov/security/requests");
    let entries: Vec<std::path::PathBuf> = std::fs::read_dir(&requests)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    for entry in &entries {
        std::fs::write(entry, "not json").unwrap();
    }
    let report = project
        .supercov_with(&["security", "src/users.js"], &through(&base))
        .exits(2);
    assert!(
        report.contains("  error  src/users.js  (invalid security cache; use --refresh)"),
        "{report}"
    );
    for entry in &entries {
        std::fs::remove_file(entry).unwrap();
        std::fs::create_dir(entry).unwrap();
    }
    let report = project
        .supercov_with(&["security", "src/users.js"], &through(&base))
        .exits(2);
    assert!(
        report.contains(&format!(
            "  error  src/users.js  (cannot read the security cache {}: ",
            entries[0].display()
        )) && report.contains("; use --refresh)"),
        "{report}"
    );
}

#[test]
fn the_line_pass_reads_files_no_parser_knows_and_offers_only_lines_that_hold_code() {
    let project = Project::empty("line-edges");
    project.write("package.json", r#"{"name":"edges","type":"module"}"#);
    project.write("src/plain.go", "package p\nfunc A() int { return 1 }\n");
    project.write("src/routes.rb", "app.get(\"/orders\") { |req| req }\n");
    project.write(
        "src/q.go",
        "package q\n\n// look up an order\nfunc Find(db DB, id string) { db.query(\"SELECT * FROM o WHERE id = \" + id) }\n",
    );
    project.write(
        "src/q.rb",
        "# find an order\ndb.query(\"SELECT md5(\" + params[:id] + \")\")\n",
    );
    project.write(
        "src/top.js",
        "db.query(\"SELECT \" + process.argv[2]);\nexport function later(req, db) {\n  return 1;\n}\n",
    );
    project.write(
        "src/klass.js",
        "export default class {\n  #run(req, db) { return db.query(\"SELECT \" + req.q); }\n}\n",
    );
    project.git(&["init", "-q"]);
    let (base, seen) = gateway(MODEL, answer_reviewer);
    let report = project
        .supercov_with(&["security", "src/"], &through(&base))
        .succeeds();
    let entered = |path: &str| -> Vec<String> {
        let seen = seen.lock().unwrap();
        let confirm = seen
            .iter()
            .map(|(_, _, request)| request)
            .find(|request| {
                request["state"]["file"]["path"] == path && request["questions"]["c0"].is_object()
            })
            .unwrap_or_else(|| panic!("no line asked of {path}"));
        confirm["questions"]["s0"]["criteria"]
            .as_object()
            .map(|options| options.keys().cloned().collect())
            .unwrap_or_default()
    };
    // Where the outside value enters: a line that holds code, never a blank
    // line or a comment, and in JavaScript `#` starts a private member.
    assert_eq!(entered("src/q.go"), ["L1", "L4", "same", "none"]);
    assert_eq!(entered("src/q.rb"), ["L2", "same", "none"]);
    assert_eq!(entered("src/top.js"), ["L1", "same", "none"]);
    assert_eq!(entered("src/klass.js"), ["L2", "same", "none"]);
    contains_all(
        &report,
        &[
            "          line 2  0.90  db.query(\"SELECT \" + req.q)  (enters at line 2)\n",
            "    0.90  weak_cryptography\n          line 2  0.90  db.query(\"SELECT md5(\" + params[:id] + \")\")",
            "  src/routes.rb\n    0.90  destination_from_input  (file-level only)\n",
            "  src/plain.go\n    0.90  destination_from_input  (file-level only)\n    0.90  injection_sink  (file-level only)\n",
        ],
    );
    assert!(
        report.contains("    0.90  missing_authorization  (file-level only)\n    0.90  path_from_input  (file-level only)\n    0.90  secret_in_source  (file-level only)\n    0.90  sensitive_data_exposure  (file-level only)\n    0.90  unchecked_mass_assignment  (file-level only)\n    0.90  unescaped_output  (file-level only)\n    0.90  unsafe_code_execution  (file-level only)\n    0.90  weak_authentication  (file-level only)\n    0.90  weak_cryptography  (file-level only)\n\nNarrow"),
        "routes.rb, last, has no line confirmed: {report}"
    );
}

#[test]
fn security_beside_a_run_whose_tests_catch_nothing_counts_each_executed_flagged_file() {
    let project = shop("security-unasserted");
    project.write(
        "test/handler.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { handler } from \"../src/handler.js\";\n\ntest(\"a missing page is refused\", async () => {\n  const db = { query: async () => [] };\n  const req = { query: { id: \"1\", file: \"a\", title: \"t\" }, params: { name: \"no-such-page\" } };\n  await assert.rejects(handler(req, { send() {} }, db), /ENOENT/);\n});\n",
    );
    project.write(
        "src/unused.js",
        "export function unused(a) {\n  return eval(a);\n}\n",
    );
    project.git(&["init", "-q"]);
    let run = project.measure(&[]);
    let (no, _) = gateway(MODEL, answer_no);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&no))
        .succeeds();
    let (yes, _) = gateway(MODEL, answer_yes);
    let report = project
        .supercov_with(&["security", "src/", "--run", "latest"], &through(&yes))
        .succeeds();
    contains_all(
        &report,
        &[
            "; 2 are executed by tests that are not judged to catch any line of them.\n",
            "  src/unused.js\n",
        ],
    );
    assert_eq!(
        report
            .matches(
                "    executed by tests, but no test is judged to catch a change to any line of it\n"
            )
            .count(),
        2,
        "{report}"
    );
    let calm = project
        .supercov_with(
            &["security", "src/", "--run", "latest", "--refresh"],
            &through(&no),
        )
        .succeeds();
    assert!(
        calm.starts_with("Security: 0 of 3 files flagged, 3 clean.\n"),
        "{calm}"
    );
}

#[test]
fn a_patch_review_reads_a_large_change_as_its_diff_and_names_what_it_cannot_read() {
    let project = Project::cart("patch-sizes");
    let mut source = String::new();
    for i in 0..1600 {
        source.push_str(&format!(
            "export function f{i}(a, b) {{\n  if (a > {i} && b < {i}) {{\n    return a * b + {i};\n  }}\n  return eval(\"a\" + b);\n}}\n\n"
        ));
    }
    project.write("src/big.js", &source);
    project.commit("big");
    let (base, _) = gateway(MODEL, answer_yes);
    let jev = through(&base);
    // A large file changed in one line is read as the diff.
    project.edit("src/big.js", "return a * b + 7;", "return a * b + 8;");
    let review = project
        .supercov_with(&["quality", "patch", "src/big.js"], &jev)
        .succeeds();
    assert!(
        review.contains("(unified diff only: both versions exceed the request budget)"),
        "{review}"
    );
    // A security review of it too.
    let review = project
        .supercov_with(&["security", "patch", "src/big.js"], &jev)
        .succeeds();
    assert!(
        review.contains("(unified diff only: both versions exceed the request budget)"),
        "{review}"
    );
    // A large new file has no previous version, and its diff is the file.
    project.write("src/huge.js", &source);
    let refused = project
        .supercov_with(&["quality", "patch", "src/huge.js"], &jev)
        .exits(2);
    assert!(
        refused.contains("over the 30000 token request budget, and so is its diff; a change cannot be split into windows, but `supercov quality src/huge.js` reads the file in them"),
        "{refused}"
    );
    std::fs::remove_file(project.root.join("src/huge.js")).unwrap();
    project.git(&["checkout", "-q", "src/big.js"]);
    // A generated file is left out unless all are asked for.
    project.write("src/gen.js", "// @generated\nexport const g = 1;\n");
    project.edit("src/cart.js", "return 0;", "return 0 ;");
    let skipped = |args: &[&str]| -> Vec<serde_json::Value> {
        project.supercov_with(args, &jev).json()["skipped"]
            .as_array()
            .unwrap()
            .clone()
    };
    assert!(
        skipped(&["quality", "patch", "--json"])
            .contains(&serde_json::json!({"path": "src/gen.js", "reason": "generated"}))
    );
    let all = project
        .supercov_with(&["quality", "patch", "--all", "--json"], &jev)
        .json();
    assert!(
        all["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "src/gen.js"),
        "{}",
        all["files"]
    );
    std::fs::remove_file(project.root.join("src/gen.js")).unwrap();
    // A file emptied is no deletion, and neither introduces anything.
    project.write("src/cart.js", "");
    assert_eq!(
        skipped(&["quality", "patch", "--json"]),
        [
            serde_json::json!({"path": "src/cart.js", "reason": "emptied; an empty file cannot introduce anything"})
        ]
    );
    assert!(
        project
            .supercov_with(&["quality", "patch"], &jev)
            .succeeds()
            .contains("No changed source files to review.")
    );
}

#[test]
fn a_clean_branch_is_reviewed_against_the_branch_its_remote_names() {
    let project = Project::cart("patch-automatic");
    project.git(&["branch", "-M", "main"]);
    project.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
    ]);
    // Neither a tool's hidden directory nor a blank name makes the tree dirty.
    project.write(".notes/todo.txt", "later\n");
    project.write(" ", "");
    let (base, _) = gateway(MODEL, answer_no);
    let review = project
        .supercov_with(&["quality", "patch"], &through(&base))
        .succeeds();
    assert!(
        review.starts_with("Reviewing changes since this branch left main.\n"),
        "{review}"
    );
}

#[cfg(unix)]
#[test]
fn python_is_read_by_its_own_parser_and_by_patterns_when_that_cannot_run() {
    let project = Project::empty("python-lines");
    project.write("pyproject.toml", "[project]\nname = \"app\"\n");
    project.write(
        "src/app.py",
        "from flask import request\n\ndef view(db):\n    rows = db.query(\"SELECT \" + request.args[\"id\"])\n    return rows\n",
    );
    project.write("src/bad.py", "def broken(:\n    db.query(\"x\" + y)\n");
    project.git(&["init", "-q"]);
    let classified = |seen: &common::Seen, path: &str| {
        seen.lock().unwrap().iter().any(|(_, _, request)| {
            request["state"]["file"]["path"] == path && request["questions"]["n0"].is_object()
        })
    };
    let (base, seen) = gateway(MODEL, answer_reviewer);
    let report = project
        .supercov_with(&["security", "src/"], &through(&base))
        .succeeds();
    // The parser's nodes are classified; a file it cannot parse is read by
    // the patterns.
    assert!(classified(&seen, "src/app.py"), "{report}");
    assert!(!classified(&seen, "src/bad.py"), "{report}");
    contains_all(
        &report,
        &[
            "line 4  0.90  rows = db.query(\"SELECT \" + request.args[\"id\"])",
            "line 2  0.90  db.query(\"x\" + y)",
        ],
    );
    // An interpreter that fails leaves every file to the patterns.
    project.write_script("fakebin/python3", "#!/bin/sh\nexit 1\n");
    let path = format!(
        "{}:{}",
        project.root.join("fakebin").display(),
        std::env::var("PATH").unwrap()
    );
    let (base, seen) = gateway(MODEL, answer_reviewer);
    let mut env = through(&base).to_vec();
    env.push(("PATH", &path));
    let report = project
        .supercov_with(&["security", "src/app.py", "--refresh"], &env)
        .succeeds();
    assert!(!classified(&seen, "src/app.py"), "{report}");
    assert!(
        report.contains("line 4  0.90  rows = db.query(\"SELECT \" + request.args[\"id\"])"),
        "{report}"
    );
}

#[test]
fn snapshots_an_older_release_wrote_are_still_read() {
    let project = Project::empty("legacy-snapshots");
    let snapshot = |lane: &str, id: &str, manifest: &str, files: &str| {
        project.write(
            &format!(".supercov/{lane}/snapshots/{id}/manifest.json"),
            manifest,
        );
        project.write(
            &format!(".supercov/{lane}/snapshots/{id}/files.json"),
            files,
        );
    };
    // The rubric, before the catalog: no instrument, and a deepened scan.
    snapshot(
        "quality",
        "q_aaaaaaaaaaaaaaaa",
        r#"{"created_at":"2026-01-01T00-00-00-000000Z","paths":["src"],"counts":{"assessed":1,"errors":0,"declarations":3},"parent":"q_bbbbbbbbbbbbbbbb","deepened":["src/a.js"]}"#,
        r#"{"files":[{"path":"src/a.js","status":"completed"}]}"#,
    );
    // The catalog, before a file recorded what fired.
    snapshot(
        "quality",
        "q_cccccccccccccccc",
        r#"{"created_at":"2026-02-01T00-00-00-000000Z","instrument":"catalog","catalog_version":"properties-v1","model":"m","paths":["src"],"counts":{"assessed":1,"errors":0,"scored":1},"health":5.0}"#,
        r#"{"files":[{"path":"src/a.js","status":"completed","health":5.0,"checks":{"god_class":0.9,"dead_code":0.1}}]}"#,
    );
    // Security, before the totals by check.
    snapshot(
        "security",
        "q_dddddddddddddddd",
        r#"{"created_at":"2026-02-01T00-00-00-000000Z","instrument":"security","catalog_version":"security-v4","model":"m","paths":["src"],"counts":{"assessed":1,"errors":0,"flagged":1}}"#,
        r#"{"files":[{"path":"src/a.js","status":"completed","present":[{"check":"injection_sink","value":0.9}],"checks":{"injection_sink":0.9}}]}"#,
    );
    assert!(
        project
            .supercov(&["quality", "snapshots"])
            .succeeds()
            .ends_with("\nq_aaaaaaaaaaaaaaaa — 2026-01-01T00-00-00-000000Z\n   1 assessed, 0 errors; scanned src\n   3 declarations assessed, deepened from q_bbbbbbbbbbbbbbbb: src/a.js\n")
    );
    assert!(
        project
            .supercov(&["quality", "gaps", "q_aaaaaaaaaaaaaaaa"])
            .exits(2)
            .contains("snapshot q_aaaaaaaaaaaaaaaa was written by the rubric, which has no findings to narrow to; run `supercov quality` to assess with the catalog")
    );
    assert!(
        project
            .supercov(&["quality", "file", "src/a.js", "q_cccccccccccccccc"])
            .succeeds()
            .starts_with("src/a.js  quality fair (5.0/10)\n\n  yes  0.90  god_class\n   no  0.10  dead_code\n")
    );
    assert_eq!(
        project.supercov(&["security", "show"]).succeeds(),
        "Security: 1 of 1 file flagged, 0 clean.\nCatalog security-v4, model m, snapshot q_dddddddddddddddd.\n\nFlagged:\n  src/a.js\n    0.90  injection_sink\n\nNarrow with `security gaps`, or read one with `security file <path>`.\n"
    );
}

#[cfg(unix)]
#[test]
fn an_assessment_whose_store_cannot_be_written_reports_and_says_what_was_not_saved() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: reads this process's own user id.
    if unsafe { libc::geteuid() } == 0 {
        // Permissions do not stop root, and nothing else can fail this way.
        return;
    }
    let project = shop("store-unwritable");
    let store = project.root.join(".supercov");
    std::fs::create_dir_all(store.join("quality/requests")).unwrap();
    std::fs::create_dir_all(store.join("quality/snapshots")).unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (base, _) = gateway(MODEL, answer_no);
    let ran = project.supercov_with(&["quality", "src/users.js", "--json"], &through(&base));
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = ran.json();
    let denied = format!("{}: Permission denied", store.join(".gitignore").display());
    assert!(
        report["files"][0]["warning"]
            .as_str()
            .unwrap()
            .starts_with(&format!(
                "assessment completed but cache could not be saved: {denied}"
            )),
        "{}",
        report["files"][0]
    );
    assert!(
        report["snapshot_warning"]
            .as_str()
            .unwrap()
            .starts_with(&format!(
                "the assessment completed but no snapshot was saved: {denied}"
            )),
        "{}",
        report["snapshot_warning"]
    );
}

/// A declaration too large to cut, near the budget, whose last line's
/// question of where its value enters does not fit beside it: the report
/// says that line was not checked one by one, where it once said nothing.
#[test]
fn a_line_whose_question_does_not_fit_is_said_to_be_unchecked() {
    let project = Project::empty("unchecked-line");
    project.write("package.json", r#"{"name":"big","type":"module"}"#);
    let mut source = String::from("export function run(a) {\n");
    for i in 0..150 {
        source.push_str(&format!(
            "  const note{i} = \"{}\";\n",
            "a plain word ".repeat(42)
        ));
    }
    source.push_str("  return eval(a);\n}\n");
    project.write("src/big.js", &source);
    let (base, _) = gateway(MODEL, common::answer_findings);
    let report = project
        .supercov_with(&["security", "src/big.js"], &through(&base))
        .succeeds();
    assert!(
        report.contains("line(s) not checked one by one: a request asking about them did not fit beside the code, or was refused"),
        "{report}"
    );
    let json = project
        .supercov(&["security", "file", "src/big.js", "--json"])
        .json();
    assert_eq!(json["unchecked_lines"], 1, "{json}");
    let file = project
        .supercov(&["security", "file", "src/big.js"])
        .succeeds();
    assert!(file.contains("1 line(s) not checked one by one"), "{file}");

    // Lines that could not be classified at all are counted the same way.
    let small = Project::empty("unclassified-lines");
    small.write("package.json", r#"{"name":"small","type":"module"}"#);
    small.write(
        "src/run.js",
        "export function run(a) {\n  return eval(a);\n}\n",
    );
    let unclassified = small.supercov_with(
        &["security", "src/run.js"],
        &[
            through(&base)[0],
            through(&base)[1],
            through(&base)[2],
            ("SUPERCOV_TEST_FAULT", "classify-over-budget"),
        ],
    );
    assert_eq!(unclassified.code(), 0, "{}", unclassified.stderr());
    assert!(
        unclassified
            .stderr()
            .contains("test fault at classify-over-budget"),
        "{}",
        unclassified.stderr()
    );
    assert!(
        unclassified
            .stdout()
            .contains("1 line(s) not checked one by one"),
        "{}",
        unclassified.stdout()
    );
}

/// A suite whose tests reach no statement: there is nothing to assess, so
/// nothing is asked, and what is read back says so.
#[test]
fn a_run_with_no_statement_to_assess_asks_nothing() {
    let project = Project::empty("nothing-to-assess");
    project.write(
        "package.json",
        r#"{ "name": "empty", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/unused.js",
        "export const unused = (v) => (v ? 1 : 2);\n",
    );
    project.write(
        "test/constant.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\ntest(\"constant\", () => assert.equal(1, 1));\n",
    );
    project.git(&["init", "-q"]);
    project.commit("nothing");
    let run = project.measure(&[]);
    let estimate = project
        .supercov(&["runs", &run, "assertions", "assess", "--dry-run", "--json"])
        .json();
    assert_eq!(estimate["data"]["atMost"]["requests"], 0, "{estimate}");
    let (base, seen) = gateway(MODEL, answer_no);
    let assessed = project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    contains_all(
        &assessed,
        &[
            "nothing asserted (0 of 0 executed statements)",
            "0 requests",
        ],
    );
    assert!(seen.lock().unwrap().is_empty(), "{assessed}");
    let read = project.supercov(&["runs", &run, "assertions"]).succeeds();
    assert!(
        read.contains("nothing asserted (0 of 0 executed statements)"),
        "{read}"
    );
    let json = project
        .supercov(&["runs", &run, "assertions", "--json"])
        .json();
    assert_eq!(json["ok"], true, "{json}");
}

/// Each place a Jev command touches a file fails in turn, its first call
/// there (`SUPERCOV_TEST_FAULT=fs#N`): the command may stop, but says why and
/// never panics, and the same command with no fault succeeds after it.
#[test]
fn a_jev_command_whose_file_operation_fails_says_so_and_recovers() {
    let (base, _seen) = gateway(MODEL, answer_yes);
    let jev = through(&base);
    let commands: [&[&str]; 10] = [
        &["security", "patch"],
        &["quality", "--dry-run"],
        &["quality"],
        &["quality", "src/cart.js"],
        &["quality", "src"],
        &["quality", "gaps"],
        &["quality", "file", "src/cart.js"],
        &["security"],
        &["quality", "clean"],
        &["quality", "patch"],
    ];
    let project = Project::cart("jev-faults");
    for command in commands {
        let mut nth = 1;
        loop {
            let fault = format!("fs#{nth}");
            let mut env = jev.to_vec();
            env.push(("SUPERCOV_TEST_FAULT", &fault));
            let ran = project.supercov_with(command, &env);
            let said = ran.stderr();
            assert!(
                !said.contains("panicked at") && ran.code() != 101,
                "{command:?} with {fault} panicked:\n{said}"
            );
            if !said.contains("[supercov] test fault at fs") {
                break;
            }
            nth += 1;
        }
        project.supercov_with(command, &jev).succeeds();
    }
}

/// A large file read in windows, with some of its requests refused: what
/// was answered is reported, what was refused is named, and nothing stops
/// half way. Every third request after the first is refused, file windows
/// and line confirmations alike.
#[test]
fn a_windowed_review_with_refused_requests_reports_what_was_answered() {
    let project = Project::empty("windows-refused");
    project.write("package.json", r#"{"name":"big","type":"module"}"#);
    let mut source = String::new();
    for i in 0..1600 {
        source.push_str(&format!(
            "export function f{i}(a, b) {{\n  if (a > {i} && b < {i}) {{\n    return a * b + {i};\n  }}\n  return eval(\"a\" + b);\n}}\n\n"
        ));
    }
    project.write("src/big.js", &source);
    let (base, seen) = gateway_with(MODEL, answer_yes, |n| {
        (n % 3 == 2).then_some((400, "refused"))
    });
    for command in ["security", "quality"] {
        let ran = project.supercov_with(&[command, "src/big.js", "--refresh"], &through(&base));
        let said = format!("{}{}", ran.stdout(), ran.stderr());
        assert!(!said.contains("panicked at"), "{command}: {said}");
        assert!(said.contains("src/big.js"), "{command}: {said}");
    }
    // A refused line question leaves its lines unchecked, said as such.
    let report = project
        .supercov_with(&["security", "src/big.js", "--refresh"], &through(&base))
        .succeeds();
    assert!(
        report.contains("line(s) not checked one by one"),
        "{report}"
    );
    assert!(seen.lock().unwrap().len() > 10);
}

/// An answer cut off part way is named, not read as an answer.
#[test]
fn an_answer_cut_off_part_way_is_named() {
    let project = Project::cart("jev-cut");
    let (base, _seen) = gateway_with(MODEL, answer_yes, |_| Some((200, "cut")));
    let ran = project.supercov_with(&["quality", "src/cart.js"], &through(&base));
    let said = format!("{}{}", ran.stdout(), ran.stderr());
    assert!(
        said.contains("failed to read the response from 127.0.0.1"),
        "{said}"
    );
}

/// Each way a handler can import what reaches a sink: a named default
/// export, an anonymous one, every export of a module, a module with no sink
/// and a module that is not there; and a file that only imports.
#[test]
fn security_follows_each_kind_of_import() {
    let project = Project::empty("security-imports");
    project.write("package.json", r#"{"name":"orders","type":"module"}"#);
    project.write(
        "src/handler.js",
        "import save from \"./save.js\";\nimport * as store from \"./store.js\";\nimport remove from \"./remove.js\";\nimport { nothing } from \"./plain.js\";\nimport missing from \"./missing.js\";\n\nexport function handler(req, db) {\n  save(db, req.body.name);\n  store.findOrders(db, req.query.id);\n  remove(db, req.params.id);\n  missing(req);\n  return nothing();\n}\n",
    );
    project.write(
        "src/save.js",
        "export default function findOrders(db, name) {\n  return db.query(\"INSERT \" + name);\n}\n",
    );
    project.write(
        "src/remove.js",
        "export default function (db, id) {\n  return db.query(\"DELETE \" + id);\n}\n",
    );
    project.write(
        "src/store.js",
        "export function findOrders(db, id) {\n  return db.query(\"UPDATE \" + id);\n}\n",
    );
    project.write(
        "src/plain.js",
        "export function nothing() {\n  return 1;\n}\n",
    );
    project.write(
        "src/only.js",
        "import { findOrders } from \"./store.js\";\nexport { findOrders };\n",
    );
    let (base, _seen) = gateway(MODEL, answer_taint);
    let report = project
        .supercov_with(&["security", "src/"], &through(&base))
        .succeeds();
    contains_all(
        &report,
        &[
            "Cross-file paths confirmed",
            "src/handler.js:8 handler -> src/save.js:1 findOrders",
            "src/handler.js:9 handler -> src/store.js:1 findOrders",
        ],
    );
}

/// A project whose files the scope leaves out has nothing to assess, and a
/// file one line of which is over the request budget is named as unreadable,
/// saying why, rather than sent.
#[test]
fn nothing_in_scope_is_assessed_and_a_line_too_long_to_ask_is_named() {
    let (base, _seen) = gateway(MODEL, answer_yes);
    let jev = through(&base);
    let tests_only = Project::empty("jev-tests-only");
    tests_only.write("package.json", r#"{ "name": "t", "type": "module" }"#);
    tests_only.write(
        "test/a.test.js",
        "import test from \"node:test\";\ntest(\"a\", () => {});\n",
    );
    tests_only.git(&["init", "-q"]);
    contains_all(
        &tests_only.supercov_with(&["quality"], &jev).succeeds(),
        &[
            "Quality not scored: no file was assessed.\n",
            "1 of 1 not assessed are test or generated",
        ],
    );
    let minified = Project::empty("jev-minified");
    minified.write("package.json", r#"{ "name": "m", "type": "module" }"#);
    minified.write(
        "src/blob.js",
        &format!("export const blob = \"{}\";\n", "x".repeat(3_000_000)),
    );
    minified.git(&["init", "-q"]);
    assert!(
        minified
            .supercov_with(&["quality", "src/blob.js"], &jev)
            .exits(2)
            .contains("error  src/blob.js  (file is over the request budget and line 1 alone is too; it is probably minified or embeds data)")
    );
}
