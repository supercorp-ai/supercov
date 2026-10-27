//! The public commands, end to end: a node:test project in a git repository,
//! measured by the binary Cargo built, and read back the way a user reads it.
//! Each scenario asserts what the user sees -- numbers, file names, exit
//! codes -- rather than how the binary arrives at it.

mod common;

use common::{Project, contains_all};

#[test]
fn a_measured_suite_reads_back_through_every_query() {
    let project = Project::cart("queries");
    let measured = project.supercov(&["--", "node", "--test"]);
    let output = measured.succeeds();
    contains_all(
        &output,
        &["total adds prices", "pass 3", "[coverage] evidence:"],
    );
    let run = project.latest();

    let listed = project.supercov(&["runs"]).succeeds();
    contains_all(
        &listed,
        &[&run, "76.92%", "50.00%", "20.00%", "showing 1-1 of 1"],
    );

    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            &format!("run {run}"),
            "command: node --test",
            "Lines      76.92% (10/13)",
            "Branches   50.00% (4/8)",
            "MC/DC      20.00% (1/5)",
            "not assessed",
            "Exact for 3 test(s)",
            "Passed      3",
        ],
    );
    let by_id = project.supercov(&["runs", &run]).succeeds();
    assert!(by_id.contains("Lines      76.92% (10/13)"), "{by_id}");

    let gaps = project.supercov(&["runs", "latest", "gaps"]).succeeds();
    contains_all(
        &gaps,
        &[
            "src/cart.js",
            "lines 3  statements 2  functions 2  branch outcomes 4  MC/DC conditions 4",
        ],
    );
    let files = project.supercov(&["runs", "latest", "files"]).succeeds();
    assert!(files.contains("src/cart.js"), "{files}");

    let file = project
        .supercov(&["runs", "latest", "file", "src/cart.js"])
        .succeeds();
    contains_all(
        &file,
        &[
            "Lines not executed              3",
            "Functions not called            2",
            "Tests touching this file: 3",
            "zero-iteration outcome not observed",
            "`express` was never true; needs `express` true, decision true",
            "`express` was never evaluated\n",
        ],
    );

    let line = project
        .supercov(&["runs", "latest", "line", "src/cart.js:14"])
        .succeeds();
    contains_all(&line, &["NOT COVERED", "return express ? 15 : 5;", "None"]);

    let decision = project
        .supercov(&["runs", "latest", "decision", "src/cart.js:13"])
        .succeeds();
    contains_all(
        &decision,
        &[
            "express || sum < 50",
            "C1 MISSING: express",
            "C2 MISSING: sum < 50",
            "FF -> F",
        ],
    );

    let test = project
        .supercov(&["runs", "latest", "test", "shipping"])
        .succeeds();
    contains_all(
        &test,
        &[
            "shipping is free over fifty",
            "line: src/cart.js:12",
            "line: src/cart.js:16",
            "node:assert/strict.equal at test/cart.test.js:14:3",
        ],
    );

    let kinds = project.supercov(&["runs", "latest", "kinds"]).succeeds();
    assert!(kinds.contains("unit  3 test(s)  lines 76.92%"), "{kinds}");
    let runners = project.supercov(&["runs", "latest", "runners"]).succeeds();
    assert!(runners.contains("node:test  3 test(s)"), "{runners}");
    // Coverage by directory, split by the kinds of test that ran, and the
    // counts a percentage is worked out from on every file.
    let areas = project
        .supercov(&["runs", "latest", "files", "--group", "dir"])
        .succeeds();
    contains_all(
        &areas,
        &[
            "Coverage by directory, 1 deep",
            "Directory  Files  Lines    unit     All",
            "src            1     13  76.92%  76.92%",
        ],
    );
    let areas = project
        .supercov(&["runs", "latest", "files", "--group", "dir", "--json"])
        .json();
    let area = &areas["data"]["areas"][0];
    assert_eq!(area["directory"], "src");
    assert_eq!(area["totals"]["lines"], 13);
    assert_eq!(area["covered"]["lines"], 10);
    assert_eq!(area["byKind"][0]["kind"], "unit");
    assert_eq!(area["byKind"][0]["covered"]["lines"], 10);
    assert_eq!(area["coveredByTests"]["lines"], 10);
    let files = project
        .supercov(&["runs", "latest", "files", "--json"])
        .json();
    let file = &files["data"]["files"][0];
    assert_eq!(file["totals"]["lines"], 13);
    assert_eq!(file["covered"]["lines"], 10);
    assert_eq!(
        file["covered"]["branches"].as_u64().unwrap() + file["missingBranches"].as_u64().unwrap(),
        file["totals"]["branches"].as_u64().unwrap()
    );
    let wrong = project.supercov(&["runs", "latest", "files", "--depth", "2"]);
    assert_eq!(wrong.code(), 2);

    // The scope is read as a table of places first, and file by file on
    // request.
    let scope = project.supercov(&["runs", "latest", "scope"]).succeeds();
    contains_all(
        &scope,
        &[
            "Files  Status     Directory  Reason",
            "    1  INCLUDED   src        discovered package source root",
            "    1  EXCLUDED   test       test or fixture source",
            "File by file: supercov runs",
            "scope --files",
        ],
    );
    let scope = project
        .supercov(&["runs", "latest", "scope", "--files"])
        .succeeds();
    contains_all(
        &scope,
        &["INCLUDED  src/cart.js", "EXCLUDED  test/cart.test.js"],
    );

    let source = project
        .supercov(&["runs", "latest", "source", "src/cart.js", "--limit", "3"])
        .succeeds();
    contains_all(
        &source,
        &[
            "1 │ export function total(items, coupon) {",
            "Showing 1-3 of 21",
            "--offset 3",
        ],
    );
    let next = project
        .supercov(&[
            "runs",
            "latest",
            "source",
            "src/cart.js",
            "--offset",
            "11",
            "--limit",
            "1",
        ])
        .succeeds();
    assert!(
        next.contains("12 │ export function shipping(sum, express) {"),
        "{next}"
    );

    let assertions = project
        .supercov(&["runs", "latest", "assertions"])
        .succeeds();
    assert!(
        assertions.contains("assertions not assessed"),
        "{assertions}"
    );

    let impossible = project
        .supercov(&["runs", "latest", "minimize", "--target", "100"])
        .exits(2);
    assert!(
        impossible.contains("target 100% is impossible"),
        "{impossible}"
    );
    let minimized = project
        .supercov(&[
            "runs", "latest", "minimize", "--target", "70", "--metric", "lines",
        ])
        .exits(0);
    assert!(minimized.contains("test"), "{minimized}");
}

#[test]
fn every_query_answers_in_json_too() {
    let project = Project::cart("json");
    let run = project.measure(&[]);
    let summary = project.supercov(&["runs", "latest", "--json"]).json();
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["data"]["run"], run.as_str());
    for query in [
        vec!["gaps"],
        vec!["files"],
        vec!["file", "src/cart.js", "--limit", "5"],
        vec!["line", "src/cart.js:14"],
        vec!["decision", "src/cart.js:13"],
        vec!["test", "shipping"],
        vec!["kinds"],
        vec!["runners"],
        vec!["scope"],
        vec!["source", "src/cart.js", "--limit", "2"],
        vec!["assertions"],
        vec!["minimize", "--target", "50", "--metric", "lines"],
        vec!["tests", "affected"],
    ] {
        let mut args = vec!["runs", run.as_str()];
        args.extend(query.iter().copied());
        args.push("--json");
        let report = project.supercov(&args).json();
        assert_eq!(report["ok"], true, "{query:?}: {report}");
    }
    let listed = project.supercov(&["runs", "--json", "--limit", "1"]).json();
    assert_eq!(listed["data"]["runs"][0]["id"], run.as_str());
}

#[test]
fn check_gates_on_counts_and_refuses_what_it_cannot_judge() {
    let project = Project::cart("check");
    project.measure(&[]);

    let passed = project
        .supercov(&["runs", "check", "--min-lines", "50"])
        .succeeds();
    contains_all(
        &passed,
        &["lines         76.92%  10/13  (floor 50%)", "PASS"],
    );

    let failed = project
        .supercov(&[
            "runs",
            "check",
            "--min-lines",
            "100",
            "--min-branches",
            "100",
        ])
        .exits(1);
    contains_all(
        &failed,
        &[
            "FAIL: 2 floor(s) not met.",
            "(whole run): lines 76.92%  10/13 below 100%",
            "(whole run): branches 50.00%  4/8 below 100%",
        ],
    );

    // 10 of 13 lines is 76.923...%: a floor at the rounded figure fails.
    project
        .supercov(&["runs", "check", "--min-lines", "76.93"])
        .exits(1);
    project
        .supercov(&["runs", "check", "--min-lines", "76.92"])
        .succeeds();

    let per_file = project
        .supercov(&[
            "runs",
            "check",
            "--min-functions",
            "60",
            "--per-file",
            "--json",
        ])
        .json();
    assert_eq!(per_file["data"]["outcome"]["result"], "fail", "{per_file}");
    let file = &per_file["data"]["view"]["files"][0];
    assert_eq!(file["file"], "src/cart.js");
    assert_eq!(file["uncoveredLines"], serde_json::json!([14, 19, 20]));

    for (args, message) in [
        (vec!["--min-mcdc", "101"], "is above 100"),
        (vec!["--min-lines", "abc"], "is not a percentage"),
    ] {
        let mut full = vec!["runs", "check"];
        full.extend(args);
        let refused = project.supercov(&full).exits(2);
        assert!(refused.contains(message), "{refused}");
    }

    // A run that no longer matches the checkout cannot answer.
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let stale = project
        .supercov(&["runs", "check", "--min-lines", "10"])
        .exits(2);
    contains_all(&stale, &["cannot answer", "instrumented source changed"]);
}

#[test]
fn a_failing_suite_keeps_its_exit_status_and_cannot_pass_a_gate() {
    let project = Project::cart("failing");
    project.edit(
        "test/cart.test.js",
        "shipping(60, false), 0",
        "shipping(60, false), 1",
    );
    let measured = project.supercov(&["--", "node", "--test"]);
    let output = measured.exits(1);
    assert!(output.contains("fail 1"), "{output}");
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["Passed      2", "Failed      1"]);
    let gate = project
        .supercov(&["runs", "check", "--min-lines", "10"])
        .exits(2);
    assert!(gate.contains("cannot answer"), "{gate}");
    let failed = project
        .supercov(&["runs", "latest", "--filter", "failed", "--json"])
        .json();
    assert_eq!(failed["ok"], true, "{failed}");
}

#[test]
fn patch_judges_only_the_lines_a_change_touches() {
    let project = Project::cart("patch");
    // An untested change: describe() gains a line no test runs.
    project.edit(
        "src/cart.js",
        "  return items.map((item) => item.name).join(\", \");",
        "  const names = items.map((item) => item.name);\n  return names.join(\", \");",
    );
    project.measure(&[]);
    let uncovered = project
        .supercov(&["runs", "patch", "--base", "HEAD", "--min-lines", "100"])
        .exits(1);
    contains_all(
        &uncovered,
        &["0 of 2 changed executable line(s) covered", "src/cart.js"],
    );
    let annotated = project
        .supercov(&["runs", "patch", "--base", "HEAD", "--annotate", "github"])
        .succeeds();
    assert!(
        annotated.contains("::warning file=src/cart.js,line=20"),
        "{annotated}"
    );

    // Tested now: the same change passes.
    project.edit(
        "test/cart.test.js",
        "import { total, shipping } from \"../src/cart.js\";",
        "import { total, shipping, describe } from \"../src/cart.js\";\n\ntest(\"describe lists names\", () => {\n  assert.equal(describe([{ name: \"a\" }, { name: \"b\" }]), \"a, b\");\n});",
    );
    project.measure(&[]);
    let covered = project
        .supercov(&[
            "runs",
            "patch",
            "--base",
            "HEAD",
            "--min-lines",
            "100",
            "--json",
        ])
        .json();
    assert_eq!(covered["ok"], true, "{covered}");
    let text = covered.to_string();
    assert!(text.contains("\"pass\""), "{text}");

    // Only a comment changed.
    project.commit("describe");
    project.edit(
        "src/cart.js",
        "export function describe",
        "// Names, in order.\nexport function describe",
    );
    project.measure(&[]);
    let comment = project
        .supercov(&["runs", "patch", "--base", "HEAD"])
        .succeeds();
    assert!(comment.contains("No executable changes"), "{comment}");

    // Untracked new source counts as entirely added.
    project.write(
        "src/tax.js",
        "export function rate(country) {\n  return country === \"LT\" ? 0.21 : 0;\n}\n",
    );
    project.measure(&[]);
    let added = project
        .supercov(&["runs", "patch", "--base", "HEAD", "--min-lines", "100"])
        .exits(1);
    contains_all(
        &added,
        &["0 of 2 changed executable line(s) covered", "src/tax.js"],
    );

    // A source change the run has not seen cannot be mapped.
    project.edit("src/tax.js", "0.21", "0.2");
    let stale = project
        .supercov(&["runs", "patch", "--base", "HEAD"])
        .exits(2);
    assert!(stale.contains("cannot map a patch"), "{stale}");
}

#[test]
fn patch_reads_a_project_kept_below_the_top_of_its_repository() {
    // Git names a file from the top (`web/src/cart.js`), the run from where
    // it stood (`src/cart.js`), and a workflow marks a line by the first.
    let (_repository, project) = Project::cart_below("patch-below", "web");
    project.edit(
        "src/cart.js",
        "  return items.map((item) => item.name).join(\", \");",
        "  const names = items.map((item) => item.name);\n  return names.join(\", \");",
    );
    project.write(
        "src/tax.js",
        "export function rate(country) {\n  return country === \"LT\" ? 0.21 : 0;\n}\n",
    );
    project.measure(&[]);
    let patch = project
        .supercov(&["runs", "patch", "--base", "HEAD", "--annotate", "github"])
        .succeeds();
    contains_all(
        &patch,
        &[
            "0 of 4 changed executable line(s) covered",
            "  src/cart.js: ",
            "  src/tax.js: ",
            "::warning file=web/src/cart.js,line=",
            "::warning file=web/src/tax.js,line=",
        ],
    );
}

/// A script that answers each line typed to it and ends on the second.
#[cfg(unix)]
const ASKS: &str = "\
import { shipping } from './src/cart.js';
let lines = 0;
process.stdin.on('data', (typed) => {
  lines += 1;
  console.log(`line ${lines}: ${String(typed).trim()} (${shipping(60, false)})`);
  if (lines === 2) process.exit(0);
});
console.log('asking');
";

#[cfg(unix)]
#[test]
fn a_command_on_a_terminal_reads_what_is_typed() {
    // A debugger at a breakpoint, a watch mode, a question before an
    // install: the command never got the keys, and stood stopped.
    let project = Project::cart("keyboard");
    project.write("ask.mjs", ASKS);
    project.commit("asks");
    let mut terminal = common::OnTerminal::start(&project, &["--", "node", "ask.mjs"]);
    terminal.wait_for("asking");
    terminal.press("first\n");
    terminal.wait_for("line 1: first (0)");
    terminal.press("second\n");
    terminal.wait_for("line 2: second (0)");
    let (code, seen) = terminal.finish();
    assert_eq!(code, 0, "{seen}");
    assert!(seen.contains("[coverage] evidence:"), "{seen}");
    // The run is recorded as any other.
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("command: node ask.mjs"), "{summary}");
}

#[cfg(unix)]
#[test]
fn ctrl_c_at_a_command_that_reads_the_keyboard_interrupts_the_run() {
    // With the terminal lent, Ctrl-C reaches the command and not Supercov.
    // The run is still one that was interrupted: its status, and no record.
    let project = Project::cart("keyboard-interrupt");
    project.write("ask.mjs", ASKS);
    project.commit("asks");
    let mut terminal = common::OnTerminal::start(&project, &["--", "node", "ask.mjs"]);
    terminal.wait_for("asking");
    terminal.press("first\n");
    terminal.wait_for("line 1: first (0)");
    terminal.press("\u{3}");
    let (code, seen) = terminal.finish();
    assert_eq!(code, 130, "{seen}");
    assert!(!seen.contains("[coverage] evidence:"), "{seen}");
    let runs = project.supercov(&["runs", "--json"]).json();
    assert_eq!(
        runs["data"]["runs"].as_array().map(Vec::len),
        Some(0),
        "{runs}"
    );
}

#[cfg(unix)]
#[test]
fn ctrl_z_at_a_command_that_reads_the_keyboard_stops_supercov_too() {
    // The shell waits on Supercov. Were the command alone stopped, the
    // terminal would belong to nothing that reads it.
    let project = Project::cart("keyboard-stop");
    project.write("ask.mjs", ASKS);
    project.commit("asks");
    let mut terminal = common::OnTerminal::start(&project, &["--", "node", "ask.mjs"]);
    terminal.wait_for("asking");
    terminal.press("first\n");
    terminal.wait_for("line 1: first (0)");
    terminal.press("\u{1a}");
    terminal.wait_until_stopped();
    terminal.resume();
    terminal.press("second\n");
    terminal.wait_for("line 2: second (0)");
    let (code, seen) = terminal.finish();
    assert_eq!(code, 0, "{seen}");
}

#[cfg(unix)]
#[test]
fn suites_started_together_from_one_command_are_one_run_with_their_kinds() {
    // What to do where two suites run side by side: one command starts
    // both, each says its own kind, and the run holds every test.
    let project = Project::cart("together");
    project.write(
        "test/slow.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { describe } from \"../src/cart.js\";\n\ntest(\"describe lists names\", async () => {\n  await new Promise((done) => setTimeout(done, 200));\n  assert.equal(describe([{ name: \"a\" }]), \"a\");\n});\n",
    );
    project.commit("a slow suite");
    let together = "SUPERCOV_TEST_KIND=unit node --test test/cart.test.js & unit=$!; SUPERCOV_TEST_KIND=integration node --test test/slow.test.js & integration=$!; wait $unit; first=$?; wait $integration && exit $first";
    project.supercov(&["--", "sh", "-c", together]).succeeds();
    let kinds = project.supercov(&["runs", "latest", "kinds"]).succeeds();
    contains_all(&kinds, &["unit", "integration"]);
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["Total       4", "Passed      4"]);
    // A filter narrows the grouping to the tests it picks.
    let counted = |args: &[&str], field: &str| -> Vec<(String, u64)> {
        let mut all = vec!["runs", "latest"];
        all.extend_from_slice(args);
        all.push("--json");
        project.supercov(&all).json()["data"][field]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let name = entry["kind"].as_str().or(entry["runner"].as_str()).unwrap();
                (name.to_owned(), entry["tests"].as_u64().unwrap())
            })
            .collect()
    };
    assert_eq!(
        counted(&["runners", "--kind", "integration"], "runners"),
        [("node:test".to_owned(), 1)]
    );
    assert_eq!(
        counted(&["runners", "--kind", "unit"], "runners"),
        [("node:test".to_owned(), 3)]
    );
    assert_eq!(
        counted(&["kinds", "--kind", "unit"], "kinds"),
        [("unit".to_owned(), 3)]
    );
    let mut by_runner = counted(&["kinds", "--runner", "node:test"], "kinds");
    by_runner.sort();
    assert_eq!(
        by_runner,
        [("integration".to_owned(), 1), ("unit".to_owned(), 3)]
    );
}

#[test]
fn reports_carry_the_same_view_in_every_format() {
    let project = Project::cart("reports");
    project.measure(&[]);
    let lcov = project
        .supercov(&["runs", "report", "--format", "lcov"])
        .succeeds();
    contains_all(
        &lcov,
        &[
            "SF:src/cart.js",
            "FNDA:0,describe",
            "FNH:2",
            "BRH:4",
            "DA:14,0",
            "LH:10",
            "LF:13",
        ],
    );
    let cobertura = project
        .supercov(&["runs", "report", "--format", "cobertura"])
        .succeeds();
    contains_all(
        &cobertura,
        &[
            "lines-covered=\"10\" lines-valid=\"13\"",
            "branches-covered=\"4\" branches-valid=\"8\"",
            "filename=\"src/cart.js\"",
        ],
    );

    let written = project.root.join("coverage/lcov.info");
    project
        .supercov(&[
            "runs",
            "report",
            "--format",
            "lcov",
            "--output",
            "coverage/lcov.info",
        ])
        .succeeds();
    assert_eq!(std::fs::read_to_string(&written).unwrap(), lcov);
    let kept = project
        .supercov(&[
            "runs",
            "report",
            "--format",
            "lcov",
            "--output",
            "coverage/lcov.info",
        ])
        .exits(2);
    assert!(kept.contains("--force"), "{kept}");
    project
        .supercov(&[
            "runs",
            "report",
            "--format",
            "lcov",
            "--output",
            "coverage/lcov.info",
            "--force",
        ])
        .succeeds();

    project
        .supercov(&[
            "runs",
            "report",
            "--format",
            "html",
            "--output",
            "coverage/report",
        ])
        .succeeds();
    let html = std::fs::read_to_string(project.root.join("coverage/report/index.html")).unwrap();
    assert!(html.contains("src/cart.js"));

    let interactive = project
        .supercov(&["report", "--output", "coverage/portable", "--no-open"])
        .exits(0);
    assert!(interactive.contains("self-contained"), "{interactive}");
}

/// A diff says the code did not change only when neither the source nor
/// what it runs on did: a new dependency is a change in the code too.
#[test]
fn a_diff_counts_a_dependency_change_as_a_change_in_the_code() {
    let added_test = "test(\"express shipping costs fifteen\", () => {\n  assert.equal(shipping(10, true), 15);\n});\n\ntest(\"shipping is free over fifty\"";
    let same_source = "Both runs measured the same source";

    let project = Project::cart("diff-tests-only");
    let before = project.measure(&[]);
    project.edit(
        "test/cart.test.js",
        "test(\"shipping is free over fifty\"",
        added_test,
    );
    let after = project.measure(&[]);
    let diff = project.supercov(&["diff", &before, &after]).succeeds();
    assert!(diff.contains(same_source), "{diff}");

    let project = Project::cart("diff-dependency");
    let before = project.measure(&[]);
    project.edit(
        "test/cart.test.js",
        "test(\"shipping is free over fifty\"",
        added_test,
    );
    project.edit(
        "package.json",
        "\"type\": \"module\"",
        "\"type\": \"module\", \"dependencies\": { \"left-pad\": \"1.3.0\" }",
    );
    let after = project.measure(&[]);
    let diff = project.supercov(&["diff", &before, &after]).succeeds();
    assert!(!diff.contains(same_source), "{diff}");
}

/// `tests affected` reads only the tests a filter picks, as every query does:
/// a merged run's names for one runner, or only some of the affected tests.
#[test]
fn tests_affected_narrows_to_the_tests_a_filter_picks() {
    let project = Project::cart("affected-filter");
    project.measure(&[]);
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let picked = project
        .supercov(&["runs", "latest", "tests", "affected", "--title", "shipping"])
        .succeeds();
    contains_all(
        &picked,
        &[
            "1 of 1 test affected",
            "(--title shipping)",
            "shipping is free over fifty",
        ],
    );
    let others = project
        .supercov(&[
            "runs",
            "latest",
            "tests",
            "affected",
            "--names",
            "--title",
            "!shipping",
        ])
        .succeeds();
    assert_eq!(others.trim(), "", "only the shipping test was affected");
    let json = project
        .supercov(&[
            "runs",
            "latest",
            "tests",
            "affected",
            "--json",
            "--runner",
            "node:test",
        ])
        .json();
    assert_eq!(json["data"]["summary"]["tests"], 3, "{json}");
    assert_eq!(json["data"]["filter"], "--runner node:test");
    let none = project
        .supercov(&["runs", "latest", "tests", "affected", "--runner", "jest"])
        .exits(2);
    assert!(none.contains("No tests match --runner jest"), "{none}");
    let missing = project
        .supercov(&["runs", "latest", "tests", "affected", "--tag"])
        .exits(2);
    assert!(missing.contains("--tag needs a value"), "{missing}");

    // A merged run answers from its parts: each kind's affected tests alone.
    let project = Project::cart("affected-filter-merged");
    project
        .supercov_with(
            &["--", "node", "--test", "--test-name-pattern=total"],
            &[("SUPERCOV_TEST_KIND", "unit")],
        )
        .succeeds();
    let unit = project.latest();
    project
        .supercov_with(
            &["--", "node", "--test", "--test-name-pattern=shipping"],
            &[("SUPERCOV_TEST_KIND", "e2e")],
        )
        .succeeds();
    let e2e = project.latest();
    project.supercov(&["merge", &unit, &e2e]).succeeds();
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let e2e_names = project
        .supercov(&[
            "runs", "latest", "tests", "affected", "--names", "--kind", "e2e",
        ])
        .succeeds();
    assert_eq!(e2e_names.trim(), "shipping is free over fifty");
    let unit_names = project
        .supercov(&[
            "runs", "latest", "tests", "affected", "--names", "--kind", "unit",
        ])
        .succeeds();
    assert_eq!(unit_names.trim(), "", "no unit test ran the changed line");
}

/// Node's module mocks keep the CJS resolver they find when node:test first
/// loads. A preload the command gives (`--import tsx`, which maps tsconfig
/// paths by patching it) comes after Supercov's own, so the adapter must not
/// load node:test before the command's preloads have run.
#[test]
fn a_mocking_test_resolves_paths_a_later_preload_maps() {
    let project = Project::empty("mock-paths");
    project.write(
        "package.json",
        r#"{ "name": "mock-paths", "private": true }"#,
    );
    project.write(
        "alias.mjs",
        "import Module from 'node:module';\nimport path from 'node:path';\nconst resolve = Module._resolveFilename;\nModule._resolveFilename = function (request, ...rest) {\n  if (request.startsWith('@/')) request = path.resolve('src', request.slice(2));\n  return resolve.call(this, request, ...rest);\n};\n",
    );
    project.write(
        "src/lib/log.js",
        "exports.log = (line) => (line ? `log: ${line}` : 'empty');\n",
    );
    project.write("src/clock.js", "exports.now = () => Date.now();\n");
    project.write(
        "src/app.js",
        "const { log } = require('@/lib/log');\nconst { now } = require('./clock');\nexports.stamp = (line) => `${log(line)} at ${now()}`;\n",
    );
    project.write(
        "test/app.test.js",
        "const { mock, test } = require('node:test');\nconst assert = require('node:assert/strict');\nmock.module('../src/clock.js', { namedExports: { now: () => 7 } });\ntest('stamps a line', () => {\n  const { stamp } = require('../src/app.js');\n  assert.equal(stamp('a'), 'log: a at 7');\n});\n",
    );
    let command = [
        "node",
        "--import",
        "./alias.mjs",
        "--experimental-test-module-mocks",
        "--test",
        "test/app.test.js",
    ];
    let plain = std::process::Command::new(command[0])
        .args(&command[1..])
        .current_dir(&project.root)
        .output()
        .unwrap();
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stdout)
    );
    let measured = project
        .supercov(&[&["--"][..], &command].concat())
        .succeeds();
    assert!(measured.contains("pass 1"), "{measured}");
    assert!(!measured.contains("Cannot find module"), "{measured}");
}

#[test]
fn diff_and_tests_affected_follow_a_change() {
    let project = Project::cart("change");
    let before = project.measure(&[]);
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );

    let affected = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &affected,
        &[
            "1 of 3 tests affected",
            "shipping is free over fifty",
            "shipping (line 12) changed (this test ran it)",
        ],
    );
    let names = project
        .supercov(&["runs", "latest", "tests", "affected", "--names"])
        .succeeds();
    assert_eq!(names.trim(), "shipping is free over fifty");

    project.edit(
        "test/cart.test.js",
        "test(\"shipping is free over fifty\"",
        "test(\"express shipping costs twenty\", () => {\n  assert.equal(shipping(10, true), 20);\n});\n\ntest(\"shipping is free over fifty\"",
    );
    let after = project.measure(&[]);
    let diff = project.supercov(&["diff", &before, &after]).succeeds();
    contains_all(
        &diff,
        &[
            "lines +7.7pp, branches +25pp, MC/DC +20pp",
            "+ line src/cart.js:14",
            "+ MC/DC src/cart.js:13 C1 express",
        ],
    );
    let listed = project.supercov(&["runs"]).succeeds();
    assert!(listed.contains("STALE"), "{listed}");
}

#[test]
fn shards_merge_into_one_run_and_mismatched_ones_do_not() {
    let project = Project::cart("merge");
    let first = project.measure(&["--test-name-pattern=total"]);
    let second = project.measure(&["--test-name-pattern=shipping"]);
    let merged = project.supercov(&["merge", &first, &second]).exits(0);
    assert!(merged.contains("merged run"), "{merged}");
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            "command: supercov merge",
            "Lines      76.92% (10/13)",
            "Passed      3",
        ],
    );

    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let third = project.measure(&["--test-name-pattern=shipping"]);
    let refused = project.supercov(&["merge", &first, &third]);
    assert_ne!(
        refused.code(),
        0,
        "a merge across source changes is refused"
    );
}

#[test]
fn clean_removes_runs_and_keeps_what_it_is_told_to() {
    let project = Project::cart("clean");
    let oldest = project.measure(&["--test-name-pattern=total"]);
    let newest = project.measure(&["--test-name-pattern=shipping"]);

    let dry = project.supercov(&["runs", "clean", "--dry-run"]).exits(0);
    contains_all(&dry, &["would remove 2 stored run(s)", &oldest, &newest]);
    assert!(project.supercov(&["runs"]).succeeds().contains(&oldest));

    let kept = project.supercov(&["runs", "clean", "--keep", "1"]).exits(0);
    assert!(kept.contains("removed 1 stored run(s)"), "{kept}");
    let listed = project.supercov(&["runs"]).succeeds();
    assert!(
        listed.contains(&newest) && !listed.contains(&oldest),
        "{listed}"
    );

    project.supercov(&["runs", "clean"]).exits(0);
    let empty = project.supercov(&["runs", "latest"]).exits(2);
    assert!(empty.contains("No local coverage runs"), "{empty}");
}

#[test]
fn mistakes_are_named_and_end_with_two() {
    let project = Project::cart("mistakes");
    for (args, message) in [
        (vec!["runs", "latest"], "No local coverage runs"),
        (
            vec!["runs", "check", "--min-lines", "10"],
            "no local coverage runs",
        ),
        (vec!["frobnicate"], "Unknown command: frobnicate"),
        (vec!["--"], "Usage: supercov -- <test command>"),
    ] {
        let output = project.supercov(&args).exits(2);
        assert!(output.contains(message), "{args:?}: {output}");
    }
    project.measure(&[]);
    for (args, message) in [
        (vec!["runs", "latest", "bogus"], "Unknown run query: bogus"),
        (vec!["runs", "run_0000000000000000"], "run_0000000000000000"),
        (
            vec!["runs", "latest", "minimize", "--target-lines", "70"],
            "Unknown option: --target-lines",
        ),
        (
            vec!["runs", "latest", "file", "src/missing.js"],
            "src/missing.js",
        ),
        (
            vec!["runs", "latest", "line", "src/cart.js"],
            "Expected <source-file>:<line>",
        ),
    ] {
        let output = project.supercov(&args);
        assert_ne!(output.code(), 0, "{args:?}");
        let text = format!("{}{}", output.stdout(), output.stderr());
        assert!(text.contains(message), "{args:?}: {text}");
    }
    let json = project.supercov(&["runs", "latest", "bogus", "--json"]);
    assert_ne!(json.code(), 0);
}

/// A command started in a directory deleted under the shell has no project
/// to work in, and each says so instead of acting somewhere else. Linux's
/// `getcwd` refuses a removed directory; macOS may still name it.
#[cfg(target_os = "linux")]
#[test]
fn a_command_whose_directory_is_gone_says_so() {
    let parent = std::env::temp_dir().join(format!("supercov-gone-{}", std::process::id()));
    std::fs::create_dir_all(&parent).unwrap();
    for arguments in [
        &["merge"][..],
        &["import", "evidence.lcov", "--background"],
        &["--", "true"],
        &["runs", "clean"],
        &["__supervise", "--", "true"],
    ] {
        let gone = parent.join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        // `cd` into it, remove it, then become Supercov there.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(r#"cd "$1" && rmdir "$1" && shift && exec "$@""#)
            .arg("sh")
            .arg(&gone)
            .arg(env!("CARGO_BIN_EXE_supercov"))
            .args(arguments)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {stderr}");
        assert!(
            stderr.contains("could not resolve the current directory"),
            "{arguments:?}: {stderr}"
        );
    }
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn help_and_bundled_guides_need_no_project() {
    let project = Project::empty("help");
    for args in [
        vec!["help"],
        vec!["--help"],
        vec!["runs", "--help"],
        vec!["runs", "latest", "--help"],
    ] {
        let help = project.supercov(&args).succeeds();
        assert!(help.contains("supercov"), "{args:?}: {help}");
    }
    // `help <command>` is `<command> --help`, for every command.
    for command in [
        "quality", "security", "docs", "runs", "diff", "report", "merge", "import",
    ] {
        let help = project.supercov(&["help", command]).succeeds();
        assert_eq!(
            help,
            project.supercov(&[command, "--help"]).succeeds(),
            "{command}"
        );
    }
    let unknown = project.supercov(&["help", "fly"]).exits(2);
    assert!(unknown.contains("No help for fly"), "{unknown}");
    let guides = project.supercov(&["docs"]).succeeds();
    contains_all(
        &guides,
        &["Bundled guides", "getting-started", "assertions", "cli"],
    );
    let guide = project.supercov(&["docs", "assertions"]).succeeds();
    assert!(guide.starts_with("# Assertion coverage"), "{guide}");
    // Every guide listed is in the binary: an installed one has no
    // repository to read a missing one from.
    for topic in guides.lines().skip(1).map(str::trim) {
        let guide = project.supercov(&["docs", topic]).succeeds();
        assert!(guide.starts_with("# "), "{topic}: {guide}");
    }
    let missing = project.supercov(&["docs", "no-such-guide"]);
    assert_ne!(missing.code(), 0);
    let version = project.supercov(&["--version"]).succeeds();
    assert!(version.contains(env!("CARGO_PKG_VERSION")), "{version}");

    // Every setting the CLI guide's table lists is named where people and
    // agents look first.
    let help = project.supercov(&["--help"]).succeeds();
    let cli = project.supercov(&["docs", "cli"]).succeeds();
    let table = cli
        .split("## Environment variables")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the CLI guide lists its environment variables");
    let settings = table
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|line| line.split('`').next())
        .collect::<Vec<_>>();
    assert!(settings.len() >= 5, "{table}");
    for setting in settings {
        assert!(
            help.contains(setting),
            "{setting} is not in --help:\n{help}"
        );
    }
}

#[test]
fn every_way_of_importing_node_assert_is_credited_to_its_test() {
    let project = Project::empty("assert-styles");
    project.write(
        "package.json",
        r#"{ "name": "styles", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/lib.js",
        "export function* evens(limit) {\n  for (let n = 0; n < limit; n += 2) {\n    yield n;\n  }\n}\n\nexport const label = (value) => (value ? `on:${value}` : \"off\");\n",
    );
    project.write(
        "test/default.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { evens } from \"../src/lib.js\";\ntest(\"default import\", () => {\n  assert.deepStrictEqual([...evens(5)], [0, 2, 4]);\n  assert.ok(true);\n});\n",
    );
    project.write(
        "test/namespace.test.js",
        "import test from \"node:test\";\nimport * as assert from \"node:assert/strict\";\nimport { label } from \"../src/lib.js\";\ntest(\"namespace import\", () => {\n  assert.equal(label(\"x\"), \"on:x\");\n});\n",
    );
    project.write(
        "test/named.test.js",
        "import test from \"node:test\";\nimport { strict as assert, notEqual } from \"node:assert\";\nimport { equal, ok } from \"node:assert/strict\";\nimport { label } from \"../src/lib.js\";\ntest(\"named imports\", () => {\n  equal(label(\"x\"), \"on:x\");\n  ok(label(0) === \"off\");\n  assert.match(label(\"y\"), /on/);\n  notEqual(label(1), \"off\");\n});\n",
    );
    project.write(
        "test/require.test.cjs",
        "const test = require(\"node:test\");\nconst assert = require(\"node:assert/strict\");\ntest(\"require\", async () => {\n  const { label } = await import(\"../src/lib.js\");\n  assert.strictEqual(label(\"\"), \"off\");\n});\n",
    );
    project.git(&["init", "-q"]);
    let measured = project.supercov(&["--", "node", "--test"]).succeeds();
    assert!(measured.contains("pass 4"), "{measured}");
    for (test, assertions) in [
        (
            "default import",
            &[
                "node:assert.deepStrictEqual at test/default.test.js:5:3",
                "node:assert.ok at test/default.test.js:6:3",
            ][..],
        ),
        (
            "namespace import",
            &["node:assert/strict.equal at test/namespace.test.js:5:3"][..],
        ),
        (
            "named imports",
            &[
                "node:assert/strict.equal at test/named.test.js:6:3",
                "node:assert/strict.ok at test/named.test.js:7:3",
                "node:assert/strict.match at test/named.test.js:8:3",
                "node:assert.notEqual at test/named.test.js:9:3",
            ][..],
        ),
        (
            "require",
            &["node:assert/strict.strictEqual at test/require.test.cjs:5:3"][..],
        ),
    ] {
        let read = project
            .supercov(&["runs", "latest", "test", test])
            .succeeds();
        contains_all(&read, assertions);
    }
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("Lines      100.00% (4/4)"), "{summary}");
}

#[test]
fn a_function_shipped_as_text_runs_where_no_probe_exists() {
    // A function sent to a worker, a `vm` context or a browser page goes as
    // its source text; a probe inside it names a global that is not there.
    let project = Project::empty("shipped");
    project.write(
        "package.json",
        r#"{ "name": "shipped", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/task.js",
        "export function square(n) {\n  if (n < 0) {\n    throw new Error(\"negative\");\n  }\n  return n * n;\n}\nexport const cube = (n) => n * n * n;\nexport const shipped = [square.toString(), `${cube}`, String(square)];\nexport function local(flag) {\n  return flag ? \"a\" : \"b\";\n}\n",
    );
    project.write(
        "test/task.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport vm from \"node:vm\";\nimport { shipped, local } from \"../src/task.js\";\ntest(\"shipped functions run\", () => {\n  assert.equal(vm.runInNewContext(`(${shipped[0]})(4)`), 16);\n  assert.equal(vm.runInNewContext(`(${shipped[1]})(2)`), 8);\n  assert.equal(vm.runInNewContext(`(${shipped[2]})(3)`), 9);\n  assert.equal(local(true), \"a\");\n});\n",
    );
    project.git(&["init", "-q"]);
    let measured = project.supercov(&["--", "node", "--test"]).succeeds();
    assert!(measured.contains("pass 1"), "{measured}");
    // What is left as source is declared, never silently uncovered.
    let file = project
        .supercov(&["runs", "latest", "file", "src/task.js"])
        .succeeds();
    assert!(file.contains("Measurement limitations         2"), "{file}");
    let local = project
        .supercov(&["runs", "latest", "line", "src/task.js:10"])
        .succeeds();
    assert!(!local.contains("NOT COVERED"), "{local}");

    // A function reached by name through `+`, `||`, `?:` or an assignment
    // is shipped as text too. A local of the same name read elsewhere is a
    // different binding and leaves the function measured.
    project.write(
        "src/named.js",
        "const worker = (n) => { if (n) { return 1; } return 0; };\nexport const joined = \"(\" + worker + \")\";\nconst fallback = (n) => { if (n) { return 2; } return 0; };\nexport const chosen = \"\" + (null || fallback);\nconst branch = (n) => { if (n) { return 3; } return 0; };\nexport const picked = String(Math.random() >= 0 ? branch : null);\nlet later;\nlater = (n) => { if (n) { return 4; } return 0; };\nexport const assigned = `${later}`;\nexport function total(n) {\n  if (n > 1) {\n    return n;\n  }\n  return 0;\n}\nexport function counter() {\n  let total = 0;\n  if (total < 5) total += 1;\n  return total;\n}\n",
    );
    project.write(
        "test/named.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport vm from \"node:vm\";\nimport { joined, chosen, picked, assigned, total, counter } from \"../src/named.js\";\ntest(\"named functions run as text\", () => {\n  assert.equal(vm.runInNewContext(`${joined}(1)`), 1);\n  assert.equal(vm.runInNewContext(`(${chosen})(1)`), 2);\n  assert.equal(vm.runInNewContext(`(${picked})(1)`), 3);\n  assert.equal(vm.runInNewContext(`(${assigned})(1)`), 4);\n  assert.equal(total(2), 2);\n  assert.equal(counter(), 1);\n});\n",
    );
    let measured = project
        .supercov(&["--", "node", "--test", "test/named.test.js"])
        .succeeds();
    assert!(measured.contains("pass 1"), "{measured}");
    let named = project
        .supercov(&["runs", "latest", "file", "src/named.js"])
        .succeeds();
    assert!(
        named.contains("Measurement limitations         4"),
        "{named}"
    );
    assert!(named.contains("   14  NOT COVERED   return 0;"), "{named}");
}

#[test]
fn every_subcommand_explains_itself_and_names_a_mistake() {
    let project = Project::cart("usage");
    project.measure(&[]);
    for (args, help) in [
        (vec!["runs", "check", "--help"], "check [--min-lines <pct>]"),
        (vec!["runs", "patch", "--help"], "patch --base <ref>"),
        (
            vec!["runs", "report", "--help"],
            "--format lcov|cobertura|html",
        ),
        (
            vec!["merge", "--help"],
            "With no run named, merges the runs recorded on the source as it is now",
        ),
        (
            vec!["runs", "clean", "--help"],
            "Saved quality assessments are never removed here",
        ),
    ] {
        let output = project.supercov(&args).succeeds();
        assert!(output.contains(help), "{args:?}: {output}");
    }
    for (args, message) in [
        (
            vec!["runs", "check", "--min-foo", "3"],
            "unknown metric in --min-foo",
        ),
        (
            vec!["runs", "check", "--min-lines"],
            "--min-lines needs a percentage",
        ),
        (vec!["runs", "check", "--bogus"], "unknown option --bogus"),
        (vec!["runs", "patch"], "patch needs --base <ref>"),
        (vec!["runs", "patch", "--base"], "--base needs a value"),
        (
            vec!["runs", "patch", "--base", "HEAD", "--min-lines", "x"],
            "is not a percentage",
        ),
        (
            vec!["runs", "patch", "--base", "HEAD", "--annotate", "gitlab"],
            "--annotate only supports github",
        ),
        (
            vec!["runs", "patch", "--base", "HEAD", "--max-annotations", "x"],
            "--max-annotations needs a whole number",
        ),
        (
            vec!["runs", "patch", "--base", "HEAD", "--bogus"],
            "unknown option --bogus",
        ),
        (vec!["runs", "report"], "report needs --format"),
        (vec!["runs", "report", "--format"], "--format needs a value"),
        (
            vec!["runs", "report", "--format", "pdf"],
            "unknown format pdf",
        ),
        (vec!["runs", "report", "--bogus"], "unknown option --bogus"),
        (
            vec!["merge", "run_0000000000000001"],
            "Usage: supercov merge",
        ),
        (
            vec!["merge", "run_0000000000000001", "run_0000000000000002"],
            "run_0000000000000001: no such local run",
        ),
        (
            vec!["runs", "clean", "--keep", "x"],
            "--keep must be a non-negative integer",
        ),
        (
            vec!["runs", "clean", "--bogus"],
            "Unknown clean option: --bogus",
        ),
        (
            vec!["docs", "cli", "assertions"],
            "docs accepts at most one topic",
        ),
        (vec!["docs", "nosuch"], "unknown guide \"nosuch\""),
    ] {
        let output = project.supercov(&args).exits(2);
        assert!(output.contains(message), "{args:?}: {output}");
    }
}

#[test]
fn a_command_supercov_cannot_measure_is_refused_with_the_reason() {
    let empty = Project::empty("unknown");
    let unknown = empty.supercov(&["--", "make", "test"]).exits(2);
    assert!(
        unknown.contains("could not recognize this project's language or test framework"),
        "{unknown}"
    );

    let runner = empty.supercov(&["--", "phpunit"]).exits(2);
    assert!(
        runner.contains("This looks like a PHP test run (the test command runs `phpunit`)"),
        "{runner}"
    );

    empty.write("composer.json", "{}");
    let project = empty.supercov(&["--", "make", "test"]).exits(2);
    assert!(
        project.contains("This looks like a PHP project (composer.json is present)"),
        "{project}"
    );

    let polyglot = empty
        .supercov(&["--", "sh", "-c", "node --test && cargo test"])
        .exits(2);
    assert!(
        polyglot.contains("launches multiple language frontends (javascript, rust)"),
        "{polyglot}"
    );
}

#[test]
fn a_per_file_floor_names_where_a_file_falls_short() {
    let project = Project::cart("per-file");
    let mut untested = String::from("export function untested(kind) {\n");
    for n in 0..14 {
        untested.push_str(&format!("  if (kind === {n}) {{ return {n}; }}\n"));
    }
    untested.push_str("  return -1;\n}\n");
    project.write("src/untested.js", &untested);
    project.measure(&[]);
    let failed = project
        .supercov(&["runs", "check", "--min-lines", "50", "--per-file"])
        .exits(1);
    contains_all(
        &failed,
        &[
            "src/untested.js: lines 0.00%  0/16 below 50%",
            "uncovered lines: 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, and 6 more",
        ],
    );
    assert!(!failed.contains("src/cart.js: lines"), "{failed}");
}

#[test]
fn a_platform_gated_item_counts_only_where_it_compiles() {
    let project = Project::empty("cfg");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"platform\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nextra = []\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn platform() -> &'static str {\n    native()\n}\n\n#[cfg(unix)]\nfn native() -> &'static str {\n    \"unix\"\n}\n\n#[cfg(windows)]\nfn native() -> &'static str {\n    \"windows\"\n}\n\n#[cfg(feature = \"extra\")]\npub fn extra() -> u32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn names_the_platform() {\n        assert!(!super::platform().is_empty());\n    }\n}\n",
    );
    project.git(&["init", "-q"]);
    let measured = project.supercov(&["--", "cargo", "test"]);
    measured.succeeds();
    // The variant this host compiles is measured and covered; the other one
    // no test here could ever reach, and it is not counted.
    let (built, absent) = if cfg!(windows) { (11, 6) } else { (6, 11) };
    let covered = project
        .supercov(&["runs", "latest", "line", &format!("src/lib.rs:{built}")])
        .succeeds();
    assert!(
        covered.contains("COVERED") && !covered.contains("NOT COVERED"),
        "{covered}"
    );
    let gaps = project
        .supercov(&["runs", "latest", "file", "src/lib.rs", "--json"])
        .json();
    let lines = gaps["data"]["gapLines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["line"].as_u64().unwrap())
        .collect::<Vec<_>>();
    // A feature is not the target's to settle; the build settles it. This
    // one was off, so `extra` was never compiled and is not counted either.
    assert!(lines.is_empty(), "{gaps}");
    assert!(
        measured.stderr().contains(
            "1 item(s) behind a #[cfg] no build compiled are outside the denominator: src/lib.rs:15 cfg(feature = \"extra\")"
        ),
        "{}",
        measured.stderr()
    );
    // Built with the feature, it is code no test runs.
    project
        .supercov(&["--", "cargo", "test", "--features", "extra"])
        .succeeds();
    let gaps = project
        .supercov(&["runs", "latest", "file", "src/lib.rs", "--json"])
        .json();
    let lines = gaps["data"]["gapLines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["line"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines, [16, 17], "{gaps}");
    assert!(!lines.contains(&absent));
}

#[test]
fn a_crate_with_no_library_runs_its_tests_and_no_doctests() {
    // A Worker or a plugin: `cargo test` runs its unit tests and, since a
    // cdylib cannot have doctests, none of those. `--doc` alone is Cargo's
    // own error.
    let project = Project::empty("cdylib");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"plugin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn double(value: u32) -> u32 {\n    value * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    );
    project.git(&["init", "-q"]);
    let output = project.supercov(&["--", "cargo", "test"]).succeeds();
    assert!(output.contains("1 passed"), "{output}");
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["Lines      100.00% (2/2)", "Passed      1"]);
    let doc = project.supercov(&["--", "cargo", "test", "--doc"]).exits(1);
    assert!(doc.contains("no library targets found"), "{doc}");

    // A crate that is only a binary has no library to doctest either.
    let binary = Project::empty("bin-only");
    binary.write(
        "Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    binary.write(
        "src/main.rs",
        "fn double(value: u32) -> u32 {\n    value * 2\n}\n\nfn main() {\n    println!(\"{}\", double(2));\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    );
    binary.git(&["init", "-q"]);
    let output = binary.supercov(&["--", "cargo", "test"]).succeeds();
    assert!(output.contains("1 passed"), "{output}");
}

#[test]
fn a_run_of_two_test_kinds_narrows_pages_and_groups() {
    let project = Project::cart("views");
    project.write(
        "src/tax.js",
        "export function tax(c) {\n  return c === \"LT\" || c === \"LV\" ? 0.21 : 0;\n}\n",
    );
    project.write(
        "test/tax.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { tax } from \"../src/tax.js\";\ntest(\"tax in LT\", () => { assert.equal(tax(\"LT\"), 0.21); });\n",
    );
    project.commit("tax");
    project
        .supercov_with(
            &["--", "node", "--test", "test/cart.test.js"],
            &[("SUPERCOV_TEST_KIND", "unit")],
        )
        .succeeds();
    let unit = project.latest();
    project
        .supercov_with(
            &["--", "node", "--test", "test/tax.test.js"],
            &[("SUPERCOV_TEST_KIND", "e2e")],
        )
        .succeeds();
    let e2e = project.latest();
    project.supercov(&["merge", &unit, &e2e]).exits(0);

    let kinds = project.supercov(&["runs", "latest", "kinds"]).succeeds();
    contains_all(&kinds, &["e2e  1 test(s)", "unit  3 test(s)"]);
    let narrowed = project
        .supercov(&["runs", "latest", "gaps", "--kind", "e2e"])
        .succeeds();
    contains_all(
        &narrowed,
        &[
            "Projection: kind e2e",
            "[covered elsewhere: 25; nowhere: 15]",
            "file --kind 'e2e'",
        ],
    );
    let runner = project
        .supercov(&[
            "runs",
            "latest",
            "files",
            "--runner",
            "node:test",
            "--limit",
            "1",
        ])
        .succeeds();
    contains_all(
        &runner,
        &["showing 1-1 of 2", "next page:", "--offset 1 --limit 1"],
    );
    let second = project
        .supercov(&["runs", "latest", "files", "--offset", "1", "--limit", "1"])
        .succeeds();
    assert!(second.contains("showing 2-2 of 2"), "{second}");

    let grouped = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--group",
            "decision",
            "--sort",
            "missing",
        ])
        .succeeds();
    contains_all(
        &grouped,
        &[
            "MC/DC by decision",
            "decisions 3, with missing conditions 3",
            "missing 2/2  express || sum < 50",
        ],
    );
    let refused = project
        .supercov(&["runs", "latest", "file", "src/cart.js", "--sort", "missing"])
        .exits(2);
    assert!(
        refused.contains("--sort requires --group decision"),
        "{refused}"
    );
    let decision = project
        .supercov(&[
            "runs",
            "latest",
            "decision",
            "src/tax.js:2",
            "--kind",
            "e2e",
        ])
        .succeeds();
    contains_all(&decision, &["C2 MISSING: c === \"LV\"", "T- -> T  tests=1"]);
    let line = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:13",
            "--kind",
            "unit",
            "--limit",
            "1",
        ])
        .succeeds();
    assert!(line.contains("PARTIAL"), "{line}");

    // Directories, the summary and a file's decisions narrow to one kind or
    // runner, and a filter no test matches is refused.
    for (args, expected) in [
        (
            &["runs", "latest", "files", "--group", "dir", "--kind", "e2e"][..],
            &[
                "Coverage by directory, 1 deep — kind e2e",
                "src            2     15  13.33%",
            ][..],
        ),
        (
            &[
                "runs",
                "latest",
                "files",
                "--group",
                "dir",
                "--runner",
                "node:test",
            ],
            &["— runner node:test", "src            2     15  80.00%"],
        ),
        (
            &["runs", "latest", "files", "--group", "dir"],
            &[
                "Lines     e2e    unit     All",
                "src            2     15  13.33%  66.67%  80.00%",
            ],
        ),
        (
            &["runs", "latest", "--kind", "e2e"],
            &["(kind e2e)", "Lines      13.33% (2/15)", "Total       1"],
        ),
        (
            &["runs", "latest", "--runner", "node:test"],
            &[
                "(runner node:test)",
                "Lines      80.00% (12/15)",
                "Total       4",
            ],
        ),
        (
            &[
                "runs",
                "latest",
                "file",
                "src/cart.js",
                "--group",
                "decision",
                "--kind",
                "e2e",
            ],
            &["decisions 3, with missing conditions 3; conditions missing 5/5"],
        ),
    ] {
        contains_all(&project.supercov(args).succeeds(), expected);
    }
    for args in [
        &[
            "runs",
            "latest",
            "files",
            "--group",
            "dir",
            "--kind",
            "integration",
        ][..],
        &[
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--group",
            "decision",
            "--kind",
            "integration",
        ],
    ] {
        let refused = project.supercov(args).exits(2);
        assert!(
            refused.contains("No tests match kind=integration"),
            "{refused}"
        );
    }

    // After an edit, the summary says the run is stale.
    project.edit("src/tax.js", "0.21", "0.2");
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(
        summary.contains("[STALE: instrumented source changed"),
        "{summary}"
    );
}

#[test]
fn the_report_compares_two_runs_and_names_a_mistake() {
    let project = Project::cart("report-compare");
    let older = project.measure(&["--test-name-pattern=total"]);
    let newer = project.measure(&[]);
    let help = project.supercov(&["report", "--help"]).succeeds();
    contains_all(&help, &["--compare <run-id>", "--no-open"]);
    for (args, message) in [
        (vec!["report", "--compare"], "--compare requires a run ID"),
        (vec!["report", "--output"], "--output requires a path"),
        (
            vec!["report", "--output", ""],
            "--output requires a non-empty path",
        ),
    ] {
        let output = project.supercov(&args).exits(2);
        assert!(output.contains(message), "{args:?}: {output}");
    }
    project
        .supercov(&[
            "report",
            &newer,
            "--compare",
            &older,
            "--runs",
            "2",
            "--output",
            "compare.html",
            "--no-open",
        ])
        .exits(0);
    // The page carries its data gzipped and base64-encoded, so it needs no
    // server; both runs are in it.
    let html = project.read("compare.html");
    let encoded = html
        .split(|character: char| !(character.is_ascii_alphanumeric() || "+/=".contains(character)))
        .max_by_key(|run| run.len())
        .unwrap();
    use base64::Engine as _;
    use std::io::Read as _;
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut payload = String::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .read_to_string(&mut payload)
        .unwrap();
    assert!(
        payload.contains(&older) && payload.contains(&newer),
        "both runs are in the report"
    );
    let missing = project
        .supercov(&[
            "report",
            &newer,
            "--compare",
            "run_0000000000000009",
            "--no-open",
            "--output",
            "x.html",
        ])
        .exits(2);
    assert!(missing.contains("run_0000000000000009"), "{missing}");
}

/// A bug in Supercov's instrumenter, made to happen on a file
/// (`SUPERCOV_TEST_FAULT`), costs that file its measurement and nothing more:
/// it runs as written, the suite passes, and the run names the file and the
/// error to report.
#[test]
fn an_internal_error_instrumenting_a_file_leaves_it_unmeasured_and_said() {
    let project = Project::cart("instrumenter-bug");
    let fault = [("SUPERCOV_TEST_FAULT", "instrument-panic")];
    let measured = project.supercov_with(&["--", "node", "--test"], &fault);
    assert_eq!(measured.code(), 0, "{}", measured.stderr());
    assert!(
        measured.stdout().contains("pass 3"),
        "{}",
        measured.stdout()
    );
    assert!(
        measured.stderr().contains("test fault at instrument-panic"),
        "{}",
        measured.stderr()
    );
    let line = project
        .supercov(&["runs", "latest", "line", "src/cart.js:1"])
        .succeeds();
    contains_all(
        &line,
        &[
            "NOT MEASURED",
            "Supercov hit an internal error: injected fault at instrument-panic instrumenting this file, a bug to report",
        ],
    );

    // The same in Rust: the file compiles as written.
    let project = Project::empty("instrumenter-bug-rust");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn half(x: i32) -> i32 { x / 2 }\n#[cfg(test)]\nmod tests { #[test] fn h() { assert_eq!(super::half(4), 2); } }\n",
    );
    project.git(&["init", "-q"]);
    let measured = project.supercov_with(&["--", "cargo", "test"], &fault);
    assert_eq!(measured.code(), 0, "{}", measured.stderr());
    assert!(
        measured.stderr().contains("test fault at instrument-panic"),
        "{}",
        measured.stderr()
    );
    let line = project
        .supercov(&["runs", "latest", "line", "src/lib.rs:1"])
        .succeeds();
    contains_all(
        &line,
        &[
            "NOT MEASURED",
            "Supercov hit an internal error: injected fault at instrument-panic instrumenting this file, a bug to report",
        ],
    );
}

#[test]
fn a_source_file_that_does_not_parse_runs_as_written_and_is_declared() {
    let project = Project::cart("unparsable");
    project.write(
        "src/broken.js",
        "export function broken( {\n  return 1;\n}\n",
    );
    // The suite never loads it, and passes with or without Supercov.
    let measured = project.supercov(&["--", "node", "--test"]).succeeds();
    assert!(measured.contains("pass 3"), "{measured}");
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            "Lines      76.92% (10/13)",
            "Incomplete — 1 blocking limitation(s) in 1 file(s)",
        ],
    );
    let line = project
        .supercov(&["runs", "latest", "line", "src/broken.js:1"])
        .succeeds();
    contains_all(
        &line,
        &[
            "NOT MEASURED",
            "Supercov could not parse this file (Expected `:` but found `decimal` at line 2, column 10)",
            "(read from the current working tree)",
        ],
    );

    // A test file that does not parse fails in its runner, exactly as it
    // does without Supercov.
    project.write(
        "test/broken.test.js",
        "import test from \"node:test\";\ntest(\"broken\", () => {\n",
    );
    let plain = std::process::Command::new("node")
        .args(["--test"])
        .current_dir(&project.root)
        .output()
        .unwrap();
    let under = project.supercov(&["--", "node", "--test"]);
    assert_eq!(under.code(), plain.status.code().unwrap());
    assert!(under.stdout().contains("fail 1"), "{}", under.stdout());
}

#[cfg(unix)]
#[test]
fn links_inside_the_project_are_carried_and_ones_leaving_it_are_not() {
    use std::os::unix::fs::symlink;
    let project = Project::cart("links");
    let outside = Project::empty("links-outside");
    outside.write("shared.js", "export const shared = 1;\n");
    symlink(
        outside.root.join("shared.js"),
        project.root.join("src/escaping.js"),
    )
    .unwrap();
    symlink("cart.js", project.root.join("src/alias.js")).unwrap();
    symlink("nowhere.js", project.root.join("src/dangling.js")).unwrap();
    std::fs::create_dir(project.root.join("data")).unwrap();
    symlink("../src", project.root.join("data/sources")).unwrap();

    let measured = project.supercov(&["--", "node", "--test"]).succeeds();
    assert!(measured.contains("pass 3"), "{measured}");
    let said = project.supercov(&["--", "node", "--test"]).stderr();
    assert!(
        said.contains("omitting symlink outside the isolated project"),
        "{said}"
    );
    assert!(said.contains("src/escaping.js"), "{said}");
    // The author's links are left as they were.
    assert!(
        std::fs::symlink_metadata(project.root.join("src/escaping.js"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("Lines      76.92% (10/13)"), "{summary}");
}

#[cfg(unix)]
#[test]
fn a_store_that_is_a_link_elsewhere_is_refused() {
    let project = Project::cart("store-link");
    let elsewhere = Project::empty("store-elsewhere");
    std::os::unix::fs::symlink(&elsewhere.root, project.root.join(".supercov")).unwrap();
    let refused = project.supercov(&["--", "node", "--test"]);
    assert_ne!(refused.code(), 0);
    assert!(
        refused.stderr().contains("unsafe Supercov storage path"),
        "{}",
        refused.stderr()
    );
    assert!(
        std::fs::read_dir(&elsewhere.root).unwrap().next().is_none(),
        "nothing written through the link"
    );
}

#[test]
fn cargo_flags_the_build_owns_are_left_to_it_and_runs_with_no_test_are_refused() {
    let project = Project::empty("cargo-flags");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"flags\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn double(x: u32) -> u32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    );
    project.git(&["init", "-q"]);
    // The build reads Cargo's JSON into its own target directory; a command
    // asking for another format or directory still measures.
    for flags in [
        vec!["--message-format", "json"],
        vec!["--message-format=short"],
        vec!["--target-dir", "elsewhere"],
        vec!["--target-dir=elsewhere"],
    ] {
        let mut args = vec!["--", "cargo", "test"];
        args.extend(flags.iter().copied());
        let measured = project.supercov(&args).succeeds();
        let said = format!(
            "{measured}{}",
            project.supercov(&["runs", "latest"]).succeeds()
        );
        assert!(
            said.contains("Lines      100.00% (2/2)"),
            "{flags:?}: {said}"
        );
    }
    for (args, message) in [
        (
            vec!["--", "cargo", "test", "--no-run"],
            "`--no-run` builds the tests without running any",
        ),
        (
            vec!["--", "cargo", "test", "--", "--list"],
            "libtest --list does not execute a test suite",
        ),
        (
            vec!["--", "cargo", "bench"],
            "requires `cargo test` or `cargo nextest run`",
        ),
    ] {
        // Cargo quotes a refused argument in color when CI asks for it.
        let refused = project.supercov_with(&args, &[("CARGO_TERM_COLOR", "never")]);
        assert_ne!(refused.code(), 0, "{args:?}");
        assert!(
            refused.stderr().contains(message),
            "{args:?}: {}",
            refused.stderr()
        );
    }
}

#[test]
fn a_rust_command_supercov_cannot_select_tests_from_is_refused_with_why() {
    let project = Project::empty("cargo-refusals");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"refusals\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn double(x: u32) -> u32 {\n    x * 2\n}\n\npub fn half(x: u32) -> u32 {\n    x / 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n\n    #[test]\n    fn halves() {\n        assert_eq!(super::half(4), 2);\n    }\n}\n",
    );
    project.write(
        "package.json",
        "{\"scripts\":{\"test\":\"cargo test 'unclosed\",\"chained\":\"cargo test && echo done\"}}\n",
    );
    project.git(&["init", "-q"]);
    // A package script is expanded and split the way a shell would.
    for (args, message) in [
        (
            vec!["--", "npm", "test"],
            "contains an incomplete quote or escape",
        ),
        (
            vec!["--", "npm", "run", "chained"],
            "the Cargo test command contains an unsupported shell boundary",
        ),
    ] {
        // Cargo quotes a refused argument in color when CI asks for it.
        let refused = project.supercov_with(&args, &[("CARGO_TERM_COLOR", "never")]);
        assert_ne!(refused.code(), 0, "{args:?}");
        assert!(
            refused.stderr().contains(message),
            "{args:?}: {}",
            refused.stderr()
        );
    }
    std::fs::remove_file(project.root.join("package.json")).unwrap();
    for (args, message) in [
        (
            vec!["--", "sh", "-c", "cargo build && cargo test"],
            "contains a shell boundary before `test`",
        ),
        (
            vec!["--", "make", "test"],
            "does not expose a stable Cargo invocation",
        ),
        (
            vec!["--", "cargo", "test", "nextest", "run"],
            "ambiguously contains both test and nextest run",
        ),
        (
            vec!["--", "cargo", "test", "--frobnicate"],
            "does not recognize option --frobnicate",
        ),
        (
            vec!["--", "cargo", "test", "--package"],
            "Cargo option --package has no value",
        ),
        (
            vec!["--", "cargo", "test", "--doc", "--lib"],
            "--doc cannot be combined with another explicit target selection",
        ),
        (
            vec!["--", "cargo", "test", "doubles", "halves"],
            "unexpected argument 'halves' found",
        ),
        (
            vec!["--", "cargo", "test", "--", "--skip"],
            "libtest --skip has no filter value",
        ),
        (
            vec!["--", "cargo", "test", "--", "--test-threads"],
            "libtest --test-threads has no value",
        ),
        (
            vec!["--", "cargo", "test", "--", "--test-threads", "0"],
            "must not be 0",
        ),
        (
            vec!["--", "cargo", "test", "--", "--test-threads=many"],
            "must be a number > 0",
        ),
        (
            vec![
                "--",
                "cargo",
                "test",
                "--",
                "--test-threads",
                "2",
                "--test-threads=3",
            ],
            "was provided more than once",
        ),
        (
            vec![
                "--",
                "cargo",
                "test",
                "--",
                "--test-threads=2",
                "--test-threads",
                "3",
            ],
            "was provided more than once",
        ),
        (
            vec!["--", "cargo", "test", "--", "-Z"],
            "libtest -Z has no feature value",
        ),
        (
            vec!["--", "cargo", "test", "--", "--format"],
            "libtest --format has no value",
        ),
        (
            vec!["--", "cargo", "test", "--", "-h"],
            "libtest -h does not execute a test suite",
        ),
        (
            vec!["--", "cargo", "test", "--", "--frobnicate"],
            "libtest discovery contract does not recognize option --frobnicate",
        ),
    ] {
        // Cargo quotes a refused argument in color when CI asks for it.
        let refused = project.supercov_with(&args, &[("CARGO_TERM_COLOR", "never")]);
        assert_ne!(refused.code(), 0, "{args:?}");
        assert!(
            refused.stderr().contains(message),
            "{args:?}: {}",
            refused.stderr()
        );
    }
    // Selections libtest understands narrow the run to the tests they name.
    for (args, tests) in [
        (
            vec!["--", "cargo", "test", "--", "--exact", "tests::doubles"],
            1,
        ),
        (vec!["--", "cargo", "test", "--", "--skip", "halves"], 1),
        (
            vec![
                "--",
                "cargo",
                "test",
                "--",
                "--skip=doubles",
                "--format",
                "terse",
            ],
            1,
        ),
        (
            vec![
                "--",
                "cargo",
                "test",
                "-j",
                "2",
                "--",
                "--test-threads",
                "1",
            ],
            2,
        ),
    ] {
        project.supercov(&args).succeeds();
        let summary = project.supercov(&["runs", "latest"]).succeeds();
        assert!(
            summary.contains(&format!("Passed      {tests}")),
            "{args:?}: {summary}"
        );
    }
}

#[test]
fn the_harness_commands_refuse_input_they_cannot_read() {
    let project = Project::cart("harness-commands");
    let run = project.measure(&[]);
    let input = project.root.join("input.json");
    let input_path = input.to_str().unwrap();
    let feed = |command: &str, bytes: &[u8]| {
        std::fs::write(&input, bytes).unwrap();
        project.supercov_with(&[command], &[("SUPERCOV_INTERNAL_INPUT_FILE", input_path)])
    };
    for (command, invalid) in [
        ("__run-js-direct", "invalid direct JavaScript run input"),
        ("__benchmark-js-transform", "invalid Rust benchmark input"),
        ("__instrument-js", "invalid Rust instrumenter input"),
        ("__query-stored-run", "invalid stored query input"),
    ] {
        let missing = project.supercov_with(
            &[command],
            &[("SUPERCOV_INTERNAL_INPUT_FILE", "no-such-input.json")],
        );
        assert!(
            missing
                .exits(2)
                .contains("failed to read Rust engine input"),
            "{command}"
        );
        assert!(
            feed(command, b"\xff\xfe").exits(2).contains("is not UTF-8"),
            "{command}"
        );
        assert!(feed(command, b"{").exits(2).contains(invalid), "{command}");
    }

    let source = r#"[{"file":"a.js","source":"export const f = (x) => x ? 1 : 2;\n"}]"#;
    let timed = feed("__benchmark-js-transform", source.as_bytes()).json();
    assert_eq!(timed["files"], 1);
    let instrumented = feed("__instrument-js", source.as_bytes()).json();
    assert_eq!(instrumented.as_array().unwrap().len(), 1);
    let broken = r#"[{"file":"broken.js","source":"let = ;"}]"#;
    for command in ["__benchmark-js-transform", "__instrument-js"] {
        assert!(
            feed(command, broken.as_bytes())
                .exits(2)
                .contains("broken.js"),
            "{command}"
        );
    }

    let root = project.root.to_str().unwrap();
    let query = |run_id: &str, command: &str, newer: Option<&str>| {
        serde_json::json!({
            "root": root,
            "query": {"runId": run_id, "filter": "passed", "command": command, "target": 20.0},
            "newerRunId": newer,
        })
        .to_string()
    };
    let minimized = feed(
        "__query-stored-run",
        query(&run, "minimize", None).as_bytes(),
    )
    .json();
    assert_eq!(minimized["ok"], true, "{minimized}");
    let diffed = feed(
        "__query-stored-run",
        query(&run, "diff", Some(&run)).as_bytes(),
    )
    .json();
    assert_eq!(diffed["ok"], true, "{diffed}");
    for (request, message) in [
        (
            query(&run, "diff", None),
            "stored diff requires a newer run ID",
        ),
        (query("no-such-run", "summary", None), "stored query failed"),
    ] {
        assert!(
            feed("__query-stored-run", request.as_bytes())
                .exits(2)
                .contains(message),
            "{request}"
        );
    }

    // The supervisor runs a command and passes its exit code on.
    assert!(
        project
            .supercov(&["__supervise"])
            .exits(2)
            .contains("test command must not be empty")
    );
    project
        .supercov(&["__supervise", "--", "sh", "-c", "exit 3"])
        .exits(3);
    for name in [
        "SUPERCOV_DIAGNOSTIC_INTERVAL_MS",
        "SUPERCOV_COMMAND_TIMEOUT_MS",
    ] {
        assert!(
            project
                .supercov_with(&["__supervise", "--", "true"], &[(name, "soon")])
                .exits(2)
                .contains(name),
            "{name}"
        );
    }
    // A command still running is reported at each interval, in ms then s.
    let slow = project
        .supercov_with(
            &["__supervise", "--", "sleep", "2.5"],
            &[("SUPERCOV_DIAGNOSTIC_INTERVAL_MS", "400")],
        )
        .exits(0);
    let reports = slow
        .lines()
        .filter(|line| line.starts_with("[supercov] command still running after "))
        .collect::<Vec<_>>();
    assert!(reports.iter().any(|line| line.ends_with("ms")), "{slow}");
    assert!(
        reports
            .iter()
            .any(|line| line.ends_with('s') && !line.ends_with("ms")),
        "{slow}"
    );
    project.supercov(&["__sweep-trash"]).exits(2);
    project.supercov(&["__sweep-trash", root]).succeeds();
}

/// A sandbox SDK as a package would ship it: it mounts a host directory into
/// a "guest" (a symlink here) and runs a command there with exactly the
/// environment the call passes.
const SANDBOX: &str = r#"import { spawn } from "node:child_process";
import { mkdirSync, symlinkSync } from "node:fs";
import { dirname } from "node:path";

export const sandbox = {
  async boot(options) {
    const mount = options.mounts?.[0] ?? { source: options.hostPath, target: options.guestPath };
    mkdirSync(dirname(mount.target), { recursive: true });
    symlinkSync(mount.source, mount.target, "dir");
    return {
      run(argv, options) {
        // A remote guest sees only what the call hands it: Supercov has to
        // have translated the project root into the guest's path already.
        if (options.env.SUPERCOV_PROJECT_ROOT !== mount.target) return 97;
        return new Promise((resolve, reject) => {
          const child = spawn(argv[0], argv.slice(1), {
            cwd: mount.target,
            env: options.env,
            stdio: "inherit",
          });
          child.once("error", reject);
          child.once("close", (code) => resolve(code));
        });
      },
    };
  },
};

export default sandbox;
"#;

#[test]
fn code_a_test_runs_inside_a_sandbox_it_mounts_is_credited_to_that_test() {
    let project = Project::empty("sandbox-launch");
    project.write(
        "package.json",
        r#"{ "name": "remote", "private": true, "type": "module" }"#,
    );
    project.write("src/cart.js", common::CART);
    project.write(
        "node_modules/opaque-sandbox/package.json",
        r#"{ "name": "opaque-sandbox", "type": "module", "exports": "./index.mjs" }"#,
    );
    project.write("node_modules/opaque-sandbox/index.mjs", SANDBOX);
    project.write(
        "guest/describe.mjs",
        "import { describe } from \"../src/cart.js\";\nif (describe([{ name: \"pen\" }]) !== \"pen\") process.exit(1);\n",
    );
    project.write(
        "test/remote.test.js",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sandbox } from "opaque-sandbox";

test("the guest describes the cart", async () => {
  const target = join(mkdtempSync(join(tmpdir(), "guest-")), "workspace");
  const machine = await sandbox.boot({ mounts: [{ source: process.cwd(), target }] });
  const code = await machine.run([process.execPath, "guest/describe.mjs"], { env: process.env });
  assert.equal(code, 0);
});
"#,
    );
    // The same launch through a namespace import and a parenthesized callee,
    // and through a default import with the mapping spelled out.
    project.write(
        "test/namespace.test.js",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import * as sdk from "opaque-sandbox";

test("a namespace import launches too", async () => {
  const target = join(mkdtempSync(join(tmpdir(), "guest-")), "workspace");
  const machine = await (sdk.sandbox).boot({ mounts: [{ source: process.cwd(), target }] });
  assert.equal(await machine.run([process.execPath, "guest/describe.mjs"], { env: process.env }), 0);
});
"#,
    );
    project.write(
        "test/default.test.js",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import box from "opaque-sandbox";

test("a default import launches too", async () => {
  const guestPath = join(mkdtempSync(join(tmpdir(), "guest-")), "workspace");
  const machine = await box.boot({ hostPath: process.cwd(), guestPath });
  assert.equal(await machine.run([process.execPath, "guest/describe.mjs"], { env: process.env }), 0);
});
"#,
    );
    // Product code that launches the sandbox is rewritten the same way.
    project.write(
        "src/remote.js",
        r#"import { sandbox } from "opaque-sandbox";
import * as sdk from "opaque-sandbox";
import box from "opaque-sandbox";

export async function runInGuest(how, script, target) {
  const options = { mounts: [{ source: process.cwd(), target }] };
  const machine =
    how === "named"
      ? await sandbox.boot(options)
      : how === "namespace"
        ? await (sdk.sandbox).boot(options)
        : await box.boot(options);
  return machine.run([process.execPath, script], { env: process.env });
}
"#,
    );
    project.write(
        "test/source.test.js",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runInGuest } from "../src/remote.js";

test("product code launches the guest", async () => {
  for (const how of ["named", "namespace", "default"]) {
    const guest = join(mkdtempSync(join(tmpdir(), "guest-")), "workspace");
    assert.equal(await runInGuest(how, "guest/describe.mjs", guest), 0);
  }
});
"#,
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", "node", "--test"]).succeeds();
    let summary = project.supercov(&["runs", "latest", "--json"]).json();
    let transport = &summary["data"]["transport"];
    assert_eq!(transport["workspaceCapabilities"], 6, "{transport}");
    assert_eq!(transport["remoteLaunches"], 6, "{transport}");
    // `describe` runs only in the guest, and its line is the launching tests'.
    let line = project
        .supercov(&["runs", "latest", "line", "src/cart.js:20"])
        .succeeds();
    contains_all(
        &line,
        &[
            "the guest describes the cart",
            "a namespace import launches too",
            "a default import launches too",
            "product code launches the guest",
        ],
    );
}

#[test]
fn optional_calls_through_typescript_wrappers_are_measured_alternative_by_alternative() {
    let project = Project::empty("optional-calls");
    project.write(
        "package.json",
        r#"{ "name": "calls", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/calls.ts",
        r#"type Fn = (() => string) | undefined;

export class Box {
  #hook: Fn;
  constructor(hook: Fn) {
    this.#hook = hook;
  }
  hook(): string | undefined {
    return this.#hook?.();
  }
}

export function call(f: Fn, table?: Record<string, Fn>, key = "k", box?: { run?: Fn; make?: () => Fn }) {
  return [
    (f as Fn)?.(),
    (f satisfies Fn)?.(),
    f!?.(),
    (f)?.(),
    table?.[key]?.(),
    box?.run?.(),
    box?.run!?.(),
    box?.make?.()?.(),
    (box?.make?.())?.(),
  ];
}

export function never(f: Fn) {
  return [
    f?.(),
  ];
}
"#,
    );
    project.write(
        "test/calls.test.ts",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { Box, call } from "../src/calls.ts";

test("calls each shape when present", () => {
  const f = () => "y";
  assert.deepEqual(call(f, { k: f }, "k", { run: f, make: () => f }), Array(9).fill("y"));
  assert.equal(new Box(f).hook(), "y");
});

test("skips each shape when absent", () => {
  assert.deepEqual(call(undefined, undefined, "k", undefined), Array(9).fill(undefined));
  assert.equal(new Box(undefined).hook(), undefined);
});
"#,
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", "node", "--test"]).succeeds();
    let file = project
        .supercov(&["runs", "latest", "file", "src/calls.ts"])
        .succeeds();
    contains_all(
        &file,
        &[
            "Branch outcomes not taken       9",
            "key = \"k\"",
            "table?.[key]?.()",
            "box?.run?.()",
            "box?.run!?.()",
        ],
    );
    // A line inside an expression that never ran is not covered, though it
    // has no line obligation of its own.
    assert!(file.contains("   29  NOT COVERED   f?.()"), "{file}");
    let never = project
        .supercov(&["runs", "latest", "line", "src/calls.ts:29"])
        .succeeds();
    assert!(never.contains("Status\n  NOT COVERED"), "{never}");
    // A chain that stops at `box?.` never asked whether `make()` returned
    // nothing; parentheses end the chain, so there the call did see nothing.
    let chained = project
        .supercov(&["runs", "latest", "line", "src/calls.ts:22"])
        .succeeds();
    assert_eq!(
        chained
            .matches("Unobserved: nullish / short-circuited")
            .count(),
        2,
        "{chained}"
    );
    let parenthesized = project
        .supercov(&["runs", "latest", "line", "src/calls.ts:23"])
        .succeeds();
    assert_eq!(
        parenthesized
            .matches("Unobserved: nullish / short-circuited")
            .count(),
        1,
        "{parenthesized}"
    );

    project.edit(
        "test/calls.test.ts",
        "test(\"skips each shape when absent\"",
        "test(\"skips each shape whose own step is absent\", () => {\n  const f = () => \"y\";\n  assert.deepEqual(call(f, {}, undefined, {}), [\"y\", \"y\", \"y\", \"y\", undefined, undefined, undefined, undefined, undefined]);\n  assert.deepEqual(call(f, {}, undefined, { make: () => undefined }).slice(7), [undefined, undefined]);\n});\n\ntest(\"skips each shape when absent\"",
    );
    project.supercov(&["--", "node", "--test"]).succeeds();
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &["Lines      77.78% (7/9)", "Branches   94.74% (36/38)"],
    );
}

#[cfg(unix)]
#[test]
fn a_switch_case_reached_by_falling_through_is_not_credited_and_nested_switches_count() {
    // `level(20)` falls from `case n > 10` into `case n > 5`, and `kind(0)`
    // from `case 0` into `case 1`: neither later test matched. A switch inside
    // a switch with no default used to be neither measured nor declared.
    let project = Project::empty("switches");
    project.write(
        "package.json",
        r#"{ "name": "switches", "type": "module" }"#,
    );
    project.write("src/s.js", "export function level(n) {\n  let out = \"\";\n  switch (true) {\n    case n > 10:\n      out += \"high \";\n    case n > 5:\n      out += \"mid\";\n      break;\n    default:\n      out = \"low\";\n  }\n  return out;\n}\nexport function kind(x) {\n  switch (x) {\n    case 0:\n    case 1:\n      return \"small\";\n  }\n  return \"other\";\n}\nexport function nested(a, b) {\n  switch (a) {\n    case 1:\n      switch (b) {\n        case \"x\":\n          return \"1x\";\n        case \"y\":\n          return \"1y\";\n      }\n  }\n  return \"?\";\n}\nexport function looped(xs) {\n  let total = 0;\n  outer: for (const x of xs) {\n    switch (x) {\n      case -1:\n        break outer;\n      case 0:\n        continue;\n      default:\n        total += x;\n    }\n  }\n  return total;\n}\n");
    project.write("test/s.test.js", "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { level, kind, nested, looped } from \"../src/s.js\";\ntest(\"switches\", () => {\n  assert.equal(level(20), \"high mid\");\n  assert.equal(kind(0), \"small\");\n  assert.equal(nested(1, \"x\"), \"1x\");\n  assert.equal(looped([1, 0, 2, -1, 5]), 3);\n});\n");
    project.git(&["init", "-q"]);
    let measured = project.supercov(&["--", "node", "--test"]).succeeds();
    assert!(measured.contains("pass 1"), "{measured}");
    let file = project
        .supercov(&["runs", "latest", "file", "src/s.js"])
        .succeeds();
    contains_all(
        &file,
        &[
            "    3  PARTIAL       true\n       Unobserved: branch outcome not observed: case n > 5\n       Unobserved: branch outcome not observed: default\n",
            "   15  PARTIAL       x\n       Unobserved: branch outcome not observed: case 1\n",
            "   25  PARTIAL       b\n       Unobserved: branch outcome not observed: case \"y\"\n       Unobserved: switch no-match outcome not observed\n",
        ],
    );
    // Labels still reach the loop around the switch, and each of its cases
    // was selected.
    assert!(!file.contains("   37  PARTIAL"), "{file}");
    assert!(!file.contains("   38  PARTIAL"), "{file}");
}

#[test]
fn an_optional_link_a_chain_cut_short_before_it_is_not_credited() {
    // In `a?.b?.c` the object of `?.c` is undefined when `a` is, though
    // `?.c` never ran, and so is the object of `?.b` in `f?.()?.b` when `f`
    // is. Each link is credited only with what it saw itself,
    // and a getter or call between two links that evaluates chains of its
    // own does not disturb them.
    let project = Project::empty("optional-links");
    project.write(
        "package.json",
        r#"{ "name": "chain", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/c.js",
        "export const realNullish = (o) => o?.inner?.value;\nexport const three = (a) => a?.b?.c?.d;\nexport const viaCall = (o) => o?.get().x?.y;\nconst noisy = { get a() { const cut = null; cut?.z?.w; return { b: 1 }; } };\nexport const viaGetter = (o) => o?.a?.b;\nexport { noisy };\nexport class Holder {\n  #inner;\n  constructor(inner) { this.#inner = inner; }\n  read(self) { return self?.#inner?.value; }\n}\nexport const callThenMember = (f) => f?.()?.b;\nexport const callReturnsNothing = (f) => f?.()?.b;\n",
    );
    project.write(
        "test/c.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport * as c from \"../src/c.js\";\ntest(\"chains\", () => {\n  // both links of realNullish: first nullish, then second nullish\n  assert.equal(c.realNullish(null), undefined);\n  assert.equal(c.realNullish({ inner: null }), undefined);\n  assert.equal(c.realNullish({ inner: { value: 1 } }), 1);\n  // three: a null, then b null; c never null\n  assert.equal(c.three(null), undefined);\n  assert.equal(c.three({ b: null }), undefined);\n  assert.equal(c.three({ b: { c: { d: 4 } } }), 4);\n  // viaCall: o null only; x never null\n  assert.equal(c.viaCall(null), undefined);\n  assert.equal(c.viaCall({ get: () => ({ x: { y: 2 } }) }), 2);\n  // viaGetter: o present, getter cuts its own chain short; a never null\n  assert.equal(c.viaGetter(c.noisy), 1);\n  assert.equal(c.viaGetter(null), undefined);\n  const h = new c.Holder({ value: 3 });\n  assert.equal(h.read(h), 3);\n  assert.equal(h.read(null), undefined);\n  // callThenMember: f null only; f() never returns nullish\n  assert.equal(c.callThenMember(null), undefined);\n  assert.equal(c.callThenMember(() => ({ b: 1 })), 1);\n  // callReturnsNothing: f null, f() null, and f() an object\n  assert.equal(c.callReturnsNothing(null), undefined);\n  assert.equal(c.callReturnsNothing(() => null), undefined);\n  assert.equal(c.callReturnsNothing(() => ({ b: 1 })), 1);\n});\n",
    );
    project.git(&["init", "-q"]);
    let measured = project.supercov(&["--", "node", "--test"]).succeeds();
    assert!(measured.contains("pass 1"), "{measured}");
    let file = project
        .supercov(&["runs", "latest", "file", "src/c.js"])
        .succeeds();
    let gaps = file
        .lines()
        .skip_while(|line| !line.starts_with(" LINE"))
        .filter(|line| !line.contains("Inspect:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        gaps,
        " LINE  STATUS        SOURCE\n    2  PARTIAL       a?.b?.c?.d\n       Unobserved: nullish short-circuit outcome not observed\n    3  PARTIAL       o?.get().x?.y\n       Unobserved: nullish short-circuit outcome not observed\n    4  PARTIAL       cut?.z\n       Unobserved: non-nullish continuation outcome not observed\n       Unobserved: nullish short-circuit outcome not observed\n       Unobserved: non-nullish continuation outcome not observed\n    5  PARTIAL       o?.a?.b\n       Unobserved: nullish short-circuit outcome not observed\n   10  PARTIAL       self?.#inner?.value\n       Unobserved: nullish short-circuit outcome not observed\n   12  PARTIAL       f?.()?.b\n       Unobserved: nullish short-circuit outcome not observed\nshowing 1-6 of 6 gap lines",
    );
}

#[cfg(unix)]
#[test]
fn a_project_with_links_sockets_and_pipes_is_measured_through_them() {
    let outside = Project::empty("links-outside");
    outside.write("secret.js", "export const secret = 1;\n");
    outside.write(
        "store/fake-pkg/package.json",
        r#"{ "name": "fake-pkg", "type": "module", "exports": "./index.js" }"#,
    );
    outside.write(
        "store/fake-pkg/index.js",
        "export const triple = (x) => x * 3;\n",
    );
    let project = Project::empty("links");
    project.write(
        "package.json",
        r#"{ "name": "links", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/math.js",
        "export function double(x) {\n  return x * 2;\n}\n",
    );
    let link = |target: &std::path::Path, name: &str| {
        std::os::unix::fs::symlink(target, project.root.join(name)).unwrap();
    };
    // `lib` is a conventional source directory name; here it is a link.
    link(std::path::Path::new("src"), "lib");
    link(&project.root.join("src"), "abs-lib");
    link(std::path::Path::new("nowhere.js"), "dangling");
    link(&project.root.join("nowhere.js"), "abs-dangling");
    link(&outside.root, "escape");
    // pnpm keeps the root node_modules in a store elsewhere.
    link(&outside.root.join("store"), "node_modules");
    // A running dev server's socket and a pipe hold no source.
    std::fs::create_dir(project.root.join("tmp")).unwrap();
    // A socket path is limited to about a hundred bytes, so it is bound
    // short and moved in.
    let short = std::env::temp_dir().join(format!("supercov-{}.sock", std::process::id()));
    let _listener = std::os::unix::net::UnixListener::bind(&short).unwrap();
    std::fs::rename(&short, project.root.join("tmp/dev.sock")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(project.root.join("tmp/pipe"))
            .status()
            .unwrap()
            .success()
    );
    project.write(
        "test/links.test.js",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, lstatSync } from "node:fs";
import { double } from "../lib/math.js";
import { double as absDouble } from "../abs-lib/math.js";
import { triple } from "fake-pkg";

test("links inside the project resolve in the copy", () => {
  assert.equal(double(2), 4);
  assert.equal(absDouble(3), 6);
  assert.equal(triple(2), 6);
});

test("dangling links stay, a link out of the project does not", () => {
  assert.ok(lstatSync("dangling").isSymbolicLink());
  assert.ok(lstatSync("abs-dangling").isSymbolicLink());
  assert.equal(existsSync("escape"), false);
});
"#,
    );
    project.git(&["init", "-q"]);
    let measured = project.supercov(&["--", "node", "--test"]);
    assert!(
        measured
            .exits(0)
            .contains("omitting symlink outside the isolated project"),
        "the link out of the project is named"
    );
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["Lines      100.00% (2/2)", "Passed      2"]);
}

#[test]
fn the_gate_and_the_export_count_what_ran_outside_a_test_as_the_summary_does() {
    // A script no runner attributes test by test: everything it runs is
    // background coverage.
    let script = Project::empty("aggregate-gate");
    script.write("package.json", r#"{ "name": "agg", "type": "module" }"#);
    script.write(
        "src/math.js",
        "export function double(x) {\n  return x * 2;\n}\n",
    );
    script.write(
        "run.mjs",
        "import { double } from \"./src/math.js\";\nif (double(2) !== 4) process.exit(1);\n",
    );
    script.git(&["init", "-q"]);
    script.supercov(&["--", "node", "run.mjs"]).succeeds();
    let summary = script.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("Lines      100.00% (2/2)"), "{summary}");
    let gate = script
        .supercov(&["runs", "latest", "check", "--min-lines", "100"])
        .succeeds();
    assert!(gate.contains("lines        100.00%  2/2"), "{gate}");
    script
        .supercov(&["runs", "report", "--format", "lcov", "--output", "out.lcov"])
        .succeeds();
    let lcov = script.read("out.lcov");
    contains_all(&lcov, &["LF:2", "LH:2"]);

    // A module's top level runs as it is imported, before any test starts.
    let tests = Project::empty("import-gate");
    tests.write("package.json", r#"{ "name": "imp", "type": "module" }"#);
    tests.write(
        "src/pages.js",
        "const prefix = \"/pages/\";\n\nexport function page(name) {\n  return prefix + name;\n}\n",
    );
    tests.write(
        "test/pages.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { page } from \"../src/pages.js\";\n\ntest(\"names a page\", () => assert.equal(page(\"a\"), \"/pages/a\"));\n",
    );
    tests.git(&["init", "-q"]);
    tests.supercov(&["--", "node", "--test"]).succeeds();
    let summary = tests.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("Lines      100.00% (3/3)"), "{summary}");
    let gate = tests
        .supercov(&["runs", "latest", "check", "--min-lines", "100"])
        .succeeds();
    assert!(gate.contains("lines        100.00%  3/3"), "{gate}");
}

#[test]
fn the_file_view_counts_and_labels_lines_as_the_summary_and_line_view_do() {
    let project = Project::empty("line-states");
    project.write("package.json", r#"{ "name": "states" }"#);
    project.write(
        "src/states.js",
        "const scope = { value: 7 };\nfunction sloppy() {\n  with (scope) {\n    return value;\n  }\n}\nfunction both(a, b) {\n  if (a && b) {\n    return 1;\n  }\n  return 0;\n}\nfunction never() {\n  return 2;\n}\nfunction half(flag) {\n  return flag ? 1 : 0;\n}\nmodule.exports = { sloppy, both, never, half };\n",
    );
    project.write(
        "test/states.test.js",
        "const test = require(\"node:test\");\nconst assert = require(\"node:assert/strict\");\nconst { sloppy, both, half } = require(\"../src/states.js\");\ntest(\"sloppy reads the scope\", () => assert.equal(sloppy(), 7));\ntest(\"both, true and true\", () => assert.equal(both(true, true), 1));\ntest(\"both, false\", () => assert.equal(both(false, true), 0));\ntest(\"half, true\", () => assert.equal(half(true), 1));\n",
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", "node", "--test"]).succeeds();
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("Branches   75.00% (3/4)"), "{summary}");

    // `half` only ever returned 1: the decision's false outcome is the one
    // branch the summary counts as missing, and the file view says so too.
    let file = project
        .supercov(&["runs", "latest", "file", "src/states.js"])
        .succeeds();
    contains_all(
        &file,
        &[
            "Branch outcomes not taken       1",
            "   17  PARTIAL       flag",
            "Unobserved: decision never false",
        ],
    );
    let counts = project
        .supercov(&["runs", "latest", "file", "src/states.js", "--json"])
        .succeeds();
    let counts: serde_json::Value = serde_json::from_str(&counts).unwrap();
    assert_eq!(counts["data"]["counts"]["missingBranches"], 1, "{counts}");
    project
        .supercov(&["runs", "report", "--format", "lcov", "--output", "out.lcov"])
        .succeeds();
    let lcov = project.read("out.lcov");
    contains_all(&lcov, &["BRF:4", "BRH:3", "BRDA:17,1,0,1", "BRDA:17,1,1,-"]);
    assert_eq!(
        lcov.lines()
            .filter(|line| line.starts_with("BRDA:"))
            .count(),
        4,
        "{lcov}"
    );

    // The `with` line ran; only its body is outside measurement. Both views
    // call it partial, and the body, which nothing recorded, not measured.
    assert!(
        file.contains("    3  PARTIAL       with (scope) {"),
        "{file}"
    );
    assert!(!file.contains("NOT MEASURED"), "{file}");
    let with_line = project
        .supercov(&["runs", "latest", "line", "src/states.js:3"])
        .succeeds();
    assert!(with_line.contains("Status\n  PARTIAL"), "{with_line}");
    let body = project
        .supercov(&["runs", "latest", "line", "src/states.js:4"])
        .succeeds();
    assert!(body.contains("Status\n  NOT MEASURED"), "{body}");
}

#[test]
fn a_line_query_answers_every_line_it_can_and_warns_only_about_files_that_changed() {
    let project = Project::cart("line-answers");
    project.write(
        "src/tax.js",
        "export function rate(country) {\n  return country === \"LT\" ? 0.21 : 0;\n}\n",
    );
    project.write(
        "test/tax.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { rate } from \"../src/tax.js\";\n\ntest(\"rate in LT\", () => {\n  assert.equal(rate(\"LT\"), 0.21);\n});\n",
    );
    project.measure(&[]);

    // One mistyped path among several: the others are still answered, the
    // mistake is named beside them, and the call succeeds.
    let mixed = project.supercov(&[
        "runs",
        "latest",
        "line",
        "src/cart.js:14",
        "src/crat.js:3",
        "src/tax.js:2",
    ]);
    let answered = mixed.succeeds();
    contains_all(
        &answered,
        &[
            "return express ? 15 : 5;",
            "return country === \"LT\" ? 0.21 : 0;",
            "Not answered\n  src/crat.js:3: ",
        ],
    );
    let json = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:14",
            "src/crat.js:3",
            "--json",
        ])
        .json();
    assert_eq!(json["ok"], true, "{json}");
    assert_eq!(
        json["data"]["failed"][0]["location"], "src/crat.js:3",
        "{json}"
    );
    assert_eq!(
        json["data"]["failed"].as_array().unwrap().len(),
        1,
        "{json}"
    );
    // Every path wrong is still a mistake.
    let wrong = project
        .supercov(&["runs", "latest", "line", "src/crat.js:3"])
        .exits(2);
    assert!(wrong.contains("src/crat.js"), "{wrong}");

    // A blank line is no code at all, and the answer says why.
    let blank = project
        .supercov(&["runs", "latest", "line", "src/cart.js:11"])
        .succeeds();
    contains_all(
        &blank,
        &[
            "Status\n  NOT MEASURED",
            "Nothing on this line is code the run measures",
        ],
    );

    // Only a test changed: the source lines asked about are as the run saw
    // them, so nothing is stale about the answer.
    project.edit("test/tax.test.js", "rate in LT", "rate in Lithuania");
    let after_test_edit = project.supercov(&["runs", "latest", "line", "src/tax.js:2"]);
    after_test_edit.succeeds();
    assert!(
        !after_test_edit.stderr().contains("stale run"),
        "{}",
        after_test_edit.stderr()
    );

    // cart.js changed: a question about tax.js is still answered from the
    // run with no warning, one about cart.js names the file that changed.
    project.edit("src/cart.js", "return 0;", "return 0; // free");
    let unchanged = project.supercov(&["runs", "latest", "line", "src/tax.js:2"]);
    let unchanged_output = unchanged.succeeds();
    assert!(
        !unchanged.stderr().contains("stale run"),
        "{}",
        unchanged.stderr()
    );
    assert!(
        !unchanged_output.contains("working tree"),
        "{unchanged_output}"
    );
    let changed = project.supercov(&["runs", "latest", "line", "src/cart.js:16"]);
    changed.succeeds();
    assert!(
        changed.stderr().contains("stale run") && changed.stderr().contains("src/cart.js"),
        "{}",
        changed.stderr()
    );
}

#[test]
fn a_patch_on_a_failing_run_shows_its_coverage_and_still_cannot_pass() {
    let project = Project::cart("patch-failing");
    project.edit(
        "src/cart.js",
        "  return items.map((item) => item.name).join(\", \");",
        "  const names = items.map((item) => item.name);\n  return names.join(\", \");",
    );
    project.edit(
        "test/cart.test.js",
        "shipping(60, false), 0",
        "shipping(60, false), 1",
    );
    project.supercov(&["--", "node", "--test"]).exits(1);
    let human = project
        .supercov(&["runs", "patch", "--base", "HEAD"])
        .exits(2);
    contains_all(
        &human,
        &[
            "this run cannot pass a patch gate",
            "0 of 2 changed executable line(s) covered",
            "src/cart.js",
        ],
    );
    let ran = project.supercov(&["runs", "patch", "--base", "HEAD", "--json"]);
    ran.exits(2);
    let json = ran.json();
    assert_eq!(json["data"]["result"], "error", "{json}");
    assert_eq!(json["data"]["patch"]["eligible"], 2, "{json}");
    assert_eq!(json["data"]["patch"]["covered"], 0, "{json}");
    assert!(
        json["data"]["reasons"]
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty()),
        "{json}"
    );
}

#[test]
fn a_page_too_large_for_one_reply_says_where_the_rest_starts() {
    let project = Project::empty("page-cut");
    project.write(
        "package.json",
        r#"{ "name": "wide", "private": true, "type": "module" }"#,
    );
    let mut source = String::new();
    for index in 0..400 {
        source.push_str(&format!(
            "export function step{index}(flag) {{\n  return flag ? \"a long enough value for line {index} to take some room in a reply\" : {index};\n}}\n"
        ));
    }
    project.write("src/wide.js", &source);
    project.write(
        "test/wide.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { step0 } from \"../src/wide.js\";\n\ntest(\"step0\", () => {\n  assert.equal(step0(false), 0);\n});\n",
    );
    project.git(&["init", "-q"]);
    project.measure(&[]);
    let page = project.supercov(&[
        "runs",
        "latest",
        "file",
        "src/wide.js",
        "--limit",
        "1000",
        "--json",
    ]);
    let json = page.json();
    let cut = &json["data"]["pageCut"];
    assert_eq!(cut["asked"], 1000, "{json}");
    let returned = cut["returned"].as_u64().unwrap();
    assert!(returned > 0 && returned < 1000, "{json}");
    assert_eq!(cut["nextOffset"], returned, "{json}");
    assert!(
        page.stderr().contains(&format!(
            "this page holds {returned} of the 1000 entries asked for"
        )),
        "{}",
        page.stderr()
    );
    // A page that fits is not marked.
    let small = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/wide.js",
            "--limit",
            "5",
            "--json",
        ])
        .json();
    assert!(small["data"]["pageCut"].is_null(), "{small}");
}

/// A page cut to fit carries a note of the cut, and the note is part of
/// the reply: fitted without it, a page that had just fit went over by the
/// note and was refused. These lines, sixty long ones then short ones, give
/// a page that fits within the note's size of the limit.
#[test]
fn a_cut_page_fits_with_its_note_of_the_cut() {
    let project = Project::empty("page-cut-note");
    project.write(
        "package.json",
        r#"{ "name": "wide", "private": true, "type": "module" }"#,
    );
    let mut source = String::new();
    for index in 0..400 {
        let filler = "y".repeat(if index < 60 { 177 } else { 0 });
        source.push_str(&format!(
            "export function step{index}(flag) {{\n  return flag === \"v{index}{filler}\" ? 1 : {index};\n}}\n"
        ));
    }
    project.write("src/wide.js", &source);
    project.write(
        "test/wide.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { step0 } from \"../src/wide.js\";\ntest(\"step0\", () => { assert.equal(step0(false), 0); });\n",
    );
    project.git(&["init", "-q"]);
    project.measure(&[]);
    let page = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/wide.js",
            "--metric",
            "branches",
            "--limit",
            "1000",
            "--json",
        ])
        .json();
    assert_eq!(page["ok"], true, "{}", page["error"]);
    assert_eq!(page["data"]["pageCut"]["asked"], 1000, "{page}");
}

#[test]
fn todo_tests_are_counted_as_node_test_counts_them() {
    // supergateway's suite: node said 1,076 tests, 1,057 passed, 6 todo;
    // Supercov said 1,071 total and 1,058 passed. A body-less todo was never
    // recorded, a todo whose body failed was a failure, and tests in a
    // describe.todo were passes.
    let project = Project::cart("todo");
    project.write(
        "test/todo.test.js",
        r#"import test, { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { total } from '../src/cart.js';
test.todo('todo without a body');
test.todo('todo with a passing body', () => assert.equal(total([]), 0));
test.todo('todo with a failing body', () => assert.equal(1, 2));
test('option todo, passing', { todo: true }, () => assert.equal(1, 1));
test('option todo with a reason, failing', { todo: 'later' }, () => assert.equal(1, 2));
test('context todo', (t) => { t.todo('not yet'); assert.equal(1, 1); });
test.skip('skipped', () => assert.equal(1, 2));
describe('suite', () => { it.todo('it todo'); });
describe.todo('todo suite', () => { it('inside a todo suite', () => assert.equal(1, 1)); });
"#,
    );
    project.commit("todo");
    let native = std::process::Command::new("node")
        .args(["--test", "--test-reporter", "spec"])
        .current_dir(&project.root)
        .output()
        .unwrap();
    let native = String::from_utf8_lossy(&native.stdout).into_owned();
    let count = |label: &str| -> String {
        native
            .lines()
            .find_map(|line| line.trim().strip_prefix(&format!("\u{2139} {label} ")))
            .unwrap_or_else(|| panic!("no {label} in\n{native}"))
            .trim()
            .to_owned()
    };
    project.measure(&[]);
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    for (label, ours) in [
        ("tests", "Total"),
        ("pass", "Passed"),
        ("skipped", "Skipped"),
        ("todo", "Todo"),
    ] {
        let expected = count(label);
        assert!(
            summary.contains(&format!("  {ours:<11} {expected}\n")),
            "{ours} should be node's {label} {expected}:\n{summary}"
        );
    }
    assert!(!summary.contains("  Failed"), "{summary}");
}

#[test]
fn printed_commands_name_the_way_supercov_was_started() {
    let project = Project::cart("launchers");
    let run = project.measure(&[]);
    let root = project.root.to_string_lossy().into_owned();
    // The PyPI, Ruby and Rust packages run the binary itself; npm sets the
    // package root; the Go launcher says so.
    for (env, command) in [
        (vec![], "supercov"),
        (
            vec![("SUPERCOV_PACKAGE_ROOT", root.as_str())],
            "npx supercov",
        ),
        (
            vec![("SUPERCOV_LAUNCHER", "go")],
            "go run github.com/supercorp-ai/supercov/cmd/supercov@latest",
        ),
    ] {
        let summary = project.supercov_with(&["runs", "latest"], &env).succeeds();
        assert!(
            summary.contains(&format!(
                "not assessed — {command} runs {run} assertions assess"
            )),
            "{env:?}: {summary}"
        );
        assert!(
            summary.contains(&format!("  {command} runs '{run}' files\n")),
            "{env:?}: {summary}"
        );
        if command == "supercov" {
            assert!(!summary.contains("npx supercov"), "{summary}");
        }
    }
}

#[test]
fn every_listing_pages_and_names_its_next_page() {
    let project = Project::cart("pages");
    project.write(
        "src/tax.js",
        "export function tax(sum, rate) {\n  return rate > 0 ? sum * rate : 0;\n}\n",
    );
    project.write(
        "test/tax.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { tax } from \"../src/tax.js\";\n\ntest(\"taxes a sum\", () => assert.equal(tax(10, 0.5), 5));\n",
    );
    project.commit("tax");
    let first = project.measure(&["--test-name-pattern", "total"]);
    let second = project.measure(&[]);
    let run = format!("'{second}'");
    for (args, next) in [
        (
            vec!["runs", "--limit", "1"],
            "supercov runs --offset 1 --limit 1".to_owned(),
        ),
        (
            vec!["runs", "latest", "files", "--limit", "1"],
            format!("runs {run} files --offset 1 --limit 1"),
        ),
        (
            vec!["runs", "latest", "gaps", "--limit", "1"],
            format!("runs {run} gaps --offset 1 --limit 1"),
        ),
        // A filtered listing's next page keeps the filter.
        (
            vec![
                "runs", "latest", "gaps", "--filter", "passed", "--limit", "1",
            ],
            format!("runs {run} gaps --filter passed --offset 1 --limit 1"),
        ),
        (
            vec![
                "runs", "latest", "files", "--filter", "passed", "--limit", "1",
            ],
            format!("runs {run} files --filter passed --offset 1 --limit 1"),
        ),
        (
            vec!["runs", "latest", "file", "src/cart.js", "--limit", "1"],
            format!("runs {run} file 'src/cart.js' --offset 1 --limit 1"),
        ),
        (
            vec!["runs", "latest", "line", "src/cart.js:13", "--limit", "1"],
            format!("runs {run} line 'src/cart.js:13' --offset 1 --limit 1"),
        ),
        (
            vec![
                "runs",
                "latest",
                "decision",
                "src/cart.js:13",
                "--limit",
                "1",
            ],
            format!("runs {run} decision 'src/cart.js:13' --offset 1 --limit 1"),
        ),
        (
            vec!["runs", "latest", "scope", "--files", "--limit", "1"],
            format!("runs {run} scope --files --offset 1 --limit 1"),
        ),
        (
            vec!["runs", "latest", "source", "src/cart.js", "--limit", "2"],
            format!("runs {run} source 'src/cart.js' --offset 2 --limit 2"),
        ),
        (
            vec![
                "runs",
                "latest",
                "test",
                "total adds prices",
                "--limit",
                "1",
            ],
            format!("runs {run} test 'total adds prices' --offset 1 --limit 1"),
        ),
        (
            vec!["diff", &first, &second, "--limit", "1"],
            format!("diff '{first}' {run} --offset 1 --limit 1"),
        ),
    ] {
        let listed = project.supercov(&args).succeeds();
        assert!(listed.contains(&next), "{args:?}: no {next:?} in\n{listed}");
        // The command it names reads the next page.
        let mut words = vec![String::new()];
        let mut quoted = false;
        for character in next.trim_start_matches("supercov ").chars() {
            match character {
                '\'' => quoted = !quoted,
                ' ' if !quoted => words.push(String::new()),
                _ => words.last_mut().unwrap().push(character),
            }
        }
        let words = words.iter().map(String::as_str).collect::<Vec<_>>();
        let following = project.supercov(&words).succeeds();
        assert_ne!(following, listed, "{words:?} repeated the first page");
    }

    // A run whose suite failed says so where runs are listed.
    project.write(
        "test/broken.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\n\ntest(\"breaks\", () => assert.equal(1, 2));\n",
    );
    project.supercov(&["--", "node", "--test"]).exits(1);
    let listed = project.supercov(&["runs", "--limit", "1"]).succeeds();
    assert!(listed.contains("FAILED (exit 1)"), "{listed}");
}

#[test]
fn the_html_report_says_what_it_could_not_embed() {
    let project = Project::cart("html-edges");
    let mut big = String::from("export function big(x) {\n  return x + 1;\n}\n");
    big.push_str(&format!("// {}\n", "x".repeat(1024 * 1024)));
    project.write("src/big.js", &big);
    project.write(
        "test/big.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { big } from \"../src/big.js\";\n\ntest(\"big adds one\", () => assert.equal(big(1), 2));\n",
    );
    project.commit("big");
    project.measure(&[]);
    project
        .supercov(&["report", "--no-open", "--output", "fresh.html"])
        .succeeds();
    let fresh = report_data(&project.read("fresh.html"));
    let run = &fresh["runs"][0];
    assert_eq!(
        run["omittedSources"],
        serde_json::json!(["src/big.js"]),
        "{run}"
    );
    assert_eq!(run["stale"], false);
    assert!(
        run["sources"]["src/cart.js"].is_object(),
        "{}",
        run["sources"]
    );

    // Source that changed since the run is not shown beside its lines.
    project.edit("src/cart.js", "return 0;", "return 1;");
    project
        .supercov(&["report", "--no-open", "--output", "stale.html"])
        .succeeds();
    let stale = report_data(&project.read("stale.html"));
    let run = &stale["runs"][0];
    assert_eq!(run["stale"], true);
    assert_eq!(
        run["staleReasons"],
        serde_json::json!(["instrumented source changed"])
    );
    assert_eq!(run["sources"], serde_json::json!({}), "{run}");

    // A directory gets the report under its default name.
    std::fs::create_dir(project.root.join("reports")).unwrap();
    project
        .supercov(&["report", "--no-open", "--output", "reports"])
        .succeeds();
    report_data(&project.read("reports/supercov-report.html"));

    let empty = Project::empty("html-nothing");
    empty.git(&["init", "-q"]);
    let nothing = empty.supercov(&["report", "--no-open"]).exits(2);
    assert!(
        nothing.contains("no local coverage runs and no saved assessments to report"),
        "{nothing}"
    );
}

/// The data a report page renders from: gzip-compressed JSON, base64-encoded
/// into its `report-data` script.
fn report_data(html: &str) -> serde_json::Value {
    use base64::Engine;
    use std::io::Read;
    let start = html
        .find(r#"<script id="report-data" type="application/octet-stream">"#)
        .expect("report data")
        + r#"<script id="report-data" type="application/octet-stream">"#.len();
    let end = start + html[start..].find("</script>").expect("report data end");
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(&html[start..end])
        .unwrap();
    let mut json = String::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .read_to_string(&mut json)
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn a_lock_an_older_release_holds_is_left_alone_and_one_left_behind_is_taken_over() {
    let project = Project::cart("locks");
    let lock = project.root.join(".supercov/locks/active.json");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let owner = |pid: u32| {
        format!(r#"{{"runId":"run_other","pid":{pid},"startedAt":"2026-10-02T00:00:00Z"}}"#)
    };
    // A run of a release that locked nothing wrote this file and is alive
    // while its process is; this test's process stands in for it. The run
    // started beside it takes a copy of its own.
    std::fs::write(&lock, owner(std::process::id())).unwrap();
    let beside = project.supercov(&["--", "node", "--test"]);
    assert_eq!(beside.code(), 0, "{}", beside.stderr());
    assert!(
        beside.stderr().contains("workspace-2"),
        "{}",
        beside.stderr()
    );
    // One whose owner has gone, or that was never finished, holds nothing.
    for left in [owner(u32::MAX - 1), "{".to_owned()] {
        std::fs::write(&lock, left).unwrap();
        let alone = project.supercov(&["--", "node", "--test"]);
        assert_eq!(alone.code(), 0, "{}", alone.stderr());
        assert!(
            !alone.stderr().contains("workspace-2"),
            "{}",
            alone.stderr()
        );
    }

    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);

    // The trash sweeper keeps the same rules for its own lock. The runs above
    // each started one in the background; it is let finish first.
    let trash = project.root.join(".supercov/.trash");
    let started = std::time::Instant::now();
    while trash.join(".deleter.lock").exists() && started.elapsed().as_secs() < 30 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let root = project.root.to_str().unwrap();
    let sweep = |deleter: &str, modified: Option<std::time::SystemTime>| {
        std::fs::create_dir_all(trash.join("left")).unwrap();
        let path = trash.join(".deleter.lock");
        std::fs::write(&path, deleter).unwrap();
        if let Some(modified) = modified {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        project.supercov(&["__sweep-trash", root]).succeeds();
        let swept = !trash.join("left").exists();
        let _ = std::fs::remove_file(&path);
        swept
    };
    assert!(
        !sweep(&std::process::id().to_string(), None),
        "a live sweeper is left to it"
    );
    assert!(!sweep("", None), "a sweeper starting up is left to it");
    assert!(
        sweep(&(u32::MAX - 1).to_string(), None),
        "a dead sweeper's lock is taken over"
    );
    assert!(sweep("", Some(old)), "an old unowned lock is taken over");
}

#[test]
fn a_selector_that_matches_nothing_or_several_says_so() {
    let project = Project::cart("selectors");
    project.write(
        "src/pick.js",
        "export const pick = (a, b) => (a ? 1 : 2) + (b ? 3 : 4);\n",
    );
    project.write(
        "test/pick.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { pick } from \"../src/pick.js\";\n\ntest(\"picks\", () => assert.equal(pick(true, false), 5));\n",
    );
    project.commit("pick");
    project.measure(&[]);
    for (args, message) in [
        (
            vec!["runs", "latest", "gaps", "--kind", "e2e"],
            "No tests match kind=e2e",
        ),
        (
            vec!["runs", "latest", "gaps", "--runner", "jest"],
            "No tests match runner=jest",
        ),
        (
            vec!["runs", "latest", "file", "src/cart.js", "--kind", "e2e"],
            "No tests match kind=e2e",
        ),
        (
            vec!["runs", "latest", "minimize", "--target", "150"],
            "--target must be between 0 and 100",
        ),
        (
            vec!["runs", "latest", "test", "nosuch"],
            "Test not found: nosuch",
        ),
        (
            vec!["runs", "latest", "decision", "src/cart.js:99"],
            "Decision not found: src/cart.js:99",
        ),
        (
            vec!["runs", "latest", "test", "total", "--limit", "-1"],
            "--limit must be a number of entries, or 0 for all of them",
        ),
    ] {
        let refused = project.supercov(&args).exits(2);
        assert!(refused.contains(message), "{args:?}: {refused}");
    }
    // A selector two tests or two decisions answer to lists them to choose from.
    let tests = project
        .supercov(&["runs", "latest", "test", "total"])
        .succeeds();
    contains_all(
        &tests,
        &[
            "total adds prices [",
            "total applies a coupon [",
            "showing 1-2 of 2 matching tests",
        ],
    );
    let decisions = project
        .supercov(&["runs", "latest", "decision", "src/pick.js:1"])
        .succeeds();
    contains_all(
        &decisions,
        &[
            "src/pick.js:1:32  a",
            "src/pick.js:1:46  b",
            "showing 1-2 of 2 matching decisions",
        ],
    );
    // Minimizing over one runner's tests.
    let minimized = project
        .supercov(&[
            "runs",
            "latest",
            "minimize",
            "--runner",
            "node:test",
            "--target",
            "50",
            "--metric",
            "lines",
        ])
        .succeeds();
    assert!(
        minimized.contains("exact minimum 1/4 test(s) for 50% lines"),
        "{minimized}"
    );
}

#[test]
fn phase_timing_says_where_setup_went_when_asked() {
    let project = Project::cart("phase-timing");
    let timed = project
        .supercov_with(&["--", "node", "--test"], &[("SUPERCOV_PHASE_TIMING", "1")])
        .exits(0);
    contains_all(
        &timed,
        &[
            "[supercov] setup detail runtime=",
            "| files=",
            "[supercov] timings initialization=",
        ],
    );
    let quiet = project.supercov(&["--", "node", "--test"]).exits(0);
    assert!(!quiet.contains("setup detail"), "{quiet}");
}

#[test]
fn a_pnpm_workspace_finds_the_source_of_each_package_it_lists() {
    let project = Project::empty("pnpm-workspace");
    project.write(
        "package.json",
        r#"{ "name": "mono", "private": true, "type": "module" }"#,
    );
    project.write(
        "pnpm-workspace.yaml",
        "packages:\n  - \"packages/*\"\n  - 'tools/cli'\n  - \"!packages/ignored\"\n  - \"apps/**\"\n# the rest is not packages\ncatalog:\n  left-pad: 1.0.0\n",
    );
    project.write(
        "packages/a/package.json",
        r#"{ "name": "a", "type": "module", "exports": { ".": ["./entry/index.js"] } }"#,
    );
    project.write(
        "packages/a/entry/index.js",
        "export const a = (x) => x + 1;\n",
    );
    project.write(
        "packages/a/entry/types.d.ts",
        "export declare const a: (x: number) => number;\n",
    );
    project.write(
        "tools/cli/package.json",
        r#"{ "name": "cli", "type": "module" }"#,
    );
    project.write("tools/cli/main.js", "export const run = () => \"ran\";\n");
    project.write(
        "apps/web/package.json",
        r#"{ "name": "web", "type": "module" }"#,
    );
    project.write("apps/web/page.js", "export const page = () => 1;\n");
    project.write(
        "test/all.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { a } from \"../packages/a/entry/index.js\";\nimport { run } from \"../tools/cli/main.js\";\n\ntest(\"both packages\", () => {\n  assert.equal(a(1), 2);\n  assert.equal(run(), \"ran\");\n});\n",
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", "node", "--test"]).succeeds();
    let scope = project
        .supercov(&["runs", "latest", "scope", "--files", "--limit", "50"])
        .succeeds();
    contains_all(
        &scope,
        &[
            // An array of export targets names the entry file; a package
            // with no conventional source directory is its own root.
            "INCLUDED  packages/a/entry/index.js  discovered package source root  [package packages/a]",
            "INCLUDED  tools/cli/main.js  discovered package source root  [package tools/cli]",
            "INCLUDED  apps/web/page.js  discovered package source root  [package apps/web]",
            "EXCLUDED  packages/a/entry/types.d.ts  TypeScript declaration",
        ],
    );
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("Lines      66.67% (2/3)"), "{summary}");
}

#[test]
fn absolute_source_roots_name_the_same_rust_code_in_the_workspace() {
    let project = Project::empty("rust-roots");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"roots\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub mod extra;\n\npub fn double(x: u32) -> u32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    );
    project.write(
        "src/extra.rs",
        "pub fn half(x: u32) -> u32 {\n    x / 2\n}\n",
    );
    project.git(&["init", "-q"]);
    let measured = |roots: &str| {
        project
            .supercov_with(
                &["--", "cargo", "test"],
                &[("SUPERCOV_SOURCE_ROOTS", roots)],
            )
            .succeeds();
        project.supercov(&["runs", "latest"]).succeeds()
    };
    // The run measures a copy; a root written as the project's absolute path
    // names the same file there.
    let file = project.root.join("src/lib.rs");
    let one = measured(file.to_str().unwrap());
    assert!(one.contains("Lines      100.00% (2/2)"), "{one}");
    let whole = measured(project.root.to_str().unwrap());
    assert!(whole.contains("Lines      50.00% (2/4)"), "{whole}");
}

#[test]
fn cargo_configuration_and_target_flags_reach_the_measured_build() {
    let project = Project::empty("cargo-config");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"config\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn double(x: u32) -> u32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    );
    project.git(&["init", "-q"]);
    let rustc = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .unwrap();
    let host = String::from_utf8(rustc.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .unwrap();
    let target_eq = format!("--target={host}");
    let mut measured = vec![
        vec!["--", "cargo", "test", "--config", "build.incremental=false"],
        vec!["--", "cargo", "test", "--config=build.incremental=false"],
        vec!["--", "cargo", "test", "--target", &host],
        vec!["--", "cargo", "test", &target_eq],
    ];
    // A toolchain selector goes through rustup, where there is one.
    let rustup = std::process::Command::new("rustup")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if rustup {
        measured.push(vec!["--", "cargo", "+stable", "test"]);
    }
    for args in measured {
        project.supercov(&args).succeeds();
        let summary = project.supercov(&["runs", "latest"]).succeeds();
        assert!(
            summary.contains("Lines      100.00% (2/2)"),
            "{args:?}: {summary}"
        );
    }
    for (args, message) in [
        (
            vec!["--", "cargo", "test", "--config"],
            "Cargo option --config has no value",
        ),
        (
            vec!["--", "cargo", "test", "--target"],
            "Cargo option --target has no value",
        ),
        // What Cargo refuses itself, it says.
        (
            vec!["--", "cargo", "test", "--config="],
            "was not a TOML dotted key expression",
        ),
        (vec!["--", "cargo", "test", "--target="], "target was empty"),
    ] {
        // Cargo quotes a refused argument in color when CI asks for it.
        let refused = project.supercov_with(&args, &[("CARGO_TERM_COLOR", "never")]);
        assert_ne!(refused.code(), 0, "{args:?}");
        assert!(
            refused.stderr().contains(message),
            "{args:?}: {}",
            refused.stderr()
        );
    }
}

#[cfg(unix)]
#[test]
fn a_workspace_directory_the_project_does_not_own_is_never_written_through() {
    let project = Project::cart("planted-workspace");
    let elsewhere = Project::empty("planted-target");
    std::fs::create_dir_all(project.root.join(".supercov")).unwrap();
    // A link where Supercov keeps its isolated copy, pointing out of the
    // project, as a hostile checkout or a confused script could leave it.
    std::os::unix::fs::symlink(&elsewhere.root, project.root.join(".supercov/workspaces")).unwrap();
    let measured = project.supercov(&["--", "node", "--test"]).exits(0);
    assert!(measured.contains(".supercov/workspaces-"), "{measured}");
    assert!(
        std::fs::read_dir(&elsewhere.root).unwrap().next().is_none(),
        "nothing was written through the link"
    );
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("Lines      76.92% (10/13)"), "{summary}");
}

#[test]
fn cargo_configuration_planted_in_the_rust_workspace_is_refused() {
    let project = Project::empty("planted-cargo");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"planted\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn double(x: u32) -> u32 {\n    x * 2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    );
    project.git(&["init", "-q"]);
    let first = project.supercov(&["--", "cargo", "test"]).exits(0);
    let workspace = first
        .lines()
        .find_map(|line| {
            line.strip_prefix("[supercov] detected Rust; instrumenting isolated Cargo workspace ")
        })
        .expect("the run names its workspace");
    let container = std::path::Path::new(workspace)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    // A Cargo configuration there would change how the copy builds.
    std::fs::create_dir(container.join(".cargo")).unwrap();
    let refused = project.supercov(&["--", "cargo", "test"]);
    assert_ne!(refused.code(), 0);
    assert!(
        refused
            .stderr()
            .contains("would change how Supercov builds the project but not how `cargo test` does"),
        "{}",
        refused.stderr()
    );
    std::fs::remove_dir(container.join(".cargo")).unwrap();
    project.supercov(&["--", "cargo", "test"]).succeeds();
}

#[test]
fn affected_test_names_select_exactly_those_tests_in_their_runner() {
    let project = Project::empty("runner-names");
    project.write("package.json", r#"{ "name": "names", "type": "module" }"#);
    project.write(
        "src/fee.js",
        "export function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\n\nexport function tax(sum) {\n  return sum * 2;\n}\n",
    );
    // One name in two describe blocks; only one of them runs fee.
    project.write(
        "test/fee.test.js",
        "import { describe, test } from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { fee, tax } from \"../src/fee.js\";\n\ndescribe(\"small\", () => {\n  test(\"charges\", () => assert.equal(fee(10), 5));\n});\ndescribe(\"large\", () => {\n  test(\"charges\", () => assert.equal(tax(10), 20));\n});\n",
    );
    project.git(&["init", "-q"]);
    project.commit("names");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let tests = project
        .supercov(&["runs", "latest", "test", "charges"])
        .succeeds();
    contains_all(&tests, &["small > charges [", "large > charges ["]);
    project.edit("src/fee.js", "? 0 : 5", "? 0 : 6");
    let names = project
        .supercov(&["runs", "latest", "tests", "affected", "--names"])
        .succeeds();
    assert_eq!(names.trim(), "small charges");
    // node:test matches a test by its suites and its name joined by spaces.
    let selected = std::process::Command::new("node")
        // Node 22 reports in TAP when its output is not a terminal.
        .args([
            "--test",
            "--test-reporter",
            "spec",
            "--test-name-pattern",
            names.trim(),
        ])
        .current_dir(&project.root)
        .output()
        .unwrap();
    let output = String::from_utf8_lossy(&selected.stdout);
    assert!(output.contains("ℹ tests 1\n"), "{output}");
}

/// Sandbox SDKs pass a command's environment in different places: an `env`
/// option, an `envs` option (E2B), or a plain map after the command
/// (Daytona) or a builder method (Testcontainers). Each refuses unless
/// Supercov already moved the project root to the guest's path.
const SANDBOX_SHAPES: &str = r#"import { spawnSync } from "node:child_process";
import { mkdirSync, symlinkSync } from "node:fs";
import { dirname } from "node:path";

function go(target, argv, environment) {
  if (environment?.SUPERCOV_PROJECT_ROOT !== target) return 97;
  return spawnSync(argv[0], argv.slice(1), { cwd: target, env: environment, stdio: "inherit" }).status;
}

export const sandbox = {
  boot(options) {
    const { source, target } = options.mounts[0];
    mkdirSync(dirname(target), { recursive: true });
    symlinkSync(source, target, "dir");
    return {
      exec: (argv, options) => go(target, argv, options.env),
      run: (argv, options) => go(target, argv, options.envs),
      execute: (argv, environment) => go(target, argv, environment),
    };
  },
};

// A builder, as Testcontainers is: mounts, environment and command set one
// call at a time, then started.
export class Container {
  withBindMounts(mounts) { this.mount = mounts[0]; return this; }
  withEnvironment(environment) { this.environment = environment; return this; }
  withCommand(argv) { this.argv = argv; return this; }
  start() {
    mkdirSync(dirname(this.mount.target), { recursive: true });
    symlinkSync(this.mount.source, this.mount.target, "dir");
    return go(this.mount.target, this.argv, this.environment);
  }
}
"#;

#[test]
fn each_way_a_sandbox_sdk_takes_an_environment_reaches_the_guest() {
    let project = Project::empty("sandbox-shapes");
    project.write("package.json", r#"{ "name": "shapes", "type": "module" }"#);
    project.write("src/cart.js", common::CART);
    project.write(
        "node_modules/sandbox-shapes/package.json",
        r#"{ "name": "sandbox-shapes", "type": "module", "exports": "./index.mjs" }"#,
    );
    project.write("node_modules/sandbox-shapes/index.mjs", SANDBOX_SHAPES);
    project.write(
        "guest/describe.mjs",
        "import { describe } from \"../src/cart.js\";\nif (describe([{ name: \"pen\" }]) !== \"pen\") process.exit(1);\n",
    );
    project.write(
        "test/shapes.test.js",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sandbox, Container } from "sandbox-shapes";

const machine = () =>
  sandbox.boot({ mounts: [{ source: process.cwd(), target: join(mkdtempSync(join(tmpdir(), "guest-")), "workspace") }] });
const argv = [process.execPath, "guest/describe.mjs"];
// Windows' own environment has names like this one in it.
const windowsLike = { ...process.env, "ProgramFiles(x86)": "C:\\Program Files (x86)" };

test("an env option", () => assert.equal(machine().exec(argv, { env: { ...process.env } }), 0));
test("an envs option", () => assert.equal(machine().run(argv, { envs: { ...process.env } }), 0));
test("an environment map", () => assert.equal(machine().execute(argv, windowsLike), 0));
test("a builder", () => {
  const target = join(mkdtempSync(join(tmpdir(), "guest-")), "workspace");
  const started = new Container()
    .withBindMounts([{ source: process.cwd(), target }])
    .withEnvironment(windowsLike)
    .withCommand(argv)
    .start();
  assert.equal(started, 0);
});
"#,
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", "node", "--test"]).succeeds();
    let summary = project.supercov(&["runs", "latest", "--json"]).json();
    assert_eq!(
        summary["data"]["transport"]["remoteLaunches"], 4,
        "{summary}"
    );
    // Every guest had the settings, so none set itself up.
    assert_eq!(
        summary["data"]["transport"]["guestProcesses"], 0,
        "{summary}"
    );
    let line = project
        .supercov(&["runs", "latest", "line", "src/cart.js:20"])
        .succeeds();
    contains_all(
        &line,
        &[
            "an env option",
            "an envs option",
            "an environment map",
            "a builder",
        ],
    );
}

#[cfg(unix)]
#[test]
fn a_guest_started_with_none_of_supercovs_settings_runs_and_is_counted() {
    let project = Project::empty("clean-guest");
    project.write("package.json", r#"{ "name": "clean", "type": "module" }"#);
    project.write("src/cart.js", common::CART);
    // A VM: the project mounted at `target`, a command run there with only
    // what the VM provides.
    project.write(
        "node_modules/fake-vm/package.json",
        r#"{ "name": "fake-vm", "type": "module", "exports": "./index.mjs" }"#,
    );
    project.write(
        "node_modules/fake-vm/index.mjs",
        r#"import { spawnSync } from "node:child_process";
import { mkdirSync, symlinkSync } from "node:fs";
import { dirname } from "node:path";
export function boot({ source, target }) {
  mkdirSync(dirname(target), { recursive: true });
  symlinkSync(source, target, "dir");
  return {
    run(argv) {
      const result = spawnSync("/usr/bin/env", ["-i", `PATH=${process.env.PATH}`, ...argv], { cwd: target, encoding: "utf8" });
      return { code: result.status, stderr: result.stderr };
    },
  };
}
"#,
    );
    project.write(
        "guest/describe.mjs",
        "import { describe } from \"../src/cart.js\";\nif (describe([{ name: \"pen\" }]) !== \"pen\") process.exit(1);\n",
    );
    project.write(
        "test/vm.test.js",
        r#"import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { boot } from "fake-vm";

test("the guest describes the cart", () => {
  const target = join(mkdtempSync(join(tmpdir(), "guest-")), "workspace");
  const result = boot({ source: process.cwd(), target }).run(["node", "guest/describe.mjs"]);
  assert.equal(result.code, 0, result.stderr);
});
"#,
    );
    project.git(&["init", "-q"]);
    // The instrumented source used to throw at its first probe in the guest.
    project.supercov(&["--", "node", "--test"]).succeeds();
    let line = project
        .supercov(&["runs", "latest", "line", "src/cart.js:20"])
        .succeeds();
    contains_all(&line, &["Status\n  COVERED", "Background / unattributed"]);
    // The run says so, and how many.
    let summary = project.supercov(&["runs", "latest", "--json"]).json();
    assert_eq!(
        summary["data"]["transport"]["guestProcesses"], 1,
        "{summary}"
    );
    let text = project.supercov(&["runs", "latest"]).succeeds();
    assert!(
        text.contains("  Guests           1 process(es) ran the workspace in a container or VM without Supercov's settings"),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn jest_tests_are_named_by_their_describe_blocks_and_selected_by_jest() {
    let project = Project::empty("jest-names");
    project.write(
        "package.json",
        r#"{ "name": "jest-names", "scripts": { "test": "jest" } }"#,
    );
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../node_modules");
    std::os::unix::fs::symlink(
        modules.canonicalize().unwrap(),
        project.root.join("node_modules"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nfunction tax(sum) {\n  return sum * 2;\n}\nmodule.exports = { fee, tax };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { fee, tax } = require(\"../src/fee\");\ndescribe(\"small\", () => {\n  test(\"charges\", () => expect(fee(10)).toBe(5));\n});\ndescribe(\"large\", () => {\n  describe(\"orders\", () => {\n    test(\"charges\", () => expect(tax(10)).toBe(20));\n  });\n});\n",
    );
    project.git(&["init", "-q"]);
    project.commit("jest");
    project.supercov(&["--", "npx", "jest"]).succeeds();
    let tests = project
        .supercov(&["runs", "latest", "test", "charges"])
        .succeeds();
    contains_all(&tests, &["small > charges [", "large > orders > charges ["]);
    project.edit("src/fee.js", "? 0 : 5", "? 0 : 6");
    let names = project
        .supercov(&["runs", "latest", "tests", "affected", "--names"])
        .succeeds();
    assert_eq!(names.trim(), "small charges");
    let selected = std::process::Command::new(project.root.join("node_modules/.bin/jest"))
        .args(["-t", names.trim()])
        .current_dir(&project.root)
        .output()
        .unwrap();
    let output = String::from_utf8_lossy(&selected.stderr);
    assert!(output.contains("1 failed, 1 skipped, 2 total"), "{output}");
}

#[cfg(unix)]
#[test]
fn mocha_tests_are_attributed_one_by_one_and_selected_by_grep() {
    let project = Project::empty("mocha-tests");
    project.write(
        "package.json",
        r#"{ "name": "mocha-tests", "type": "module", "scripts": { "test": "mocha" } }"#,
    );
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../node_modules");
    std::os::unix::fs::symlink(
        modules.canonicalize().unwrap(),
        project.root.join("node_modules"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\n");
    project.write(
        "src/cart.js",
        "export function total(items) {\n  return items.reduce((sum, item) => sum + item.price, 0);\n}\nexport function discount(sum, code) {\n  if (code === \"HALF\") {\n    return sum / 2;\n  }\n  return sum;\n}\nexport function prepare() {\n  return 1;\n}\nlet calls = 0;\nexport function flaky() {\n  calls += 1;\n  return calls > 1;\n}\n",
    );
    project.write(
        "test/cart.test.js",
        "import assert from \"node:assert/strict\";\nimport { total, discount, prepare, flaky } from \"../src/cart.js\";\ndescribe(\"cart\", () => {\n  before(() => prepare());\n  describe(\"total\", () => {\n    it(\"adds prices\", () => assert.equal(total([{ price: 2 }, { price: 3 }]), 5));\n  });\n  describe(\"discount\", () => {\n    it(\"halves with HALF\", () => assert.equal(discount(10, \"HALF\"), 5));\n    it.skip(\"keeps the sum otherwise\", () => assert.equal(discount(10, \"X\"), 10));\n  });\n  it(\"passes on its second try\", function () {\n    this.retries(1);\n    assert.ok(flaky());\n  });\n});\n",
    );
    project.git(&["init", "-q"]);
    project.commit("mocha");
    // Serially and with --parallel, where each worker runs its own files.
    for command in [
        &["--", "npx", "mocha"][..],
        &["--", "npx", "mocha", "--parallel"],
    ] {
        project.supercov(command).succeeds();
        let summary = project.supercov(&["runs", "latest"]).succeeds();
        contains_all(
            &summary,
            &[
                "Attribution      Exact for 4 test(s)",
                "Passed      2",
                "Flaky       1",
                "Skipped     1",
            ],
        );
    }
    // The before hook ran as setup, not inside the first test.
    let hook = project
        .supercov(&["runs", "latest", "line", "src/cart.js:11"])
        .succeeds();
    assert!(!hook.contains("adds prices ["), "{hook}");
    let line = project
        .supercov(&["runs", "latest", "line", "src/cart.js:6"])
        .succeeds();
    contains_all(
        &line,
        &[
            "cart > discount > halves with HALF [",
            "unit/mocha",
            "node:assert/strict.equal (passed)",
        ],
    );
    assert!(!line.contains("adds prices ["), "{line}");
    project.edit("src/cart.js", "return sum / 2;", "return sum * 0.5;");
    let names = project
        .supercov(&["runs", "latest", "tests", "affected", "--names"])
        .succeeds();
    assert!(
        names
            .lines()
            .any(|name| name == "cart discount halves with HALF"),
        "{names}"
    );
    let selected = std::process::Command::new(project.root.join("node_modules/.bin/mocha"))
        .args(["--grep", "cart discount halves with HALF"])
        .current_dir(&project.root)
        .output()
        .unwrap();
    let output = String::from_utf8_lossy(&selected.stdout);
    assert!(output.contains("1 passing"), "{output}");
    assert!(!output.contains("pending"), "{output}");
}

#[cfg(unix)]
#[test]
fn a_one_line_helper_that_returns_a_check_is_still_awaited() {
    let project = Project::empty("returned-check");
    project.write(
        "package.json",
        r#"{ "name": "returned-check", "private": true, "type": "module" }"#,
    );
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../node_modules");
    std::os::unix::fs::symlink(
        modules.canonicalize().unwrap(),
        project.root.join("node_modules"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\n");
    project.write("src/one.ts", "export const one = () => 1;\n");
    project.write(
        "playwright.config.ts",
        "export default { testDir: \"tests\", reporter: \"line\" };\n",
    );
    // `settles` returns the check; `await settles(...)` waits for it only if
    // the helper still does once its `expect` is measured.
    project.write(
        "tests/helper.spec.ts",
        "import { expect, test } from \"@playwright/test\";\nimport { one } from \"../src/one\";\nlet done = false;\nconst slow = () => new Promise<number>((resolve) => setTimeout(() => { done = true; resolve(one()); }, 300));\nconst settles = (value: Promise<number>) => expect(value).resolves.toBe(1);\ntest(\"waits for its check\", async () => {\n  await settles(slow());\n  expect(done).toBe(true);\n});\n",
    );
    project.git(&["init", "-q"]);
    project.commit("returned check");
    let run = project
        .supercov(&["--", "npx", "playwright", "test"])
        .succeeds();
    assert!(run.contains("1 passed"), "{run}");
}

#[cfg(unix)]
#[test]
fn ava_tests_running_at_once_are_attributed_each_to_its_own() {
    let project = Project::empty("ava-tests");
    project.write(
        "package.json",
        r#"{ "name": "ava-tests", "type": "module", "scripts": { "test": "ava" } }"#,
    );
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../node_modules");
    std::os::unix::fs::symlink(
        modules.canonicalize().unwrap(),
        project.root.join("node_modules"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\n");
    project.write(
        "src/cart.js",
        "export function discount(sum, code) {\n  if (code === \"HALF\") {\n    return sum / 2;\n  }\n  return sum;\n}\n",
    );
    // `halves` is still waiting when `keeps` runs: AVA runs a file's tests
    // at once, so only the carrier, not the clock, can tell them apart.
    project.write(
        "test/cart.test.js",
        "import test from \"ava\";\nimport { discount } from \"../src/cart.js\";\ntest(\"halves\", async (t) => {\n  await new Promise((resolve) => setTimeout(resolve, 50));\n  t.is(discount(10, \"HALF\"), 5);\n});\ntest(\"keeps\", async (t) => {\n  t.is(discount(10, \"X\"), 10);\n  await new Promise((resolve) => setTimeout(resolve, 50));\n});\ntest.skip(\"skipped\", (t) => t.pass());\ntest.todo(\"later\");\n",
    );
    project.git(&["init", "-q"]);
    project.commit("ava");
    for command in [
        &["--", "npx", "ava"][..],
        &["--", "npx", "ava", "--no-worker-threads"],
    ] {
        project.supercov(command).succeeds();
        let summary = project.supercov(&["runs", "latest"]).succeeds();
        contains_all(
            &summary,
            &[
                "Attribution      Exact for 4 test(s)",
                "Passed      2",
                "Skipped     2",
            ],
        );
        let halved = project
            .supercov(&["runs", "latest", "line", "src/cart.js:3"])
            .succeeds();
        assert!(halved.contains("halves [ava:"), "{halved}");
        assert!(!halved.contains("keeps [ava:"), "{halved}");
        let kept = project
            .supercov(&["runs", "latest", "line", "src/cart.js:5"])
            .succeeds();
        assert!(kept.contains("keeps [ava:"), "{kept}");
        assert!(!kept.contains("halves [ava:"), "{kept}");
    }
    project.edit("src/cart.js", "return sum / 2;", "return sum * 0.5;");
    // Both ran discount; the skipped and todo tests have no pass to keep.
    let names = project
        .supercov(&["runs", "latest", "tests", "affected", "--names"])
        .succeeds();
    assert_eq!(names.trim(), "halves\nkeeps\nlater\nskipped");
    let selected = std::process::Command::new(project.root.join("node_modules/.bin/ava"))
        .args(["--match", "halves"])
        .current_dir(&project.root)
        .output()
        .unwrap();
    // AVA on its own, with nothing of Supercov's. When it fails it says why
    // on the other stream, and this printed an empty string.
    let output = String::from_utf8_lossy(&selected.stdout);
    assert!(
        output.contains("1 test passed"),
        "{:?}\n{output}\n{}",
        selected.status,
        String::from_utf8_lossy(&selected.stderr)
    );
}

#[cfg(unix)]
#[test]
fn tap_subtests_running_at_once_are_attributed_each_to_its_own() {
    let project = Project::empty("tap-tests");
    project.write("package.json", r#"{ "name": "tap-tests" }"#);
    // tap 15 and 16 are libtap with a CLI that runs each test file as a
    // process of its own, marked with TAP_CHILD_ID; this is one such process.
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../node_modules")
        .canonicalize()
        .unwrap();
    project.write(
        "node_modules/tap/package.json",
        r#"{ "name": "tap", "version": "16.0.0", "main": "index.js" }"#,
    );
    project.write(
        "node_modules/tap/index.js",
        "module.exports = require(\"libtap\");\n",
    );
    std::os::unix::fs::symlink(
        modules.join("libtap"),
        project.root.join("node_modules/libtap"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\n");
    project.write(
        "src/cart.js",
        "exports.discount = function discount(sum, code) {\n  if (code === \"HALF\") {\n    return sum / 2;\n  }\n  return sum;\n};\n",
    );
    // With two jobs `halves` is still waiting when `group > keeps` runs.
    project.write(
        "test/cart.test.js",
        "const t = require(\"tap\");\nconst { discount } = require(\"../src/cart\");\nt.jobs = 2;\nt.test(\"halves\", async (t) => {\n  await new Promise((resolve) => setTimeout(resolve, 50));\n  t.equal(discount(10, \"HALF\"), 5);\n});\nt.test(\"group\", async (t) => {\n  t.test(\"keeps\", async (t) => {\n    t.equal(discount(10, \"X\"), 10);\n    await new Promise((resolve) => setTimeout(resolve, 50));\n  });\n});\nt.test(\"skipped\", { skip: true }, async (t) => t.pass());\n",
    );
    project.git(&["init", "-q"]);
    project
        .supercov_with(
            &["--", "node", "test/cart.test.js"],
            &[("TAP_CHILD_ID", "1")],
        )
        .succeeds();
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            "Attribution      Exact for 4 test(s)",
            "Passed      3",
            "Skipped     1",
        ],
    );
    let halved = project
        .supercov(&["runs", "latest", "line", "src/cart.js:3"])
        .succeeds();
    assert!(halved.contains("halves [tap:"), "{halved}");
    assert!(!halved.contains("keeps [tap:"), "{halved}");
    let kept = project
        .supercov(&["runs", "latest", "line", "src/cart.js:5"])
        .succeeds();
    assert!(kept.contains("group > keeps [tap:"), "{kept}");
    assert!(!kept.contains("halves [tap:"), "{kept}");
    assert!(!kept.contains("  group [tap:"), "{kept}");
}

#[cfg(unix)]
#[test]
fn typescript_tests_compiled_into_another_directory_run_and_are_measured() {
    let project = Project::empty("tsc-outdir");
    project.write(
        "package.json",
        r#"{ "name": "tsc-outdir", "scripts": { "test": "tsc -p . && node --test dist/test/fee.test.js" } }"#,
    );
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../node_modules");
    std::os::unix::fs::symlink(
        modules.canonicalize().unwrap(),
        project.root.join("node_modules"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\ndist\n");
    // A strict check of the test file too, and CommonJS output in `dist/`.
    project.write(
        "tsconfig.json",
        r#"{ "compilerOptions": { "module": "commonjs", "target": "es2022", "outDir": "dist", "rootDir": ".", "strict": true, "types": ["node"] }, "include": ["src", "test"] }"#,
    );
    project.write(
        "src/fee.ts",
        "export function fee(sum: number): number {\n  return sum > 100 ? 0 : 5;\n}\n",
    );
    project.write(
        "test/fee.test.ts",
        "import { test } from \"node:test\";\nimport { equal } from \"node:assert/strict\";\nimport { fee } from \"../src/fee\";\n\ntest(\"small orders pay\", () => equal(fee(10), 5));\n",
    );
    project.git(&["init", "-q"]);
    // The test file used to import the runtime by a path that pointed nowhere
    // once compiled into dist/.
    project.supercov(&["--", "npm", "test"]).succeeds();
    let line = project
        .supercov(&["runs", "latest", "line", "src/fee.ts:2"])
        .succeeds();
    assert!(line.contains("small orders pay ["), "{line}");
}

#[test]
fn the_command_runs_as_given_and_a_build_is_the_commands_to_run() {
    let project = Project::empty("built");
    project.write(
        "package.json",
        r#"{ "name": "built", "scripts": { "build": "node build.js", "test": "node --test test/fee.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\ndist\n.supercov\n");
    project.write(
        "build.js",
        "const fs = require('node:fs');\nfs.mkdirSync('dist', { recursive: true });\nfs.copyFileSync('src/fee.js', 'dist/fee.js');\n",
    );
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../dist/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    project.git(&["init", "-q"]);

    // Supercov used to run `npm run build` first here, unasked. The command
    // runs as given now, in a copy that starts without build output, and a
    // run that fails says what that means.
    let unbuilt = project.supercov(&["--", "npm", "test"]);
    assert_ne!(unbuilt.code(), 0, "{}", unbuilt.stdout());
    let said = format!("{}{}", unbuilt.stdout(), unbuilt.stderr());
    assert!(
        said.contains("starts without build output") && said.contains("npm run build && npm test"),
        "{said}"
    );

    // With the build in the command, it compiles the instrumented copy, and
    // what the tests run is measured.
    let built = project
        .supercov(&["--", "sh", "-c", "npm run build && npm test"])
        .succeeds();
    assert!(!built.contains("starts without build output"), "{built}");
    let line = project
        .supercov(&["runs", "latest", "line", "src/fee.js:2"])
        .succeeds();
    assert!(line.contains("small orders pay"), "{line}");

    // A test that fails on its own is named, and the build is not blamed.
    project.write(
        "test/wrong.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('large orders are free', () => equal(fee(200), 5));\n",
    );
    let failed = project.supercov(&[
        "--",
        "sh",
        "-c",
        "npm run build && node --test test/fee.test.js test/wrong.test.js",
    ]);
    assert_ne!(failed.code(), 0, "{}", failed.stdout());
    let said = failed.stderr();
    assert!(
        said.contains("1 test(s) failed: large orders are free"),
        "{said}"
    );
    assert!(!said.contains("starts without build output"), "{said}");
}

#[cfg(unix)]
#[test]
fn a_tool_that_judges_source_reads_it_as_it_was_written() {
    let project = Project::empty("judged");
    let script = |tool: &str| {
        format!(
            r#"{{ "name": "judged", "scripts": {{ "test": "{tool} src && node --test test/fee.test.js" }} }}"#
        )
    };
    project.write("package.json", &script("oxlint"));
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    // What a native linter does, as far as it matters here: it reads the
    // files itself, fails on a line nobody wrote, and leaves a report.
    let linter = "#!/bin/sh\nif grep -rl supercov \"$1\"; then exit 3; fi\ngrep -c '' \"$1\"/fee.js > lint-report.txt\n";
    for tool in ["oxlint", "housestyle"] {
        project.write_script(&format!("node_modules/.bin/{tool}"), linter);
    }
    project.git(&["init", "-q"]);

    // Biome, oxlint, dprint, cspell and knip read the instrumented files and
    // failed a test script that ran them first. The workspace's own `.bin`
    // starts such a tool on the source as it was written, and what it writes
    // is the command's output like any other.
    project.supercov(&["--", "npm", "test"]).succeeds();
    assert_eq!(project.read("lint-report.txt"), "4\n");
    let line = project
        .supercov(&["runs", "latest", "line", "src/fee.js:2"])
        .succeeds();
    assert!(line.contains("small orders pay"), "{line}");

    // The same tool installed for the whole machine, and started without a
    // package manager, is found through PATH.
    let machine = project.root.join("machine-tools");
    std::fs::create_dir(&machine).unwrap();
    std::fs::rename(
        project.root.join("node_modules/.bin/oxlint"),
        machine.join("oxlint"),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        machine.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    project
        .supercov_with(&["--", "sh", "-c", "oxlint src"], &[("PATH", &path)])
        .succeeds();

    // A tool Supercov does not know reads what is there, until it is named.
    project.write("package.json", &script("housestyle"));
    let unnamed = project.supercov(&["--", "npm", "test"]);
    assert_ne!(unnamed.code(), 0, "{}", unnamed.stdout());
    project
        .supercov_with(
            &["--", "npm", "test"],
            &[("SUPERCOV_SOURCE_TOOLS", "housestyle")],
        )
        .succeeds();
}

#[test]
fn nothing_is_still_being_deleted_when_the_command_has_ended() {
    let project = Project::empty("swept");
    project.write(
        "package.json",
        r#"{ "name": "swept", "scripts": { "test": "node --test test/fee.test.js" } }"#,
    );
    project.write(".gitignore", ".supercov\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    // A project of some size: the copy a run replaces takes a while to unlink.
    for directory in 0..60 {
        let assets = project.root.join(format!("assets/d{directory}"));
        std::fs::create_dir_all(&assets).unwrap();
        for file in 0..100 {
            std::fs::write(assets.join(format!("f{file}.txt")), "x").unwrap();
        }
    }
    project.git(&["init", "-q"]);

    // The copy went to `.supercov/.trash` before the command started and was
    // unlinked only after it ended, by a process Supercov waited a tenth of a
    // second for. It was still going when the next command began, and one
    // that walks the project met a directory on its way out: AVA, which looks
    // for test files in every directory, failed on `EINVAL: invalid argument,
    // scandir '.supercov/.trash/...'`.
    for _ in 0..3 {
        project.supercov(&["--", "npm", "test"]).succeeds();
        let left = match std::fs::read_dir(project.root.join(".supercov/.trash")) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name != ".deleter.lock")
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        assert!(left.is_empty(), "{left:?}");
    }
}

#[test]
fn a_report_the_command_writes_names_the_project_and_not_the_copy() {
    // Under Vitest `coverage-final.json` keyed every file by its path in
    // Supercov's copy, and `clover.xml` and Cobertura's `<source>` named the
    // copy too: the command runs there, so anything that writes where it is
    // writes that. A reporter that writes where it ran stands in for them.
    let project = Project::empty("named");
    project.write(
        "package.json",
        r#"{ "name": "named", "scripts": { "test": "node --test test/fee.test.js && node report.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\ncoverage\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('pays', () => equal(fee(1), 5));\n",
    );
    project.write(
        "report.js",
        "const fs = require('node:fs');\nconst path = require('node:path');\nconst file = path.resolve('src/fee.js');\nfs.mkdirSync('coverage', { recursive: true });\nfs.writeFileSync('coverage/coverage-final.json', JSON.stringify({ [file]: { path: file } }));\nfs.writeFileSync('coverage/cobertura.xml', `<source>${process.cwd()}</source>`);\n",
    );
    project.git(&["init", "-q"]);
    let run = project.supercov(&["--", "npm", "test"]);
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    // Windows spells the test's own directory `\\?\C:\…`; a tool spells it
    // without the prefix.
    let plain = |path: &std::path::Path| {
        let text = path.display().to_string();
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    };
    let root = plain(&project.root);
    let report: serde_json::Value =
        serde_json::from_str(&project.read("coverage/coverage-final.json")).unwrap();
    let file = plain(&project.root.join("src").join("fee.js"));
    assert_eq!(report[&file]["path"], file, "{report}");
    assert_eq!(
        project.read("coverage/cobertura.xml"),
        format!("<source>{root}</source>")
    );
}

#[cfg(unix)]
#[test]
fn a_comparison_says_which_files_the_difference_is_in() {
    // Two end-to-end runs of one commit differed by 79 rows, and 45 of the 63
    // lost lines were one component that rendered in one run and not in the
    // other. The rows said so only once they were counted by hand.
    let project = Project::empty("where");
    project.write(
        "package.json",
        r#"{ "name": "where", "scripts": { "test": "node --test test/*.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "src/panel.js",
        "function panel(rows) {\n  const out = [];\n  for (const row of rows) {\n    out.push(row.name);\n  }\n  out.sort();\n  return out.join(',');\n}\nmodule.exports = { panel };\n",
    );
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  if (sum > 100) {\n    return 0;\n  }\n  return 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/app.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\nconst { panel } = require('../src/panel.js');\nconst state = process.env.STATE;\ntest('renders', () => {\n  if (state === 'full') equal(panel([{ name: 'a' }]), 'a');\n  equal(fee(state === 'full' ? 1 : 500), state === 'full' ? 5 : 0);\n});\n",
    );
    project.git(&["init", "-q"]);
    let run = |state: &str| {
        project
            .supercov_with(&["--", "npm", "test"], &[("STATE", state)])
            .succeeds();
        project.supercov(&["runs", "--limit", "1", "--json"]).json()["data"]["runs"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (full, bare) = (run("full"), run("bare"));
    let text = project.supercov(&["diff", &full, &bare]).succeeds();
    // The file that lost most comes first, with what each lost and gained.
    contains_all(
        &text,
        &[
            "in 2 files, most lost first:\n  src/panel.js: lost 6 lines, 1 branch\n  src/fee.js: lost 1 line, 1 branch; gained 1 line, 1 branch\n",
            "- line src/panel.js:2",
        ],
    );
    assert!(
        text.find("in 2 files").unwrap() < text.find("- line").unwrap(),
        "{text}"
    );
    let json = project.supercov(&["diff", &full, &bare, "--json"]).json();
    assert_eq!(json["data"]["files"], 2, "{json}");
    assert_eq!(json["data"]["byFile"][0]["file"], "src/panel.js");
    assert_eq!(json["data"]["byFile"][0]["lost"]["lines"], 6);
    assert_eq!(json["data"]["byFile"][1]["gained"]["branches"], 1);
    // The summary counts the whole difference, not the page shown.
    let paged = project
        .supercov(&["diff", &full, &bare, "--limit", "1", "--json"])
        .json();
    assert_eq!(paged["data"]["byFile"][0]["lost"]["lines"], 6);
}

#[cfg(unix)]
#[test]
fn a_comparison_says_when_both_runs_measured_the_same_source() {
    let project = Project::empty("steady");
    project.write(
        "package.json",
        r#"{ "name": "steady", "scripts": { "test": "node --test test/*.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  if (sum > 100) {\n    return 0;\n  }\n  return 5;\n}\nmodule.exports = { fee };\n",
    );
    // What the test reaches depends on something outside the code, as an
    // end-to-end suite's does on timing.
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\nconst order = process.env.ORDER;\ntest('pays', () => {\n  if (order !== 'small') equal(fee(500), 0);\n  if (order !== 'large') equal(fee(1), 5);\n});\n",
    );
    project.git(&["init", "-q"]);
    let run = |order: &str| {
        project
            .supercov_with(&["--", "npm", "test"], &[("ORDER", order)])
            .succeeds();
        project.supercov(&["runs", "--limit", "1", "--json"]).json()["data"]["runs"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // Two end-to-end runs of one commit read "lost: 11 lines, 31 branches",
    // and nothing said that no code had changed between them.
    let (small, large) = (run("small"), run("large"));
    let same = project.supercov(&["diff", &small, &large]).succeeds();
    contains_all(
        &same,
        &[
            "gained: 1 line, 1 branch,",
            "lost: 1 line, 1 branch,",
            "Both runs measured the same source with the same test files",
            "Run again before acting on a loss.",
        ],
    );
    // What was lost is listed, before what was gained. The text listed gains
    // alone: "lost: 2 lines, 12 branches" stood over 73 gains.
    let lost = same.find("- line src/fee.js:5").expect(&same);
    assert!(
        lost < same.find("+ line src/fee.js:3").expect(&same),
        "{same}"
    );
    contains_all(
        &same,
        &["- branch src/fee.js:2 false", "+ branch src/fee.js:2 true"],
    );
    // A difference in one file is its own summary.
    assert!(!same.contains("most lost first"), "{same}");
    let json = project.supercov(&["diff", &small, &large, "--json"]).json();
    assert_eq!(json["data"]["inputs"]["sourceChanged"], false);
    assert_eq!(json["data"]["inputs"]["testsChanged"], false);
    // A comparison with nothing but losses had an empty body.
    let both = run("both");
    let only = project.supercov(&["diff", &both, &small]).succeeds();
    contains_all(
        &only,
        &[
            "gained: 0 lines, 0 branches,",
            "lost: 1 line, 1 branch,",
            "- line src/fee.js:3",
            "- branch src/fee.js:2 true",
            "showing 1-1 of 1 per category",
        ],
    );
    assert!(!only.contains("\n+ "), "{only}");
    // Nothing differs, nothing to explain.
    let again = run("large");
    let equal = project.supercov(&["diff", &large, &again]).succeeds();
    assert!(!equal.contains("Both runs measured"), "{equal}");

    // A test added, as an agent adds one: what differs is still not the code.
    project.write(
        "test/other.test.js",
        "const { test } = require('node:test');\ntest('other', () => {});\n",
    );
    let added = run("small");
    let tests = project.supercov(&["diff", &large, &added]).succeeds();
    contains_all(
        &tests,
        &["Both runs measured the same source, and the test files differ."],
    );

    // The code changed: a loss may be the change's.
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  if (sum > 200) {\n    return 0;\n  }\n  return 5;\n}\nmodule.exports = { fee };\n",
    );
    let changed = run("large");
    let source = project.supercov(&["diff", &added, &changed]).succeeds();
    assert!(source.contains("gained: 1 line,"), "{source}");
    assert!(!source.contains("Both runs measured"), "{source}");
    assert_eq!(
        project
            .supercov(&["diff", &added, &changed, "--json"])
            .json()["data"]["inputs"]["sourceChanged"],
        true
    );
}

#[test]
fn tests_that_recorded_no_coverage_can_be_listed() {
    let project = Project::empty("unevidenced");
    project.write(
        "package.json",
        r#"{ "name": "unevidenced", "scripts": { "test": "node --test test/fee.test.js test/static.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    project.write(
        "test/static.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\ntest('two is two', () => equal(1 + 1, 2));\ntest('names are strings', () => equal(typeof 'a', 'string'));\n",
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", "npm", "test"]).succeeds();

    // The summary named one test for 63 and no query named the rest.
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            "2 tests made assertions, but Supercov received no source-coverage evidence:",
            "    All of them: ",
            "tests without-evidence",
        ],
    );
    let listed = project
        .supercov(&["runs", "latest", "tests", "without-evidence"])
        .succeeds();
    contains_all(
        &listed,
        &[
            "Tests that made assertions and recorded no coverage: 2",
            "test/static.test.js > names are strings [",
            "test/static.test.js > two is two [",
            "passed, 1 assertion; its file imports no measured source",
            "showing 1-2 of 2",
        ],
    );
    assert!(!listed.contains("small orders pay"), "{listed}");
    let page = project
        .supercov(&[
            "runs",
            "latest",
            "tests",
            "without-evidence",
            "--limit",
            "1",
            "--json",
        ])
        .json();
    assert_eq!(page["command"], "coverage.tests-without-evidence");
    assert_eq!(page["pagination"]["total"], 2);
    assert_eq!(page["data"]["tests"][0]["name"], "names are strings");
    assert_eq!(page["data"]["tests"][0]["file"], "test/static.test.js");
    assert_eq!(page["data"]["tests"][0]["assertions"], 1);
    // It checks only test code: its file imports nothing measured.
    assert_eq!(page["data"]["tests"][0]["importsMeasured"], false);
    // `tests affected` is still the other query under `tests`.
    let other = project.supercov(&["runs", "latest", "tests", "nonsense"]);
    assert_eq!(other.code(), 2);
}

#[test]
fn a_directory_says_what_ran_while_no_test_was_running() {
    let project = Project::empty("rollup");
    project.write(
        "package.json",
        r#"{ "name": "rollup", "scripts": { "test": "node --test test/fee.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "boot.js",
        "const settings = { rate: 5 };\nconst limit = 100;\nmodule.exports = { settings, limit };\n",
    );
    project.write(
        "src/app/fee.js",
        "const { settings, limit } = require('../../boot.js');\nfunction fee(sum) {\n  return sum > limit ? 0 : settings.rate;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/app/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    project.git(&["init", "-q"]);
    project
        .supercov_with(
            &["--", "npm", "test"],
            &[
                ("SUPERCOV_SOURCE_ROOTS", "src,boot.js"),
                ("SUPERCOV_TEST_KIND", "unit"),
            ],
        )
        .succeeds();

    // `.` read unit 0.00% and All 100.00%, and `src` 50.00% and 100.00%: a
    // total above every kind, with nothing to say where the rest came from.
    // The files were loaded before the first test started.
    let areas = project
        .supercov(&["runs", "latest", "files", "--group", "dir"])
        .succeeds();
    contains_all(
        &areas,
        &[
            "Directory  Files  Lines    unit  no test      All",
            "src            1      4  50.00%   50.00%  100.00%",
            ".              1      3   0.00%  100.00%  100.00%",
            "\"no test\" is code that ran only while no test was running",
        ],
    );
    let json = project
        .supercov(&["runs", "latest", "files", "--group", "dir", "--json"])
        .json();
    let root = json["data"]["areas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|area| area["directory"] == ".")
        .unwrap();
    assert_eq!(root["covered"]["lines"], 3);
    assert_eq!(root["coveredByTests"]["lines"], 0);
    // One kind asked for is that kind's coverage, with nothing to compare.
    let unit = project
        .supercov(&[
            "runs", "latest", "files", "--group", "dir", "--kind", "unit",
        ])
        .succeeds();
    assert!(!unit.contains("no test"), "{unit}");
}

#[cfg(unix)]
#[test]
fn a_tools_change_to_source_reaches_the_project_and_the_file_stays_measured() {
    let project = Project::empty("fixed");
    project.write(
        "package.json",
        r#"{ "name": "fixed", "scripts": { "test": "prettier --write src/fee.js && oxlint --fix src && node --test test/fee.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  var rate=5;\n  return sum > 100 ? 0 : rate;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    // A formatter that runs on Node and writes through `fs`, and a native
    // linter that fixes what it reads, which has to be the formatted text.
    project.write_script(
        "node_modules/.bin/prettier",
        "#!/usr/bin/env node\nconst fs = require('node:fs');\nconst file = process.argv[3];\nfs.writeFileSync(file, fs.readFileSync(file, 'utf8').replace('rate=5', 'rate = 5'));\n",
    );
    project.write_script(
        "node_modules/.bin/oxlint",
        "#!/bin/sh\nif grep -rl supercov \"$2\"; then exit 3; fi\ngrep -q 'var rate = 5' \"$2\"/fee.js || exit 4\nsed 's/var rate/const rate/' \"$2\"/fee.js > fixed.tmp && mv fixed.tmp \"$2\"/fee.js\n",
    );
    project.git(&["init", "-q"]);

    // Prettier with `--write` and ESLint with `--fix` wrote the source they
    // had read over the instrumented copy, so the file ran unmeasured and
    // read 0% covered under passing tests, and a native tool's fix was
    // dropped. Each change is kept beside the copy, the next tool reads it,
    // and the project has it when the command ends.
    let run = project.supercov(&["--", "npm", "test"]);
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    assert!(
        run.stderr()
            .contains("the command's tools changed 1 source file(s)"),
        "{}",
        run.stderr()
    );
    assert_eq!(
        project.read("src/fee.js"),
        "function fee(sum) {\n  const rate = 5;\n  return sum > 100 ? 0 : rate;\n}\nmodule.exports = { fee };\n"
    );
    assert!(!project.root.join("fixed.tmp").exists());
    let line = project
        .supercov(&["runs", "latest", "line", "src/fee.js:3"])
        .succeeds();
    assert!(line.contains("small orders pay"), "{line}");

    // The next run starts from the fixed source: nothing is left to change,
    // and it measures the file as the project now has it.
    project.write(
        "node_modules/.bin/oxlint",
        "#!/bin/sh\ngrep -q 'const rate = 5' \"$2\"/fee.js\n",
    );
    let again = project.supercov(&["--", "npm", "test"]);
    assert_eq!(again.code(), 0, "{}{}", again.stdout(), again.stderr());
    assert!(
        !again.stderr().contains("source file(s)"),
        "{}",
        again.stderr()
    );
    let line = project
        .supercov(&["runs", "latest", "line", "src/fee.js:3"])
        .succeeds();
    assert!(line.contains("small orders pay"), "{line}");
}

#[cfg(unix)]
#[test]
fn a_workspace_packages_own_tool_reads_source_as_it_was_written() {
    let project = Project::empty("judged-package");
    project.write(
        "package.json",
        r#"{ "name": "judged-package", "private": true, "workspaces": ["packages/*"], "scripts": { "test": "cd packages/app && npm test" } }"#,
    );
    project.write(
        "packages/app/package.json",
        r#"{ "name": "app", "scripts": { "test": "oxlint src && node --test test/fee.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "packages/app/src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "packages/app/test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    // pnpm, and npm without hoisting, give a package its tools in a `.bin`
    // of its own, which is first on PATH when its scripts run.
    project.write_script(
        "packages/app/node_modules/.bin/oxlint",
        "#!/bin/sh\nif grep -rl supercov \"$1\"; then exit 3; fi\ngrep -c '' \"$1\"/fee.js > lint-report.txt\n",
    );
    project.git(&["init", "-q"]);

    project.supercov(&["--", "npm", "test"]).succeeds();
    assert_eq!(project.read("packages/app/lint-report.txt"), "4\n");
    // The project's own tool is what it was.
    assert!(
        project
            .read("packages/app/node_modules/.bin/oxlint")
            .starts_with("#!/bin/sh\nif grep"),
    );
}

#[cfg(windows)]
#[test]
fn a_tool_that_judges_source_reads_it_as_it_was_written_on_windows() {
    let project = Project::empty("judged");
    project.write(
        "package.json",
        r#"{ "name": "judged", "scripts": { "test": "oxlint src && node --test test/fee.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  return sum > 100 ? 0 : 5;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    // What a native linter does, as far as it matters here: it reads the
    // files itself, fails on a line nobody wrote, and leaves a report. npm
    // runs a script with cmd.exe, which starts the `.cmd` shim.
    project.write(
        "node_modules/.bin/oxlint.cmd",
        "@ECHO off\r\nfindstr /S /M /C:\"supercov\" \"%1\\*\" >NUL 2>NUL && exit /b 3\r\necho judged> lint-report.txt\r\nexit /b 0\r\n",
    );
    project.git(&["init", "-q"]);

    let run = project.supercov(&["--", "npm", "test"]);
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    assert!(project.read("lint-report.txt").starts_with("judged"));
    let line = project
        .supercov(&["runs", "latest", "line", "src/fee.js:2"])
        .succeeds();
    assert!(line.contains("small orders pay"), "{line}");
}

#[cfg(unix)]
#[test]
fn a_workspace_package_another_package_imports_is_measured_through_that_import() {
    let project = Project::empty("monorepo");
    project.write(
        "package.json",
        r#"{ "name": "mono", "private": true, "workspaces": ["packages/*"], "scripts": { "test": "npm test --workspaces --if-present" } }"#,
    );
    project.write(
        "packages/money/package.json",
        r#"{ "name": "money", "type": "module", "exports": "./src/money.js", "scripts": { "test": "node --test" } }"#,
    );
    project.write(
        "packages/money/src/money.js",
        "export function cents(amount) {\n  return Math.round(amount * 100);\n}\n",
    );
    project.write(
        "packages/money/test/money.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { cents } from \"../src/money.js\";\ntest(\"cents\", () => assert.equal(cents(1.5), 150));\n",
    );
    project.write(
        "packages/format/package.json",
        r#"{ "name": "@acme/format", "type": "module", "exports": "./src/format.js" }"#,
    );
    project.write(
        "packages/format/src/format.js",
        "export function label(cents) {\n  return `${cents}c`;\n}\n",
    );
    project.write(
        "packages/cart/package.json",
        r#"{ "name": "cart", "type": "module", "scripts": { "test": "node --test" } }"#,
    );
    project.write(
        "packages/cart/src/cart.js",
        "import { cents } from \"money\";\nimport { label } from \"@acme/format\";\nexport function total(prices) {\n  return label(prices.reduce((sum, price) => sum + cents(price), 0));\n}\n",
    );
    project.write(
        "packages/cart/test/cart.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\ntest(\"total\", () => assert.equal(total([1, 2]), \"300c\"));\n",
    );
    // What npm install makes of the workspaces: links into the project.
    std::fs::create_dir_all(project.root.join("node_modules/@acme")).unwrap();
    std::os::unix::fs::symlink("../packages/money", project.root.join("node_modules/money"))
        .unwrap();
    std::os::unix::fs::symlink("../packages/cart", project.root.join("node_modules/cart")).unwrap();
    std::os::unix::fs::symlink(
        "../../packages/format",
        project.root.join("node_modules/@acme/format"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\n");
    project.git(&["init", "-q"]);
    project.commit("mono");
    project.supercov(&["--", "npm", "test"]).succeeds();
    // cart's test reaches money and format through node_modules.
    let money = project
        .supercov(&["runs", "latest", "line", "packages/money/src/money.js:2"])
        .succeeds();
    contains_all(&money, &["cents [", "total ["]);
    let format = project
        .supercov(&["runs", "latest", "line", "packages/format/src/format.js:2"])
        .succeeds();
    assert!(format.contains("total ["), "{format}");
    project.edit(
        "packages/money/src/money.js",
        "amount * 100",
        "amount * 1000",
    );
    let affected = project
        .supercov(&["runs", "latest", "tests", "affected", "--names"])
        .succeeds();
    assert_eq!(affected.trim(), "cents\ntotal");
}

#[test]
fn source_roots_named_after_a_first_run_take_effect() {
    // The workspace of a first, automatic run was reused for every later one:
    // SUPERCOV_SOURCE_ROOTS changed which files are source but not a byte of
    // any of them, so the run still reported automatic scope and the same
    // counts, with nothing to say the variable had been ignored.
    let project = Project::cart("roots-after-first-run");
    project.write(
        "extra/tax.js",
        "export function tax(total) {\n  return total > 100 ? 20 : 0;\n}\n",
    );
    project.write(
        "test/tax.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { tax } from \"../extra/tax.js\";\ntest(\"tax applies over a hundred\", () => {\n  assert.equal(tax(200), 20);\n});\n",
    );
    project.commit("tax");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let automatic = project.supercov(&["runs", "latest", "scope"]).succeeds();
    contains_all(
        &automatic,
        &[
            "mode automatic",
            "    1  AMBIGUOUS  extra      unclassified first-party source",
            // What would settle it, ready to copy.
            "SUPERCOV_SOURCE_ROOTS=src,extra",
        ],
    );
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            "Unclassified source: extra 1",
            "SUPERCOV_SOURCE_ROOTS=src,extra",
        ],
    );

    project
        .supercov_with(
            &["--", "node", "--test"],
            &[("SUPERCOV_SOURCE_ROOTS", "src,extra")],
        )
        .succeeds();
    let named = project
        .supercov(&["runs", "latest", "scope", "--files"])
        .succeeds();
    contains_all(
        &named,
        &[
            "mode explicit; roots extra, src; included 2",
            "INCLUDED  extra/tax.js  explicit source root",
        ],
    );
    assert!(!named.contains("SUPERCOV_SOURCE_ROOTS="), "{named}");
    let line = project
        .supercov(&["runs", "latest", "line", "extra/tax.js:2"])
        .succeeds();
    assert!(line.contains("tax applies over a hundred"), "{line}");
}

#[test]
fn a_run_in_which_no_test_ran_is_marked_in_the_listing() {
    // Four runs made only to copy files out of the workspace sat in `runs`
    // as 0.00% beside the real ones, with nothing to tell them apart.
    let project = Project::cart("no-tests-ran");
    project.supercov(&["--", "node", "--test"]).succeeds();
    project.supercov(&["--", "node", "-e", "0"]).succeeds();
    let runs = project.supercov(&["runs"]).succeeds();
    let rows = runs.lines().collect::<Vec<_>>();
    assert!(rows[1].ends_with("NO TESTS RAN"), "{runs}");
    assert!(!rows[2].contains("NO TESTS RAN"), "{runs}");
    let listed = project.supercov(&["runs", "--json"]).json();
    assert_eq!(listed["data"]["runs"][0]["tests"], 0);
    assert_eq!(listed["data"]["runs"][1]["tests"], 3);
}

/// A suite of one test that takes a moment, so that runs started together
/// are running together.
fn slow_suite(project: &Project, name: &str, function: &str, call: &str) {
    project.write(
        &format!("test/{name}.test.js"),
        &format!(
            "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport {{ {function} }} from \"../src/cart.js\";\n\ntest(\"{name}\", async () => {{\n  await new Promise((done) => setTimeout(done, 600));\n  assert.ok({call} !== undefined);\n}});\n"
        ),
    );
}

#[test]
fn runs_started_at_once_each_have_a_copy_and_all_are_recorded() {
    // Suites that share nothing run side by side without Supercov. Under it
    // the second was refused, because every run rebuilt the one copy.
    let project = Project::cart("side-by-side");
    slow_suite(&project, "unit", "total", "total([], null)");
    slow_suite(&project, "integration", "shipping", "shipping(10, true)");
    slow_suite(&project, "e2e", "describe", "describe([])");
    project.commit("three suites");
    let ran = std::thread::scope(|scope| {
        ["unit", "integration", "e2e"]
            .map(|kind| {
                let project = &project;
                scope.spawn(move || {
                    let file = format!("test/{kind}.test.js");
                    project.supercov_with(
                        &["--", "node", "--test", &file],
                        &[("SUPERCOV_TEST_KIND", kind)],
                    )
                })
            })
            .map(|thread| thread.join().unwrap())
    });
    let mut copies = Vec::new();
    for run in &ran {
        assert_eq!(run.code(), 0, "{}", run.stderr());
        let stderr = run.stderr();
        let copy = stderr
            .lines()
            .find_map(|line| line.split("instrumenting isolated workspace ").nth(1))
            .unwrap_or_else(|| panic!("{stderr}"))
            .to_owned();
        // What a test prints names the project, whichever copy it ran in.
        assert!(
            !run.stdout().contains(".supercov/workspaces"),
            "{}",
            run.stdout()
        );
        copies.push(copy);
    }
    copies.sort();
    copies.dedup();
    assert_eq!(
        copies.len(),
        3,
        "each run has a copy of its own: {copies:?}"
    );

    let runs = project.supercov(&["runs", "--json"]).json();
    let runs = runs["data"]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 3, "{runs:?}");
    for run in runs {
        let id = run["id"].as_str().unwrap();
        let summary = project.supercov(&["runs", id]).succeeds();
        contains_all(&summary, &["Total       1", "Passed      1"]);
    }

    // Cleaning wants the store to itself, and says who has it.
    let cleaned = std::thread::scope(|scope| {
        let running =
            scope.spawn(|| project.supercov(&["--", "node", "--test", "test/unit.test.js"]));
        std::thread::sleep(std::time::Duration::from_millis(350));
        let cleaned = project.supercov(&["runs", "clean"]);
        assert_eq!(running.join().unwrap().code(), 0);
        cleaned
    });
    assert_eq!(cleaned.code(), 2, "{}", cleaned.stderr());
    contains_all(
        &cleaned.stderr(),
        &[
            "is active in this project",
            "can be cleaned when no run is active",
        ],
    );
    // And then removes every copy.
    project.supercov(&["runs", "clean"]).succeeds();
    assert!(!project.root.join(".supercov/workspaces").exists());
}

#[test]
fn merge_with_no_run_named_adds_up_the_runs_on_the_source_as_it_is() {
    // A script that measured three suites had to find what each run was
    // called, and took the newest directory's name, which is a guess.
    let project = Project::cart("merge-current");
    slow_suite(&project, "unit", "total", "total([], null)");
    slow_suite(&project, "integration", "shipping", "shipping(10, true)");
    project.commit("two suites");
    let nothing = project.supercov(&["merge"]);
    assert_eq!(nothing.code(), 2, "{}", nothing.stdout());
    assert!(
        nothing
            .stderr()
            .contains("no run was recorded on the source as it is now"),
        "{}",
        nothing.stderr()
    );

    let measure = |kind: &str| {
        let file = format!("test/{kind}.test.js");
        project
            .supercov_with(
                &["--", "node", "--test", &file],
                &[("SUPERCOV_TEST_KIND", kind)],
            )
            .succeeds();
        project.latest()
    };
    let unit = measure("unit");
    let one = project.supercov(&["merge"]).succeeds();
    assert!(one.contains("nothing to merge it with"), "{one}");
    let integration = measure("integration");
    // The same command again: its first run is not what the suite did last.
    let again = measure("unit");

    let merged = project.supercov(&["merge"]).succeeds();
    contains_all(
        &merged,
        &["merging 2 runs", &again, &integration, "merged run run_"],
    );
    assert!(!merged.contains(&unit), "{merged}");
    let kinds = project.supercov(&["runs", "latest", "kinds"]).succeeds();
    contains_all(&kinds, &["unit", "integration"]);
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["Total       2", "Passed      2"]);

    // Asked again, it is the merge already made, and no third run of it.
    let recorded = |project: &Project| {
        project.supercov(&["runs", "--json"]).json()["data"]["runs"]
            .as_array()
            .unwrap()
            .len()
    };
    let before = recorded(&project);
    let same = project.supercov(&["merge"]).succeeds();
    assert!(same.contains("already merged as"), "{same}");
    assert_eq!(recorded(&project), before);

    // Once the source has moved on, none of them is of this source.
    project.edit("src/cart.js", "return 0;", "return 1;");
    assert_eq!(project.supercov(&["merge"]).code(), 2);
}

#[test]
fn a_run_of_another_release_is_named_for_what_it_is() {
    // Merging a run recorded before an upgrade answered "differing domains:
    // instrumenter, tests, configuration", which names nothing to do.
    let project = Project::cart("merge-release");
    let older = project.measure(&["test/cart.test.js"]);
    project.write(
        "test/more.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { describe } from \"../src/cart.js\";\n\ntest(\"more\", () => {\n  assert.equal(describe([{ name: \"a\" }]), \"a\");\n});\n",
    );
    let newer = project.measure(&["test/more.test.js"]);
    let recorded = serde_json::from_str::<serde_json::Value>(
        &project.read(&format!(".supercov/runs/{newer}/run.json")),
    )
    .unwrap();
    assert_eq!(
        recorded["recordedBy"],
        env!("CARGO_PKG_VERSION"),
        "{recorded}"
    );
    // What a run of 4.5.0 looks like to this release.
    let path = format!(".supercov/runs/{older}/run.json");
    let mut metadata = serde_json::from_str::<serde_json::Value>(&project.read(&path)).unwrap();
    metadata["integrity"]["fingerprint"]["instrumenter"] = serde_json::json!("0".repeat(64));
    metadata["recordedBy"] = serde_json::json!("4.5.0");
    project.write(&path, &serde_json::to_string_pretty(&metadata).unwrap());

    let refused = project.supercov(&["merge", &older, &newer]);
    assert_eq!(refused.code(), 2, "{}", refused.stdout());
    contains_all(
        &refused.stderr(),
        &[
            &format!(
                "Cannot merge run {older}: it was recorded by Supercov 4.5.0, which instruments differently"
            ),
            "Measure it again to merge it.",
        ],
    );
    let left = project.supercov(&["merge"]);
    assert!(
        left.stdout().contains(&format!(
            "left out {older} (node --test test/cart.test.js): it was recorded by Supercov 4.5.0"
        )),
        "{}",
        left.stdout()
    );

    // A run from before runs said which release made them: older than the
    // first that did, on the day it ran.
    metadata.as_object_mut().unwrap().remove("recordedBy");
    let day = metadata["startedAt"].as_str().unwrap()[..10].to_owned();
    project.write(&path, &serde_json::to_string_pretty(&metadata).unwrap());
    let refused = project.supercov(&["merge", &older, &newer]);
    assert!(
        refused.stderr().contains(&format!(
            "it was recorded on {day} by a Supercov older than 4.7.0"
        )),
        "{}",
        refused.stderr()
    );
}

#[test]
fn a_run_of_a_new_test_alone_is_added_to_the_runs_before_it() {
    // Raising a suite's coverage one test at a time: measure only what was
    // added, and add it to what is known. `merge` refused it ("differing
    // domains: tests"), so every new test meant the whole suite again.
    let project = Project::cart("merge-grown");
    let baseline = project.measure(&[]);
    let before = project.supercov(&["runs", &baseline]).succeeds();
    contains_all(&before, &["76.92% (10/13)", "Total       3"]);

    let add = |name: &str| {
        project.write(
            &format!("test/{name}.test.js"),
            &format!(
                "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport {{ describe }} from \"../src/cart.js\";\n\ntest(\"{name}\", () => {{\n  assert.equal(describe([{{ name: \"a\" }}]), \"a\");\n}});\n"
            ),
        );
        project
            .supercov(&["--", "node", "--test", &format!("test/{name}.test.js")])
            .succeeds();
        project.latest()
    };
    let first = add("names");
    project.supercov(&["merge", &baseline, &first]).succeeds();
    let merged = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&merged, &["Total       4", "Passed      4"]);
    assert!(
        !merged.contains("76.92%"),
        "the added test's lines count: {merged}"
    );
    // It is a run of the checkout as it is, so a floor can be asked of it.
    project
        .supercov(&["runs", "latest", "check", "--min-lines", "80"])
        .succeeds();

    // With no run named, the same sum, and the next test added to it.
    let second = add("more-names");
    let summed = project.supercov(&["merge"]).succeeds();
    contains_all(&summed, &["merging 3 runs", &baseline, &first, &second]);
    contains_all(
        &project.supercov(&["runs", "latest"]).succeeds(),
        &["Total       5", "Passed      5"],
    );

    // A test changed since a run makes that run's record one of other tests.
    project.edit(
        "test/names.test.js",
        "test(\"names\"",
        "test(\"names, again\"",
    );
    let third = add("yet-more");
    let refused = project.supercov(&["merge", &first, &third]);
    assert_eq!(refused.code(), 2, "{}", refused.stdout());
    contains_all(
        &refused.stderr(),
        &[
            "test/names.test.js, a file of its own tests, has changed",
            "Its own tests changed; measure them again.",
        ],
    );
    // Accepting changes does not make a changed test's record its own.
    let still = project.supercov(&["merge", "--allow-changed", &first, &third]);
    assert_eq!(still.code(), 2, "{}", still.stdout());
    assert!(
        still.stderr().contains("Its own tests changed"),
        "{}",
        still.stderr()
    );
    let left = project.supercov(&["merge"]).succeeds();
    contains_all(
        &left,
        &[
            &format!("left out {first}"),
            "test/names.test.js, a file of its own tests, has changed. Measure it again to count it.",
            "merging 3 runs",
        ],
    );
    assert!(!left.contains(&format!("  {first}")), "{left}");
}

#[test]
fn new_tests_are_added_across_the_helpers_and_settings_they_bring() {
    // A batch of new end-to-end specs usually brings a helper or two and a
    // setting of the runner's. Every spec loads the helpers, so merge refused
    // to add a run of the new specs to the baseline, and an edit to one
    // runner's settings made every other runner's run out of date too. Each
    // file is judged by what the earlier tests did with it.
    let project = Project::cart("merge-across");
    project.write(
        "test/helpers.js",
        "export const item = (name) => ({ name });\n",
    );
    project.write(
        "test/helped.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { describe } from \"../src/cart.js\";\nimport { item } from \"./helpers.js\";\n\ntest(\"helped\", () => {\n  assert.equal(describe([item(\"a\")]), \"a\");\n});\n",
    );
    project.write("runner.config.js", "export default { timeout: 1000 };\n");
    project.write(
        "playwright.config.js",
        "export default { timeout: 1000 };\n",
    );
    project.commit("helpers");
    let baseline = project.measure(&[]);

    // The batch: the helper gains a function, the other runner's settings
    // change, and one new test uses the new function.
    project.edit(
        "test/helpers.js",
        "\n",
        "\n\nexport const items = (...names) => names.map(item);\n",
    );
    project.edit("playwright.config.js", "timeout: 1000", "timeout: 2000");
    project.write(
        "test/batch.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { describe } from \"../src/cart.js\";\nimport { items } from \"./helpers.js\";\n\ntest(\"batch\", () => {\n  assert.equal(describe(items(\"a\")), \"a\");\n});\n",
    );
    let batch = project.measure(&["test/batch.test.js"]);
    let summed = project.supercov(&["merge"]).succeeds();
    contains_all(&summed, &["merging 2 runs", &baseline, &batch]);
    let merged = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &merged,
        &[
            "Passed      5",
            "merged across: ",
            "test/helpers.js (only added items)",
            "playwright.config.js (playwright settings; `node --test` ran under node:test)",
        ],
    );
    let json = project.supercov(&["runs", "latest", "--json"]).json();
    assert_eq!(json["data"]["stale"], false, "{json}");
    assert!(
        json["data"]["mergedAcross"]
            .as_array()
            .is_some_and(|across| across.iter().any(|item| item
                .as_str()
                .is_some_and(|item| item.starts_with("test/helpers.js")))),
        "{json}"
    );

    // A function the earlier tests ran changing is a change to what they did.
    project.edit("test/helpers.js", "({ name })", "({ name: name.trim() })");
    let again = project.measure(&["test/batch.test.js"]);
    let refused = project.supercov(&["merge", &baseline, &again]);
    assert_eq!(refused.code(), 2, "{}", refused.stdout());
    contains_all(
        &refused.stderr(),
        &[
            "test/helpers.js changed: ",
            "item (line 1)",
            "pass --allow-changed if the change alters nothing its tests ran",
        ],
    );
    let left = project.supercov(&["merge"]);
    contains_all(
        &left.stdout(),
        &[
            &format!("left out {baseline}"),
            "test/helpers.js changed: ",
            "or pass --allow-changed",
        ],
    );
    // The user can say it alters nothing they ran; the merged run says so.
    project
        .supercov(&["merge", "--allow-changed", &baseline, &again])
        .succeeds();
    contains_all(
        &project.supercov(&["runs", "latest"]).succeeds(),
        &["test/helpers.js (accepted)", "--allow-changed"],
    );

    // Settings no runner owns -- a build's, a tool's -- may set how any test
    // runs.
    project.edit("test/helpers.js", "({ name: name.trim() })", "({ name })");
    project.edit("runner.config.js", "timeout: 1000", "timeout: 2000");
    let settings = project.measure(&["test/batch.test.js"]);
    let refused = project.supercov(&["merge", &baseline, &settings]);
    assert_eq!(refused.code(), 2, "{}", refused.stdout());
    assert!(
        refused.stderr().contains("runner.config.js has changed"),
        "{}",
        refused.stderr()
    );

    // The product code changing is never accepted: nothing is of this
    // source, and a merge of the runs named is of the source they ran.
    project.edit("src/cart.js", "return 0;", "return 1;");
    let source = project.supercov(&["merge", "--allow-changed"]);
    assert_eq!(source.code(), 2);
    assert!(
        source
            .stdout()
            .contains("src/cart.js, which it measured, has changed"),
        "{}",
        source.stdout()
    );
    project
        .supercov(&["merge", "--allow-changed", &baseline, &batch])
        .succeeds();
    let old = project.supercov(&["runs", "latest", "--json"]).json();
    assert_eq!(old["data"]["stale"], true, "{old}");
}

#[test]
fn source_left_out_of_what_is_measured_is_judged_like_a_helper() {
    // A project keeps simulators outside SUPERCOV_SOURCE_ROOTS on purpose.
    // A change to one made every run out of date for merge, the suites
    // that never load it included.
    let project = Project::cart("merge-unmeasured");
    project.write(
        "simulators/echo.js",
        "export const echo = (value) => value;\n",
    );
    project.write(
        "test/echo.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { echo } from \"../simulators/echo.js\";\nimport { describe } from \"../src/cart.js\";\n\ntest(\"echo\", () => {\n  assert.equal(echo(describe([{ name: \"a\" }])), \"a\");\n});\n",
    );
    project.commit("simulators");
    let roots = [("SUPERCOV_SOURCE_ROOTS", "src")];
    let measure = |file: &str| {
        project
            .supercov_with(&["--", "node", "--test", file], &roots)
            .succeeds();
        project.latest()
    };
    let cart = measure("test/cart.test.js");
    measure("test/echo.test.js");
    project.edit("simulators/echo.js", "=> value", "=> String(value)");
    let echo = measure("test/echo.test.js");
    let summed = project.supercov_with(&["merge"], &roots).succeeds();
    contains_all(&summed, &["merging 2 runs", &cart, &echo]);
    contains_all(
        &project.supercov(&["runs", "latest"]).succeeds(),
        &["simulators/echo.js (`node --test test/cart.test.js` does not load it)"],
    );
}

#[test]
fn a_test_is_affected_by_a_helper_its_file_loads() {
    // A helper sits beside the tests, where nothing is measured, so no test's
    // record names it: a change to a helper every test called left every
    // test unaffected.
    let project = Project::cart("affected-helper");
    project.write(
        "test/helpers.js",
        "export const item = (name) => ({ name });\n",
    );
    project.write(
        "test/helped.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { describe } from \"../src/cart.js\";\nimport { item } from \"./helpers.js\";\n\ntest(\"helped\", () => {\n  assert.equal(describe([item(\"a\")]), \"a\");\n});\n",
    );
    project.commit("helpers");
    project.measure(&[]);
    let affected = |project: &Project| {
        let data = project
            .supercov(&["runs", "latest", "tests", "affected", "--json"])
            .json()["data"]
            .clone();
        data["affected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|test| {
                (
                    test["name"].as_str().unwrap().to_owned(),
                    test["reasons"].to_string(),
                )
            })
            .collect::<Vec<_>>()
    };
    // Only added: no test written before can have run it.
    project.edit(
        "test/helpers.js",
        "\n",
        "\n\nexport const items = (...names) => names.map(item);\n",
    );
    assert_eq!(affected(&project), Vec::<(String, String)>::new());
    // A function the helped test runs: that test, and no other.
    project.edit("test/helpers.js", "({ name })", "({ name: name.trim() })");
    let found = affected(&project);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("helped"), "{found:?}");
    assert!(
        found[0].1.contains("test/helpers.js changed: ")
            && found[0].1.contains("item (line 1)")
            && found[0].1.contains("(this test's file loads it)"),
        "{found:?}"
    );
}

#[test]
fn gaps_export_writes_every_obligation_of_a_run_too_large_for_one_reply() {
    // `gaps --format jsonl` exists for the runs whose gaps do not fit in a
    // JSON reply. It read its data back through that reply, so it refused
    // exactly those, with the reply's 64 KB message.
    const FILES: usize = 400;
    let project = Project::empty("gaps-export");
    project.write(
        "package.json",
        r#"{ "name": "rules", "private": true, "type": "module" }"#,
    );
    let mut imports = String::new();
    let mut calls = String::new();
    // The second half under long paths: a page of them weighs more than a
    // page of the first half, as entries do in a real run.
    let path = |index: usize| {
        if index < FILES / 2 {
            format!("src/rules/rule{index}.js")
        } else {
            format!(
                "src/rules/a/directory/named/at/the/length/real/projects/nest/their/rules/rule{index}.js"
            )
        }
    };
    for index in 0..FILES {
        project.write(
            &path(index),
            "export function allowed(admin, owner, open) {\n  if ((admin && owner) || open) {\n    return true;\n  }\n  return false;\n}\n",
        );
        imports.push_str(&format!(
            "import {{ allowed as rule{index} }} from '../{}';\n",
            path(index)
        ));
        calls.push_str(&format!(
            "  assert.equal(rule{index}(true, true, false), true);\n"
        ));
    }
    project.write(
        "test/rules.test.js",
        &format!(
            "import assert from 'node:assert/strict';\nimport test from 'node:test';\n{imports}\ntest('admins who own it are allowed', () => {{\n{calls}}});\n"
        ),
    );
    project.git(&["init", "-q"]);
    project.commit("rules");
    let run = project.measure(&[]);

    // Too large for one reply: each page holds what fits and names the next,
    // and following them reaches every file. A page that did not fit used to
    // be refused, which stopped a pager after its first page.
    let mut offset = 0;
    let mut paged = std::collections::BTreeSet::new();
    let mut pages = 0;
    loop {
        let page = project
            .supercov(&[
                "runs",
                &run,
                "gaps",
                "--json",
                "--limit",
                "1000",
                "--offset",
                &offset.to_string(),
            ])
            .json();
        assert_eq!(page["ok"], true, "{page}");
        pages += 1;
        for gap in page["data"]["gaps"].as_array().unwrap() {
            paged.insert(gap["file"].as_str().unwrap().to_owned());
        }
        let pagination = &page["pagination"];
        if pagination["hasMore"] != true {
            break;
        }
        assert!(
            pagination["returned"].as_u64().unwrap() < 1000,
            "{pagination}"
        );
        offset = pagination["nextOffset"].as_u64().unwrap();
    }
    assert!(pages > 1, "a run this size takes more than one reply");
    assert_eq!(paged.len(), FILES);

    let written = project
        .supercov(&[
            "runs",
            &run,
            "gaps",
            "--format",
            "jsonl",
            "--metric",
            "mcdc",
            "--output",
            "gaps.jsonl",
        ])
        .exits(0);
    let exported = project.read("gaps.jsonl");
    let rows: Vec<serde_json::Value> = exported
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    // Only the all-true call ran, so no condition of `(admin && owner) ||
    // open` is shown to matter: three each, on the decision's line.
    assert_eq!(rows.len(), FILES * 3, "{written}");
    assert!(rows.iter().all(|row| row["line"] == 2), "{exported}");
    let files: std::collections::BTreeSet<&str> =
        rows.iter().filter_map(|row| row["file"].as_str()).collect();
    assert_eq!(files.len(), FILES);
    assert!(
        written.contains(&format!(
            "wrote {} uncovered obligation(s) in {FILES} file(s)",
            FILES * 3
        )),
        "{written}"
    );

    // And the same on standard output.
    let printed = project
        .supercov(&[
            "runs", &run, "gaps", "--format", "jsonl", "--metric", "mcdc",
        ])
        .succeeds();
    assert_eq!(printed, exported);
}

#[cfg(unix)]
#[test]
fn the_copy_carries_what_a_project_holds_and_keeps_its_deletions() {
    let project = Project::cart("copy-shapes");
    // A package of its own, with its own installed tools.
    project.write(
        "packages/tool/node_modules/dep/index.js",
        "module.exports = 1;\n",
    );
    project.write_script(
        "packages/tool/node_modules/.bin/tool",
        "#!/bin/sh\necho tool\n",
    );
    // A link to a directory of the project, and one to somewhere outside it.
    std::os::unix::fs::symlink("src", project.root.join("linked-src")).unwrap();
    let outside = Project::empty("copy-shapes-outside");
    outside.write("readme.txt", "outside\n");
    std::os::unix::fs::symlink(&outside.root, project.root.join("outside")).unwrap();
    project.write("notes/old.txt", "keep me\n");
    project.write(
        "test/shapes.test.js",
        "import assert from 'node:assert/strict';\nimport { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';\nimport test from 'node:test';\n\ntest('reads through a link, deletes a note, writes a large report', () => {\n  assert.match(readFileSync('linked-src/cart.js', 'utf8'), /export function/);\n  rmSync('notes/old.txt');\n  mkdirSync('out', { recursive: true });\n  writeFileSync('out/big.txt', process.cwd() + '\\n' + 'x'.repeat(17 << 20));\n});\n",
    );
    project.commit("shapes");

    let measured = project.supercov(&["--", "node", "--test", "test/shapes.test.js"]);
    let said = format!("{}{}", measured.stdout(), measured.stderr());
    assert_eq!(measured.code(), 0, "{said}");
    contains_all(
        &said,
        &[
            "the command deleted 1 file(s) in the isolated workspace; deletions are not propagated to the project: notes/ 1",
            "omitting symlink outside the isolated project",
        ],
    );
    assert_eq!(project.read("notes/old.txt"), "keep me\n");
    // Too large to rewrite, the report comes back as the command wrote it.
    let report = std::fs::read(project.root.join("out/big.txt")).unwrap();
    assert!(report.len() > 17 << 20);
    let copy = format!("{}/.supercov/workspaces/", project.root.display());
    assert!(
        report.starts_with(copy.as_bytes()),
        "a file this large still names the copy it was written in"
    );
}

#[test]
fn every_human_query_pages_and_names_its_filters() {
    let project = Project::cart("human-pages");
    project.write(
        "src/tax.js",
        "export function tax(amount, region) {\n  if (region === 'EU' || amount > 1000) {\n    return amount * 0.2;\n  }\n  return 0;\n}\n",
    );
    project.write(
        "test/tax.test.js",
        "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { tax } from '../src/tax.js';\n\ntest('EU pays tax', () => {\n  assert.equal(tax(10, 'EU'), 2);\n});\n",
    );
    project.commit("tax");
    let run = project.measure(&[]);

    // Every list cut to one row says how to read the next.
    for args in [
        vec!["files", "--limit", "1"],
        vec!["gaps", "--limit", "1"],
        vec!["file", "src/cart.js", "--limit", "1"],
        vec!["decision", "src/tax.js:2", "--limit", "1"],
        vec!["line", "src/tax.js:2", "--limit", "1"],
        vec!["test", "EU pays tax", "--limit", "1"],
    ] {
        let mut all = vec!["runs", run.as_str()];
        all.extend(args.iter().copied());
        let output = project.supercov(&all).succeeds();
        assert!(
            output.contains("next page: supercov runs"),
            "{args:?}\n{output}"
        );
        assert!(output.contains("--limit 1"), "{args:?}\n{output}");
    }

    // A narrowed view says what it was narrowed to.
    let narrowed = project
        .supercov(&[
            "runs",
            &run,
            "files",
            "--filter",
            "passed",
            "--kind",
            "unit",
            "--runner",
            "node:test",
        ])
        .succeeds();
    contains_all(&narrowed, &["passed", "unit", "node:test"]);

    // A line the run recorded nothing about is read from the working tree,
    // and says when that changed since the run.
    let current = project
        .supercov(&["runs", &run, "line", "src/tax.js:4"])
        .succeeds();
    assert!(
        current.contains("(read from the current working tree)"),
        "{current}"
    );
    project.edit("src/tax.js", "amount * 0.2", "amount * 0.25");
    let stale = project
        .supercov(&["runs", &run, "line", "src/tax.js:4"])
        .succeeds();
    assert!(
        stale.contains("(read from the working tree, which changed since this run)"),
        "{stale}"
    );

    // Only what a run gained over another.
    let gained = project
        .supercov(&["diff", &run, &run, "--gained"])
        .succeeds();
    assert!(!gained.is_empty(), "{gained}");
}

#[test]
fn changes_since_a_run_name_the_tests_they_reach_and_block_a_merge() {
    let project = Project::empty("changes-since");
    project.write(
        "package.json",
        r#"{ "name": "changes", "private": true, "type": "module", "dependencies": {} }"#,
    );
    project.write(
        "src/a.js",
        "export const a = (x) => (x > 1 ? 'big' : 'small');\n",
    );
    project.write("src/b.js", "export const b = (x) => (x ? 1 : 0);\n");
    project.write("test/helper.js", "export const helper = () => 1;\n");
    project.write(
        "test/a.test.js",
        "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { a } from '../src/a.js';\nimport { helper } from './helper.js';\n\ntest('a', () => {\n  assert.equal(a(2), 'big');\n  assert.equal(helper(), 1);\n});\n",
    );
    project.write(
        "__tests__/b.test.js",
        "import assert from 'node:assert/strict';\nimport test from 'node:test';\nimport { b } from '../src/b.js';\n\ntest('b', () => {\n  assert.equal(b(1), 1);\n});\n",
    );
    project.git(&["init", "-q"]);
    project.commit("changes");
    let first = project.measure(&["test/a.test.js"]);

    // A dependency added between two runs keeps them apart.
    project.edit(
        "package.json",
        r#""dependencies": {}"#,
        r#""dependencies": { "left-pad": "1.3.0" }"#,
    );
    let second = project.measure(&["__tests__/b.test.js"]);
    let refused = project.supercov(&["merge", &first, &second]).exits(2);
    assert!(
        refused.contains("differing domains: dependencies"),
        "{refused}"
    );

    // A removed source, a changed helper and the dependency change, each
    // named with the tests it reaches.
    project.edit(
        "package.json",
        r#""dependencies": { "left-pad": "1.3.0" }"#,
        r#""dependencies": {}"#,
    );
    std::fs::remove_file(project.root.join("src/b.js")).unwrap();
    project.edit("test/helper.js", "=> 1", "=> 2");
    let since_first = project
        .supercov(&["runs", &first, "tests", "affected"])
        .succeeds();
    contains_all(
        &since_first,
        &[
            "test/a.test.js › a",
            "test/helper.js changed: helper (line 1) (this test's file loads it)",
            "src/b.js  removed",
        ],
    );
    let since_second = project
        .supercov(&["runs", &second, "tests", "affected"])
        .succeeds();
    contains_all(
        &since_second,
        &[
            "__tests__/b.test.js › b",
            "dependencies or lockfile changed (every test)",
            "src/b.js removed (this test ran code in it)",
            "test/helper.js changed: helper (line 1) (a file tests may load)",
        ],
    );
}

#[test]
fn runs_of_two_languages_are_refused_for_what_they_are() {
    // Each language's instrumenter is its own, so the two runs differed
    // there first, and the refusal said the Python run was "recorded by
    // another build of Supercov" -- the same build, minutes apart.
    let project = Project::cart("two-languages");
    project.write("pkg/__init__.py", "");
    project.write(
        "pkg/half.py",
        "def half(x):\n    return x / 2 if x else 0\n",
    );
    project.write(
        "test_half.py",
        "import unittest\n\nfrom pkg.half import half\n\n\nclass Half(unittest.TestCase):\n    def test_half(self):\n        self.assertEqual(half(2), 1)\n",
    );
    project.commit("python beside");
    let javascript = project.measure(&[]);
    project
        .supercov(&["--", "python3", "-m", "unittest", "-q", "test_half"])
        .succeeds();
    let python = project.latest();
    let refused = project.supercov(&["merge", &javascript, &python]).exits(2);
    assert!(
        refused.contains(&format!(
            "Cannot merge run {python}: it measured python, and run {javascript} measured javascript; runs of different languages are not merged"
        )),
        "{refused}"
    );
}

#[cfg(unix)]
#[test]
fn a_command_is_watched_stopped_and_passed_through_whole() {
    let project = Project::cart("supervision");
    // A timeout of nothing is refused before anything runs.
    let refused = project
        .supercov_with(
            &["--", "node", "--test"],
            &[("SUPERCOV_COMMAND_TIMEOUT_MS", "0")],
        )
        .exits(1);
    assert!(
        refused.contains(
            "SUPERCOV_COMMAND_TIMEOUT_MS must be a positive integer number of milliseconds"
        ),
        "{refused}"
    );
    // A command that ignores SIGTERM is killed once the grace runs out.
    let stubborn = project.supercov_with(
        &[
            "--",
            "node",
            "-e",
            "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)",
        ],
        &[("SUPERCOV_COMMAND_TIMEOUT_MS", "300")],
    );
    let said = format!("{}{}", stubborn.stdout(), stubborn.stderr());
    assert_eq!(stubborn.code(), 124, "{said}");
    assert!(said.contains("command still running after"), "{said}");
    // Asked to be verbose, a slow command's diagnostics name its processes.
    let verbose = project.supercov_with(
        &["--", "node", "-e", "setTimeout(() => {}, 900)"],
        &[
            ("SUPERCOV_DIAGNOSTIC_INTERVAL_MS", "200"),
            ("SUPERCOV_VERBOSE", "1"),
        ],
    );
    let said = format!("{}{}", verbose.stdout(), verbose.stderr());
    assert_eq!(verbose.code(), 0, "{said}");
    assert!(said.contains("exe=node"), "{said}");
    // A line longer than any relay buffer, with no newline, comes through whole.
    let long = project
        .supercov(&[
            "--",
            "node",
            "-e",
            "process.stdout.write('x'.repeat(70000))",
        ])
        .succeeds();
    assert!(long.contains(&"x".repeat(70000)), "{} bytes", long.len());
}

#[test]
fn cargo_reached_in_every_form_runs_what_it_was_asked() {
    let project = Project::empty("cargo-forms");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"forms\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nloud = []\nextra = []\n",
    );
    project.write(
        "src/lib.rs",
        "/// Doubles.\n///\n/// ```\n/// assert_eq!(forms::double(2), 4);\n/// ```\npub fn double(x: u32) -> u32 {\n    if cfg!(feature = \"loud\") { x * 2 } else { x + x }\n}\n",
    );
    project.write(
        "tests/forms.rs",
        "#[test]\nfn doubles() {\n    assert_eq!(forms::double(3), 6);\n}\n\n#[test]\nfn slow_one() {\n    assert_eq!(forms::double(1), 2);\n}\n",
    );
    // Through a package script, split as a shell splits it: attached short
    // flags, a quoted feature list and a quoted skip after `--`.
    project.write(
        "package.json",
        r#"{ "name": "forms", "private": true, "scripts": { "test": "cargo test -pforms -j2 --features \"loud extra\" --test forms -- --skip='slow_one'" } }"#,
    );
    project.git(&["init", "-q"]);
    project.commit("forms");
    let scripted = project.supercov(&["--", "npm", "test"]);
    let said = format!("{}{}", scripted.stdout(), scripted.stderr());
    assert_eq!(scripted.code(), 0, "{said}");
    assert!(
        said.contains("1 passed") || said.contains("Rust coverage: 1 test"),
        "{said}"
    );

    // Verbose, timed and quiet forms, with colour asked for and refused.
    for (args, env) in [
        (
            vec!["--", "cargo", "test", "-vv", "--timings"],
            vec![("CLICOLOR_FORCE", "1")],
        ),
        (
            vec!["--", "cargo", "test", "-q"],
            vec![("NO_COLOR", "1"), ("CLICOLOR", "0")],
        ),
    ] {
        let ran = project.supercov_with(&args, &env);
        assert_eq!(ran.code(), 0, "{args:?}\n{}{}", ran.stdout(), ran.stderr());
    }

    // Doctests alone.
    let doc = project.supercov(&["--", "cargo", "test", "--doc"]);
    let said = format!("{}{}", doc.stdout(), doc.stderr());
    assert_eq!(doc.code(), 0, "{said}");
    assert!(said.contains("Rust coverage: 1 test"), "{said}");

    // Edition 2024 merges doctests into one harness, which `-q` leaves
    // printing a mark per case and no name.
    project.write(
        "merged/Cargo.toml",
        "[package]\nname = \"merged\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    );
    project.write(
        "merged/src/lib.rs",
        "/// ```\n/// assert_eq!(merged::half(4), 2);\n/// ```\n///\n/// ```ignore\n/// merged::half(0);\n/// ```\n///\n/// ```\n/// assert_eq!(merged::half(2), 5);\n/// ```\npub fn half(x: u32) -> u32 {\n    if x > 0 { x / 2 } else { 0 }\n}\n",
    );
    let quiet = std::process::Command::new(env!("CARGO_BIN_EXE_supercov"))
        .args(["--", "cargo", "test", "-q", "--doc"])
        .current_dir(project.root.join("merged"))
        .output()
        .unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&quiet.stdout),
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert_eq!(quiet.status.code(), Some(101), "{said}");
    // rustdoc names the file as the platform writes paths.
    let said = said.replace('\\', "/");
    contains_all(
        &said,
        &[
            "running 3 tests",
            "failures:\n    src/lib.rs - half (line 9)",
            "test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out",
        ],
    );

    // A test that does not compile is reported as the build failing.
    project.write(
        "tests/broken.rs",
        "#[test]\nfn broken() {\n    let x: u32 = \"text\";\n}\n",
    );
    let broken = project.supercov(&["--", "cargo", "test"]);
    let said = format!("{}{}", broken.stdout(), broken.stderr());
    assert_ne!(broken.code(), 0, "{said}");
    assert!(said.contains("mismatched types"), "{said}");
}

#[test]
fn a_go_run_names_the_tests_it_could_not_claim_and_the_files_it_could_not_read() {
    let Some(go) = go() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let project = Project::empty("go-holes");
    project.write("go.mod", "module example.com/holes\n\ngo 1.22\n");
    project.write(
        "sign.go",
        "package holes\n\nimport \"fmt\"\n\nfunc Sign(n int) string {\n\tif n > 0 {\n\t\treturn \"positive\"\n\t}\n\treturn \"not positive\"\n}\n\nfunc Show(n int) { fmt.Println(Sign(n)) }\n",
    );
    // A generator kept beside the package, as `go generate` keeps them: the
    // build never compiles it, so it is no obligation and no package of
    // the directory's.
    project.write(
        "gen.go",
        "//go:build ignore\n\npackage main\n\nimport \"fmt\"\n\nfunc main() {\n\tif len(\"x\") > 0 {\n\t\tfmt.Println(\"generated\")\n\t}\n}\n",
    );
    // Behind a tag nothing sets: a tool of another package, and a source and
    // a test that would not parse if anything read them. Discovery cannot
    // answer a tag, so it reads them.
    project.write(
        "a_tool.go",
        "//go:build tools\n\npackage main\n\nfunc main() {}\n",
    );
    project.write(
        "broken.go",
        "//go:build never\n\npackage holes\n\nfunc Broken( {\n",
    );
    project.write(
        "broken_test.go",
        "//go:build never\n\npackage holes\n\nfunc Broken( {\n",
    );
    project.write(
        "sign_test.go",
        "package holes\n\nimport \"testing\"\n\nfunc TestPositive(t *testing.T) {\n\tif Sign(1) != \"positive\" {\n\t\tt.Fatal(\"positive\")\n\t}\n}\n\nfunc TestParallel(t *testing.T) {\n\tt.Parallel()\n\tif Sign(-1) != \"not positive\" {\n\t\tt.Fatal(\"not positive\")\n\t}\n}\n\nfunc ExampleShow() {\n\tShow(2)\n\t// Output: positive\n}\n",
    );
    project.git(&["init", "-q"]);
    let ran = project.supercov(&["--", go, "test", "./..."]);
    let said = format!("{}{}", ran.stdout(), ran.stderr());
    assert_eq!(ran.code(), 0, "{said}");
    contains_all(
        &said,
        &[
            "Go coverage: 3 test(s) across 2 source file(s) in 1 package(s)",
            "2 test(s) could not announce themselves (t.Parallel, Example or Fuzz)",
            "1 source file(s) did not parse and are absent from the denominator",
            "1 test file(s) did not parse; the tests they declare ran unattributed",
        ],
    );
    // What the unclaimed tests reached still counts.
    let line = project
        .supercov(&["runs", "latest", "line", "sign.go:9"])
        .succeeds();
    assert!(
        line.contains("COVERED") && !line.contains("NOT COVERED"),
        "{line}"
    );
    let files = project.supercov(&["runs", "latest", "files"]).succeeds();
    assert!(
        files.contains("sign.go") && !files.contains("gen.go"),
        "{files}"
    );
    // Asked about, the parallel test says why it holds nothing.
    let parallel = project
        .supercov(&["runs", "latest", "test", "TestParallel"])
        .succeeds();
    contains_all(
        &parallel,
        &[
            "outcome passed",
            "coverage not attributable to this test; it ran alongside others",
        ],
    );
}

#[test]
fn a_run_of_one_test_script_points_at_the_whole_suite_however_it_was_named() {
    let project = Project::cart("scripts");
    let scripts = |scripts: &str| {
        project.write(
            "package.json",
            &format!(r#"{{ "name": "cart", "private": true, "type": "module", "scripts": {{ {scripts} }} }}"#),
        );
    };
    let hints = |command: &[&str]| {
        let mut args = vec!["--"];
        args.extend_from_slice(command);
        let ran = project.supercov(&args);
        assert_eq!(
            ran.code(),
            0,
            "{command:?}\n{}{}",
            ran.stdout(),
            ran.stderr()
        );
        let run = project.latest();
        project.supercov(&["runs", &run, "--json"]).json()["data"]["hints"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|hint| hint.as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    scripts(
        r#""test": "node --test", "test:unit": "node --test test/cart.test.js", "test:other": "node --test""#,
    );
    // The whole suite, by any of its names, needs no pointer to itself.
    for command in [
        &["npm", "test"][..],
        &["npm", "t"],
        &["npm", "run", "-s", "test"],
        &["npm", "--silent", "run-script", "test"],
    ] {
        assert_eq!(hints(command), Vec::<String>::new(), "{command:?}");
    }
    // One script of several names the others and the whole command.
    // pnpm where it is installed: not every machine has it.
    let pnpm = std::process::Command::new("pnpm")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    let mut commands = vec![&["npm", "run", "test:unit"][..]];
    if pnpm {
        commands.push(&["pnpm", "test:unit"]);
    }
    for command in commands {
        let said = hints(command).join("\n");
        contains_all(
            &said,
            &[
                &format!("wrapped `{}`", command.join(" ")),
                "Other test scripts include test:other.",
                "-- npm test`",
            ],
        );
        assert!(!said.contains("include test:unit"), "{said}");
    }
    // With nothing else to name, it still names the whole command.
    scripts(r#""test": "node --test", "test:unit": "node --test test/cart.test.js""#);
    let said = hints(&["npm", "run", "test:unit"]).join("\n");
    assert!(
        said.contains("-- npm test`") && !said.contains("Other test scripts"),
        "{said}"
    );
    // A command of its own beside a lone `test` script is not one of several,
    // and a project with no `test` script has no whole suite to point at.
    scripts(r#""test": "node --test""#);
    assert_eq!(hints(&["node", "--test"]), Vec::<String>::new());
    scripts(r#""unit": "node --test""#);
    assert_eq!(hints(&["npm", "run", "unit"]), Vec::<String>::new());
}

#[test]
fn merge_says_what_moved_under_each_run_it_leaves_out() {
    let project = Project::cart("merge-moved");
    project.write(
        "test/more.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { shipping } from \"../src/cart.js\";\n\ntest(\"more\", () => {\n  assert.equal(shipping(10, true), 15);\n});\n",
    );
    project.write("src/util.js", "export const util = 1;\n");
    project.commit("more");
    // A script that measures suites one by one is told each run's id.
    let id_file = project.root.join("first-run-id");
    let first = project.supercov_with(
        &["--", "node", "--test", "test/cart.test.js"],
        &[("SUPERCOV_RUN_ID_FILE", id_file.to_str().unwrap())],
    );
    assert_eq!(first.code(), 0, "{}", first.stderr());
    assert_eq!(
        project.read("first-run-id").trim(),
        project.latest(),
        "the id written is the run published"
    );
    // A file it cannot write to costs the id, not the run.
    let unwritable = project.root.join("missing/run-id");
    let second = project.supercov_with(
        &["--", "node", "--test", "test/more.test.js"],
        &[("SUPERCOV_RUN_ID_FILE", unwritable.to_str().unwrap())],
    );
    assert_eq!(second.code(), 0, "{}", second.stderr());
    assert!(
        second.stderr().contains("could not write the run id to"),
        "{}",
        second.stderr()
    );

    let left_out = |because: &str| {
        let refused = project.supercov(&["merge"]);
        assert_eq!(refused.code(), 2, "{}", refused.stdout());
        let said = format!("{}{}", refused.stdout(), refused.stderr());
        contains_all(
            &said,
            &[
                &format!("(node --test test/cart.test.js): {because}"),
                &format!("(node --test test/more.test.js): {because}"),
            ],
        );
    };
    // A measured file added is counted, one removed is named, and with
    // every one removed there is no source left to compare.
    project.write("src/extra.js", "export const extra = 1;\n");
    left_out("the source has changed (1 file(s) added)");
    std::fs::remove_file(project.root.join("src/extra.js")).unwrap();
    std::fs::remove_file(project.root.join("src/util.js")).unwrap();
    left_out("the source has changed (src/util.js)");
    let cart = project.read("src/cart.js");
    std::fs::remove_file(project.root.join("src/cart.js")).unwrap();
    left_out("its source cannot be measured as it is now");
    project.write("src/cart.js", &cart);
    project.write("src/util.js", "export const util = 1;\n");

    let manifest = project.read("package.json");
    project.write(
        "package.json",
        &manifest.replacen(
            "\"private\": true,",
            "\"private\": true, \"dependencies\": { \"left-pad\": \"1.3.0\" },",
            1,
        ),
    );
    left_out("the dependencies have changed");
    project.write("package.json", &manifest);

    // A configuration file added may change how the tests run; told it does
    // not, merge counts both runs.
    project.write("test.config.mjs", "export default {};\n");
    left_out("a configuration file was added");
    let merged = project.supercov(&["merge", "--allow-changed"]).succeeds();
    assert!(merged.contains("merging 2 runs"), "{merged}");
}

#[test]
fn a_floor_on_what_a_run_has_none_of_cannot_pass() {
    let project = Project::empty("nothing-eligible");
    project.write(
        "package.json",
        r#"{ "name": "flat", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/add.js",
        "export function add(a, b) {\n  return a + b;\n}\n",
    );
    project.write(
        "test/add.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { add } from \"../src/add.js\";\ntest(\"add\", () => assert.equal(add(1, 2), 3));\n",
    );
    project.git(&["init", "-q"]);
    project.measure(&[]);
    // No branch anywhere is not every branch taken.
    let refused = project
        .supercov(&["runs", "check", "--min-branches", "80"])
        .exits(2);
    contains_all(
        &refused,
        &[
            "branches         n/a  0/0  (floor 80%)",
            "branches has nothing eligible in this run, which is not the same as complete",
        ],
    );

    // Per file, a file with none is passed over and the rest are judged.
    project.write(
        "src/sign.js",
        "export function sign(n) {\n  if (n > 0) {\n    return 1;\n  }\n  return -1;\n}\n",
    );
    project.write(
        "test/sign.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { sign } from \"../src/sign.js\";\ntest(\"sign\", () => assert.equal(sign(1), 1));\n",
    );
    project.measure(&[]);
    let failed = project
        .supercov(&["runs", "check", "--min-branches", "80", "--per-file"])
        .exits(1);
    contains_all(
        &failed,
        &[
            "src/sign.js: branches 50.00%  1/2 below 80%",
            "uncovered lines: 5\n",
        ],
    );
    assert!(!failed.contains("src/add.js"), "{failed}");
}

#[test]
fn the_latest_run_answers_patch_and_report_without_its_id() {
    let project = Project::empty("latest-patch");
    project.write(
        "package.json",
        r#"{ "name": "latest", "private": true, "type": "module" }"#,
    );
    let sign = "export function sign(n) {\n  if (n > 0) {\n    return 1;\n  }\n  return -1;\n}\n";
    project.write("src/sign.js", sign);
    project.write(
        "test/sign.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { sign } from \"../src/sign.js\";\ntest(\"sign\", () => {\n  assert.equal(sign(1), 1);\n  assert.equal(sign(-1), -1);\n});\n",
    );
    project.git(&["init", "-q"]);
    project.commit("base");

    // A comment changes nothing a floor could judge.
    project.write("src/sign.js", &format!("// Which side of zero.\n{sign}"));
    project.measure(&[]);
    let quiet = project
        .supercov(&["runs", "patch", "--base", "HEAD", "--min-lines", "90"])
        .succeeds();
    contains_all(
        &quiet,
        &[
            "No executable changes.",
            "PASS: nothing executable changed, so 90% does not apply.",
        ],
    );

    // A block no test enters reads as one range.
    project.write(
        "src/sign.js",
        "export function sign(n) {\n  if (n > 0) {\n    return 1;\n  }\n  if (n < -100) {\n    const big = n * 2;\n    return big;\n  }\n  return -1;\n}\n",
    );
    project.measure(&[]);
    let patch = project
        .supercov(&["runs", "patch", "--base", "HEAD"])
        .succeeds();
    contains_all(
        &patch,
        &[
            "1 of 3 changed executable line(s) covered (33.33%)",
            "src/sign.js: 6-7\n",
        ],
    );

    // An HTML report goes to the file named, or into the directory named.
    project
        .supercov(&[
            "runs",
            "report",
            "--format",
            "html",
            "--output",
            "out/sign.html",
        ])
        .succeeds();
    project
        .supercov(&["runs", "report", "--format", "html", "--output", "out/site"])
        .succeeds();
    for file in ["out/sign.html", "out/site/index.html"] {
        assert!(project.read(file).contains("src/sign.js"), "{file}");
    }
}

#[test]
fn a_decision_is_found_again_by_the_location_its_line_lists() {
    let project = Project::empty("decision-location");
    project.write(
        "package.json",
        r#"{ "name": "pick", "private": true, "type": "module" }"#,
    );
    let long = format!(
        "  return (a && {name}) || (c && {name}) ? 4 : c || a ? 5 : 6;",
        name = "an_argument_named_at_such_length_that_the_line_it_sits_on_runs_on"
    );
    project.write(
        "src/pick.js",
        &format!(
            "export function pick(a, b, c) {{\n  return a && b ? 1 : (c || a) ? 2 : 3;\n}}\n\nexport function long(a, c, an_argument_named_at_such_length_that_the_line_it_sits_on_runs_on) {{\n{long}\n}}\n"
        ),
    );
    project.write(
        "test/pick.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { pick, long } from \"../src/pick.js\";\ntest(\"pick\", () => {\n  assert.equal(pick(1, 1, 0), 1);\n  assert.equal(long(0, 0, 1), 6);\n});\n",
    );
    project.git(&["init", "-q"]);
    project.measure(&[]);

    // A line of two decisions lists both, each at its own column.
    let listed = project
        .supercov(&["runs", "latest", "decision", "src/pick.js:2"])
        .succeeds();
    assert!(
        listed.contains("showing 1-2 of 2 matching decisions"),
        "{listed}"
    );
    let location = listed
        .lines()
        .find(|line| line.ends_with("c || a"))
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_else(|| panic!("{listed}"))
        .to_owned();
    // Named by the location listed, it is the one decision.
    let one = project
        .supercov(&["runs", "latest", "decision", &location])
        .succeeds();
    contains_all(&one, &[&location, "c || a", "C1 MISSING: c"]);
    assert!(!one.contains("matching decisions"), "{one}");
    // A gap says what run would close it: the one seen, the condition
    // flipped, the outcome the other way; or that nothing evaluated it.
    let gaps = project
        .supercov(&["runs", "latest", "line", "src/pick.js:2", "--json"])
        .json();
    let needs = gaps["data"]["remaining"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|gap| gap["kind"] == "mcdc")
        .map(|gap| {
            (
                gap["missingCondition"].as_str().unwrap().to_owned(),
                gap["neededVector"].as_str().unwrap_or("").to_owned(),
                gap["needs"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        needs.contains(&(
            "a".into(),
            "F- -> F".into(),
            "`a` false, decision false".into()
        )),
        "{needs:?}"
    );
    // No test evaluated `c`: both runs of the pair that would show it.
    assert!(
        needs.contains(&(
            "c".into(),
            "FF -> F and T- -> T".into(),
            "`c` false, `a` false, decision false; and `c` true, decision true".into()
        )),
        "{needs:?}"
    );
    for missing in ["src/pick.js:2:1", "src/pick.js:3"] {
        let refused = project
            .supercov(&["runs", "latest", "decision", missing])
            .exits(2);
        assert!(refused.contains("Decision not found"), "{refused}");
    }

    // The file's gaps quote a decision too long to show whole, cut short.
    let file = project
        .supercov(&["runs", "latest", "file", "src/pick.js"])
        .succeeds();
    assert!(
        file.lines()
            .any(|line| line.contains("(a && an_argument") && line.ends_with('…')),
        "{file}"
    );
}

#[cfg(unix)]
#[test]
fn a_workspace_package_runs_with_its_own_node_modules_and_tools() {
    let project = Project::empty("monorepo");
    project.write(
        "package.json",
        r#"{ "name": "mono", "private": true, "workspaces": ["packages/*"] }"#,
    );
    project.write(".gitignore", "node_modules\n");
    // The root holds one version of a dependency, the package another; a
    // pnpm-style package is a link into the root's store; the package's own
    // tool is in its own `.bin`.
    project.write(
        "node_modules/dep/package.json",
        r#"{ "name": "dep", "version": "1.0.0", "main": "index.js" }"#,
    );
    project.write(
        "node_modules/dep/index.js",
        "module.exports = { version: 1 };\n",
    );
    project.write(
        "packages/app/node_modules/dep/package.json",
        r#"{ "name": "dep", "version": "2.0.0", "main": "index.js" }"#,
    );
    project.write(
        "packages/app/node_modules/dep/index.js",
        "module.exports = { version: 2 };\n",
    );
    let store = "node_modules/.pnpm/left@3.0.0/node_modules/left";
    project.write(
        &format!("{store}/package.json"),
        r#"{ "name": "left", "version": "3.0.0", "main": "index.js" }"#,
    );
    project.write(
        &format!("{store}/index.js"),
        "module.exports = (s) => \" \" + s;\n",
    );
    std::os::unix::fs::symlink(
        format!("../../../{store}"),
        project.root.join("packages/app/node_modules/left"),
    )
    .unwrap();
    project.write_script(
        "packages/app/node_modules/.bin/apptool",
        "#!/usr/bin/env node\nconsole.log(\"the package's own tool ran\");\n",
    );
    project.write(
        "packages/app/package.json",
        r#"{ "name": "app", "private": true, "scripts": { "test": "apptool && node --test" } }"#,
    );
    project.write(
        "packages/app/src/label.js",
        "const dep = require(\"dep\");\nconst left = require(\"left\");\nexports.label = (n) => (dep.version === 2 && n > 0 ? left(\"v2\") : \"other\");\n",
    );
    project.write(
        "packages/app/test/label.test.js",
        "const test = require(\"node:test\");\nconst assert = require(\"node:assert\");\nconst { label } = require(\"../src/label.js\");\ntest(\"label\", () => {\n  assert.equal(label(1), \" v2\");\n  assert.equal(label(0), \"other\");\n});\n",
    );
    project.git(&["init", "-q"]);
    let ran = project.supercov(&["--", "npm", "test", "-w", "packages/app"]);
    let said = format!("{}{}", ran.stdout(), ran.stderr());
    assert_eq!(ran.code(), 0, "{said}");
    assert!(said.contains("the package's own tool ran"), "{said}");
    let line = project
        .supercov(&["runs", "latest", "line", "packages/app/src/label.js:3"])
        .succeeds();
    // Measured and run: the test's " v2" was only reachable through the
    // package's own `dep` and the linked `left`.
    assert!(
        line.contains("dep.version === 2") && !line.contains("NOT COVERED"),
        "{line}"
    );
    // The links in the project are as they were.
    assert!(
        std::fs::symlink_metadata(project.root.join("packages/app/node_modules/left"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// `from` and everything under it, copied to `to`.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Homebrew's Go is not on a non-login shell's PATH on macOS.
fn go() -> Option<&'static str> {
    ["go", "/opt/homebrew/bin/go", "/usr/local/go/bin/go"]
        .into_iter()
        .find(|go| {
            std::process::Command::new(go)
                .arg("version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
}

#[test]
fn a_file_a_test_deletes_in_the_copy_is_named_and_kept() {
    let Some(go) = go() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let project = Project::empty("go-deletes");
    project.write("go.mod", "module example.com/deletes\n\ngo 1.22\n");
    project.write("stale.txt", "left from a run before\n");
    project.write(
        "clean.go",
        "package deletes\n\nimport \"os\"\n\nfunc Clean(path string) bool {\n\tif err := os.Remove(path); err != nil {\n\t\treturn false\n\t}\n\treturn true\n}\n",
    );
    project.write(
        "clean_test.go",
        "package deletes\n\nimport \"testing\"\n\nfunc TestClean(t *testing.T) {\n\tif !Clean(\"stale.txt\") || Clean(\"stale.txt\") {\n\t\tt.Fatal(\"clean\")\n\t}\n}\n",
    );
    project.git(&["init", "-q"]);
    let ran = project.supercov(&["--", go, "test", "./..."]);
    let said = format!("{}{}", ran.stdout(), ran.stderr());
    assert_eq!(ran.code(), 0, "{said}");
    assert!(
        said.contains("the command deleted 1 file(s) in the isolated workspace; deletions are not propagated to the project: stale.txt"),
        "{said}"
    );
    assert_eq!(project.read("stale.txt"), "left from a run before\n");
}

#[test]
fn assertions_reached_through_require_a_shebang_or_types_are_credited() {
    let project = Project::empty("assert-forms");
    project.write("package.json", r#"{ "name": "forms", "private": true }"#);
    project.write(
        "src/lib.js",
        "exports.label = (v) => (v ? \"on:\" + v : \"off\");\n",
    );
    project.write(
        "src/item.ts",
        "export interface Item { name: string }\nexport const named = (item: Item): string => (item.name ? item.name : \"anonymous\");\n",
    );
    project.write(
        "test/strict.test.js",
        "const test = require(\"node:test\");\nconst assert = require(\"assert\").strict;\nconst { label } = require(\"../src/lib.js\");\ntest(\"strict property\", () => {\n  assert.equal(label(\"x\"), \"on:x\");\n});\n",
    );
    project.write(
        "test/destructured.test.js",
        "const test = require(\"node:test\");\nconst { strictEqual, notEqual: differs } = require(\"node:assert\");\nconst { label } = require(\"../src/lib.js\");\ntest(\"destructured\", () => {\n  strictEqual(label(\"\"), \"off\");\n  differs(label(1), \"off\");\n});\n",
    );
    project.write(
        "test/shebang.test.js",
        "#!/usr/bin/env node\nconst test = require(\"node:test\");\nconst assert = require(\"node:assert\");\nconst { label } = require(\"../src/lib.js\");\ntest(\"shebang\", () => {\n  assert.ok(label(2));\n});\n",
    );
    project.write(
        "test/typed.test.ts",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport type { Item } from \"../src/item.ts\";\nimport { named } from \"../src/item.ts\";\ntest(\"typed\", () => {\n  const item: Item = { name: \"\" };\n  assert.equal(named(item), \"anonymous\");\n  assert.equal(named({ name: \"b\" }), \"b\");\n});\n",
    );
    project.git(&["init", "-q"]);
    let measured = project.supercov(&["--", "node", "--test"]);
    assert_eq!(measured.code(), 0, "{}", measured.stderr());
    assert!(
        measured
            .stderr()
            .contains("attributed 6 native node:assert call(s)"),
        "{}",
        measured.stderr()
    );
    for (test, assertions) in [
        (
            "strict property",
            &["node:assert/strict.equal at test/strict.test.js:5:3"][..],
        ),
        (
            "destructured",
            &[
                "node:assert.strictEqual at test/destructured.test.js:5:3",
                "node:assert.notEqual at test/destructured.test.js:6:3",
            ],
        ),
        ("shebang", &["node:assert.ok at test/shebang.test.js:6:3"]),
        (
            "typed",
            &[
                "node:assert.equal at test/typed.test.ts:7:3",
                "node:assert.equal at test/typed.test.ts:8:3",
            ],
        ),
    ] {
        let read = project
            .supercov(&["runs", "latest", "test", test])
            .succeeds();
        contains_all(&read, assertions);
    }
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &["Lines      100.00% (2/2)", "Branches   100.00% (4/4)"],
    );

    // A test file that does not parse runs as written, fails as Node fails
    // it, and the run cannot pass a gate.
    project.write(
        "test/broken.test.js",
        "const test = require(\"node:test\");\nconst assert = require(\"node:assert\");\ntest(\"broken\", () => {\n  assert.ok(1;\n});\n",
    );
    let broken = project.supercov(&["--", "node", "--test"]);
    assert_eq!(broken.code(), 1, "{}", broken.stderr());
    assert!(
        broken.stdout().contains("SyntaxError"),
        "{}",
        broken.stdout()
    );
    let refused = project
        .supercov(&["runs", "latest", "check", "--min-lines", "50"])
        .exits(2);
    assert!(
        refused.contains("the wrapped test command did not pass"),
        "{refused}"
    );
}

#[test]
fn many_lines_are_answered_in_one_call_and_every_gap_says_what_is_missing() {
    let project = Project::empty("line-batch");
    project.write(
        "package.json",
        r#"{ "name": "ship", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/ship.js",
        "export function ship(sum, express, member) {\n  if (express || sum < 50) {\n    return express ? 15 : 5;\n  }\n  if (member && sum > 100) {\n    return 0;\n  }\n  return 2;\n}\n",
    );
    project.write(
        "test/ship.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { ship } from \"../src/ship.js\";\ntest(\"express\", () => assert.equal(ship(10, true, false), 15));\ntest(\"member\", () => assert.equal(ship(200, false, true), 0));\n",
    );
    project.git(&["init", "-q"]);
    project.measure(&[]);

    // Each missing condition names the value it never took, in every view.
    let decisions = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/ship.js",
            "--group",
            "decision",
        ])
        .succeeds();
    contains_all(
        &decisions,
        &[
            "`sum < 50` was never true; needs `express` false, `sum < 50` true, decision true",
            "`member` was never false; needs `member` false, decision false",
        ],
    );
    let rows = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/ship.js",
            "--group",
            "decision",
            "--json",
        ])
        .json();
    let first = &rows["data"]["decisions"][0];
    assert_eq!(first["missing"][0]["condition"], "sum < 50", "{rows}");
    assert_eq!(
        first["missing"][0]["missingOutcomes"],
        serde_json::json!(["true"]),
        "{rows}"
    );
    let decision = project
        .supercov(&["runs", "latest", "decision", "src/ship.js:2"])
        .succeeds();
    assert!(
        decision.contains("C2 MISSING: sum < 50 (never true)"),
        "{decision}"
    );

    // Several lines, named or listed in a file, from one opened run.
    let both = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/ship.js:2",
            "src/ship.js:5",
            "--json",
        ])
        .json();
    assert_eq!(both["command"], "coverage.lines", "{both}");
    let lines = both["data"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "{both}");
    assert_eq!(lines[1]["location"]["line"], 5);
    let gaps = lines[1]["remaining"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|gap| gap["kind"] == "mcdc")
        .map(|gap| {
            (
                gap["missingCondition"].as_str().unwrap().to_owned(),
                gap["missingOutcomes"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        gaps,
        [
            ("member".to_owned(), serde_json::json!(["false"])),
            ("sum > 100".to_owned(), serde_json::json!(["false"])),
        ]
    );
    // One line answers as it always did.
    let one = project
        .supercov(&["runs", "latest", "line", "src/ship.js:2", "--json"])
        .json();
    assert_eq!(one["command"], "coverage.line", "{one}");
    project.write("lines.txt", "src/ship.js:2\n# skipped\n\nsrc/ship.js:5\n");
    let listed = project
        .supercov(&["runs", "latest", "line", "--from-file", "lines.txt"])
        .succeeds();
    contains_all(
        &listed,
        &[
            "src/ship.js:2",
            "src/ship.js:5",
            "`sum > 100` was never false",
        ],
    );

    // More than one reply holds: a page of the lines that fit, the rest by
    // name, and `nextOffset` for a pager. A location asked twice is one.
    let mut source = String::new();
    for value in 0..200 {
        source.push_str(&format!("export const v{value} = {value};\n"));
    }
    project.write("src/many.js", &source);
    project.write(
        "test/many.test.js",
        "import test from \"node:test\";\nimport \"../src/many.js\";\ntest(\"loads\", () => {});\n",
    );
    let many_run = project.measure(&["test/many.test.js"]);
    let mut locations = (1..=200)
        .map(|line| format!("src/many.js:{line}"))
        .collect::<Vec<_>>();
    locations.push("src/many.js:1".into());
    let mut answered = Vec::new();
    let mut offset = 0;
    loop {
        let mut arguments = vec!["runs".to_owned(), many_run.clone(), "line".into()];
        arguments.extend(locations.iter().cloned());
        arguments.extend(["--json".into(), "--offset".into(), offset.to_string()]);
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        let page = project.supercov(&arguments).json();
        assert_eq!(page["ok"], true, "{page}");
        assert_eq!(page["pagination"]["total"], 200, "{page}");
        for line in page["data"]["lines"].as_array().unwrap() {
            answered.push(line["location"]["line"].as_u64().unwrap());
        }
        match page["pagination"]["nextOffset"].as_u64() {
            Some(next) => {
                assert!(
                    page["data"]["remaining"]
                        .as_array()
                        .is_some_and(|rest| !rest.is_empty())
                );
                offset = next as usize;
            }
            None => break,
        }
    }
    assert!(offset > 0, "200 lines take more than one reply");
    assert_eq!(answered, (1..=200).collect::<Vec<_>>());
}

#[test]
fn a_measured_run_reads_back_for_some_of_its_tests_by_tag_title_or_file() {
    let project = Project::empty("test-filters");
    project.write(
        "package.json",
        r#"{ "name": "shop", "private": true, "type": "module" }"#,
    );
    project.write(
        "src/shop.js",
        "export function price(sum, member) {\n  if (member && sum > 100) {\n    return sum * 0.9;\n  }\n  return sum;\n}\nexport function label(sum) {\n  return sum > 50 ? \"big\" : \"small\";\n}\n",
    );
    project.write(
        "test/api/price.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { price } from \"../../src/shop.js\";\ntest(\"member discount @api\", () => assert.equal(price(200, true), 180));\n",
    );
    project.write(
        "test/ui/label.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { label } from \"../../src/shop.js\";\ntest(\"big label @ui @e2e\", () => assert.equal(label(60), \"big\"));\n",
    );
    project.git(&["init", "-q"]);
    let run = project.measure(&["test/api/price.test.js", "test/ui/label.test.js"]);

    // A tag naming a test level is the test's kind; the others stay tags.
    let kinds = project.supercov(&["runs", &run, "kinds"]).succeeds();
    contains_all(&kinds, &["e2e  1 test(s)", "unit  1 test(s)"]);
    let detail = project
        .supercov(&["runs", &run, "test", "big label"])
        .succeeds();
    assert!(detail.contains("node:test e2e; tags @ui @e2e"), "{detail}");

    let lines = |arguments: &[&str]| {
        let mut all = vec!["runs", run.as_str(), "--json"];
        all.extend_from_slice(arguments);
        let summary = project.supercov(&all).json();
        let lines = &summary["data"]["coverage"]["lines"];
        (
            lines["covered"].as_u64().unwrap(),
            lines["total"].as_u64().unwrap(),
        )
    };
    assert_eq!(lines(&[]), (5, 6));
    // The price test runs the discount; the label test the ternary.
    assert_eq!(lines(&["--tag", "@api"]), (3, 6));
    assert_eq!(lines(&["--tag", "API"]), (3, 6));
    assert_eq!(lines(&["--title", "big label"]), (2, 6));
    assert_eq!(lines(&["--test-file", "test/ui"]), (2, 6));
    assert_eq!(lines(&["--test-file", "test/ui/label.test.js"]), (2, 6));
    // Any of one criterion; every criterion given.
    assert_eq!(lines(&["--tag", "api", "--tag", "ui"]), (5, 6));
    assert_eq!(lines(&["--tag", "api", "--test-file", "test/api"]), (3, 6));
    // A leading `!` leaves tests out: everything but the API tests.
    assert_eq!(lines(&["--tag", "!api"]), (2, 6));
    assert_eq!(lines(&["--test-file", "!test/api"]), (2, 6));
    let neither = project
        .supercov(&["runs", &run, "--tag", "!api", "--tag", "!e2e", "--json"])
        .json();
    assert_eq!(neither["error"]["code"], "TEST_FILTER_EMPTY", "{neither}");
    let none = project
        .supercov(&[
            "runs",
            &run,
            "--tag",
            "api",
            "--test-file",
            "test/ui",
            "--json",
        ])
        .json();
    assert_eq!(none["error"]["code"], "TEST_FILTER_EMPTY", "{none}");
    assert_eq!(
        none["error"]["message"],
        "No tests match --tag api --test-file test/ui"
    );
    // A query below the summary says the same.
    let files = project
        .supercov(&["runs", &run, "files", "--tag", "nightly", "--json"])
        .json();
    assert_eq!(files["error"]["code"], "TEST_FILTER_EMPTY", "{files}");
    assert_eq!(files["error"]["message"], "No tests match --tag nightly");
    let plain = project
        .supercov(&["runs", &run, "gaps", "--title", "never named"])
        .exits(2);
    assert!(
        plain.contains("No tests match --title never named"),
        "{plain}"
    );

    // Every query reads the same tests, and says which.
    let summary = project
        .supercov(&["runs", &run, "--test-file", "test/api"])
        .succeeds();
    assert!(summary.contains("(tests in test/api)"), "{summary}");
    let file = project
        .supercov(&["runs", &run, "file", "src/shop.js", "--tag", "ui"])
        .succeeds();
    assert!(file.contains("member && sum > 100"), "{file}");
    let line = project
        .supercov(&[
            "runs",
            &run,
            "line",
            "src/shop.js:3",
            "--tag",
            "ui",
            "--json",
        ])
        .json();
    assert_eq!(line["data"]["covered"], false, "{line}");
    let gaps = project
        .supercov(&["runs", &run, "gaps", "--format", "jsonl", "--tag", "api"])
        .succeeds();
    assert!(gaps.lines().any(|gap| gap.contains("sum > 50")), "{gaps}");

    // Reports too, which a CI step uploads.
    let lcov = project
        .supercov(&["runs", &run, "report", "--format", "lcov", "--tag", "e2e"])
        .succeeds();
    contains_all(&lcov, &["SF:src/shop.js", "LH:2", "LF:6"]);
    let lcov = project
        .supercov(&["runs", &run, "report", "--format", "lcov", "--kind", "unit"])
        .succeeds();
    assert!(lcov.contains("LH:3"), "{lcov}");

    // Gates and the interactive report read the same tests.
    project
        .supercov(&["runs", &run, "check", "--min-lines", "80"])
        .succeeds();
    let gate = project
        .supercov(&["runs", &run, "check", "--min-lines", "80", "--tag", "e2e"])
        .exits(1);
    contains_all(
        &gate,
        &[
            "reading only the tests --tag e2e picks",
            "lines 33.33%  2/6 below 80%",
        ],
    );
    let written = project
        .supercov(&[
            "report",
            &run,
            "--no-open",
            "--tag",
            "api",
            "--output",
            "api.html",
        ])
        .exits(0);
    assert!(written.contains("api.html"), "{written}");
    let refused = project
        .supercov(&["report", &run, "--no-open", "--tag", "nothing"])
        .exits(2);
    assert!(refused.contains("matches --tag nothing"), "{refused}");

    // Worked out once from the stored run, then read back.
    let filtered = |root: &std::path::Path| {
        std::fs::read_dir(root.join(".supercov/runs").join(&run))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("filtered-"))
            .count()
    };
    let before = filtered(&project.root);
    lines(&["--tag", "@API"]);
    assert_eq!(
        filtered(&project.root),
        before,
        "the same filter reads its index"
    );
}

#[cfg(unix)]
#[test]
fn a_playwright_suite_reads_back_by_its_tags() {
    let project = Project::empty("playwright-tags");
    project.write(
        "package.json",
        r#"{ "name": "playwright-tags", "private": true, "type": "module" }"#,
    );
    let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../node_modules");
    std::os::unix::fs::symlink(
        modules.canonicalize().unwrap(),
        project.root.join("node_modules"),
    )
    .unwrap();
    project.write(".gitignore", "node_modules\n");
    project.write(
        "src/ship.js",
        "export const ship = (sum) => (sum > 100 ? 0 : 5);\nexport const label = (sum) => (sum > 50 ? \"big\" : \"small\");\n",
    );
    project.write(
        "playwright.config.js",
        "export default { testDir: \"tests\", reporter: \"line\" };\n",
    );
    // A tag given to the test, and one in the title of its group.
    project.write(
        "tests/ship.spec.js",
        "import { test, expect } from \"@playwright/test\";\nimport { ship, label } from \"../src/ship.js\";\ntest(\"small orders pay\", { tag: \"@api\" }, () => expect(ship(10)).toBe(5));\ntest.describe(\"labels @ui\", () => {\n  test(\"big label\", () => expect(label(60)).toBe(\"big\"));\n});\n",
    );
    project.git(&["init", "-q"]);
    project.commit("playwright tags");
    project
        .supercov(&["--", "npx", "playwright", "test"])
        .succeeds();
    let detail = project
        .supercov(&["runs", "latest", "test", "small orders"])
        .succeeds();
    assert!(detail.contains("playwright e2e; tags @api"), "{detail}");
    // The run declares a background runner, which no picked test ran under.
    for tag in ["@api", "ui"] {
        let summary = project
            .supercov(&["runs", "latest", "--tag", tag, "--json"])
            .json();
        let lines = &summary["data"]["coverage"]["lines"];
        assert_eq!(
            (lines["covered"].as_u64(), lines["total"].as_u64()),
            (Some(1), Some(2)),
            "{tag}: {summary}"
        );
    }
}

#[test]
fn a_stale_run_names_the_files_that_changed_since_it() {
    let project = Project::empty("stale-names");
    project.write(
        "package.json",
        r#"{ "name": "stale-names", "private": true, "type": "module" }"#,
    );
    project.write("src/shop.js", "export const price = (sum) => sum * 2;\n");
    project.write(
        "test/price.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { price } from \"../src/shop.js\";\ntest(\"doubles\", () => assert.equal(price(2), 4));\n",
    );
    project.git(&["init", "-q"]);
    project.commit("start");
    let run = project.measure(&["test/price.test.js"]);
    project.write(
        "test/price.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { price } from \"../src/shop.js\";\ntest(\"doubles\", () => assert.equal(price(3), 6));\n",
    );
    project.write(
        "test/more.test.js",
        "import test from \"node:test\";\ntest(\"more\", () => {});\n",
    );
    let said = project.supercov(&["runs", &run]).exits(0);
    assert!(
        said.contains("changed since the run: test/more.test.js, test/price.test.js"),
        "{said}"
    );
}

/// A page names the command for the next one; that command, run as printed,
/// shows the next page.
#[test]
fn each_page_names_a_next_page_command_that_shows_it() {
    let project = Project::cart("next-page");
    project.supercov(&["--", "node", "--test"]).succeeds();
    for args in [
        &["file", "src/cart.js", "--group", "decision"][..],
        &["test", "total"],
        &["minimize", "--metric", "lines", "--target", "76"],
    ] {
        let first = project
            .supercov(&[&["runs", "latest"][..], args, &["--limit", "1"]].concat())
            .succeeds();
        assert!(first.contains("showing 1-1 of "), "{first}");
        let next = first
            .lines()
            .find_map(|line| line.strip_prefix("next page: supercov "))
            .unwrap_or_else(|| panic!("no next page in\n{first}"));
        let words = next
            .split(' ')
            .map(|word| word.trim_matches('\''))
            .collect::<Vec<_>>();
        assert!(
            words.ends_with(&["--offset", "1", "--limit", "1"]),
            "{next}"
        );
        let second = project.supercov(&words).succeeds();
        assert!(second.contains("showing 2-2 of "), "{next}\n{second}");
        assert!(
            first.contains(&format!("all {} at once", total(&first))),
            "{first}"
        );
    }

    fn total(page: &str) -> String {
        page.split("showing 1-1 of ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .to_owned()
    }
}

#[test]
fn a_limit_of_zero_lists_every_entry_as_the_other_commands_do() {
    let project = Project::cart("limit-zero-everywhere");
    let run = project.measure(&[]);
    let paged = project
        .supercov(&["runs", &run, "files", "--limit", "1", "--json"])
        .succeeds();
    let paged: serde_json::Value = serde_json::from_str(&paged).unwrap();
    let total = paged["pagination"]["total"].as_u64().unwrap();
    let every = project
        .supercov(&["runs", &run, "files", "--limit", "0", "--json"])
        .succeeds();
    let every: serde_json::Value = serde_json::from_str(&every).unwrap();
    let page = &every["pagination"];
    assert_eq!(page["returned"].as_u64(), Some(total), "{every}");
    assert_eq!(
        page["limit"].as_u64(),
        Some(total),
        "the page says how many all was: {every}"
    );
    assert_eq!(page["hasMore"], false, "{every}");
    let refused = project
        .supercov(&["runs", &run, "files", "--limit", "x"])
        .exits(2);
    assert!(refused.contains("0 for all of them"), "{refused}");
}

#[test]
fn import_says_what_is_wrong_with_its_arguments_and_takes_values_inline() {
    let project = Project::cart("import-arguments");
    project.measure(&[]);
    project.write("lcov.info", "SF:src/cart.js\nDA:1,1\nend_of_record\n");

    contains_all(
        &project.supercov(&["import", "--help"]).succeeds(),
        &["Usage: supercov import <tracefile>"],
    );
    contains_all(
        &project.supercov(&["docs", "-h"]).succeeds(),
        &["Usage: supercov docs [topic]"],
    );
    for (arguments, said) in [
        (
            &["import", "lcov.info", "--test"][..],
            "--test needs a value",
        ),
        (&["import", "--bogus"][..], "unknown option --bogus"),
        (
            &["import", "lcov.info", "other.info", "--test", "x"][..],
            "unexpected argument other.info",
        ),
        (
            &["import", "lcov.info"][..],
            "--test <name> or --background",
        ),
        (&["import", "--background"][..], "name the evidence file"),
        (
            &["import", "lcov.info", "--test=  "][..],
            "the imported test needs a name",
        ),
        (
            &["import", "lcov.info", "--test=device", "--status=flaky"][..],
            "--status is passed or failed, not flaky",
        ),
    ] {
        let stderr = project.supercov(arguments).exits(2);
        assert!(stderr.contains(said), "{arguments:?}: {stderr}");
    }
    // Every value given inline, as `--name=value`.
    project
        .supercov(&["import", "lcov.info", "--test=device", "--status=passed"])
        .succeeds();
}

#[test]
fn a_kind_or_runner_no_test_has_is_said_to_match_nothing_by_every_query() {
    let project = Project::cart("filter-matches-nothing");
    project.measure(&[]);
    for query in ["summary", "scope", "gaps", "files", "kinds", "runners"] {
        for (flag, value, said) in [
            ("--kind", "e2e", "No tests match kind=e2e"),
            ("--runner", "jest", "No tests match runner=jest"),
        ] {
            let stderr = project
                .supercov(&["runs", "latest", query, flag, value])
                .exits(2);
            assert!(stderr.contains(said), "{query} {flag} {value}: {stderr}");
            assert!(
                !stderr.contains("invalid coverage index"),
                "{query}: {stderr}"
            );
        }
    }
    contains_all(
        &project
            .supercov(&["runs", "latest", "summary", "--runner", "node:test"])
            .succeeds(),
        &["(runner node:test)"],
    );
}

#[test]
fn a_line_that_is_not_in_the_file_is_refused_not_read_as_unmeasured() {
    let project = Project::cart("line-bounds");
    project.measure(&[]);
    let lines = project.read("src/cart.js").lines().count();
    let past = project
        .supercov(&["runs", "latest", "line", "src/cart.js:9999"])
        .exits(2);
    assert!(
        past.contains(&format!(
            "src/cart.js has {lines} lines; there is no line 9999"
        )),
        "{past}"
    );
    let zero = project
        .supercov(&["runs", "latest", "line", "src/cart.js:0"])
        .exits(2);
    assert!(zero.contains("with lines counted from 1"), "{zero}");
    // The last line is still a line.
    project
        .supercov(&["runs", "latest", "line", &format!("src/cart.js:{lines}")])
        .succeeds();
    // Several lines answer what they can, as before.
    project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:1",
            "src/cart.js:9999",
        ])
        .succeeds();
}

#[test]
fn diff_refuses_a_third_run() {
    let project = Project::cart("diff-extra");
    project.measure(&[]);
    let id = project.latest();
    let said = project.supercov(&["diff", &id, &id, "extra"]).exits(2);
    assert!(said.contains("unexpected argument extra"), "{said}");
    project.supercov(&["diff", &id, &id]).succeeds();
}

#[test]
fn a_command_that_cannot_start_exits_as_a_shell_would() {
    let project = Project::cart("cannot-start");
    let missing = project.supercov(&["--", "no-such-program-xyz"]).exits(127);
    assert!(
        missing.contains("could not spawn no-such-program-xyz"),
        "{missing}"
    );
    #[cfg(unix)]
    {
        project.write("not-executable.sh", "#!/bin/sh\nexit 0\n");
        let denied = project.supercov(&["--", "./not-executable.sh"]).exits(126);
        assert!(denied.contains("could not spawn"), "{denied}");
    }
}

#[test]
fn a_page_past_the_end_says_so() {
    let project = Project::cart("page-past-end");
    project.measure(&[]);
    let gaps = project
        .supercov(&["runs", "latest", "gaps", "--offset", "999999"])
        .succeeds();
    contains_all(
        &gaps,
        &["showing none of 1: --offset 999999 is past the end"],
    );
    contains_all(
        &project.supercov(&["runs", "latest", "gaps"]).succeeds(),
        &["showing 1-1 of 1"],
    );
}

#[test]
fn a_file_the_scope_left_out_says_why_rather_than_not_found() {
    let project = Project::cart("scope-reason");
    project.measure(&[]);
    for args in [
        &["runs", "latest", "file", "test/cart.test.js"][..],
        &["runs", "latest", "line", "test/cart.test.js:3"][..],
    ] {
        let said = project.supercov(args).exits(2);
        assert!(
            said.contains("test/cart.test.js is outside the run's scope: test or fixture source"),
            "{args:?}: {said}"
        );
    }
    // Among several lines, that location says why and the others answer.
    let several = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:1",
            "test/cart.test.js:3",
        ])
        .succeeds();
    assert!(several.contains("outside the run's scope"), "{several}");
    let missing = project
        .supercov(&["runs", "latest", "file", "src/none.js"])
        .exits(2);
    assert!(
        missing.contains("Source file not found: src/none.js"),
        "{missing}"
    );
}

#[test]
fn every_query_flag_is_read_and_a_wrong_one_named() {
    let project = Project::cart("query-flags");
    project.measure(&[]);
    for (args, said) in [
        (
            &["runs", "latest", "gaps", "--offset", "nan"][..],
            "--offset must be a non-negative integer",
        ),
        (
            &["runs", "latest", "gaps", "--offset", "1.5"][..],
            "--offset must be a non-negative integer",
        ),
        (
            &["runs", "latest", "gaps", "--offset", "-1"][..],
            "--offset must be a non-negative integer",
        ),
        (
            &["runs", "latest", "gaps", "--offset", "1e20"][..],
            "--offset must be a non-negative integer",
        ),
        (
            &["runs", "latest", "gaps", "--filter", "bogus"][..],
            "--filter must be all, passed, or failed",
        ),
        (
            &["runs", "latest", "minimize", "--target", "inf"][..],
            "--target must be between 0 and 100",
        ),
        (
            &["runs", "latest", "minimize", "--metric", "nope"][..],
            "--metric must be all, lines",
        ),
        (
            &["runs", "latest", "files", "--sort", "missing"][..],
            "--sort requires --group decision",
        ),
        (
            &["runs", "latest", "files", "--sort", "bogus"][..],
            "--sort must be location or missing",
        ),
        (
            &["runs", "latest", "gaps", "--depth", "2"][..],
            "--depth requires --group dir",
        ),
        (
            &[
                "runs",
                "latest",
                "file",
                "src/cart.js",
                "--group",
                "decision",
                "--metric",
                "lines",
            ][..],
            "--group decision",
        ),
        (
            &["runs", "latest", "line"][..],
            "Usage: supercov runs <run-id> line",
        ),
        (
            &["runs", "latest", "test"][..],
            "Usage: supercov runs <run-id> test",
        ),
    ] {
        let stderr = project.supercov(args).exits(2);
        assert!(stderr.contains(said), "{args:?}: {stderr}");
    }
    contains_all(
        &project
            .supercov(&["runs", "latest", "gaps", "--filter", "passed"])
            .succeeds(),
        &["passed attempts only"],
    );
    for (metric, target, said) in [
        ("statements", "50", "for 50% statements"),
        ("functions", "50", "for 50% functions"),
        ("branches", "50", "for 50% branches"),
        ("mcdc", "10", "for 10% mcdc"),
    ] {
        contains_all(
            &project
                .supercov(&[
                    "runs", "latest", "minimize", "--metric", metric, "--target", target,
                ])
                .succeeds(),
            &[said],
        );
    }
    project
        .supercov(&["runs", "latest", "files", "--sort", "location"])
        .succeeds();
    project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--group",
            "decision",
            "--sort",
            "missing",
        ])
        .succeeds();
    project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--group",
            "decision",
            "--metric",
            "mcdc",
        ])
        .succeeds();
}

#[test]
fn a_diff_keeps_only_what_the_tests_a_filter_picks_covered() {
    let project = Project::cart("diff-filters");
    project.write(
        "test/describe.test.js",
        "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\nimport { describe } from \"../src/cart.js\";\n\ntest(\"describe lists names\", () => {\n  assert.equal(describe([{ name: \"a\" }]), \"a\");\n});\n",
    );
    project.commit("an integration test");
    project
        .supercov(&[
            "--",
            "sh",
            "-c",
            "SUPERCOV_TEST_KIND=unit node --test test/cart.test.js",
        ])
        .succeeds();
    let unit_only = project.latest();
    project
        .supercov(&[
            "--",
            "sh",
            "-c",
            "SUPERCOV_TEST_KIND=unit node --test test/cart.test.js && SUPERCOV_TEST_KIND=integration node --test test/describe.test.js",
        ])
        .succeeds();
    let both = project.latest();
    let gained = |filter: &[&str]| {
        let mut args = vec!["diff", unit_only.as_str(), both.as_str(), "--json"];
        args.extend_from_slice(filter);
        project.supercov(&args).json()["data"]["gained"]["lineCount"]
            .as_u64()
            .unwrap()
    };
    let everything = gained(&[]);
    assert!(everything > 0, "the new test covered new lines");
    // The unit tests are the same in both runs; the integration test is new.
    assert_eq!(gained(&["--kind", "unit"]), 0);
    // Run A has no integration test: all its lines are gained.
    assert!(gained(&["--kind", "integration"]) >= everything);
    assert_eq!(gained(&["--runner", "node:test"]), everything);
    // The same unit tests in both runs cover the same share of the code.
    let unit = project
        .supercov(&["diff", &unit_only, &both, "--kind", "unit", "--json"])
        .json();
    assert_eq!(unit["data"]["delta"]["lines"], 0, "{unit}");
    // A filtered comparison says so.
    let human = project
        .supercov(&["diff", &unit_only, &both, "--kind", "integration"])
        .succeeds();
    assert!(
        human.lines().next().unwrap().contains("integration"),
        "{human}"
    );
    let refused = project
        .supercov(&["diff", &unit_only, &both, "--runner", "jest"])
        .exits(2);
    assert!(refused.contains("No tests match runner=jest"), "{refused}");
}

#[test]
fn tests_affected_lists_files_names_and_why_and_says_when_it_cannot_narrow() {
    let project = Project::cart("affected-forms");
    project.measure(&[]);
    project.edit("src/cart.js", "let sum = 0;", "let sum = 0 + 0;");
    project.edit(
        "test/cart.test.js",
        "total adds prices",
        "total adds all prices",
    );
    assert_eq!(
        project
            .supercov(&["runs", "latest", "tests", "affected", "--files"])
            .succeeds()
            .trim(),
        "test/cart.test.js"
    );
    for narrowing in ["--ran-changed", "--asserting"] {
        let ran = project.supercov(&["runs", "latest", "tests", "affected", narrowing, "--names"]);
        assert_eq!(ran.code(), 0);
        assert!(
            ran.stderr()
                .contains("no assertion assessment; listing every affected test"),
            "{narrowing}"
        );
        assert_eq!(ran.stdout().lines().count(), 3, "{narrowing}");
    }
    let json = project
        .supercov(&["runs", "latest", "tests", "affected", "--json"])
        .json();
    let affected = json["data"]["affected"].as_array().unwrap();
    assert_eq!(affected.len(), 3);
    let reasons = |name: &str| {
        affected.iter().find(|test| test["name"] == name).unwrap()["reasons"]
            .as_array()
            .unwrap()
            .len()
    };
    // The shipping test never ran the changed `total`; the other two did.
    assert_eq!(reasons("shipping is free over fifty"), 1);
    assert_eq!(reasons("total applies a coupon"), 2);
    contains_all(
        &project
            .supercov(&["runs", "latest", "tests", "affected"])
            .succeeds(),
        &[
            "3 of 3 tests affected",
            "test file changed: top level",
            "this test ran it",
        ],
    );
}

#[test]
fn minimize_without_a_target_keeps_what_the_tests_cover() {
    let project = Project::cart("minimize-default");
    project.measure(&[]);
    // The cart's tests cover 76.92% of its lines: no target, no 100% to miss.
    let said = project.supercov(&["runs", "latest", "minimize"]).succeeds();
    contains_all(
        &said,
        &[
            "exact minimum 3/3 test(s) keeping the coverage of all 3 tests in every measured metric",
            "lines 76.92%",
        ],
    );
    contains_all(
        &project
            .supercov(&["runs", "latest", "minimize", "--metric", "lines"])
            .succeeds(),
        &["keeping the lines coverage of all 3 tests", "lines 76.92%"],
    );
    let json = project
        .supercov(&["runs", "latest", "minimize", "--json"])
        .json();
    assert_eq!(json["data"]["target"], serde_json::Value::Null, "{json}");
    // An explicit target is still held to.
    assert!(
        project
            .supercov(&["runs", "latest", "minimize", "--target", "100"])
            .exits(2)
            .contains("target 100% is impossible")
    );
}

#[test]
fn a_branch_lists_its_unrun_outcomes_in_one_order() {
    let project = Project::cart("branch-order");
    project.measure(&[]);
    // Each call is a new process, with its own hash order.
    for _ in 0..8 {
        contains_all(
            &project
                .supercov(&["runs", "latest", "line", "src/cart.js:14"])
                .succeeds(),
            &["Unobserved: true; false"],
        );
    }
}

#[test]
fn covered_unmeasured_and_empty_views_say_which_they_are() {
    let project = Project::cart("empty-views");
    let first = project.measure(&[]);
    for at in 0..2 {
        project.write(
            &format!("src/part{at}.js"),
            &format!("export function part{at}(a) {{\n  return a ? {at} : -{at};\n}}\n"),
        );
        project.write(
            &format!("test/part{at}.test.js"),
            &format!(
                "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ part{at} }} from \"../src/part{at}.js\";\ntest(\"part{at} both\", () => {{\n  assert.equal(part{at}(true), {at});\n  assert.equal(part{at}(false), -{at});\n}});\n"
            ),
        );
    }
    project.commit("parts");
    let second = project.measure(&[]);
    // Every line covered is not "nothing measured".
    let covered = project
        .supercov(&["runs", "latest", "file", "src/part1.js"])
        .succeeds();
    contains_all(
        &covered,
        &["No gap lines: every line is covered and measured."],
    );
    assert!(!covered.contains("Nothing measured"), "{covered}");
    // Everything passed, so no attempt failed.
    for query in ["kinds", "runners"] {
        contains_all(
            &project
                .supercov(&["runs", "latest", query, "--filter", "failed"])
                .succeeds(),
            &[
                "No tests in this view (failed attempts only)",
                "showing 0-0 of 0",
            ],
        );
    }
    // Only gains: no "most lost first" over a list with nothing lost.
    let gained = project.supercov(&["diff", &first, &second]).succeeds();
    contains_all(&gained, &["in 2 files:", "src/part0.js: gained"]);
    assert!(!gained.contains("most lost first"), "{gained}");
    contains_all(
        &project.supercov(&["diff", &second, &first]).succeeds(),
        &["in 2 files, most lost first:", "src/part0.js: lost"],
    );
}

#[test]
fn every_view_names_its_filters_pages_and_notes() {
    let project = Project::cart("view-notes");
    let first = project.measure(&[]);
    for at in 0..12 {
        project.write(
            &format!("src/part{at}.js"),
            &format!("export function part{at}(a) {{\n  return a ? {at} : -{at};\n}}\n"),
        );
        project.write(
            &format!("test/part{at}.test.js"),
            &format!("import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport {{ part{at} }} from \"../src/part{at}.js\";\ntest(\"part{at} both\", () => {{\n  assert.equal(part{at}(true), {at});\n  assert.equal(part{at}(false), -{at});\n}});\n"),
        );
    }
    project.write(
        "test/none.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\ntest(\"asserts alone\", () => {\n  assert.equal(1 + 1, 2);\n});\n",
    );
    project.commit("parts");
    let second = project.measure(&[]);
    let said = |args: &[&str]| project.supercov(args).succeeds();
    // Gains over more files than are listed.
    contains_all(
        &said(&["diff", &first, &second, "--gained"]),
        &[
            "in 12 files:",
            "  ... and 2 more files",
            "+ line src/part0.js:1",
        ],
    );
    // A page of a filtered comparison carries the filters to the next one.
    contains_all(
        &said(&[
            "diff",
            &first,
            &second,
            "--filter",
            "passed",
            "--runner",
            "node:test",
            "--limit",
            "1",
        ]),
        &[
            "(passed attempts only, runner node:test)",
            "showing 1-1 of 24 per category",
            "--filter passed --runner 'node:test' --offset 1 --limit 1",
        ],
    );
    contains_all(
        &said(&["diff", &first, &second, "--kind", "unit", "--limit", "2"]),
        &["(kind unit)", "--kind 'unit' --offset 2 --limit 2"],
    );
    contains_all(
        &said(&["runs", "latest", "tests", "without-evidence"]),
        &[
            "Tests that made assertions and recorded no coverage: 1",
            "asserts alone",
            "1 assertion; its file imports no measured source",
        ],
    );
    for (args, next) in [
        (
            &["runs", "latest", "minimize", "--limit", "2"][..],
            "minimize --offset 2 --limit 2",
        ),
        (
            &["runs", "latest", "scope", "--files", "--limit", "3"][..],
            "scope --files --offset 3 --limit 3",
        ),
        (
            &["runs", "latest", "files", "--limit", "3"][..],
            "files --offset 3 --limit 3",
        ),
    ] {
        contains_all(&said(args), &[next]);
    }
    contains_all(
        &said(&["runs", "latest", "--test-file", "test/part3.test.js"]),
        &["(tests in test/part3.test.js)", "Lines      5.41% (2/37)"],
    );
    contains_all(
        &said(&["runs", "latest", "gaps", "--title", "part"]),
        &[
            "Projection: title containing \"part\"",
            "file --title 'part' 'src/cart.js'",
        ],
    );
    contains_all(
        &said(&["runs", "latest", "line", "src/part3.js:2"]),
        &[
            "COVERED",
            "Test phases",
            "node:assert/strict.equal (passed)",
        ],
    );
    // A run whose command failed says its coverage cannot gate.
    project.write(
        "src/cart.js",
        &common::CART.replace("return 0;", "return 1;"),
    );
    project.supercov(&["--", "node", "--test"]).exits(1);
    contains_all(
        &said(&["runs", "latest"]),
        &[
            "status: wrapped command exited 1 — coverage below is diagnostic and cannot gate",
            "Failed      1",
        ],
    );
    contains_all(
        &said(&["runs", "latest", "decision", "src/cart.js:14"]),
        &[
            "C1 MISSING: express (never evaluated)",
            "confidence unexecuted",
        ],
    );
    contains_all(
        &said(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:14",
            "src/cart.js:21",
            "src/cart.js:2",
        ]),
        &["showing 1-3 of 5 per category", "every test and phase:"],
    );
}

#[test]
fn check_patch_and_affected_tests_answer_or_say_what_is_missing() {
    let project = Project::cart("check-patch-affected");
    project.measure(&[]);
    contains_all(
        &project
            .supercov(&["runs", "latest", "check", "--min-mcdc", "10"])
            .succeeds(),
        &[
            "mcdc          20.00%  1/5  (floor 10%)",
            "PASS: every floor is met.",
        ],
    );
    contains_all(
        &project
            .supercov(&["runs", "latest", "check", "--min-lines", "99"])
            .exits(1),
        &[
            "FAIL: 1 floor(s) not met.",
            "(whole run): lines 76.92%  10/13 below 99%",
        ],
    );
    for (args, said) in [
        (
            &["runs", "latest", "check", "--min-branches", "abc"][..],
            "\"abc\" is not a percentage",
        ),
        (
            &["runs", "latest", "check", "--bogus"][..],
            "unknown option --bogus",
        ),
        (
            &["runs", "latest", "check"][..],
            "no floor requested; give at least one --min-<metric>",
        ),
        (
            &["runs", "run_0000000000000000"][..],
            "Coverage run not found: run_0000000000000000",
        ),
        (&["runs", "latest", "patch"][..], "patch needs --base <ref>"),
        (
            &["runs", "latest", "patch", "--json"][..],
            "patch needs --base <ref>",
        ),
        (
            &["runs", "latest", "patch", "extra"][..],
            "unknown option extra",
        ),
        (
            &["report", "--runs", "0"][..],
            "--runs must be between 1 and 20",
        ),
        (&["report", "--bogus"][..], "Unknown report option: --bogus"),
    ] {
        contains_all(&project.supercov(args).exits(2), &[said]);
    }
    project.write(
        "src/cart.js",
        &format!(
            "{}\nexport function extra() {{ return 1; }}\n",
            common::CART.replace("sum * 100", "sum * 1000")
        ),
    );
    contains_all(
        &project
            .supercov(&["runs", "latest", "tests", "affected"])
            .succeeds(),
        &[
            "3 of 3 tests affected by changes since the run",
            "src/cart.js: top level, total (line 1); added extra (line 23) changed",
            "Changed files:",
        ],
    );
    assert_eq!(
        project
            .supercov(&["runs", "latest", "tests", "affected", "--names"])
            .succeeds(),
        "shipping is free over fifty\ntotal adds prices\ntotal applies a coupon\n"
    );
    assert_eq!(
        project
            .supercov(&["runs", "latest", "tests", "affected", "--files"])
            .succeeds(),
        "test/cart.test.js\n"
    );
}

#[test]
fn a_changed_helper_reaches_the_tests_that_load_it_and_an_unplaced_one_reaches_all() {
    let project = Project::cart("affected-helpers");
    project.write(
        "test/helpers.js",
        "export function sample() {\n  return [{ price: 10, quantity: 2 }];\n}\n",
    );
    project.write("test/unplaced.js", "export const unused = 1;\n");
    project.write(
        "src/extra.js",
        "export function extra() {\n  return 1;\n}\n",
    );
    project.write(
        "test/helped.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\nimport { sample } from \"./helpers.js\";\n// mentions extra by name\ntest(\"helped\", () => {\n  assert.equal(total(sample()), 20);\n});\n",
    );
    project.commit("helpers");
    project.measure(&[]);
    project.write(
        "test/helpers.js",
        "export function sample() {\n  return [{ price: 10, quantity: 3 }];\n}\n",
    );
    project.write("test/unplaced.js", "export const unused = 2;\n");
    project.write(
        "src/extra.js",
        "export function extra() {\n  return 2;\n}\nexport function added() {}\n",
    );
    let said = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &said,
        &[
            "4 of 4 tests affected by changes since the run",
            "test/helpers.js changed: sample (line 1) (this test's file loads it)",
            "test/unplaced.js changed: top level (a file tests may load)",
        ],
    );
    // Only the test that imports the helper is reached through it.
    assert_eq!(said.matches("test/helpers.js changed").count(), 1, "{said}");
    assert_eq!(
        said.matches("test/unplaced.js changed").count(),
        4,
        "{said}"
    );
}

#[test]
fn tests_affected_reads_formatting_other_runners_and_dependencies_apart() {
    let project = Project::cart("affected-shapes");
    project.write("vitest.config.js", "export default {};\n");
    project.commit("config");
    project.measure(&[]);
    let said = |args: &[&str]| project.supercov(args).succeeds();
    contains_all(
        &said(&["runs", "latest", "tests", "affected"]),
        &[
            "0 of 3 tests affected",
            "No captured file has changed.",
            "Working tree: matches the run.",
        ],
    );
    // Asked for an action it does not have, flag or word: refused.
    for action in ["-x", "nope"] {
        contains_all(
            &project
                .supercov(&["runs", "latest", "tests", action])
                .exits(2),
            &["unknown tests action"],
        );
    }
    contains_all(
        &said(&["runs", "latest", "tests", "--help"]),
        &["Usage: supercov runs <run> tests affected"],
    );
    // Another runner's settings and a comment in a test change no test.
    project.write("vitest.config.js", "export default { test: {} };\n");
    project.write(
        "test/cart.test.js",
        &format!("{}\n// touched\n", common::CART_TESTS),
    );
    contains_all(
        &said(&["runs", "latest", "tests", "affected"]),
        &[
            "0 of 3 tests affected",
            "test/cart.test.js  formatting",
            "vitest.config.js  declarations  top level",
        ],
    );
    assert_eq!(
        said(&["runs", "latest", "tests", "affected", "--names"]),
        ""
    );
    // A dependency change reaches every test.
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" }, "dependencies": {"x": "1"} }"#,
    );
    contains_all(
        &said(&["runs", "latest", "tests", "affected"]),
        &[
            "3 of 3 tests affected",
            "dependencies or lockfile changed (every test)",
            "package.json  changed",
        ],
    );
    assert_eq!(
        said(&["runs", "latest", "tests", "affected", "--files"]),
        "test/cart.test.js\n"
    );
}

#[test]
fn a_patch_counts_changed_and_untracked_lines_against_its_base() {
    let project = Project::cart("patch-lines");
    project.write(
        "src/cart.js",
        &format!(
            "{}\nexport function more(x) {{\n  return x ? 1 : 2;\n}}\n",
            common::CART
        ),
    );
    project.write("src/new.js", "export const fresh = 1;\n");
    project.measure(&[]);
    let lines = [
        "0 of 3 changed executable line(s) covered (0.00%)",
        "src/cart.js: 23-24",
        "src/new.js: 1",
    ];
    contains_all(
        &project
            .supercov(&["runs", "latest", "patch", "--base", "HEAD"])
            .succeeds(),
        &lines,
    );
    let json = project
        .supercov(&["runs", "latest", "patch", "--base", "HEAD", "--json"])
        .json();
    assert_eq!(json["data"]["patch"]["eligible"], 3, "{json}");
    contains_all(
        &project
            .supercov(&[
                "runs",
                "latest",
                "patch",
                "--base",
                "HEAD",
                "--min-lines",
                "90",
            ])
            .exits(1),
        &["FAIL: below 90%"],
    );
    let annotated = project
        .supercov(&[
            "runs",
            "latest",
            "patch",
            "--base",
            "HEAD",
            "--annotate",
            "github",
            "--max-annotations",
            "1",
        ])
        .succeeds();
    assert_eq!(annotated.matches("::warning").count(), 1, "{annotated}");
    contains_all(
        &project
            .supercov(&["runs", "latest", "patch", "--base", "nope-ref"])
            .exits(2),
        &["git merge-base nope-ref HEAD failed"],
    );
    // A run of another tree cannot map this one's patch.
    project.write("src/cart.js", common::CART);
    contains_all(
        &project
            .supercov(&["runs", "latest", "patch", "--base", "HEAD"])
            .exits(2),
        &["the run no longer matches the current checkout"],
    );
}

#[test]
fn runs_that_ran_no_tests_keep_the_command_status_and_say_so() {
    let project = Project::cart("odd-js-runs");
    project
        .supercov(&["--", "node", "-e", "process.exit(3)"])
        .exits(3);
    project.supercov(&["--", "node", "-e", ""]).succeeds();
    project
        .supercov(&["--", "node", "--test", "test/none.test.js"])
        .exits(1);
    let listed = project.supercov(&["runs"]).succeeds();
    contains_all(
        &listed,
        &[
            "FAILED (exit 3); NO TESTS RAN",
            "FAILED (exit 1); NO TESTS RAN",
            "  NO TESTS RAN",
        ],
    );
}

/// Pages past the end, views with no tests, files and decisions that are not
/// there, and the narrower views of one run each read as what they are.
#[test]
fn odd_pages_and_empty_views_read_as_what_they_are() {
    let project = Project::cart("odd-views");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let past = project
        .supercov(&["runs", "latest", "files", "--offset", "100"])
        .succeeds();
    assert!(
        past.contains(
            "every included source file\n\nshowing none of 1: --offset 100 is past the end"
        ),
        "{past}"
    );
    let past = project
        .supercov(&["runs", "latest", "gaps", "--offset", "100"])
        .succeeds();
    assert!(
        past.contains("unresolved obligations\n\nshowing none of 1: --offset 100 is past the end"),
        "{past}"
    );
    let runners = project
        .supercov(&["runs", "latest", "runners", "--offset", "5"])
        .succeeds();
    assert!(
        runners.starts_with("showing none of 1: --offset 5 is past the end"),
        "{runners}"
    );
    let kinds = project
        .supercov(&["runs", "latest", "kinds", "--filter", "failed"])
        .succeeds();
    assert!(
        kinds.contains("No tests in this view (failed attempts only)"),
        "{kinds}"
    );
    let directories = project
        .supercov(&["runs", "latest", "files", "--group", "dir"])
        .succeeds();
    assert!(
        directories.contains("src            1     13  76.92%  76.92%"),
        "{directories}"
    );
    let file = project
        .supercov(&["runs", "latest", "file", "src/cart.js", "--limit", "1"])
        .succeeds();
    contains_all(
        &file,
        &[
            "showing 1-1 of 6 gap lines",
            "file 'src/cart.js' --offset 1 --limit 1",
            "all 6 at once",
        ],
    );
    let missing = project
        .supercov(&["runs", "latest", "file", "package.json"])
        .exits(2);
    assert!(
        missing.contains("Source file not found: package.json"),
        "{missing}"
    );
    let decision = project
        .supercov(&["runs", "latest", "decision", "src/cart.js:1"])
        .exits(2);
    assert!(
        decision.contains("Decision not found: src/cart.js:1"),
        "{decision}"
    );
    let line = project
        .supercov(&["runs", "latest", "line", "src/cart.js:1"])
        .succeeds();
    assert!(
        line.contains("Function at column 8 — covered (2 covering tests)"),
        "{line}"
    );
    let scope = project
        .supercov(&["runs", "latest", "scope", "--files", "--limit", "1"])
        .succeeds();
    contains_all(
        &scope,
        &["INCLUDED  src/cart.js", "showing 1-1 of 2", "all 2 at once"],
    );
    let failed = project
        .supercov(&["runs", "latest", "--filter", "failed"])
        .succeeds();
    contains_all(
        &failed,
        &[
            "(failed attempts only)",
            "No test recorded coverage",
            "Total       0",
        ],
    );
    let titled = project
        .supercov(&["runs", "latest", "--title", "nothing-matches"])
        .exits(2);
    assert!(
        titled.contains("No tests match --title nothing-matches"),
        "{titled}"
    );
    let without = project
        .supercov(&["runs", "latest", "tests", "without-evidence"])
        .succeeds();
    assert!(without.contains("recorded no coverage: 0"), "{without}");
    let source = project
        .supercov(&["runs", "latest", "source", "src/cart.js", "--limit", "2"])
        .succeeds();
    contains_all(
        &source,
        &[
            " 2 │   let sum = 0;",
            "Showing 1-2 of 21",
            "--offset 2 --limit 2",
        ],
    );
}

/// The run list pages with its filter and marks a run with no exit status;
/// a line with no code, one that never ran and one past the end each say
/// so; a test that asserted without reaching measured source is listed with
/// why, and counts read in the singular where there is one.
#[test]
fn run_lists_lines_and_tests_read_as_what_they_are() {
    let project = Project::cart("more-views");
    project.write(
        "test/none.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\n\ntest(\"asserts nothing measured\", () => {\n  assert.ok(true);\n});\n",
    );
    project.commit("none");
    project.supercov(&["--", "node", "--test"]).succeeds();
    project.supercov(&["--", "node", "--test"]).succeeds();
    let listed = project
        .supercov(&["runs", "--filter", "passed", "--limit", "1"])
        .succeeds();
    contains_all(
        &listed,
        &[
            "showing 1-1 of 2",
            "runs --filter passed --offset 1 --limit 1",
            "all 2 at once",
        ],
    );
    let blank = project
        .supercov(&["runs", "latest", "line", "src/cart.js:11"])
        .succeeds();
    contains_all(
        &blank,
        &[
            "NOT MEASURED",
            "Nothing on this line is code the run measures",
        ],
    );
    let unrun = project
        .supercov(&["runs", "latest", "line", "src/cart.js:20"])
        .succeeds();
    contains_all(
        &unrun,
        &[
            "NOT COVERED",
            "function not called",
            "Function at column 20",
        ],
    );
    let beyond = project
        .supercov(&["runs", "latest", "line", "src/cart.js:40"])
        .exits(2);
    assert!(
        beyond.contains("src/cart.js has 21 lines; there is no line 40"),
        "{beyond}"
    );
    let without = project
        .supercov(&["runs", "latest", "tests", "without-evidence"])
        .succeeds();
    assert!(
        without.contains("test/none.test.js > asserts nothing measured [")
            && without.contains("passed, 1 assertion; its file imports no measured source"),
        "{without}"
    );
    let past = project
        .supercov(&[
            "runs",
            "latest",
            "tests",
            "without-evidence",
            "--offset",
            "1",
        ])
        .succeeds();
    assert!(
        past.contains("recorded no coverage: 1\n\nshowing none of 1: --offset 1 is past the end"),
        "{past}"
    );
    let test = project
        .supercov(&["runs", "latest", "test", "asserts nothing measured"])
        .succeeds();
    contains_all(
        &test,
        &[
            "0 lines, 0 hits, 0 decisions, 1 phase\n",
            "node:assert/strict.ok at test/none.test.js:5:3",
        ],
    );
    let unknown = project
        .supercov(&["runs", "latest", "test", "nonexistent"])
        .exits(2);
    assert!(unknown.contains("Test not found: nonexistent"), "{unknown}");
    let minimum = project
        .supercov(&["runs", "latest", "minimize", "--limit", "1"])
        .succeeds();
    contains_all(
        &minimum,
        &[
            "exact minimum 3/4 test(s)",
            "showing 1-1 of 3",
            "minimize --offset 1 --limit 1",
        ],
    );
    let impossible = project
        .supercov(&["runs", "latest", "minimize", "--target", "50"])
        .exits(2);
    assert!(
        impossible.contains("target 50% is impossible"),
        "{impossible}"
    );
    let run = project.latest();
    let path = project
        .root
        .join(".supercov/runs")
        .join(&run)
        .join("run.json");
    let mut metadata: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    metadata["testExitCode"] = serde_json::Value::Null;
    std::fs::write(&path, metadata.to_string()).unwrap();
    let listed = project.supercov(&["runs"]).succeeds();
    assert!(
        listed.contains("INVALID (exit status unavailable)"),
        "{listed}"
    );
}

/// A diff counts in the singular where there is one and says when nothing
/// differs; a report that fails writes no progress line to a pipe; and the
/// mistakes `diff`, `merge`, `import` and `report` can be handed are named.
#[test]
fn comparisons_and_their_mistakes_read_as_what_they_are() {
    let project = Project::cart("compare-views");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let first = project.latest();
    project.edit(
        "test/cart.test.js",
        "assert.equal(shipping(60, false), 0);",
        "assert.equal(shipping(60, false), 0);\n  assert.equal(shipping(10, true), 15);",
    );
    project.commit("express");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let second = project.latest();
    let gained = project.supercov(&["diff", &first, &second]).succeeds();
    contains_all(
        &gained,
        &[
            "gained: 1 line, 2 branches, 1 MC/DC condition\n",
            "lost: 0 lines, 0 branches, 0 MC/DC conditions\n",
            "+ MC/DC src/cart.js:13 C1 express",
        ],
    );
    let ran = project.supercov(&["diff", &first, &first]);
    assert_eq!(
        ran.stderr().matches("stale run").count(),
        1,
        "{}",
        ran.stderr()
    );
    let same = ran.succeeds();
    assert!(
        same.contains("Nothing was gained or lost.\n") && !same.contains("showing"),
        "{same}"
    );
    let half = project.supercov(&["diff", &first]).exits(2);
    assert!(
        half.contains("Usage: supercov diff <older-run> <newer-run>"),
        "{half}"
    );
    let unknown = project.supercov(&["diff", &first, "nope"]).exits(2);
    assert!(
        unknown.contains("Coverage run not found: nope"),
        "{unknown}"
    );
    let alone = project.supercov(&["merge"]).succeeds();
    assert!(alone.contains("nothing to merge it with"), "{alone}");
    let changed = project.supercov(&["merge", &first, &second]).exits(2);
    assert!(
        changed.contains("a file of its own tests, has changed"),
        "{changed}"
    );
    let twice = project.supercov(&["merge", &first, &first]).exits(2);
    assert!(twice.contains("Each merged run must be unique"), "{twice}");
    let keep = project.supercov(&["runs", "clean", "--keep", "x"]).exits(2);
    assert!(
        keep.contains("--keep must be a non-negative integer"),
        "{keep}"
    );
    let report = project.supercov(&["report", "nope"]);
    assert_eq!(report.code(), 2);
    assert_eq!(
        report.stderr(),
        "[supercov] report failed: coverage run not found: nope\n"
    );
    let unread = project
        .supercov(&["import", "missing.info", "--test", "t"])
        .exits(2);
    assert!(unread.contains("Cannot read missing.info"), "{unread}");
    project.write(
        "cov.info",
        "SF:src/cart.js\nDA:20,1\nend_of_record\nSF:/elsewhere/x.js\nDA:1,1\nend_of_record\n",
    );
    let unnamed = project.supercov(&["import", "cov.info"]).exits(2);
    assert!(
        unnamed.contains("--test <name> or --background"),
        "{unnamed}"
    );
    let status = project
        .supercov(&[
            "import", "cov.info", "--test", "outside", "--status", "weird",
        ])
        .exits(2);
    assert!(
        status.contains("--status is passed or failed, not weird"),
        "{status}"
    );
    let imported = project.supercov(&["import", "cov.info", "--test", "outside"]);
    assert_eq!(imported.code(), 0, "{}", imported.stderr());
    contains_all(
        &format!("{}{}", imported.stdout(), imported.stderr()),
        &[
            "imported 1 line(s) that ran",
            "left out 1 file(s) of the evidence: /elsewhere/x.js (outside the project)",
        ],
    );
}

/// A word or a flag a command does not take is refused by name instead of
/// being read as a run id or a filter that is not one.
#[test]
fn words_and_flags_a_command_does_not_take_are_refused() {
    let project = Project::cart("refused-words");
    project.supercov(&["--", "node", "--test"]).succeeds();
    for (args, expected) in [
        (vec!["runs", "latest", "files", "-q"], "Unknown option: -q"),
        (
            vec!["runs", "latest", "files", "src/cart.js"],
            "Unexpected argument: src/cart.js",
        ),
        (
            vec!["runs", "latest", "gaps", "x"],
            "Unexpected argument: x",
        ),
        (
            vec!["runs", "latest", "file", "src/cart.js", "extra"],
            "Unexpected argument: extra",
        ),
        (
            vec!["runs", "latest", "decision", "a", "b"],
            "Unexpected argument: b",
        ),
        (
            vec!["runs", "latest", "decision"],
            "Usage: supercov runs <run-id> decision",
        ),
        (
            vec!["runs", "abcdefghijklmnopqrst"],
            "Coverage run not found: abcdefghijklmnopqrst",
        ),
        (vec!["report", "--tag"], "--tag needs a value"),
        (vec!["merge", "--tag", "x"], "Unknown option: --tag"),
    ] {
        let refused = project.supercov(&args).exits(2);
        assert!(refused.contains(expected), "{args:?}: {refused}");
    }
    // A test name may begin with a dash.
    let named = project.supercov(&["runs", "latest", "test", "-x"]).exits(2);
    assert!(named.contains("Test not found: -x"), "{named}");
}

/// An import of nothing the run measures makes no run; a line past a file's
/// end, evidence of no project file, a name already imported and both ways
/// of crediting at once are each refused by what is wrong.
#[test]
fn imports_that_cannot_add_anything_are_refused() {
    let project = Project::cart("import-refusals");
    project.write("lib/extra.js", "export const x = 1;\nexport const y = 2;\n");
    project.commit("extra");
    project.supercov(&["--", "node", "--test"]).succeeds();
    project.write(
        "excluded.info",
        "SF:test/cart.test.js\nDA:1,1\nend_of_record\n",
    );
    project.write(
        "extra.info",
        "SF:lib/extra.js\nDA:1,1\nDA:2,0\nend_of_record\n",
    );
    project.write("beyond.info", "SF:src/cart.js\nDA:99,1\nend_of_record\n");
    project.write(
        "nothing.info",
        "SF:/elsewhere/a.js\nDA:1,1\nend_of_record\n",
    );
    let before = project.latest();
    for (args, expected) in [
        (
            vec!["import", "excluded.info", "--test", "t"],
            "Nothing in excluded.info is a file the run measures: test/cart.test.js (the run leaves it out: test or fixture source)",
        ),
        (
            vec!["import", "beyond.info", "--test", "t"],
            "records line 99 of src/cart.js, which has 21 lines",
        ),
        (
            vec!["import", "nothing.info", "--test", "t"],
            "Nothing in nothing.info is a file of this project: /elsewhere/a.js (outside the project)",
        ),
        (
            vec!["import", "extra.info", "--test", "t", "--background"],
            "--test and --background each name who the lines are credited to",
        ),
        (
            vec!["import", "extra.info", "--test", "t", "--run", "nope"],
            "coverage run not found: nope",
        ),
    ] {
        let refused = project.supercov(&args).exits(2);
        assert!(refused.contains(expected), "{args:?}: {refused}");
    }
    assert_eq!(project.latest(), before, "a refused import makes no run");
    project
        .supercov(&["import", "extra.info", "--background"])
        .succeeds();
    project
        .supercov(&["import", "extra.info", "--test", "again"])
        .succeeds();
    let twice = project
        .supercov(&["import", "extra.info", "--test", "again"])
        .exits(2);
    assert!(
        twice.contains("already holds an imported test named again"),
        "{twice}"
    );
    let runners = project.supercov(&["runs", "latest", "runners"]).succeeds();
    contains_all(
        &runners,
        &[
            "background  0 test(s)",
            "lines  1 test(s)",
            "node:test  3 test(s)",
        ],
    );
}

/// Go's flags reach `go test` through GOFLAGS, so a suite started from a
/// Makefile is measured, and a command that is not `go test` is left as
/// written instead of being handed `-count=1`.
#[test]
fn a_go_suite_behind_make_is_measured_and_other_go_commands_are_left_alone() {
    // The Makefile runs `go` by name.
    if go() != Some("go") {
        eprintln!("skipping: no Go toolchain on PATH");
        return;
    }
    let made = Project::empty("go-make");
    made.write("go.mod", "module example.com/made\n\ngo 1.21\n");
    made.write(
        "x.go",
        "package x\n\nfunc F(a bool) int {\n\tif a {\n\t\treturn 1\n\t}\n\treturn 2\n}\n",
    );
    made.write(
        "x_test.go",
        "package x\n\nimport \"testing\"\n\nfunc TestX(t *testing.T) { F(true) }\n",
    );
    made.write("Makefile", "test:\n\tgo test ./...\n");
    let measured = made.supercov(&["--", "make", "test"]);
    assert_eq!(measured.code(), 0, "{}", measured.stderr());
    assert!(
        measured.stderr().contains("added -count=1"),
        "{}",
        measured.stderr()
    );
    let summary = made.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["command: make test", "Passed      1"]);
    let vetted = made.supercov(&["--", "go", "vet", "./..."]);
    assert!(
        !vetted.stderr().contains("malformed import path"),
        "{}",
        vetted.stderr()
    );
}

/// A merged run is answered from the runs it was merged from, and a test
/// both of them ran is one test, counted once, with its reasons together.
#[test]
fn tests_affected_on_a_merge_counts_a_test_both_parents_ran_once() {
    let project = Project::cart("merged-affected");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let first = project.latest();
    project
        .supercov(&["--", "node", "--test", "test/cart.test.js"])
        .succeeds();
    let second = project.latest();
    project.supercov(&["merge", &first, &second]).succeeds();
    project.edit("src/cart.js", "return 0;", "return 1;");
    let affected = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &affected,
        &[
            "1 of 3 tests affected by changes since the run",
            "test/cart.test.js › shipping is free over fifty",
            "src/cart.js: shipping (line 12) changed (this test ran it)",
        ],
    );
    let json = project
        .supercov(&["runs", "latest", "tests", "affected", "--json"])
        .json();
    assert_eq!(
        json["data"]["mergedFrom"],
        serde_json::json!([first, second])
    );
    assert_eq!(json["data"]["summary"]["tests"], 3);
    assert_eq!(json["data"]["summary"]["unaffected"], 2);
}

/// Output sent to one file for both streams (`> log 2>&1`) arrives through
/// one relay and names the project rather than Supercov's copy.
#[test]
fn both_streams_into_one_file_name_the_project() {
    let Some(go) = go() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let project = Project::empty("one-file");
    project.write("go.mod", "module example.com/onefile\n\ngo 1.21\n");
    project.write(
        "x.go",
        "package x\n\nfunc F(a bool) int {\n\tif a {\n\t\treturn 1\n\t}\n\treturn 2\n}\n",
    );
    project.write(
        "x_test.go",
        "package x\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestX(t *testing.T) {\n\tdir, _ := os.Getwd()\n\tfmt.Println(\"out \" + dir)\n\tfmt.Fprintln(os.Stderr, \"err \"+dir)\n\tF(true)\n}\n",
    );
    let log = project.root.join("both.log");
    let file = std::fs::File::create(&log).unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_supercov"))
        .args(["--", go, "test", "-v", "./..."])
        .current_dir(&project.root)
        .env_remove("SUPERCOV_SOURCE_ROOTS")
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .status()
        .unwrap();
    let written = std::fs::read_to_string(&log).unwrap();
    assert!(status.success(), "{written}");
    // The project as given, or its long form: Windows runners' TEMP is a
    // short `RUNNER~1` path, which Go names in full (canonical paths there
    // carry `\\?\`). Line by line: a Windows line may end in `\r\n`.
    let canonical = std::fs::canonicalize(&project.root).unwrap();
    let canonical = canonical.display().to_string();
    let roots = [
        project.root.display().to_string(),
        canonical
            .strip_prefix(r"\\?\")
            .unwrap_or(&canonical)
            .to_owned(),
    ];
    for stream in ["out", "err"] {
        assert!(
            written
                .lines()
                .any(|line| roots.iter().any(|root| line == format!("{stream} {root}"))),
            "{written}"
        );
    }
    // Supercov's own lines name the copy; the command's do not.
    assert!(
        written
            .lines()
            .filter(|line| !line.starts_with("[supercov]"))
            .all(|line| !line.contains("workspaces/workspace/")),
        "{written}"
    );
}

/// A runner named alone narrows every view to its tests and says so; a
/// runner no test used is refused by name.
#[test]
fn a_runner_alone_narrows_every_view_or_is_refused() {
    let project = Project::cart("runner-filters");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let gaps = project
        .supercov(&["runs", "latest", "gaps", "--runner", "node:test"])
        .succeeds();
    contains_all(
        &gaps,
        &[
            "Projection: runner node:test.",
            "[covered elsewhere: 0; nowhere: 15]",
        ],
    );
    for args in [
        vec!["runs", "latest", "gaps", "--runner", "vitest"],
        vec!["runs", "latest", "files", "--runner", "vitest"],
        vec!["runs", "latest", "--runner", "vitest"],
        vec![
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--runner",
            "vitest",
        ],
    ] {
        let refused = project.supercov(&args).exits(2);
        assert!(
            refused.contains("No tests match runner=vitest"),
            "{args:?}: {refused}"
        );
    }
    let summary = project
        .supercov(&["runs", "latest", "--runner", "node:test"])
        .succeeds();
    contains_all(
        &summary,
        &["(runner node:test)", "files --runner 'node:test'"],
    );
    let kinds = project
        .supercov(&["runs", "latest", "kinds", "--runner", "node:test"])
        .succeeds();
    assert!(kinds.contains("unit  3 test(s)  lines 76.92%"), "{kinds}");
    let minimum = project
        .supercov(&["runs", "latest", "minimize", "--runner", "node:test"])
        .succeeds();
    assert!(minimum.contains("exact minimum 3/3 test(s)"), "{minimum}");
    let directories = project
        .supercov(&[
            "runs",
            "latest",
            "files",
            "--group",
            "dir",
            "--runner",
            "node:test",
        ])
        .succeeds();
    assert!(
        directories.contains("Coverage by directory, 1 deep — runner node:test"),
        "{directories}"
    );
    let decision = project
        .supercov(&[
            "runs",
            "latest",
            "decision",
            "src/cart.js:13",
            "--runner",
            "node:test",
        ])
        .succeeds();
    assert!(decision.contains("FF -> F  tests=1"), "{decision}");
    let tests = project
        .supercov(&["runs", "latest", "test", "total"])
        .succeeds();
    contains_all(
        &tests,
        &[
            "total adds prices",
            "total applies a coupon",
            "showing 1-2 of 2 matching tests",
        ],
    );
}

/// Nothing changed is nothing affected; a changed test file affects each of
/// its tests and `--files`/`--names` list them alone; a dependency change
/// affects every test; and a narrowing option without a list to narrow is
/// refused on standard error, leaving standard output empty for a script.
#[test]
fn tests_affected_reads_each_kind_of_change_and_refuses_on_stderr() {
    let project = Project::cart("affected-shapes");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let clean = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &clean,
        &[
            "0 of 3 tests affected",
            "No captured file has changed.",
            "Working tree: matches the run.",
        ],
    );
    let refused = project.supercov(&["runs", "latest", "tests", "affected", "--ran-changed"]);
    assert_eq!(refused.code(), 2);
    assert_eq!(refused.stdout(), "");
    assert!(
        refused.stderr().starts_with(
            "[supercov] --asserting and --ran-changed narrow --names or --files\n\nUsage:"
        ),
        "{}",
        refused.stderr()
    );
    let unassessed = project.supercov(&[
        "runs",
        "latest",
        "tests",
        "affected",
        "--files",
        "--ran-changed",
    ]);
    assert_eq!(unassessed.code(), 0);
    assert!(
        unassessed
            .stderr()
            .contains("this run has no assertion assessment")
    );
    project.edit(
        "test/cart.test.js",
        "total adds prices",
        "total adds up prices",
    );
    let own = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &own,
        &["3 of 3 tests affected", "test file changed: top level"],
    );
    assert_eq!(
        project
            .supercov(&["runs", "latest", "tests", "affected", "--files"])
            .succeeds(),
        "test/cart.test.js\n"
    );
    assert_eq!(
        project
            .supercov(&["runs", "latest", "tests", "affected", "--names"])
            .succeeds(),
        "shipping is free over fifty\ntotal adds prices\ntotal applies a coupon\n"
    );
    project.edit(
        "test/cart.test.js",
        "total adds up prices",
        "total adds prices",
    );
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" }, "dependencies": { "x": "1" } }"#,
    );
    let dependency = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &dependency,
        &[
            "3 of 3 tests affected",
            "dependencies or lockfile changed (every test)",
        ],
    );
}

/// A wider project: directories page, decisions group and page, a long
/// decision reads whole, tests without evidence page and filter, and a file
/// the scope includes with nothing in it says so instead of "not found".
#[test]
fn a_wider_project_pages_groups_and_names_an_empty_included_file() {
    let project = Project::cart("wider-views");
    project.write(
        "lib/extra.js",
        "export const extra = (value) => value + 1;\n",
    );
    project.write("src/notes.js", "// Nothing here runs.\n");
    project.write(
        "src/long.js",
        "export function long(first, second, third, fourth) {\n  if ((first > 10 && second < 20) || (third === 'some fairly long string value' && fourth !== null)) {\n    return 1;\n  }\n  return 0;\n}\n",
    );
    project.write(
        "test/more.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { long } from \"../src/long.js\";\nimport { extra } from \"../lib/extra.js\";\n\ntest(\"long\", () => {\n  assert.equal(long(11, 1, '', null), 1);\n  assert.equal(extra(1), 2);\n});\n\ntest(\"twice nothing\", () => {\n  assert.ok(true);\n  assert.ok(1);\n});\n\ntest(\"once nothing\", () => {\n  assert.ok(true);\n});\n",
    );
    project.commit("wider");
    project
        .supercov_with(
            &["--", "node", "--test"],
            &[("SUPERCOV_SOURCE_ROOTS", "src,lib")],
        )
        .succeeds();
    let directories = project
        .supercov(&["runs", "latest", "files", "--group", "dir", "--limit", "1"])
        .succeeds();
    contains_all(
        &directories,
        &["showing 1-1 of 2", "files --group dir --offset 1 --limit 1"],
    );
    let none = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "lib/extra.js",
            "--group",
            "decision",
        ])
        .succeeds();
    assert!(
        none.contains("decisions 0, with missing conditions 0"),
        "{none}"
    );
    let paged = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--group",
            "decision",
            "--limit",
            "1",
        ])
        .succeeds();
    contains_all(
        &paged,
        &[
            "showing 1-1 of 3 decisions",
            "--group decision --offset 1 --limit 1",
        ],
    );
    let long = project
        .supercov(&["runs", "latest", "decision", "src/long.js:2"])
        .succeeds();
    contains_all(
        &long,
        &[
            "(first > 10 && second < 20) || (third === 'some fairly long string value' && fourth !== null)",
            "C4 MISSING: fourth !== null (never evaluated)",
        ],
    );
    let without = project
        .supercov(&[
            "runs",
            "latest",
            "tests",
            "without-evidence",
            "--limit",
            "1",
        ])
        .succeeds();
    contains_all(
        &without,
        &["showing 1-1 of 2", "without-evidence --offset 1 --limit 1"],
    );
    let passed = project
        .supercov(&[
            "runs",
            "latest",
            "tests",
            "without-evidence",
            "--filter",
            "passed",
        ])
        .succeeds();
    contains_all(
        &passed,
        &["2 — passed attempts only", "passed, 2 assertions"],
    );
    let empty = project
        .supercov(&["runs", "latest", "file", "src/notes.js"])
        .exits(2);
    assert!(
        empty.contains("src/notes.js holds nothing the run measures"),
        "{empty}"
    );
}

/// A merge of two kinds pages its kinds, counts a setup scope beside its
/// test, and a kind's summary counts and attributes only that kind's tests.
#[test]
fn a_merge_of_two_kinds_pages_and_narrows_by_kind() {
    let project = Project::cart("merged-kinds");
    project.write(
        "test/hooked.test.js",
        "import { test, before } from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\n\nlet base;\nbefore(() => { base = total([{ price: 1, quantity: 1 }]); });\n\ntest(\"hooked\", () => {\n  assert.equal(base, 1);\n});\n",
    );
    project.commit("hooked");
    project
        .supercov(&["--", "node", "--test", "test/cart.test.js"])
        .succeeds();
    let first = project.latest();
    project
        .supercov_with(
            &["--", "node", "--test", "test/hooked.test.js"],
            &[("SUPERCOV_TEST_KIND", "integration")],
        )
        .succeeds();
    let second = project.latest();
    project.supercov(&["merge", &first, &second]).succeeds();
    let kinds = project
        .supercov(&["runs", "latest", "kinds", "--limit", "1"])
        .succeeds();
    contains_all(
        &kinds,
        &[
            "integration  1 test(s) + 1 setup scope(s)  lines 46.15%",
            "showing 1-1 of 2",
            "kinds --offset 1 --limit 1",
        ],
    );
    let integration = project
        .supercov(&["runs", "latest", "--kind", "integration"])
        .succeeds();
    contains_all(
        &integration,
        &[
            "(kind integration)",
            "Exact for 1 test(s)",
            "Total       1",
            "Setup scopes 1",
        ],
    );
}

/// First-party code outside any root it recognises is ambiguous, blocks a
/// complete measurement, is named four directories at a time in the summary
/// and in full in `scope`, and both say which roots to name.
#[test]
fn code_outside_the_source_roots_is_named_with_the_roots_to_give() {
    let project = Project::empty("spread-scope");
    project.write(
        "package.json",
        r#"{ "name": "spread", "private": true, "type": "module" }"#,
    );
    // Code nothing the tests load reaches: a file a test imports is source.
    for dir in [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
    ] {
        project.write(
            &format!("{dir}/index.js"),
            &format!("export const {dir} = (x) => x ? 1 : 2;\n"),
        );
    }
    project.write(
        "test/all.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { main } from \"../src/main.js\";\n\ntest(\"main\", () => { assert.equal(main, 1); });\n",
    );
    project.write("src/main.js", "export const main = 1;\n");
    project.git(&["init", "-q"]);
    project.commit("spread");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let roots = "SUPERCOV_SOURCE_ROOTS=src,alpha,beta,delta,epsilon,eta,gamma,theta,zeta";
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            "Incomplete — 8 blocking limitation(s) in 8 file(s)",
            "Unclassified source: alpha 1, beta 1, delta 1, epsilon 1, 4 elsewhere",
            roots,
        ],
    );
    let scope = project.supercov(&["runs", "latest", "scope"]).succeeds();
    contains_all(
        &scope,
        &[
            "included 1, excluded 1, ambiguous 8",
            "1  AMBIGUOUS  zeta       unclassified first-party source",
            roots,
        ],
    );
}

/// A report written to an absolute path, narrowed to tests only the newest
/// run has, leaves the older run out of the comparison.
#[test]
fn a_report_to_an_absolute_path_leaves_out_runs_without_the_tests_asked_for() {
    let project = Project::cart("report-filter");
    project
        .supercov(&["--", "node", "--test", "test/cart.test.js"])
        .succeeds();
    project.write(
        "test/extra.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { describe } from \"../src/cart.js\";\n\ntest(\"describes\", () => {\n  assert.equal(describe([{ name: \"a\" }]), \"a\");\n});\n",
    );
    project.commit("extra");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let output = project.root.join("out/report.html");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    let written = project
        .supercov(&[
            "report",
            "--no-open",
            "--output",
            output.to_str().unwrap(),
            "--test-file",
            "test/extra.test.js",
        ])
        .succeeds();
    assert!(written.contains(output.to_str().unwrap()), "{written}");
    let html = std::fs::read_to_string(&output).unwrap();
    assert!(
        html.to_ascii_lowercase().starts_with("<!doctype html"),
        "{}",
        &html[..80]
    );
}

/// On a terminal, a Go test's partial line is passed on after a moment and
/// the rest joins it, the terminal's size is kept up through a long test,
/// and the copy's path is named as the project's.
#[cfg(unix)]
#[test]
fn a_go_test_on_a_terminal_passes_partial_lines_on_and_names_the_project() {
    let Some(go) = go() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let project = Project::empty("go-terminal");
    project.write("go.mod", "module example.com/term\n\ngo 1.21\n");
    project.write(
        "x.go",
        "package x\n\nfunc F(a bool) int {\n\tif a {\n\t\treturn 1\n\t}\n\treturn 2\n}\n",
    );
    project.write(
        "x_test.go",
        "package x\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\t\"testing\"\n\t\"time\"\n)\n\nfunc TestX(t *testing.T) {\n\tfmt.Print(\"prompt> \")\n\ttime.Sleep(600 * time.Millisecond)\n\tdir, _ := os.Getwd()\n\tfmt.Println(\"done in \" + dir)\n\tF(true)\n}\n",
    );
    let mut terminal = common::OnTerminal::start(&project, &["--", go, "test", "-v", "./..."]);
    terminal.wait_for("prompt> ");
    terminal.wait_for("done in ");
    let (code, seen) = terminal.finish();
    assert_eq!(code, 0, "{seen}");
    let root = project.root.display().to_string();
    assert!(seen.contains(&format!("prompt> done in {root}")), "{seen}");
}

/// A unittest run counts each test as it ended: a pass, a failure that
/// keeps the run diagnostic, an import error, and a test its decorator
/// skipped, which never enters a phase and is still a test. A suite with no
/// tests at all is refused.
#[test]
fn unittest_outcomes_each_count_and_a_decorator_skip_is_a_test() {
    let make = |label: &str, test: &str| {
        let project = Project::empty(label);
        project.write(
            "pyproject.toml",
            "[project]\nname = \"shop\"\nversion = \"0.1.0\"\n",
        );
        project.write("src/shop/__init__.py", "");
        project.write(
            "src/shop/cart.py",
            "def total(a):\n    if a:\n        return 1\n    return 2\n",
        );
        project.write("tests/__init__.py", "");
        project.write("tests/test_cart.py", test);
        project
    };
    let head = "import sys, unittest\nsys.path.insert(0, 'src')\nfrom shop.cart import total\n\nclass T(unittest.TestCase):\n";
    let command = [
        "--", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".",
    ];
    for (label, test, exit, outcome) in [
        (
            "ut-pass",
            format!("{head}    def test_total(self):\n        self.assertEqual(total(True), 1)\n"),
            0,
            "Passed      1",
        ),
        (
            "ut-fail",
            format!("{head}    def test_total(self):\n        self.assertEqual(total(True), 2)\n"),
            1,
            "Failed      1",
        ),
        (
            "ut-error",
            "import nothing_here\n".to_owned(),
            1,
            "Failed      1",
        ),
        (
            "ut-skip",
            format!("{head}    @unittest.skip('later')\n    def test_total(self):\n        pass\n"),
            0,
            "Skipped     1",
        ),
    ] {
        let project = make(label, &test);
        let ran = project.supercov(&command);
        assert_eq!(ran.code(), exit, "{label}: {}", ran.stderr());
        let summary = project.supercov(&["runs", "latest"]).succeeds();
        assert!(summary.contains(outcome), "{label}: {summary}");
    }
    let empty = make("ut-none", "# no tests\n");
    let refused = empty.supercov(&command);
    // unittest's own exit when nothing ran: 5 from Python 3.12, which
    // Supercov names as the command's; before that, 1 with no outcome.
    let said = refused.stderr();
    match refused.code() {
        5 => assert!(
            said.contains("the test command exited 5 before any test reported an outcome"),
            "{said}"
        ),
        code => {
            assert_eq!(code, 1, "{said}");
            assert!(
                said.contains("the Python run produced no test outcomes"),
                "{said}"
            );
        }
    }
}

/// A query about one file warns that the run is stale only when what it
/// reads changed: a test file's change says nothing about a source file,
/// that file's own change names it, and a dependency change names why.
#[test]
fn a_file_query_warns_of_staleness_only_for_what_it_reads() {
    let project = Project::cart("scoped-stale");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let file = |project: &Project| project.supercov(&["runs", "latest", "file", "src/cart.js"]);
    project.edit(
        "test/cart.test.js",
        "total adds prices",
        "total adds up prices",
    );
    let quiet = file(&project);
    assert_eq!(quiet.code(), 0);
    assert!(!quiet.stderr().contains("stale run"), "{}", quiet.stderr());
    project.edit("src/cart.js", "return 0;", "return 1;");
    let named = file(&project);
    assert!(
        named.stderr().contains("src/cart.js changed since the run"),
        "{}",
        named.stderr()
    );
    project.edit("src/cart.js", "return 1;", "return 0;");
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test" }, "dependencies": { "x": "1" } }"#,
    );
    let dependency = file(&project);
    assert!(
        dependency
            .stderr()
            .contains("dependencies or lockfile changed"),
        "{}",
        dependency.stderr()
    );
}

/// Asked for the failed attempts of a run that failed nothing, each view
/// reads as that projection: nothing touched, nothing covered, a minimum of
/// no tests, and a test the run has is named as left out by the filter
/// rather than missing.
#[test]
fn a_failed_projection_of_a_passing_run_reads_as_empty_and_names_what_it_leaves_out() {
    let project = Project::cart("failed-projection");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let file = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--filter",
            "failed",
        ])
        .succeeds();
    contains_all(
        &file,
        &[
            "Tests touching this file: 0",
            "showing 1-13 of 13 gap lines",
        ],
    );
    let line = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:14",
            "--filter",
            "failed",
        ])
        .succeeds();
    contains_all(&line, &["NOT COVERED", "Covering tests\n  None"]);
    let decision = project
        .supercov(&[
            "runs",
            "latest",
            "decision",
            "src/cart.js:13",
            "--filter",
            "failed",
        ])
        .succeeds();
    contains_all(&decision, &["confidence unexecuted", "vectors:\n  none"]);
    let minimum = project
        .supercov(&["runs", "latest", "minimize", "--filter", "failed"])
        .succeeds();
    assert!(minimum.contains("exact minimum 0/0 test(s)"), "{minimum}");
    let left_out = project
        .supercov(&["runs", "latest", "test", "total", "--filter", "failed"])
        .exits(2);
    assert!(
        left_out.contains("No test matching total is in this view: the run has one"),
        "{left_out}"
    );
    let missing = project
        .supercov(&[
            "runs",
            "latest",
            "test",
            "nothing-like-it",
            "--filter",
            "failed",
        ])
        .exits(2);
    assert!(
        missing.contains("Test not found: nothing-like-it"),
        "{missing}"
    );
}

/// `patch` counts the changed executable lines the run covered, fails a
/// floor it misses, annotates up to the cap, passes an empty change, and
/// names a base git cannot find; `check` passes and fails floors by count.
#[test]
fn patch_and_check_read_a_change_and_a_run_by_count() {
    let unchanged = Project::cart("patch-empty");
    unchanged.supercov(&["--", "node", "--test"]).succeeds();
    let empty = unchanged
        .supercov(&["runs", "patch", "--base", "HEAD", "--min-lines", "50"])
        .succeeds();
    contains_all(
        &empty,
        &[
            "No executable changes.",
            "PASS: nothing executable changed, so 50% does not apply.",
        ],
    );
    let below = unchanged
        .supercov(&["runs", "check", "--min-lines", "99"])
        .exits(1);
    contains_all(
        &below,
        &[
            "(floor 99%)",
            "FAIL: 1 floor(s) not met.",
            "lines 76.92%  10/13 below 99%",
        ],
    );
    let json = unchanged
        .supercov(&["runs", "check", "--min-lines", "10", "--json"])
        .json();
    assert_eq!(json["data"]["outcome"]["result"], "pass", "{json}");

    let changed = Project::cart("patch-changed");
    changed.write("src/extra.js", "export const extra = (x) => x ? 1 : 2;\n");
    changed.edit("src/cart.js", "let sum = 0;", "let sum = 0 + 0;");
    changed.supercov(&["--", "node", "--test"]).succeeds();
    let patch = changed
        .supercov(&["runs", "patch", "--base", "HEAD"])
        .succeeds();
    contains_all(
        &patch,
        &[
            "1 of 2 changed executable line(s) covered (50.00%)",
            "src/extra.js: 1",
        ],
    );
    let failed = changed
        .supercov(&["runs", "patch", "--base", "HEAD", "--min-lines", "90"])
        .exits(1);
    assert!(failed.contains("FAIL: below 90%"), "{failed}");
    let annotated = changed
        .supercov(&[
            "runs",
            "patch",
            "--base",
            "HEAD",
            "--annotate",
            "github",
            "--max-annotations",
            "1",
        ])
        .succeeds();
    assert!(
        annotated.contains("::warning file=src/extra.js,line=1,endLine=1::Changed line 1 not covered by the selected tests."),
        "{annotated}"
    );
    let json = changed
        .supercov(&["runs", "patch", "--base", "HEAD", "--json"])
        .json();
    assert_eq!(
        (
            json["data"]["patch"]["covered"].as_u64(),
            json["data"]["patch"]["eligible"].as_u64()
        ),
        (Some(1), Some(2))
    );
    let base = changed
        .supercov(&["runs", "patch", "--base", "nope"])
        .exits(2);
    assert!(
        base.contains("Could not find where this branch left nope"),
        "{base}"
    );
}

/// Many failing tests are named five at a time; a run of one test script
/// names the others the package defines.
#[test]
fn failures_are_named_five_at_a_time_and_other_scripts_are_offered() {
    let failing = Project::cart("many-failing");
    let mut tests = String::from(
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\n",
    );
    for i in 0..7 {
        tests.push_str(&format!(
            "test(\"fails {i}\", () => {{ assert.equal(total([]), {}); }});\n",
            i + 1
        ));
    }
    failing.write("test/fail.test.js", &tests);
    failing.commit("failing");
    let ran = failing.supercov(&["--", "node", "--test", "test/fail.test.js"]);
    assert_eq!(ran.code(), 1);
    assert!(
        ran.stderr()
            .contains("7 test(s) failed: fails 0, fails 1, fails 2, fails 3, fails 4, and 2 more"),
        "{}",
        ran.stderr()
    );
    let scripts = Project::cart("other-scripts");
    scripts.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test", "test:unit": "node --test test/cart.test.js", "test:e2e": "node --test" } }"#,
    );
    scripts.commit("scripts");
    scripts
        .supercov(&["--", "npm", "run", "test:unit"])
        .succeeds();
    let one = scripts.supercov(&["runs", "latest"]).succeeds();
    assert!(
        one.contains("wrapped `npm run test:unit`. Other test scripts include test:e2e."),
        "{one}"
    );
    scripts
        .supercov(&["--", "npm", "exec", "--", "node", "--test"])
        .succeeds();
    let exec = scripts.supercov(&["runs", "latest"]).succeeds();
    assert!(
        exec.contains("Other test scripts include test:unit, test:e2e."),
        "{exec}"
    );
}

#[test]
fn a_test_that_ran_alongside_others_is_undetermined_and_a_new_file_asks_for_the_whole_suite() {
    let Some(go) = go() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let project = Project::empty("go-undetermined");
    project.write("go.mod", "module example.com/u\n\ngo 1.22\n");
    project.write(
        "sign.go",
        "package u\n\nfunc Sign(n int) string {\n\tif n > 0 {\n\t\treturn \"positive\"\n\t}\n\treturn \"not positive\"\n}\n",
    );
    project.write(
        "sign_test.go",
        "package u\n\nimport \"testing\"\n\nfunc TestPositive(t *testing.T) {\n\tif Sign(1) != \"positive\" {\n\t\tt.Fatal(\"positive\")\n\t}\n}\n\nfunc TestParallel(t *testing.T) {\n\tt.Parallel()\n\tif Sign(-1) != \"not positive\" {\n\t\tt.Fatal(\"not positive\")\n\t}\n}\n",
    );
    project.git(&["init", "-q"]);
    project.commit("u");
    project.supercov(&["--", go, "test", "./..."]).succeeds();

    // The parallel test has no record of its own: a change to code the run
    // covered could have reached it, so it is neither affected nor not.
    project.edit("sign.go", "return \"not positive\"", "return \"negative\"");
    let affected = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &affected,
        &[
            "1 of 2 tests affected by changes since the run, 1 undetermined",
            "  sign_test.go › TestPositive\n    sign.go: Sign (line 3) changed (this test ran it)",
            "Undetermined — what these ran was not fully recorded",
            "  sign_test.go › TestParallel\n    sign.go: Sign (line 3) changed (the run covered it)",
        ],
    );
    let json = project
        .supercov(&["runs", "latest", "tests", "affected", "--json"])
        .json();
    assert_eq!(json["data"]["undetermined"][0]["attribution"], "run-wide");
    // Both are for a runner to run.
    assert_eq!(
        project
            .supercov(&["runs", "latest", "tests", "affected", "--names"])
            .succeeds(),
        "TestParallel\nTestPositive\n"
    );

    // A file the run never captured: no record can say who loads it.
    project.edit("sign.go", "return \"negative\"", "return \"not positive\"");
    project.write("extra.go", "package u\n\nfunc Extra() int { return 3 }\n");
    let added = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    contains_all(
        &added,
        &[
            "No captured file has changed.",
            "  A source or test file exists that the run did not capture; no test's record can say whether it loads it. Run the suite in full.",
        ],
    );
    let json = project
        .supercov(&["runs", "latest", "tests", "affected", "--json"])
        .json();
    assert_eq!(json["data"]["workingTree"]["sourceSetChanged"], true);

    // Mistakes under --json are answered in JSON, as every --json answer is.
    for (args, command, message) in [
        (
            &["tests", "affected", "--frob", "--json"][..],
            "runs.tests.affected",
            "Unknown option: --frob",
        ),
        (
            &["tests", "frob", "--json"],
            "runs.tests",
            "unknown tests action: frob",
        ),
        (
            &["assertions", "--frob", "--json"],
            "runs.assertions",
            "Unknown option: --frob",
        ),
    ] {
        let mut full = vec!["runs", "latest"];
        full.extend_from_slice(args);
        let refused = project.supercov(&full);
        assert_eq!(refused.code(), 2);
        let json: serde_json::Value = serde_json::from_str(&refused.stdout()).unwrap();
        assert_eq!(json["command"], command);
        assert_eq!(json["error"]["message"], message);
        assert_eq!(refused.stderr(), "");
    }
    // Without it, the usage follows the mistake on standard error.
    let text = project.supercov(&["runs", "latest", "tests", "frob"]);
    assert_eq!(text.code(), 2);
    assert!(
        text.stderr()
            .starts_with("[supercov] unknown tests action: frob\n\nUsage: ")
    );
}

#[test]
fn tests_whose_coverage_went_to_the_run_are_not_reported_as_missing_evidence() {
    let Some(go) = go() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let project = Project::empty("go-all-parallel");
    project.write("go.mod", "module example.com/p\n\ngo 1.22\n");
    project.write(
        "sign.go",
        "package p\n\nfunc Sign(n int) string {\n\tif n > 0 {\n\t\treturn \"positive\"\n\t}\n\treturn \"not positive\"\n}\n",
    );
    project.write(
        "sign_test.go",
        "package p\n\nimport \"testing\"\n\nfunc TestParallel(t *testing.T) {\n\tt.Parallel()\n\tif Sign(-1) != \"not positive\" {\n\t\tt.Fatal(\"not positive\")\n\t}\n}\n",
    );
    project.git(&["init", "-q"]);
    project.supercov(&["--", go, "test", "./..."]).succeeds();
    // The one test ran alongside nothing it could be told from: what it
    // reached counts run-wide, and no view reads that as evidence lost.
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &summary,
        &[
            "Lines      75.00% (3/4)",
            "  unit            1 test(s)  coverage not attributable; counted run-wide",
            "  Attribution      1 counted run-wide",
        ],
    );
    assert!(
        !summary.contains("no source-coverage evidence"),
        "{summary}"
    );
    assert!(
        project
            .supercov(&["runs", "latest", "kinds"])
            .succeeds()
            .contains("unit  1 test(s)  coverage not attributable; counted run-wide")
    );
    assert_eq!(
        project
            .supercov(&["runs", "latest", "tests", "without-evidence"])
            .succeeds(),
        "Tests that made assertions and recorded no coverage: 0\n"
    );
}

#[test]
fn a_long_decision_is_shortened_in_its_file_and_whole_on_its_own() {
    let project = Project::cart("long-decision");
    project.write(
        "src/long.js",
        "export function check(alpha, beta, gamma) {\n  if (alpha.someLongPropertyName > 10 && beta.anotherLongPropertyName < 20 && gamma.yetAnotherLongPropertyName !== 30) {\n    return 1;\n  }\n  return 0;\n}\n",
    );
    project.write(
        "test/long.test.js",
        "import test from \"node:test\";\nimport { check } from \"../src/long.js\";\ntest(\"long\", () => { check({ someLongPropertyName: 1 }, {}, {}); });\n",
    );
    project.commit("long");
    project.measure(&[]);
    let file = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/long.js",
            "--group",
            "decision",
        ])
        .succeeds();
    assert!(
        file.contains("  missing 3/3  alpha.someLongPropertyName > 10 && beta.anotherLongPropertyName < 20 && gamma.yetAnotherLongPro…\n"),
        "{file}"
    );
    let decision = project
        .supercov(&["runs", "latest", "decision", "src/long.js:2"])
        .succeeds();
    assert!(
        decision.contains("\nalpha.someLongPropertyName > 10 && beta.anotherLongPropertyName < 20 && gamma.yetAnotherLongPropertyName !== 30\n"),
        "{decision}"
    );
}

#[test]
fn a_comparison_of_runs_that_measure_different_code_says_why_the_percentages_moved() {
    let project = Project::cart("diff-moved");
    let first = project.measure(&[]);
    project.write("src/added.js", "export const added = 1;\n");
    let second = project.measure(&[]);
    let moved = project.supercov(&["diff", &first, &second]);
    let said = moved.succeeds();
    contains_all(
        &said,
        &[
            "lines -5.49pp, branches +0pp, MC/DC +0pp",
            "gained: 0 lines, 0 branches, 0 MC/DC conditions",
            "Nothing covered in one run is uncovered in the other: the percentages moved because the runs measure different code.",
        ],
    );
    assert!(!said.contains("showing"), "{said}");
    // Each stale run of the two names what changed since it.
    project.write("src/added.js", "export const added = 2;\n");
    let stale = project.supercov(&["diff", &first, &second]).stderr();
    contains_all(
        &stale,
        &[
            &format!(
                "[supercov] stale run {first}: instrumented source changed; changed since the run: src/added.js"
            ),
            &format!(
                "[supercov] stale run {second}: instrumented source changed; changed since the run: src/added.js"
            ),
        ],
    );
}

#[test]
fn a_patch_counts_new_files_and_reads_only_the_tests_a_filter_picks() {
    let project = Project::cart("patch-filtered");
    project.edit("src/cart.js", "let sum = 0;", "let sum = 0 + 0;");
    project.write("README.md", "# readme\n");
    project.write("src/fresh.js", "export const fresh = 1;\n");
    project.measure(&[]);
    // A file git does not track yet is part of the change; prose is not
    // executable and is no part of the count.
    let patch = project
        .supercov(&["runs", "latest", "patch", "--base", "HEAD"])
        .succeeds();
    contains_all(
        &patch,
        &[
            "  1 of 2 changed executable line(s) covered (50.00%)",
            "  src/fresh.js: 1",
        ],
    );
    assert!(!patch.contains("README"), "{patch}");
    let filtered = project.supercov(&[
        "runs",
        "latest",
        "patch",
        "--base",
        "HEAD",
        "--title",
        "coupon",
        "--min-lines",
        "90",
    ]);
    assert_eq!(filtered.code(), 1);
    assert!(
        filtered.stdout().ends_with("FAIL: below 90%\n"),
        "{}",
        filtered.stdout()
    );
    assert!(
        filtered
            .stderr()
            .contains("reading only the tests --title coupon picks")
    );
    let none = project
        .supercov(&[
            "runs",
            "latest",
            "patch",
            "--base",
            "HEAD",
            "--test-file",
            "test/none.js",
        ])
        .exits(2);
    assert!(
        none.contains("no test of the run matches --test-file test/none.js"),
        "{none}"
    );
}

#[test]
fn query_numbers_and_shapes_are_checked_before_anything_is_read() {
    let project = Project::cart("query-shapes");
    project.measure(&[]);
    let on_run = |args: &[&str]| {
        let mut full = vec!["runs", "latest"];
        full.extend_from_slice(args);
        project.supercov(&full)
    };
    for limit in ["1.5", "-1", "99999999999999999999"] {
        assert!(
            on_run(&["files", "--limit", limit])
                .exits(2)
                .contains("--limit must be a number of entries, or 0 for all of them")
        );
    }
    assert!(
        on_run(&["files", "--depth", "2"])
            .exits(2)
            .contains("--depth requires --group dir")
    );
    assert!(
        on_run(&["files", "--depth", "1", "--group", "dir"])
            .succeeds()
            .starts_with("Coverage by directory, 1 deep\n")
    );
    assert!(
        on_run(&["files", "--files"])
            .exits(2)
            .contains("--files is only supported by: supercov runs <run-id> scope")
    );
    assert!(
        on_run(&["line", "src/cart.js"])
            .exits(2)
            .contains("Expected <source-file>:<line>, with lines counted from 1")
    );
    // A line asked of `file` points at `line`.
    assert!(on_run(&["file", "src/cart.js:3"]).exits(2).contains(
        "Source file not found: src/cart.js:3; one line is read with `line src/cart.js:3`"
    ));
    assert!(
        on_run(&["gaps", "--metric", "all"])
            .succeeds()
            .contains("MC/DC conditions 4")
    );
    // The export writes only JSON lines, and names a place it cannot write.
    assert!(
        on_run(&["gaps", "--format", "csv"])
            .exits(2)
            .contains("gaps --format takes jsonl")
    );
    let unwritable = on_run(&[
        "gaps",
        "--format",
        "jsonl",
        "--output",
        "missing/dir/x.jsonl",
    ])
    .exits(2);
    assert!(unwritable.contains("missing/dir/x.jsonl: "), "{unwritable}");
    let picked = on_run(&[
        "gaps", "--format", "jsonl", "--metric", "mcdc", "--title", "coupon",
    ])
    .succeeds();
    assert!(
        picked.lines().all(|line| line.contains(r#""kind":"mcdc""#)) && picked.lines().count() > 0,
        "{picked}"
    );
}

#[test]
fn code_outside_the_roots_counts_when_source_loads_it_and_is_named_by_check_when_not() {
    let project = Project::cart("loaded-outside");
    project.write(
        "shared/round.js",
        "export function round(value) {\n  if (value < 0) return 0;\n  return Math.round(value);\n}\n",
    );
    project.write(
        "src/price.js",
        "import { round } from \"../shared/round.js\";\nexport const price = (value) => round(value);\n",
    );
    project.write("extra/unused.js", "export const unused = 1;\n");
    project.write(
        "test/round.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { price } from \"../src/price.js\";\n\ntest(\"rounds\", () => assert.equal(price(1.4), 1));\n",
    );
    project.commit("outside");
    project.measure(&[]);
    // Source loads it, so it is source: measured, with its gap.
    let scope = project
        .supercov(&["runs", "latest", "scope", "--files"])
        .succeeds();
    contains_all(
        &scope,
        &[
            "INCLUDED  shared/round.js  loaded by first-party source",
            "AMBIGUOUS  extra/unused.js  unclassified first-party source",
        ],
    );
    let file = project
        .supercov(&["runs", "latest", "file", "shared/round.js"])
        .succeeds();
    contains_all(
        &file,
        &[
            "Statements not executed         1",
            "Tests touching this file: 1",
        ],
    );
    // Nothing loads the other: check passes as the suite does, and says so.
    let check = project
        .supercov(&["runs", "latest", "check", "--min-lines", "10"])
        .succeeds();
    assert!(
        check.contains(
            "PASS: every floor is met.\n1 file outside the source roots that no source imports is not counted: extra/unused.js."
        ),
        "{check}"
    );
    let json = project
        .supercov(&["runs", "latest", "check", "--min-lines", "10", "--json"])
        .json();
    assert_eq!(json["data"]["view"]["complete"], false);
    // Any of its lines says why, not that the line holds no code.
    let line = project
        .supercov(&["runs", "latest", "line", "extra/unused.js:1"])
        .succeeds();
    contains_all(
        &line,
        &[
            "  NOT MEASURED\n",
            "  - source-scope: JavaScript/TypeScript under no source root that no first-party source imports",
        ],
    );
    assert!(!line.contains("Nothing on this line is code"), "{line}");
}

#[test]
fn a_cargo_filter_that_matches_no_test_says_so_when_the_run_is_read() {
    let project = Project::empty("cargo-no-match");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"nomatch\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn sign(n: i32) -> bool {\n    n > 0\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn positive() {\n        assert!(super::sign(1));\n    }\n}\n",
    );
    project.git(&["init", "-q"]);
    project
        .supercov(&["--", "cargo", "test", "--", "nothing_matches"])
        .succeeds();
    let read = project.supercov(&["runs", "latest"]).exits(2);
    assert!(
        read.contains(
            "The wrapped command passed and ran no test Supercov could record, so there is no coverage to read; check that its test filter matches a test."
        ),
        "{read}"
    );
    // Libtest's own selection, skips included, is the run's.
    project
        .supercov(&[
            "--",
            "cargo",
            "test",
            "--",
            "--exact",
            "--skip",
            "tests::positive",
        ])
        .succeeds();
    project
        .supercov(&["--", "cargo", "test", "--", "--exact", "tests::positive"])
        .succeeds();
    assert!(
        project
            .supercov(&["runs", "latest", "test", "tests::positive"])
            .succeeds()
            .contains("outcome passed")
    );
}

#[test]
fn minimize_refuses_background_evidence_and_names_the_filter_that_leaves_it_out() {
    let project = Project::cart("minimize-background");
    project.write(
        "test/setup.test.js",
        "import { before, test } from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\nconst loaded = total([{ price: 1, quantity: 1 }]);\nbefore(() => { total([{ price: 2, quantity: 1 }]); });\ntest(\"loaded\", () => assert.equal(loaded, 1));\n",
    );
    project.commit("setup");
    project.measure(&[]);
    let on_run = |args: &[&str]| {
        let mut full = vec!["runs", "latest"];
        full.extend_from_slice(args);
        project.supercov(&full)
    };
    // Module-load work ran with no test running: no subset of tests keeps it.
    let refused = on_run(&["minimize"]).exits(2);
    assert!(
        refused.contains("Narrow it to the tests' own evidence with --kind <kind> or --runner <runner>, which leave the background out"),
        "{refused}"
    );
    for target in ["150", "nan"] {
        assert!(
            on_run(&["minimize", "--target", target])
                .exits(2)
                .contains("--target must be between 0 and 100")
        );
    }
    // The filter it names: the tests alone, setup scope included.
    let minimum = on_run(&["minimize", "--kind", "unit"]).succeeds();
    assert!(
        minimum.starts_with(
            "exact minimum 3/4 test(s) keeping the coverage of all 4 tests in every measured metric"
        ),
        "{minimum}"
    );
    contains_all(
        &on_run(&["kinds"]).succeeds(),
        &[
            "background  0 test(s)  lines 46.15%",
            "unit  4 test(s) + 1 setup scope(s)  lines 76.92%",
        ],
    );
}

#[test]
fn a_line_past_the_end_of_its_file_fails_among_several_as_it_does_alone() {
    let project = Project::cart("lines-past-end");
    project.measure(&[]);
    let mixed = project
        .supercov(&["runs", "latest", "line", "src/cart.js:2", "src/cart.js:999"])
        .succeeds();
    contains_all(
        &mixed,
        &[
            "src/cart.js:2\n",
            "Not answered\n  src/cart.js:999: src/cart.js has 21 lines; there is no line 999",
        ],
    );
    assert!(!mixed.contains("Nothing on this line is code"), "{mixed}");
    let json = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:2",
            "src/cart.js:999",
            "--json",
        ])
        .json();
    assert_eq!(json["data"]["lines"].as_array().unwrap().len(), 1, "{json}");
    assert_eq!(json["data"]["failed"][0]["location"], "src/cart.js:999");
    // Every location failing fails the call, as one does.
    let none = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:998",
            "src/cart.js:999",
        ])
        .exits(2);
    assert!(
        none.contains("src/cart.js has 21 lines; there is no line 998"),
        "{none}"
    );
}

#[cfg(unix)]
#[test]
fn a_workspace_with_nested_dependencies_and_their_links_runs_in_the_copy_as_it_does_in_place() {
    let project = Project::empty("probe-nested-modules");
    project.write("package.json", r#"{ "name": "mono", "private": true, "type": "module", "workspaces": ["packages/*"], "scripts": { "test": "node --test packages/a/test/a.test.js" } }"#);
    project.write(
        "node_modules/dep/package.json",
        r#"{ "name": "dep", "type": "module", "main": "index.js" }"#,
    );
    project.write(
        "node_modules/dep/index.js",
        "export const dep = (x) => x + 1;\n",
    );
    project.write(
        "node_modules/dep/cli.js",
        "#!/usr/bin/env node\nconsole.log('dep');\n",
    );
    project.write(
        "packages/a/package.json",
        r#"{ "name": "a", "type": "module", "main": "src/index.js" }"#,
    );
    project.write(
        "packages/a/node_modules/local-dep/package.json",
        r#"{ "name": "local-dep", "type": "module", "main": "index.js" }"#,
    );
    project.write(
        "packages/a/node_modules/local-dep/index.js",
        "export const local = (x) => x * 2;\n",
    );
    project.write(
        "packages/a/node_modules/local-dep/bin.js",
        "#!/usr/bin/env node\nconsole.log('local');\n",
    );
    project.write("packages/a/src/index.js", "import { dep } from 'dep';\nimport { local } from 'local-dep';\nexport const both = (x) => (x > 0 ? local(dep(x)) : 0);\n");
    project.write("packages/a/test/a.test.js", "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { both } from '../src/index.js';\ntest('both', () => { assert.equal(both(1), 4); assert.equal(both(0), 0); });\n");
    // Links a package manager makes: commands in `.bin` at the root and in a
    // workspace package, and the package itself linked into the root.
    {
        std::fs::create_dir_all(project.root.join("node_modules/.bin")).unwrap();
        std::os::unix::fs::symlink("../dep/cli.js", project.root.join("node_modules/.bin/dep"))
            .unwrap();
        std::fs::create_dir_all(project.root.join("packages/a/node_modules/.bin")).unwrap();
        std::os::unix::fs::symlink(
            "../local-dep/bin.js",
            project.root.join("packages/a/node_modules/.bin/local"),
        )
        .unwrap();
        std::os::unix::fs::symlink("../../packages/a", project.root.join("node_modules/a"))
            .unwrap();
    }
    project.write(".gitignore", "node_modules\n");
    project.git(&["init", "-q"]);
    let plain = std::process::Command::new("npm")
        .arg("test")
        .current_dir(&project.root)
        .output()
        .unwrap();
    assert!(plain.status.success());
    project.supercov(&["--", "npm", "test"]).succeeds();
    let files = project.supercov(&["runs", "latest", "files"]).succeeds();
    assert!(
        files.contains("packages/a/src/index.js\n  covered in this projection"),
        "{files}"
    );
    assert!(!files.contains("node_modules"), "{files}");
}

#[test]
fn a_summary_narrowed_to_one_kind_lists_only_that_kind() {
    let project = Project::cart("summary-one-kind");
    project.write(
        "test/ship.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { shipping } from \"../src/cart.js\";\ntest(\"ships\", () => assert.equal(shipping(10, true), 15));\n",
    );
    project.commit("ship");
    project
        .supercov_with(
            &["--", "node", "--test", "test/cart.test.js"],
            &[("SUPERCOV_TEST_KIND", "unit")],
        )
        .succeeds();
    project
        .supercov_with(
            &["--", "node", "--test", "test/ship.test.js"],
            &[("SUPERCOV_TEST_KIND", "integration")],
        )
        .succeeds();
    project.supercov(&["merge"]).succeeds();
    let whole = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(
        &whole,
        &["  integration     1 test(s)", "  unit            3 test(s)"],
    );
    let narrowed = project
        .supercov(&["runs", "latest", "--kind", "integration"])
        .succeeds();
    contains_all(
        &narrowed,
        &[
            "(kind integration)",
            "Lines      23.08% (3/13)",
            "By test kind\n  integration     1 test(s)  lines  23.08%",
        ],
    );
    assert!(!narrowed.contains("  unit "), "{narrowed}");
    let json = project
        .supercov(&["runs", "latest", "--kind", "integration", "--json"])
        .json();
    assert_eq!(
        json["data"]["coverageByKind"].as_array().unwrap().len(),
        1,
        "{json}"
    );
    // A test the kind leaves out is named as left out.
    assert!(
        project
            .supercov(&["runs", "latest", "test", "ships", "--kind", "unit"])
            .exits(2)
            .contains("No test matching ships is in this view")
    );
}

#[test]
fn filters_pages_imports_and_help_topics_read_as_a_user_reads_them() {
    let project = Project::cart("broad-shapes");
    let first = project.measure(&[]);
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let on_run = |args: &[&str]| {
        let mut full = vec!["runs", "latest"];
        full.extend_from_slice(args);
        project.supercov(&full)
    };
    // Tests affected, narrowed every way a runner's filter wants it.
    assert_eq!(
        on_run(&["tests", "affected", "--kind", "unit", "--names"]).succeeds(),
        "shipping is free over fifty\n"
    );
    assert_eq!(
        on_run(&["tests", "affected", "--runner", "node:test", "--files"]).succeeds(),
        "test/cart.test.js\n"
    );
    assert!(
        on_run(&["tests", "affected", "--test-file", "test/cart.test.js"])
            .succeeds()
            .starts_with(&format!(
                "Run {first}: 1 of 3 tests affected by changes since the run (--test-file test/cart.test.js)\n"
            ))
    );
    assert!(
        on_run(&["tests", "affected", "--tag", "nothing"])
            .exits(2)
            .contains("No tests match --tag nothing")
    );
    // Pages past their end, and options that need another.
    assert!(
        on_run(&["scope", "--files", "--offset", "9"])
            .succeeds()
            .contains("showing none of 2: --offset 9 is past the end")
    );
    assert!(
        on_run(&["file", "src/cart.js", "--sort", "missing"])
            .exits(2)
            .contains("--sort requires --group decision")
    );
    assert!(
        on_run(&[
            "file",
            "src/cart.js",
            "--group",
            "decision",
            "--sort",
            "nonsense"
        ])
        .exits(2)
        .contains("--sort must be location or missing")
    );
    contains_all(
        &on_run(&["test", "total", "--limit", "1"]).succeeds(),
        &["showing 1-1 of 2 matching tests", "all 2 at once: "],
    );
    assert!(
        project
            .supercov(&["runs", "--limit", "1", "--offset", "5"])
            .succeeds()
            .contains("showing none of 1: --offset 5 is past the end")
    );
    assert!(
        project
            .supercov(&["runs", "clean", "--keep", "abc"])
            .exits(2)
            .contains("--keep must be a non-negative integer")
    );
    // An LCOV import names what it left out and keeps a failed outcome.
    project.write(
        "cov.info",
        "TN:\nSF:src/cart.js\nFN:12,shipping\nFNDA:1,shipping\nDA:13,1\nLF:1\nLH:1\nend_of_record\nSF:lib/elsewhere.js\nDA:1,1\nend_of_record\n",
    );
    assert!(
        project
            .supercov(&[
                "import", "cov.info", "--test", "imported", "--status", "maybe"
            ])
            .exits(2)
            .contains("--status is passed or failed, not maybe")
    );
    let imported = project.supercov(&[
        "import", "cov.info", "--test", "imported", "--status", "failed",
    ]);
    assert_eq!(imported.code(), 0, "{}", imported.stderr());
    assert!(
        imported.stderr().contains(
            "left out 1 file(s) of the evidence: lib/elsewhere.js (no such file in the project)"
        ),
        "{}",
        imported.stderr()
    );
    assert!(
        on_run(&["test", "imported"])
            .succeeds()
            .contains("outcome failed; retry 0=failed")
    );
    // --gained looks for no losses, and says only what it looked for.
    let gained = project
        .supercov(&["diff", &first, "latest", "--gained"])
        .succeeds();
    assert!(
        gained.contains("Nothing was gained.\n") && !gained.contains("or lost"),
        "{gained}"
    );
    // `help <command>` is that command's help.
    assert!(
        project
            .supercov(&["help", "runs"])
            .succeeds()
            .starts_with("Usage: supercov runs [options]")
    );
    assert!(
        project
            .supercov(&["help", "import"])
            .succeeds()
            .starts_with("Usage: supercov import")
    );
    assert!(
        project
            .supercov(&["help", "nope"])
            .exits(2)
            .contains("No help for nope: it is no command")
    );
    assert!(
        project
            .supercov(&["docs", "nonsense"])
            .exits(2)
            .contains("unknown guide \"nonsense\"")
    );
}

#[test]
fn the_wrapped_command_exits_as_it_would_alone_and_a_missing_root_is_named() {
    let project = Project::cart("wrapped-exits");
    assert!(
        project
            .supercov(&["--"])
            .exits(2)
            .contains("Usage: supercov -- <test command>")
    );
    let missing = project.supercov(&["--", "definitely-not-a-command"]);
    assert_eq!(missing.code(), 127);
    assert!(
        missing
            .stderr()
            .contains("could not spawn definitely-not-a-command"),
        "{}",
        missing.stderr()
    );
    assert_eq!(
        project
            .supercov(&["--", "node", "-e", "process.exit(3)"])
            .code(),
        3
    );
    // Ended by a signal: 128 plus the signal, as a shell says it.
    #[cfg(unix)]
    assert_eq!(
        project
            .supercov(&["--", "node", "-e", "process.kill(process.pid, 'SIGTERM')"])
            .code(),
        143
    );
    assert!(
        project
            .supercov(&["--exact-attribution", "node"])
            .exits(2)
            .contains("--exact-attribution goes before the command")
    );
    // Every root named missing is a mistake to name.
    let roots = project.supercov_with(
        &["--", "node", "--test"],
        &[("SUPERCOV_SOURCE_ROOTS", "nowhere")],
    );
    assert_eq!(roots.code(), 1);
    assert!(
        roots.stderr().contains(
            "SUPERCOV_SOURCE_ROOTS=nowhere left no JavaScript or TypeScript source to measure; nowhere does not exist"
        ),
        "{}",
        roots.stderr()
    );
    // One of several missing is a checkout without that directory.
    project
        .supercov_with(
            &["--", "node", "--test"],
            &[("SUPERCOV_SOURCE_ROOTS", "src,nowhere")],
        )
        .succeeds();
}

#[test]
fn private_batch_protocols_and_odd_arguments_say_what_went_wrong() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    // The harnesses' batch protocols fail with a message, not a panic, when
    // their output cannot be written: nothing reads the other end.
    for (command, says) in [
        (
            "__instrument-js",
            "failed to write Rust instrumenter output",
        ),
        (
            "__benchmark-js-transform",
            "failed to write Rust benchmark output",
        ),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_supercov"))
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"[{"file":"a.js","source":"export const a = 1;\n"}]"#)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{command}: {stderr}");
        assert!(stderr.contains(says), "{command}: {stderr}");
    }
    let project = Project::cart("odd-arguments");
    let first = project.measure(&[]);
    // A lone `-` is no option to merge: it names a run that does not exist.
    let merged = project.supercov(&["merge", &first, "-"]).exits(2);
    assert!(!merged.contains("Unknown option"), "{merged}");
    // An evidence path holding `=` is a path, not an option and its value.
    project.write(
        "cov=1.info",
        "TN:\nSF:src/cart.js\nDA:13,1\nLF:1\nLH:1\nend_of_record\n",
    );
    let imported = project.supercov(&["import", "cov=1.info", "--test", "imported"]);
    assert_eq!(imported.code(), 0, "{}", imported.stderr());
}

#[test]
fn filters_selectors_and_stray_entries_on_the_run_commands() {
    let project = Project::cart("run-commands");
    for directory in ["a", "b", "c", "d", "e", "f"] {
        project.write(&format!("tools/{directory}.js"), "export const tool = 1;\n");
    }
    project.commit("tools");
    let run = project.measure(&[]);
    // Six files no source imports, named five at a time.
    assert!(
        project
            .supercov(&["runs", "latest", "check", "--min-lines", "1"])
            .succeeds()
            .contains(
                "6 files outside the source roots that no source imports are not counted: \
             tools/a.js, tools/b.js, tools/c.js, tools/d.js, tools/e.js, and 1 more."
            )
    );
    // A test filter without its value, on each command that takes one.
    for (command, usage) in [
        (
            vec!["check", "--min-lines", "10"],
            "supercov runs <run-id> check",
        ),
        (
            vec!["patch", "--base", "HEAD"],
            "supercov runs <run-id> patch",
        ),
        (
            vec!["report", "--format", "lcov"],
            "supercov runs <run-id> report",
        ),
    ] {
        let mut args = vec!["runs", "latest"];
        args.extend(command);
        args.push("--kind");
        contains_all(
            &project.supercov(&args).exits(2),
            &["--kind needs a value", usage],
        );
    }
    // A decision by a file and something that is no line, and by its id.
    assert!(
        project
            .supercov(&["runs", "latest", "decision", "src/cart.js:abc"])
            .exits(2)
            .contains("Decision not found: src/cart.js:abc")
    );
    let decisions = project
        .supercov(&[
            "runs",
            "latest",
            "file",
            "src/cart.js",
            "--group",
            "decision",
            "--json",
        ])
        .json();
    let id = decisions["data"]["decisions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        project
            .supercov(&["runs", "latest", "decision", &id])
            .succeeds()
            .contains("coupon && coupon.percent > 0")
    );
    // A run stored under a name of the pre-release store, or any name but a
    // run id, is no run to list.
    let runs = project.root.join(".supercov/runs");
    for name in [
        "2026-08-25T23-33-12-211Z",
        "run_35c35ceeaf4b843Z",
        "abcdefghijklmnopqrst",
    ] {
        copy_tree(&runs.join(&run), &runs.join(name));
    }
    contains_all(
        &project.supercov(&["runs"]).succeeds(),
        &[&run, "showing 1-1 of 1"],
    );
    // A stale run's patch, as JSON: an error with its reason, no pass.
    project.edit(
        "src/cart.js",
        "return express ? 15 : 5;",
        "return express ? 20 : 5;",
    );
    let patch = project
        .supercov(&["runs", "latest", "patch", "--base", "HEAD", "--json"])
        .json();
    assert_eq!(patch["data"]["result"], "error", "{patch}");
    assert!(
        patch["data"]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("instrumented source changed"),
        "{patch}"
    );
}

/// The `wrangler` a measured Rust test starts, outside `wrangler dev`: it
/// refuses without the real one to hand over to, and otherwise becomes it
/// with the test's arguments untouched.
#[cfg(unix)]
#[test]
fn the_wrangler_shim_hands_over_or_says_why_it_cannot() {
    let project = Project::empty("wrangler-shim");
    let refused = project.supercov(&["__wrangler", "--version"]);
    assert_eq!(refused.code(), 127);
    assert!(
        refused
            .stderr()
            .contains("__wrangler runs only as the wrangler of a measured test"),
        "{}",
        refused.stderr()
    );
    project.write_script("real-wrangler", "#!/bin/sh\necho \"wrangler $*\"\nexit 3\n");
    let real = project.root.join("real-wrangler");
    let real = real.to_str().unwrap();
    let handed = project.supercov_with(
        &["__wrangler", "deploy", "--name", "it"],
        &[("SUPERCOV_WRANGLER", real)],
    );
    assert_eq!(handed.code(), 3, "{}", handed.stderr());
    assert_eq!(handed.stdout(), "wrangler deploy --name it\n");
    // The poller wants a process, a port and where to write; given a
    // process already gone, it ends.
    assert_eq!(project.supercov(&["__wrangler-poll"]).code(), 2);
    assert_eq!(
        project
            .supercov(&["__wrangler-poll", "nobody", "port", "windows"])
            .code(),
        2
    );
    let windows = project.root.join("worker.windows");
    let polled = project.supercov(&[
        "__wrangler-poll",
        "2147483646",
        "9",
        windows.to_str().unwrap(),
    ]);
    assert_eq!(polled.code(), 0, "{}", polled.stderr());
}

#[test]
fn a_run_whose_source_is_gone_is_stale_and_never_passes_a_gate() {
    let project = Project::cart("source-gone");
    let run = project.measure(&[]);
    std::fs::rename(
        project.root.join("src/cart.js"),
        project.root.join("src/cart.js.bak"),
    )
    .unwrap();
    let reason = "its source cannot be measured as it is now";
    contains_all(
        &project
            .supercov(&["runs", "latest", "check", "--min-lines", "10"])
            .exits(2),
        &["ERROR: this run cannot answer the question asked.", reason],
    );
    let files = project.supercov(&["runs", "latest", "files"]);
    assert_eq!(files.code(), 0, "{}", files.stderr());
    assert!(
        files
            .stderr()
            .contains(&format!("[supercov] stale run {run}: {reason}")),
        "{}",
        files.stderr()
    );
    assert!(
        project
            .supercov(&["runs"])
            .succeeds()
            .contains(&format!("STALE ({reason})"))
    );
    contains_all(
        &project
            .supercov(&["runs", "latest", "tests", "affected"])
            .succeeds(),
        &["3 of 3 tests affected", &format!("{reason} (every test)")],
    );
}

#[test]
fn files_named_beyond_ascii_are_changes_like_any_other() {
    let project = Project::cart("named-beyond-ascii");
    let base = project.git(&["rev-parse", "HEAD"]).trim().to_owned();
    project.write(
        "src/é.js",
        "export function hi(a) {\n  if (a) { return 1; }\n  return 2;\n}\n",
    );
    project.write(
        "test/e.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert\";\nimport { hi } from \"../src/é.js\";\ntest(\"hi\", () => assert.equal(hi(1), 1));\n",
    );
    project.commit("hi");
    let run = project.measure(&[]);
    // git quotes such a name unless asked not to; quoted, it was no file the
    // run measured, and the patch had no executable change.
    contains_all(
        &project
            .supercov(&["runs", "latest", "patch", "--base", &base])
            .succeeds(),
        &["2 of 3 changed executable line(s) covered", "src/é.js: 3"],
    );
    project.write("src/ü.js", "export const x = 1;\n");
    let files = project.supercov(&["runs", "latest", "files"]);
    assert!(
        files.stderr().contains(&format!(
            "[supercov] stale run {run}: instrumented source changed; changed since the run: src/ü.js"
        )),
        "{}",
        files.stderr()
    );
}

#[test]
fn a_patch_names_new_source_it_did_not_measure_and_not_new_tests() {
    let project = Project::cart("patch-unmeasured");
    let base = project.git(&["rev-parse", "HEAD"]).trim().to_owned();
    project.edit("src/cart.js", "  let sum = 0;", "  let sum = 0 + 0;");
    project.write("src/cart.bench.js", "export const bench = 1;\n");
    project.write("src/helper.spec.js", "export const spec = 1;\n");
    project.commit("branch");
    project.measure(&[]);
    let patch = project
        .supercov(&[
            "runs",
            "latest",
            "patch",
            "--base",
            &base,
            "--min-lines",
            "50",
        ])
        .succeeds();
    contains_all(
        &patch,
        &[
            "Changed but not measured by this run:\n    src/cart.bench.js\n",
            "PASS: at or above 50%",
        ],
    );
    assert!(!patch.contains("helper.spec.js"), "{patch}");
}

#[test]
fn a_run_left_out_of_a_merge_or_read_stale_says_what_moved() {
    // A source file added and a test file edited: the source is said to
    // have changed by the file it gained, and the test file is not named.
    let project = Project::cart("merge-moved");
    let run = project.measure(&[]);
    project.write("src/extra.js", "export const extra = 1;\n");
    project.edit(
        "test/cart.test.js",
        "total adds prices",
        "total sums prices",
    );
    let said = project.supercov(&["merge"]).exits(2);
    assert!(
        said.contains(&format!(
            "left out {run} (node --test): the source has changed (1 file(s) added). Measure it again"
        )),
        "{said}"
    );
    assert!(!said.contains("test/cart.test.js"), "{said}");

    // A measured file edited, with no record of the run's files to judge it
    // by: left out, as the measured source has changed.
    let project = Project::cart("merge-no-record");
    let run = project.measure(&[]);
    project.edit("src/cart.js", "let sum = 0;", "let sum = 0; // edited");
    let said = project
        .supercov_with(
            &["merge"],
            &[("SUPERCOV_TEST_FAULT", "manifest-unreadable")],
        )
        .exits(2);
    assert!(
        said.contains(&format!(
            "left out {run} (node --test): the source has changed. Measure it again"
        )),
        "{said}"
    );

    // A measured file removed whose name reads as configuration.
    let project = Project::cart("merge-config-named");
    project.write("src/app.config.js", "export const rate = 1;\n");
    project.commit("config-named source");
    let run = project.measure(&[]);
    std::fs::remove_file(project.root.join("src/app.config.js")).unwrap();
    let said = project.supercov(&["merge"]).exits(2);
    assert!(
        said.contains(&format!(
            "left out {run} (node --test): the source has changed. Measure it again"
        )),
        "{said}"
    );

    // Only configuration added: stale for it, no file to name; a diff with
    // a run after it says what it compared.
    let project = Project::cart("stale-configuration");
    let older = project.measure(&[]);
    project.write(".npmrc", "fund=false\n");
    project.commit("npmrc");
    let files = project.supercov(&["runs", "latest", "files"]);
    assert!(
        files.stderr().contains(&format!(
            "[supercov] stale run {older}: test/build configuration changed\n"
        )),
        "{}",
        files.stderr()
    );
    let newer = project.measure(&[]);
    let diff = project.supercov(&["diff", &older, &newer, "--json"]).json();
    assert_eq!(
        diff["data"]["inputs"],
        serde_json::json!({"sourceChanged": true, "testsChanged": false})
    );

    // Without the run's history, or without any repository, what changed is
    // still named from the run's own record.
    let project = Project::cart("stale-without-history");
    let run = project.measure(&[]);
    project.edit("src/cart.js", "return 0;", "return 0; // free");
    project.write("src/new.js", "export const n = 1;\n");
    std::fs::rename(project.root.join(".git"), project.root.join("away.git")).unwrap();
    let files = project.supercov(&["runs", "latest", "files"]);
    assert!(
        files.stderr().contains(&format!(
            "stale run {run}: instrumented source changed; changed since the run: src/cart.js\n"
        )),
        "{}",
        files.stderr()
    );
    project.git(&["init", "-q"]);
    project.git(&["add", "package.json", "src/cart.js"]);
    project.git(&["commit", "-q", "-m", "fresh"]);
    assert!(
        project
            .supercov(&["runs", "latest", "files"])
            .stderr()
            .contains("changed since the run: src/cart.js, src/new.js")
    );
}

#[test]
fn tests_without_evidence_are_explained_by_what_they_load() {
    let noop = "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\ntest(\"asserts a constant\", () => { assert.equal(1, 1); });\n";
    let project = Project::cart("without-evidence");
    project.write("test/noop.test.js", noop);
    project.write("test/plain", noop);
    project.write(
        "test/dyn.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nconst name = \"node:\" + \"path\";\nconst path = await import(name);\ntest(\"dyn import\", () => { assert.equal(typeof path.join, \"function\"); });\n",
    );
    project.commit("tests");
    let without = |command: &[&str], env: &[(&str, &str)]| {
        let mut args = vec!["--"];
        args.extend_from_slice(command);
        project.supercov_with(&args, env).succeeds();
        project
            .supercov(&["runs", "latest", "tests", "without-evidence", "--json"])
            .json()["data"]["tests"][0]
            .clone()
    };
    // A unit test that imports no source says so.
    let unit = without(&["node", "--test", "test/noop.test.js"], &[]);
    assert_eq!(unit["importsMeasured"], false, "{unit}");
    // An end-to-end test reaches source by other means than imports, a file
    // with no extension cannot be read for them, and a computed import names
    // no module: none of them is said to import nothing.
    let e2e = without(
        &["node", "--test", "test/noop.test.js"],
        &[("SUPERCOV_TEST_KIND", "e2e")],
    );
    assert_eq!(e2e["kind"], "e2e");
    assert!(e2e.get("importsMeasured").is_none(), "{e2e}");
    let plain = without(&["node", "--test", "test/plain"], &[]);
    assert_eq!(plain["file"], "test/plain");
    assert!(plain.get("importsMeasured").is_none(), "{plain}");
    let dynamic = without(&["node", "--test", "test/dyn.test.js"], &[]);
    assert!(dynamic.get("importsMeasured").is_none(), "{dynamic}");
}

// Its package-manager stand-in is a shell script.
#[cfg(unix)]
#[test]
fn line_queries_and_commands_at_their_edges() {
    // A command that only mentions another language's runner, and runs node.
    let project = Project::cart("command-edges");
    let ran = project.supercov(&["--", "sh", "-c", "echo phpunit && node --test"]);
    assert_eq!(ran.code(), 0, "{}", ran.stderr());
    // Several lines from past their end: none, and none failed.
    let lines = project
        .supercov(&[
            "runs",
            "latest",
            "line",
            "src/cart.js:2",
            "src/cart.js:3",
            "--offset",
            "5",
            "--json",
        ])
        .json();
    assert_eq!(lines["data"]["lines"], serde_json::json!([]));
    assert_eq!(lines["pagination"]["total"], 2);
    // A script of a package manager other than npm, and the others it has.
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module", "scripts": { "test": "node --test", "test:unit": "node --test", "test:e2e": "node --test" } }"#,
    );
    project.write_script("bin/pnpm", "#!/bin/sh\nexec node --test\n");
    project.commit("scripts");
    project
        .supercov(&["--", "bin/pnpm", "test:unit"])
        .succeeds();
    assert!(project.supercov(&["runs", "latest"]).succeeds().contains(
        "wrapped `bin/pnpm test:unit`. Other test scripts include test:e2e. For whole-project"
    ));
    // One line whose tests alone outgrow a reply.
    let mut tests = String::from(
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\n",
    );
    for name in ["a", "b", "c", "d", "e", "f"] {
        tests.push_str(&format!(
            "test(\"{}\", () => assert.equal(total([]), 0));\n",
            name.repeat(30_000)
        ));
    }
    project.write("test/long.test.js", &tests);
    project.commit("long names");
    project.measure(&[]);
    assert!(
        project
            .supercov(&[
                "runs",
                "latest",
                "line",
                "src/cart.js:2",
                "src/cart.js:3",
                "--json"
            ])
            .exits(2)
            .contains("\"code\":\"RESPONSE_TOO_LARGE\"")
    );
}

/// The trash a run starts emptying is empty when the run is over, however
/// long the sweep it started with takes: the end waits for that sweep rather
/// than starting another that finds the trash locked and leaves at once.
#[test]
fn the_trash_is_empty_when_a_run_ends() {
    let project = Project::cart("trash-at-end");
    let trash = project.root.join(".supercov/.trash/old-workspace");
    std::fs::create_dir_all(&trash).unwrap();
    for directory in 0..200 {
        let directory = trash.join(directory.to_string());
        std::fs::create_dir(&directory).unwrap();
        for file in 0..400 {
            std::fs::write(directory.join(file.to_string()), "").unwrap();
        }
    }
    project.measure(&[]);
    assert!(!project.root.join(".supercov/.trash/old-workspace").exists());
}

/// The read views' odd inputs and thin corners, read as a user meets them:
/// a depth of nothing, a lone dash, a colon in a file's name, a decision
/// line holding two, a line with nothing measured on it, and an unassessed
/// run read through a filter.
#[test]
fn the_read_views_name_their_odd_inputs_and_thin_corners() {
    let project = Project::cart("read-corners");
    project.write("util/name.js", "export const name = (item) => item.name;\n");
    project.write(
        "src/pick.js",
        "export const pick = (a, b) => ((a && b) ? 1 : (a || b) ? 2 : 3);\n",
    );
    project.write(
        "test/extra.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { name } from \"../util/name.js\";\nimport { pick } from \"../src/pick.js\";\n\ntest(\"names and picks\", () => {\n  assert.equal(name({ name: \"a\" }), \"a\");\n  assert.equal(pick(true, true), 1);\n});\n",
    );
    project.commit("corners");
    let run = project.measure(&[]);
    let on_run = |args: &[&str]| {
        let mut full = vec!["runs", "latest"];
        full.extend_from_slice(args);
        project.supercov(&full)
    };
    for depth in ["0", "-0"] {
        assert!(
            on_run(&["files", "--group", "dir", "--depth", depth])
                .exits(2)
                .contains("--depth must be a positive integer")
        );
    }
    // A lone dash is a file to look for, not an option.
    let dash = on_run(&["file", "-"]).exits(2);
    assert!(dash.contains("Source file not found: -"), "{dash}");
    assert!(!dash.contains("Unknown option"), "{dash}");
    // The line is after the last colon; the rest is the file.
    assert!(
        on_run(&["line", "no:such.js:2"])
            .exits(2)
            .contains("Source file not found: no:such.js")
    );
    assert!(
        on_run(&["gaps", "--format", "jsonl", "--metric"])
            .exits(2)
            .contains("--metric must be all, lines, statements, functions, branches, or mcdc")
    );
    // A directory with no branch at all has no share of them.
    let branches = on_run(&["files", "--group", "dir", "--metric", "branches"]).succeeds();
    assert!(
        branches
            .lines()
            .any(|line| line.starts_with("util ") && line.trim_end().ends_with("—")),
        "{branches}"
    );
    // Two decisions on one line are listed a page at a time.
    let decisions = on_run(&["decision", "src/pick.js:1", "--limit", "1"]).succeeds();
    contains_all(
        &decisions,
        &[
            "showing 1-1 of 2 matching decisions",
            &format!(
                "next page: supercov runs '{run}' decision 'src/pick.js:1' --offset 1 --limit 1"
            ),
        ],
    );
    // A line holding only the brace that closes a loop: nothing on it was
    // measured, and its source is today's.
    let brace = on_run(&["line", "src/cart.js:5"]).succeeds();
    contains_all(
        &brace,
        &["(read from the current working tree)", "NOT MEASURED"],
    );
    assert!(!brace.contains("Coverage details"), "{brace}");
    // A run never assessed, read through a filter, says how to assess
    // those tests.
    let unassessed = on_run(&["assertions", "--kind", "unit"]).succeeds();
    contains_all(
        &unassessed,
        &[
            "assertions not assessed for this run",
            &format!("Assess it with `supercov runs {run} assertions assess --kind 'unit'`"),
        ],
    );
}

/// A line's gap is exported with the line it is on, under `--metric lines`
/// as under every metric, and an export that cannot be written says why.
#[cfg(target_os = "linux")]
#[test]
fn exported_line_gaps_quote_their_line_and_a_failed_export_says_why() {
    let project = Project::cart("export-lines");
    let run = project.measure(&[]);
    let rows = |metric: &str| {
        project
            .supercov(&[
                "runs", &run, "gaps", "--format", "jsonl", "--metric", metric,
            ])
            .succeeds()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    let lines = rows("lines");
    // `describe` never ran: its body is a line gap, quoted by what first
    // goes unrun on it.
    let body = lines
        .iter()
        .find(|row| row["line"] == 20)
        .expect("line 20 is a gap");
    assert_eq!(body["kind"], "line");
    assert_eq!(body["lineSource"], "(item) => item.name");
    assert!(
        lines.iter().all(|row| row["lineSource"].is_string()),
        "{lines:?}"
    );
    // Each line reads as `--metric all` quotes it.
    let all = rows("all");
    for row in &lines {
        let same = all
            .iter()
            .find(|other| other["line"] == row["line"])
            .unwrap();
        assert_eq!(row["lineSource"], same["lineSource"], "{row}");
    }
    let file = project
        .supercov(&["runs", &run, "file", "src/cart.js", "--metric", "lines"])
        .succeeds();
    assert!(
        file.contains("   20  NOT COVERED   (item) => item.name"),
        "{file}"
    );
    assert!(!file.contains("(source unavailable)"), "{file}");
    // Standard output on a full disk: exit 2, and the reason.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_supercov"))
        .args([
            "runs", &run, "gaps", "--format", "jsonl", "--metric", "mcdc",
        ])
        .current_dir(&project.root)
        .stdout(std::fs::File::create("/dev/full").unwrap())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("could not write the gaps to standard output: No space left on device"),
        "{stderr}"
    );
}

/// A runner's configuration written since the run reaches only that
/// runner's tests, as an edit to one that was there does; a configuration
/// any runner reads reaches every test.
#[test]
fn a_configuration_added_since_the_run_affects_the_tests_of_its_runner() {
    let project = Project::cart("config-added");
    project.write("playwright.config.js", "export default {};\n");
    project.commit("playwright");
    project.measure(&[]);
    let affected = || {
        project
            .supercov(&["runs", "latest", "tests", "affected", "--json"])
            .json()["data"]
            .clone()
    };
    // node:test runs none of them under Playwright or Vitest.
    project.write("playwright.config.js", "export default { retries: 2 };\n");
    project.write("vitest.config.js", "export default {};\n");
    let other = affected();
    assert_eq!(other["summary"]["affected"], 0, "{other}");
    assert_eq!(other["workingTree"]["runWide"], serde_json::json!([]));
    assert_eq!(
        other["workingTree"]["reasons"],
        serde_json::json!(["test/build configuration changed"])
    );
    // A TypeScript configuration is read whatever the runner.
    project.write("tsconfig.json", "{}\n");
    let every = affected();
    assert_eq!(every["summary"]["affected"], 3, "{every}");
    assert_eq!(
        every["workingTree"]["runWide"],
        serde_json::json!(["test/build configuration changed"])
    );
    let text = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    assert!(
        text.contains("test/build configuration changed (every test)"),
        "{text}"
    );
}

/// A runner's configuration named where it is not named as one -- a
/// settings file `SUPERCOV_VITEST_CONFIG` points at -- changes the
/// configuration without any configuration file to blame: no runner can be
/// ruled out, and every test is affected.
#[test]
fn a_configuration_no_file_is_named_for_affects_every_test() {
    let project = Project::cart("affected-unnamed-configuration");
    project.write("runner-settings.mjs", "export default {};\n");
    project.commit("runner settings");
    let named = [("SUPERCOV_VITEST_CONFIG", "runner-settings.mjs")];
    let measured = project.supercov_with(&["--", "node", "--test"], &named);
    assert_eq!(measured.code(), 0, "{}", measured.stderr());
    project.write("runner-settings.mjs", "export default { test: {} };\n");
    let every = project
        .supercov_with(&["runs", "latest", "tests", "affected", "--json"], &named)
        .json()["data"]
        .clone();
    assert_eq!(every["summary"]["affected"], 3, "{every}");
    assert_eq!(
        every["workingTree"]["runWide"],
        serde_json::json!(["test/build configuration changed"])
    );
}

/// A change to hundreds of files no test ran affects no test and still
/// lists every one of them: the JSON keeps the first files and counts the
/// rest, rather than refusing the reply.
#[test]
fn tests_affected_shortens_a_long_list_of_changed_files_to_fit_one_reply() {
    const FILES: usize = 600;
    let project = Project::cart("affected-files");
    let path = |index: usize| {
        format!(
            "src/features/a/directory/named/at/the/length/real/projects/nest/their/modules/module{index}.js"
        )
    };
    for index in 0..FILES {
        project.write(&path(index), &format!("export const value = {index};\n"));
    }
    project.commit("modules");
    project.measure(&[]);
    for index in 0..FILES {
        project.write(
            &path(index),
            &format!("export const value = {};\n", index + 1),
        );
    }
    let data = project
        .supercov(&["runs", "latest", "tests", "affected", "--json"])
        .json()["data"]
        .clone();
    assert_eq!(data["summary"]["affected"], 0, "{}", data["summary"]);
    assert_eq!(data["changedFilesCount"], FILES, "{}", data["truncated"]);
    let listed = data["changedFiles"].as_array().unwrap().len();
    assert!(0 < listed && listed < FILES, "{listed}");
    assert!(
        data["truncated"].as_array().unwrap().iter().any(|note| note
            == "the changed files keep their first entries; the text view lists every one"),
        "{}",
        data["truncated"]
    );
    let text = project
        .supercov(&["runs", "latest", "tests", "affected"])
        .succeeds();
    assert!(
        text.contains(&path(FILES - 1)),
        "the text view lists every file"
    );
}

/// The report says why it will not compare a run with itself, fills its
/// runs without repeating one, writes nothing where its file is a
/// directory, opens in the browser, and shows its progress on a terminal.
#[cfg(unix)]
#[test]
fn the_report_refuses_a_run_compared_with_itself_and_opens_when_it_can() {
    let project = Project::cart("report-edges");
    let oldest = project.measure(&["--test-name-pattern=total"]);
    let middle = project.measure(&["--test-name-pattern=shipping"]);
    let newest = project.measure(&[]);
    let refused = project
        .supercov(&[
            "report",
            "--no-open",
            "--compare",
            "latest",
            "--output",
            "r.html",
        ])
        .exits(2);
    assert!(
        refused.contains(&format!(
            "report failed: --compare latest is {newest}, the run this report shows; compare it with another run"
        )),
        "{refused}"
    );
    assert!(!project.root.join("r.html").exists());
    project
        .supercov(&[
            "report",
            "--no-open",
            "--compare",
            &oldest,
            "--output",
            "r.html",
        ])
        .succeeds();
    let data = report_data(&project.read("r.html"));
    let ids = data["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        [newest.clone(), oldest.clone(), middle],
        "the comparison once"
    );
    assert_eq!(data["comparisonRunId"], oldest.as_str());
    // A directory where the report goes: the rename fails and the
    // temporary file goes with it.
    std::fs::create_dir_all(project.root.join("reports/supercov-report.html")).unwrap();
    let blocked = project
        .supercov(&["report", "--no-open", "--output", "reports"])
        .exits(2);
    assert!(
        blocked.contains(&format!(
            "report failed: {}: ",
            project.root.join("reports/supercov-report.html").display()
        )),
        "{blocked}"
    );
    let left = std::fs::read_dir(project.root.join("reports"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(left, ["supercov-report.html"]);
    // Opened the way the desktop opens a file.
    project.write_script(
        if cfg!(target_os = "macos") {
            "bin/open"
        } else {
            "bin/xdg-open"
        },
        "#!/bin/sh\necho \"$1\" > \"$(dirname \"$0\")/../opened.txt\"\n",
    );
    let path = format!(
        "{}:{}",
        project.root.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let opened = project
        .supercov_with(&["report", "--output", "r.html"], &[("PATH", &path)])
        .succeeds();
    assert!(
        opened.contains("[supercov] opened the report in your browser"),
        "{opened}"
    );
    let started = std::time::Instant::now();
    while !project.root.join("opened.txt").exists() {
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let started = std::time::Instant::now();
    while !project.read("opened.txt").contains("r.html") {
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // On a terminal, each run as it is prepared; a filter no test of the
    // run passes ends it with why.
    let shown = common::OnTerminal::start(&project, &["report", "--no-open", "--output", "t.html"]);
    let (code, seen) = shown.finish();
    assert_eq!(code, 0, "{seen}");
    assert!(
        seen.contains(&format!("[supercov] preparing report run 1/3: {newest}")),
        "{seen}"
    );
    let none = common::OnTerminal::start(&project, &["report", "--no-open", "--tag", "nothing"]);
    let (code, seen) = none.finish();
    assert_eq!(code, 2, "{seen}");
    assert!(seen.contains("report failed: "), "{seen}");
}

/// The report details a file whose only gap is code outside measurement,
/// and draws a run's multi-line statements from its coverage when the run
/// was published before their spans were saved beside it.
#[test]
fn the_report_details_an_unmeasured_gap_and_spans_a_run_never_saved() {
    let project = Project::empty("report-limited");
    project.write("package.json", r#"{ "name": "limited" }"#);
    project.write(
        "src/scoped.js",
        "const scope = { value: 7 };\nfunction sloppy() {\n  with (scope) {\n    return value;\n  }\n}\nfunction sum(a, b) {\n  return [\n    a,\n    b,\n  ].reduce((x, y) => x + y, 0);\n}\nmodule.exports = { sloppy, sum };\n",
    );
    project.write(
        "test/scoped.test.js",
        "const test = require(\"node:test\");\nconst assert = require(\"node:assert/strict\");\nconst { sloppy, sum } = require(\"../src/scoped.js\");\ntest(\"sloppy reads the scope\", () => assert.equal(sloppy(), 7));\ntest(\"sum adds\", () => assert.equal(sum(1, 2), 3));\n",
    );
    project.git(&["init", "-q"]);
    project.commit("scoped");
    let run = project.measure(&[]);
    let files = project
        .supercov(&["runs", "latest", "files", "--json"])
        .json();
    let scoped = &files["data"]["files"][0];
    assert_eq!(scoped["file"], "src/scoped.js", "{files}");
    // Every line ran: what is left is the `with` body, outside measurement.
    assert_eq!(scoped["uncoveredLines"], 0, "{scoped}");
    assert!(
        scoped["measurementLimitations"].as_u64() > Some(0),
        "{scoped}"
    );
    let report = |output: &str| {
        project
            .supercov(&["report", "--no-open", "--output", output])
            .succeeds();
        report_data(&project.read(output))
    };
    let first = report("first.html");
    assert!(
        first["runs"][0]["fileDetails"]["src/scoped.js"].is_object(),
        "{}",
        first["runs"][0]["fileDetails"]
    );
    let spans = first["runs"][0]["statementSpans"].clone();
    assert!(
        spans.as_array().is_some_and(|spans| !spans.is_empty()),
        "{spans}"
    );
    std::fs::remove_file(
        project
            .root
            .join(".supercov/runs")
            .join(&run)
            .join("statement-spans.json"),
    )
    .unwrap();
    let again = report("again.html");
    assert_eq!(again["runs"][0]["statementSpans"], spans);
}

#[cfg(unix)]
#[test]
fn a_tools_change_to_a_file_edited_while_the_command_ran_is_kept_back() {
    let project = Project::empty("kept-back");
    project.write(
        "package.json",
        r#"{ "name": "kept-back", "scripts": { "test": "oxlint --fix src && node edit.cjs && node --test test/fee.test.js" } }"#,
    );
    project.write(".gitignore", "node_modules\n.supercov\n");
    project.write(
        "src/fee.js",
        "function fee(sum) {\n  var rate = 5;\n  return sum > 100 ? 0 : rate;\n}\nmodule.exports = { fee };\n",
    );
    project.write(
        "test/fee.test.js",
        "const { test } = require('node:test');\nconst { equal } = require('node:assert/strict');\nconst { fee } = require('../src/fee.js');\ntest('small orders pay', () => equal(fee(1), 5));\n",
    );
    project.write_script(
        "node_modules/.bin/oxlint",
        "#!/bin/sh\nsed 's/var rate/const rate/' \"$2\"/fee.js > fixed.tmp && mv fixed.tmp \"$2\"/fee.js\n",
    );
    // Someone edits the project's own file while the command runs.
    project.write(
        "edit.cjs",
        "const fs = require('node:fs');\nconst file = process.env.EDITED_PROJECT + '/src/fee.js';\nfs.writeFileSync(file, fs.readFileSync(file, 'utf8').replace('var rate = 5', 'let rate = 5'));\n",
    );
    project.git(&["init", "-q"]);
    let root = project.root.display().to_string();
    let run = project.supercov_with(&["--", "npm", "test"], &[("EDITED_PROJECT", &root)]);
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    assert!(
        run.stderr().contains(
            "the command's tools changed 1 source file(s) that were edited in the project while it ran; those changes were not applied"
        ),
        "{}",
        run.stderr()
    );
    assert!(project.read("src/fee.js").contains("let rate = 5"));
}

#[test]
fn a_repository_the_workspace_cannot_mirror_is_named_under_debug() {
    let project = Project::empty("unmirrored");
    project.write(
        "package.json",
        r#"{ "name": "cart", "private": true, "type": "module" }"#,
    );
    project.write("src/cart.js", common::CART);
    project.write("test/cart.test.js", common::CART_TESTS);
    // A worktree whose repository is gone.
    project.write(".git", "gitdir: /nonexistent/supercov/worktree\n");
    let run = project.supercov_with(&["--", "node", "--test"], &[("SUPERCOV_DEBUG", "1")]);
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    assert!(
        run.stderr()
            .contains("the workspace has no git repository of its own"),
        "{}",
        run.stderr()
    );
    let quiet = project.supercov(&["--", "node", "--test"]);
    assert_eq!(quiet.code(), 0, "{}{}", quiet.stdout(), quiet.stderr());
    assert!(!quiet.stderr().contains("no git repository of its own"));
}

#[test]
fn settings_a_suite_chose_for_docker_playwright_and_uv_reach_it_unchanged() {
    // A daemon reached over TCP has no socket to relay, and a Playwright
    // cache or a uv sync someone chose is theirs.
    let project = Project::cart("chosen-settings");
    project.write(".venv/pyvenv.cfg", "home = /usr/bin\n");
    project.write(
        "test/environment.test.js",
        "import { test } from 'node:test';\nimport { equal } from 'node:assert/strict';\ntest('settings', () => {\n  equal(process.env.DOCKER_HOST, 'tcp://127.0.0.1:2375');\n  equal(process.env.PWTEST_CACHE_DIR, '/tmp/chosen-playwright-cache');\n  equal(process.env.UV_NO_SYNC, '0');\n});\n",
    );
    let run = project.supercov_with(
        &["--", "node", "--test"],
        &[
            ("DOCKER_HOST", "tcp://127.0.0.1:2375"),
            ("PWTEST_CACHE_DIR", "/tmp/chosen-playwright-cache"),
            ("UV_NO_SYNC", "0"),
        ],
    );
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    assert!(run.stdout().contains("pass 4"), "{}", run.stdout());
}

#[test]
fn a_direct_javascript_run_with_no_command_is_refused() {
    let project = Project::cart("empty-direct-command");
    let input = project.root.join("input.json");
    std::fs::write(
        &input,
        serde_json::json!({ "root": project.root, "command": [] }).to_string(),
    )
    .unwrap();
    let refused = project
        .supercov_with(
            &["__run-js-direct"],
            &[("SUPERCOV_INTERNAL_INPUT_FILE", input.to_str().unwrap())],
        )
        .exits(2);
    assert!(
        refused.contains("direct JavaScript run failed: test command must not be empty"),
        "{refused}"
    );
    assert!(!project.root.join(".supercov").exists());
}

#[test]
fn a_browser_suite_beside_servers_with_nothing_to_measure_measures_its_javascript() {
    // A Go workspace and a Maven build with no source: each server language
    // is looked for, and neither has anything to measure.
    let project = Project::cart("servers-unmeasured");
    project.write("playwright.config.js", "export default {};\n");
    project.write("go.work", "go 1.22\n\nuse ./svc\n");
    project.write("svc/go.mod", "module example.com/svc\n\ngo 1.22\n");
    project.write("pom.xml", "<project></project>\n");
    let run = project.supercov(&["--", "node", "--test"]);
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    assert!(
        !run.stderr().contains("server the tests start"),
        "{}",
        run.stderr()
    );
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("76.92%"), "{summary}");
    // Without JavaScript source there is nothing left to measure.
    std::fs::remove_dir_all(project.root.join("src")).unwrap();
    project.write(
        "test/cart.test.js",
        "import { test } from 'node:test';\ntest('nothing', () => {});\n",
    );
    let empty = project.supercov(&["--", "node", "--test"]).exits(1);
    assert!(
        empty.contains("No application source files were discovered"),
        "{empty}"
    );
}

/// Source roots named for a browser suite's JavaScript leave the Python and
/// Ruby beside it no server to measure: the run measures its JavaScript
/// instead of refusing the roots as a Python or Ruby run of its own would.
#[test]
fn source_roots_naming_only_javascript_leave_no_python_or_ruby_server() {
    let project = Project::cart("roots-javascript-only");
    project.write("playwright.config.js", "export default {};\n");
    project.write("server.py", "def handle():\n    return 1\n");
    project.write("app.rb", "def handle\n  1\nend\n");
    let run = project.supercov_with(
        &["--", "node", "--test"],
        &[("SUPERCOV_SOURCE_ROOTS", "src")],
    );
    assert_eq!(run.code(), 0, "{}{}", run.stdout(), run.stderr());
    assert!(!run.stderr().contains("left no"), "{}", run.stderr());
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    assert!(summary.contains("76.92%"), "{summary}");
}

/// A run whose query index cannot be written back says the index could not
/// be opened, and why, instead of blaming the command that ran.
#[test]
fn a_query_index_that_cannot_be_rebuilt_is_named_as_the_failure() {
    let project = Project::cart("index-unwritable");
    project.supercov(&["--", "node", "--test"]).succeeds();
    let runs = project.root.join(".supercov/runs");
    let run = std::fs::read_dir(&runs)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.path().is_dir())
        .unwrap()
        .path();
    let index = run.join("query-index.v1.bin");
    std::fs::remove_file(&index).unwrap();
    std::fs::create_dir(&index).unwrap();
    let refused = project.supercov(&["runs", "latest", "files"]).exits(2);
    assert!(
        refused.contains("Failed to open coverage index"),
        "{refused}"
    );
    assert!(!refused.contains("wrapped command"), "{refused}");
}

/// A script standing in for rustdoc while Supercov stands in for it during
/// `cargo test --doc`. It reads what to do from the files of the scenario
/// directory `FAKE_RUSTDOC_DIR` names: `standalone` and `merged` are the
/// doctests it lists (`name: test` lines), the second through Supercov's
/// runtool as a merged harness does; `merged.out` is what that harness
/// prints; and a per-name pass runs `doctest` through the runtool (`twice`
/// does so twice, `no-run` not at all) and prints `named.out`, or the name it
/// was given as passing.
#[cfg(unix)]
const FAKE_RUSTDOC: &str = r#"#!/bin/sh
fake="$FAKE_RUSTDOC_DIR"
printf '%s\n' "$*" >> "$fake/calls"
tool= pkg= mode= n=0
while [ $# -gt 0 ]; do
  case "$1" in
    --test-runtool) tool="$2"; shift ;;
    --test-runtool-arg)
      n=$((n + 1))
      [ $n -eq 2 ] && pkg="$2"
      [ $n -eq 3 ] && mode="$2"
      shift ;;
  esac
  shift
done
# Not a pass of Supercov's: a documentation build, or a listing the user
# asked for.
[ -z "$mode" ] && exit 0
case "$mode" in
  list)
    if [ -f "$fake/list.status" ]; then
      cat "$fake/list.err" >&2
      exit "$(cat "$fake/list.status")"
    fi
    cat "$fake/standalone" 2>/dev/null
    [ -f "$fake/merged" ] && "$tool" __doctest-runner "$pkg" list "$fake/../bin/harness-list"
    [ -f "$fake/vanish" ] && rm -f "$0"
    exit 0 ;;
  merged)
    "$tool" __doctest-runner "$pkg" merged "$fake/../bin/harness"
    exit $? ;;
  named)
    if [ ! -f "$fake/no-run" ]; then
      "$tool" __doctest-runner "$pkg" named "$fake/../bin/doctest" > /dev/null
      [ -f "$fake/twice" ] && "$tool" __doctest-runner "$pkg" named "$fake/../bin/doctest" > /dev/null
    fi
    if [ -f "$fake/named.out" ]; then cat "$fake/named.out"; else printf 'test %s ... ok\n' "$SUPERCOV_DOCTEST_NAME"; fi
    exit "$(cat "$fake/named.status" 2>/dev/null || echo 0)" ;;
esac
"#;

/// A project holding the stand-ins: rustdoc, the merged harness (listing
/// and running) and a standalone doctest binary.
#[cfg(unix)]
fn fake_rustdoc_project(label: &str) -> Project {
    let project = Project::empty(label);
    project.write_script(
        "bin/harness-list",
        "#!/bin/sh\ncat \"$FAKE_RUSTDOC_DIR/merged\"\n",
    );
    project.write_script(
        "bin/harness",
        "#!/bin/sh\n[ -f \"$FAKE_RUSTDOC_DIR/merged.sh\" ] && . \"$FAKE_RUSTDOC_DIR/merged.sh\"\ncat \"$FAKE_RUSTDOC_DIR/merged.out\"\nexit \"$(cat \"$FAKE_RUSTDOC_DIR/merged.status\" 2>/dev/null || echo 0)\"\n",
    );
    project.write_script(
        "bin/doctest",
        "#!/bin/sh\ncat \"$FAKE_RUSTDOC_DIR/doctest.out\" 2>/dev/null\nexit 0\n",
    );
    project
}

/// Supercov as rustdoc for one package, in a scenario of its own: the
/// files it starts with, and rustdoc's arguments. The outcome, and what
/// the wrapper recorded for the package (`.json`) or why it could not
/// (`.error`).
#[cfg(unix)]
fn wrap_rustdoc(
    project: &Project,
    scenario: &str,
    files: &[(&str, &str)],
    args: &[&str],
) -> (common::Ran, String) {
    for (file, contents) in files {
        project.write(&format!("{scenario}/{file}"), contents);
    }
    project.write(&format!("{scenario}/calls"), "");
    project.write_script(&format!("{scenario}/rustdoc"), FAKE_RUSTDOC);
    let directory = project.root.join(scenario);
    let results = directory.join("results");
    let ran = project.supercov_with(
        args,
        &[
            ("SUPERCOV_DOCTEST_WRAPPER_ROOT", results.to_str().unwrap()),
            (
                "SUPERCOV_DOCTEST_REAL_RUSTDOC",
                directory.join("rustdoc").to_str().unwrap(),
            ),
            ("FAKE_RUSTDOC_DIR", directory.to_str().unwrap()),
            ("CARGO_PKG_NAME", "fixture"),
            ("CARGO_MANIFEST_DIR", project.root.to_str().unwrap()),
            // The run that starts rustdoc names its instrumentation.
            ("SUPERCOV_RUST_PROBE_TOKEN", "0123456789abcdef"),
        ],
    );
    let recorded = ["00000000.json", "00000000.error"]
        .iter()
        .find_map(|file| std::fs::read_to_string(results.join("packages").join(file)).ok())
        .unwrap_or_default();
    (ran, recorded)
}

#[cfg(unix)]
#[test]
fn rustdoc_runs_untouched_when_it_is_not_running_doctests_for_supercov() {
    let project = fake_rustdoc_project("rustdoc-passthrough");
    for (scenario, args) in [
        ("docs", &["--crate-name", "fixture", "src/lib.rs"][..]),
        (
            "listing",
            &["--test", "src/lib.rs", "--test-args", "--list"][..],
        ),
    ] {
        let (ran, recorded) = wrap_rustdoc(&project, scenario, &[], args);
        ran.succeeds();
        assert_eq!(recorded, "", "{scenario}");
        assert_eq!(
            project.read(&format!("{scenario}/calls")),
            format!("{}\n", args.join(" ")),
            "{scenario}"
        );
    }
    // A listing rustdoc cannot produce stops the package as rustdoc did.
    let (ran, recorded) = wrap_rustdoc(
        &project,
        "unlisted",
        &[
            ("list.status", "3"),
            ("list.err", "error: cannot read crate"),
        ],
        &["--test", "src/lib.rs"],
    );
    let output = ran.exits(3);
    assert!(output.contains("error: cannot read crate"), "{output}");
    assert_eq!(recorded, "");
}

#[cfg(unix)]
#[test]
fn a_merged_doctest_harness_must_report_what_its_listing_and_arguments_select() {
    let project = fake_rustdoc_project("rustdoc-merged");
    let listed = "src/lib.rs - a (line 1): test\nsrc/lib.rs - b (line 5): test\n";
    let a = "test src/lib.rs - a (line 1) ... ok\n";
    for (scenario, out, error) in [
        (
            "ghost",
            format!("{a}test src/lib.rs - ghost (line 9) ... ok\n"),
            "the merged doctest harness reported a case the listing lacks: src/lib.rs - ghost (line 9)",
        ),
        (
            "twice",
            format!("{a}{a}"),
            "doctest src/lib.rs - a (line 1) was reported more than once",
        ),
        (
            "short",
            a.to_owned(),
            "the merged doctest harness ran a different selection than its arguments describe: src/lib.rs - b (line 5)",
        ),
    ] {
        let (ran, recorded) = wrap_rustdoc(
            &project,
            scenario,
            &[("merged", listed), ("merged.out", &out)],
            &["--test", "src/lib.rs"],
        );
        ran.exits(101);
        assert_eq!(recorded, error, "{scenario}");
    }
    // A harness that failed may have stopped short: it is not held to its
    // selection, and its exit is the package's.
    let (ran, recorded) = wrap_rustdoc(
        &project,
        "failing",
        &[
            ("merged", listed),
            ("merged.out", a),
            ("merged.status", "101"),
        ],
        &["--test", "src/lib.rs"],
    );
    ran.exits(101);
    let recorded: serde_json::Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(recorded["exit_code"], 101, "{recorded}");
    assert_eq!(
        recorded["cases"],
        serde_json::json!([{"name": "src/lib.rs - a (line 1)", "status": "passed", "evidence": null}])
    );
    // Under `--ignored` a harness runs the ignored doctests of its selection.
    let (ran, recorded) = wrap_rustdoc(
        &project,
        "ignored",
        &[
            ("merged", listed),
            ("merged.out", "test src/lib.rs - a (line 1) ... ignored\n"),
        ],
        &["--test", "src/lib.rs", "--test-args", "--ignored"],
    );
    ran.succeeds();
    let recorded: serde_json::Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(
        recorded["cases"],
        serde_json::json!([{"name": "src/lib.rs - a (line 1)", "status": "skipped", "evidence": null}])
    );
}

#[cfg(unix)]
#[test]
fn a_per_name_doctest_pass_must_run_its_doctest_alone_and_once() {
    let project = fake_rustdoc_project("rustdoc-named");
    let listed = "src/lib.rs - s (line 3): test\n";
    for (scenario, files, error) in [
        (
            "twice",
            &[("twice", "")][..],
            "doctest src/lib.rs - s (line 3) ran more than once",
        ),
        (
            "another",
            &[("named.out", "test src/lib.rs - other (line 7) ... ok\n")][..],
            "the pass for doctest src/lib.rs - s (line 3) ran src/lib.rs - other (line 7) instead",
        ),
    ] {
        let mut files = files.to_vec();
        files.push(("standalone", listed));
        let (ran, recorded) = wrap_rustdoc(&project, scenario, &files, &["--test", "src/lib.rs"]);
        ran.exits(101);
        assert_eq!(recorded, error, "{scenario}");
    }
    // Under `--ignored` a pass that runs and reports nothing passed over a
    // doctest that is not ignored...
    let (ran, recorded) = wrap_rustdoc(
        &project,
        "passed-over",
        &[("standalone", listed), ("no-run", ""), ("named.out", "")],
        &["--test", "src/lib.rs", "--test-args", "--ignored"],
    );
    ran.succeeds();
    let recorded: serde_json::Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(recorded["cases"], serde_json::json!([]), "{recorded}");
    // ... reports an ignored one as skipped ...
    let (ran, recorded) = wrap_rustdoc(
        &project,
        "ignored",
        &[
            ("standalone", listed),
            ("no-run", ""),
            ("named.out", "test src/lib.rs - s (line 3) ... ignored\n"),
        ],
        &["--test", "src/lib.rs", "--test-args", "--ignored"],
    );
    ran.succeeds();
    let recorded: serde_json::Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(recorded["cases"][0]["status"], "skipped", "{recorded}");
    // ... and one that failed without naming a doctest ran none alone.
    let (ran, recorded) = wrap_rustdoc(
        &project,
        "failed-silently",
        &[
            ("standalone", listed),
            ("no-run", ""),
            ("named.out", ""),
            ("named.status", "101"),
        ],
        &["--test", "src/lib.rs", "--test-args", "--ignored"],
    );
    ran.exits(101);
    assert_eq!(
        recorded,
        "the pass for doctest src/lib.rs - s (line 3) ran 0 doctest(s) instead of that one alone: "
    );
    // A doctest binary that ran for a pass reporting nothing is not
    // attributed to a guess.
    let (ran, recorded) = wrap_rustdoc(
        &project,
        "unclaimed",
        &[("standalone", listed), ("named.out", "")],
        &["--test", "src/lib.rs", "--test-args", "--ignored"],
    );
    ran.exits(101);
    assert_eq!(
        recorded,
        "a doctest ran that no pass selected: src/lib.rs - s (line 3)"
    );
    // A rustdoc that cannot be started for a pass stops the package.
    let (ran, _) = wrap_rustdoc(
        &project,
        "vanished",
        &[("standalone", listed), ("vanish", "")],
        &["--test", "src/lib.rs"],
    );
    let output = ran.exits(101);
    assert!(output.contains("could not launch"), "{output}");
}

#[cfg(unix)]
#[test]
fn the_doctest_runtool_records_each_binary_rustdoc_hands_it() {
    let project = fake_rustdoc_project("doctest-runtool");
    let runtool = |package: &str, mode: &str, binary: &str, files: &[(&str, &str)]| {
        for (file, contents) in files {
            project.write(&format!("{package}/{file}"), contents);
        }
        let directory = project.root.join(package);
        let ran = project.supercov_with(
            &[
                "__doctest-runner",
                directory.to_str().unwrap(),
                mode,
                project.root.join(binary).to_str().unwrap(),
            ],
            &[
                ("FAKE_RUSTDOC_DIR", directory.to_str().unwrap()),
                // Neither makes this the wrapper or a merged harness's
                // child: the runtool is named by its argument, and a child
                // by both variables.
                ("SUPERCOV_DOCTEST_WRAPPER_ROOT", directory.to_str().unwrap()),
                ("RUSTDOC_DOCTEST_RUN_NB_TEST", "0"),
                ("SUPERCOV_RUST_PROBE_TOKEN", "0123456789abcdef"),
            ],
        );
        let record = std::fs::read_to_string(directory.join("invocations/00000000.json"))
            .map(|text| serde_json::from_str::<serde_json::Value>(&text).unwrap())
            .unwrap_or_default();
        (ran, record)
    };
    // A binary listing nothing leaves no listing behind.
    let (ran, record) = runtool("empty-list", "list", "bin/doctest", &[]);
    ran.succeeds();
    assert_eq!(record, serde_json::json!({"kind": "listed"}));
    assert!(!project.root.join("empty-list/listed").exists());
    // A per-name pass whose binary is a merged harness that matched nothing.
    let (ran, record) = runtool(
        "empty-harness",
        "named",
        "bin/doctest",
        &[("doctest.out", "\nrunning 0 tests\n")],
    );
    ran.succeeds();
    assert_eq!(record, serde_json::json!({"kind": "empty-harness"}));
    // A merged harness whose children's records went missing reports its
    // cases without evidence.
    let order = r#"["src/lib.rs - a (line 1)"]"#;
    let (ran, record) = runtool(
        "children-gone",
        "merged",
        "bin/harness",
        &[
            ("merged-order.json", order),
            ("merged.sh", "rm -rf \"$SUPERCOV_DOCTEST_CHILD_DIR\"\n"),
            ("merged.out", "test src/lib.rs - a (line 1) ... ok\n"),
        ],
    );
    ran.succeeds();
    assert_eq!(
        record,
        serde_json::json!({"kind": "merged", "cases": [{"name": "src/lib.rs - a (line 1)", "status": "passed", "evidence": null}]})
    );
    // A child ran for a case the harness never reported.
    let (ran, _) = runtool(
        "stray-child",
        "merged",
        "bin/harness",
        &[
            ("merged-order.json", order),
            (
                "merged.sh",
                "printf '{\"index\":5,\"exit_code\":0,\"evidence\":\"/x\"}' > \"$SUPERCOV_DOCTEST_CHILD_DIR/00000005.json\"\n",
            ),
            ("merged.out", "test src/lib.rs - a (line 1) ... ok\n"),
        ],
    );
    let output = ran.exits(101);
    assert!(
        output.contains("merged doctest child 5 ran for a case the harness did not report"),
        "{output}"
    );
}

#[test]
fn a_crate_builds_with_the_rustflags_its_cargo_configuration_sets() {
    // Supercov sets `RUSTFLAGS` to cap lints, and Cargo then ignores
    // `build.rustflags`: the crate was built without its own `--cfg`.
    let project = Project::empty("config-rustflags");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"configured\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lints.rust]\nunexpected_cfgs = \"allow\"\n",
    );
    project.write(
        ".cargo/config.toml",
        "[build]\nrustflags = [\"--cfg\", \"configured\"]\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn configured() -> bool {\n    cfg!(configured)\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn sees_its_configured_flag() {\n        assert!(super::configured());\n    }\n}\n",
    );
    project.git(&["init", "-q"]);
    let output = project
        .supercov_with(&["--", "cargo", "test", "--lib"], &[("RUSTFLAGS", "")])
        .exits(101);
    // An empty `RUSTFLAGS` is the user's choice, and Cargo honours it.
    assert!(output.contains("1 failed"), "{output}");
    // Run as `supercov_with` runs it, but with no `RUSTFLAGS` of the test's.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_supercov"))
        .args(["--", "cargo", "test", "--lib"])
        .current_dir(&project.root)
        .env_remove("RUSTFLAGS")
        .env_remove("SUPERCOV_SOURCE_ROOTS")
        .env_remove("SUPERCOV_TEST_KIND")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    assert!(text.contains("1 passed"), "{text}");
}

/// A Ruby new enough for Supercov's Ruby frontend (3.3 or newer).
#[cfg(unix)]
fn ruby() -> Option<String> {
    let candidates = std::env::var("SUPERCOV_RUBY").ok().into_iter().chain(
        [
            "/opt/homebrew/opt/ruby@3.4/bin/ruby",
            "/opt/homebrew/opt/ruby/bin/ruby",
            "ruby",
        ]
        .map(str::to_owned),
    );
    candidates.into_iter().find(|ruby| {
        std::process::Command::new(ruby)
            .args(["-e", "exit(RUBY_VERSION >= '3.3' ? 0 : 1)"])
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Each system call that starts and contains a command, made to fail
/// (`SUPERCOV_TEST_FAULT`): a failure to set up says what could not be done
/// and runs nothing, and an interrupted call is made again. Each case checks
/// its fault was reached: Supercov says so when one fires.
#[cfg(unix)]
#[test]
fn a_failing_system_call_while_starting_a_command_is_named_or_retried() {
    // The watchdog's own sites cannot say so: it has no standard error.
    let silent = [
        "watchdog-orphaned",
        "watchdog-setsid",
        "watchdog-read",
        "proc-fd",
    ];
    let reached = |fault: &str, said: &str| {
        for site in fault.split(',') {
            let site = site.split([':', '=']).next().unwrap();
            if silent.contains(&site) {
                continue;
            }
            assert!(
                said.contains(&format!("[supercov] test fault at {site}")),
                "{site} was never reached:\n{said}"
            );
        }
    };
    let project = Project::cart("start-faults");
    let run = |fault: &str| {
        let output =
            project.supercov_with(&["--", "node", "--test"], &[("SUPERCOV_TEST_FAULT", fault)]);
        reached(fault, &output.stderr());
        output
    };
    let watchdog = "could not start the watchdog that stops the command if Supercov is killed: it ended before the command started";
    for (fault, said) in [
        (
            "parent-death-pipe",
            "could not create parent-death supervision pipe",
        ),
        (
            "parent-death-cloexec",
            "could not protect parent-death supervision pipe across exec",
        ),
        (
            "command-setpgid",
            "could not spawn node: Operation not permitted",
        ),
        ("ready-pipe", "could not spawn node: Too many open files"),
        (
            "watchdog-fork",
            "could not spawn node: Resource temporarily unavailable",
        ),
        ("watchdog-dup-input", watchdog),
        ("watchdog-dup-ready", watchdog),
        ("watchdog-orphaned", watchdog),
        ("watchdog-setsid", watchdog),
        // A readiness read that fails other than by being interrupted.
        (&format!("ready-read:{}", libc::EIO), watchdog),
    ] {
        let failed = run(fault);
        assert_eq!(failed.code(), 1, "{fault}: {}", failed.stderr());
        assert!(
            failed.stderr().contains(said),
            "{fault}: {}",
            failed.stderr()
        );
    }
    assert!(
        !project.root.join(".supercov/runs").exists() || {
            let runs = project.supercov(&["runs", "--json"]).json();
            runs["data"]["runs"].as_array().is_some_and(Vec::is_empty)
        }
    );
    // A command that could not start, its run's state not recorded either:
    // the failure is still the one said.
    let unrecorded = run("command-setpgid,failed-run-state");
    assert_eq!(unrecorded.code(), 1, "{}", unrecorded.stderr());
    assert!(
        unrecorded.stderr().contains("could not spawn node"),
        "{}",
        unrecorded.stderr()
    );
    // Interrupted once, then made again; no /proc to list descriptors from;
    // a copy taken by another run, with no server to prepare again.
    for fault in ["ready-read=1", "watchdog-read=1", "proc-fd", "copy-moved"] {
        let passed = run(fault);
        assert_eq!(passed.code(), 0, "{fault}: {}", passed.stderr());
    }
    // A source tool on PATH, and a PATH for its launchers that is not text:
    // the command keeps the PATH it had.
    project.write_script("bin/biome", "#!/bin/sh\n");
    let path = format!(
        "{}:{}",
        project.root.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let kept = project.supercov_with(
        &["--", "node", "--test"],
        &[
            ("SUPERCOV_TEST_FAULT", "launcher-path-text"),
            ("PATH", &path),
        ],
    );
    assert_eq!(kept.code(), 0, "{}", kept.stderr());
    reached("launcher-path-text", &kept.stderr());

    // A Go run passes the command's output on itself, so its relay is what
    // the remaining faults reach.
    let Some(go) = go() else {
        eprintln!("skipping the relay's faults: no Go toolchain found");
        return;
    };
    let project = Project::empty("start-faults-go");
    project.write("go.mod", "module example.com/w\n\ngo 1.22\n");
    project.write(
        "w.go",
        "package w\n\nfunc Double(n int) int { return n * 2 }\n",
    );
    project.write(
        "w_test.go",
        "package w\n\nimport \"testing\"\n\nfunc TestDouble(t *testing.T) {\n\tif Double(2) != 4 {\n\t\tt.Fatal(\"double\")\n\t}\n}\n",
    );
    project.git(&["init", "-q"]);
    // Output and errors to one file: one pipe for both, or, when it cannot be
    // made, one for each, and the file gets both either way.
    for fault in ["output-pipe", "output-clone"] {
        let log = project.root.join(format!("log-{fault}.txt"));
        let file = std::fs::File::create(&log).unwrap();
        let status = std::process::Command::new(env!("CARGO_BIN_EXE_supercov"))
            .args(["--", go, "test", "-v", "./..."])
            .current_dir(&project.root)
            .env("SUPERCOV_TEST_FAULT", fault)
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .status()
            .unwrap();
        let written = std::fs::read_to_string(&log).unwrap();
        assert!(status.success(), "{fault}: {written}");
        reached(fault, &written);
        assert!(
            written.contains("--- PASS: TestDouble"),
            "{fault}: {written}"
        );
    }
    // On a terminal: one that cannot be sized or opened leaves the command
    // the streams it had; an interrupted read is read again.
    for fault in [
        "terminal-size",
        "openpty",
        "terminal-clone",
        "terminal-read=1",
    ] {
        let terminal = common::OnTerminal::start_with(
            &project,
            &["--", go, "test", "-v", "./..."],
            &[("SUPERCOV_TEST_FAULT", fault)],
        );
        let (code, seen) = terminal.finish();
        assert_eq!(code, 0, "{fault}: {seen}");
        reached(fault, &seen);
        assert!(seen.contains("PASS"), "{fault}: {seen}");
    }
}

/// A Rust run whose evidence cannot be published says so, and leaves none
/// of its work behind, even when its copy cannot be found again to clear.
#[test]
fn a_rust_run_that_cannot_publish_says_so_and_clears_its_work() {
    let project = Project::empty("rust-unpublished");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn half(x: i32) -> i32 { x / 2 }\n#[cfg(test)]\nmod tests { #[test] fn h() { assert_eq!(super::half(4), 2); } }\n",
    );
    project.git(&["init", "-q"]);
    for fault in [
        "published-copy-differs",
        "published-copy-differs,cleanup-copy-path",
    ] {
        let failed =
            project.supercov_with(&["--", "cargo", "test"], &[("SUPERCOV_TEST_FAULT", fault)]);
        assert_ne!(failed.code(), 0, "{fault}: {}", failed.stderr());
        assert!(
            failed
                .stderr()
                .contains("evidence changed during publication"),
            "{fault}: {}",
            failed.stderr()
        );
    }
    let runs = project.supercov(&["runs", "--json"]).json();
    assert_eq!(
        runs["data"]["runs"].as_array().map(Vec::len),
        Some(0),
        "{runs}"
    );
}

/// A trash too large to empty within the command's patience is left to the
/// sweep it started, which empties it after the command has gone.
#[test]
fn a_sweep_that_outlasts_the_command_empties_the_trash_after_it() {
    let project = Project::cart("sweep-outlasts");
    let big = project.root.join(".supercov/.trash/big");
    std::fs::create_dir_all(&big).unwrap();
    for i in 0..20_000 {
        std::fs::write(big.join(i.to_string()), b"x").unwrap();
    }
    let measured = project.supercov_with(
        &["--", "node", "--test"],
        &[("SUPERCOV_TEST_FAULT", "sweep-impatient,sweep-outlasts")],
    );
    assert_eq!(measured.code(), 0, "{}", measured.stderr());
    assert!(
        measured.stderr().contains("test fault at sweep-outlasts"),
        "{}",
        measured.stderr()
    );
    let started = std::time::Instant::now();
    while big.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(120),
            "the sweep the command left never emptied the trash"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn the_parent_death_watchdog_kills_its_parents_group_when_its_input_ends() {
    use std::io::{Read, Write};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, Stdio};
    // A shell leading its own process group stands in for the command: the
    // watchdog says it is ready on descriptor 3, watches its input, and when
    // the input ends kills the shell's group. Only the watchdog holds the
    // input; the shell's own is moved away.
    let watch = |script: &str| {
        Command::new("sh")
            .args(["-c", script, env!("CARGO_BIN_EXE_supercov")])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let killed = |mut shell: std::process::Child| {
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = shell.try_wait().unwrap() {
                return status.signal();
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "the watchdog never ended its parent's group"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    // Input that sends a byte, then ends.
    let mut shell = watch(
        "exec 4<&0 0</dev/null; \"$0\" __watch-process-group 3>&1 <&4 4<&- & exec 4<&-; sleep 30",
    );
    let mut ready = [0_u8; 1];
    shell.stdout.take().unwrap().read_exact(&mut ready).unwrap();
    assert_eq!(ready, [1]);
    let mut input = shell.stdin.take().unwrap();
    input.write_all(b"x").unwrap();
    drop(input);
    assert_eq!(killed(shell), Some(libc::SIGKILL));
    // Input it cannot read counts as ended.
    let shell =
        watch("exec 0</dev/null; \"$0\" __watch-process-group 3>/dev/null 0>/dev/null & sleep 30");
    assert_eq!(killed(shell), Some(libc::SIGKILL));
    // With nowhere to say it is ready, it refuses, and kills nothing.
    let shell = watch("\"$0\" __watch-process-group 3>&- </dev/null; echo \"watchdog $?\"");
    let output = shell.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "watchdog 125\n");
}

#[cfg(unix)]
#[test]
fn ctrl_c_during_a_go_python_ruby_or_jvm_run_exits_as_the_signal_and_publishes_nothing() {
    // As a JavaScript run does: 128 plus the signal, the signal named, and no
    // run recorded for a suite that never finished.
    let interrupt = |project: &Project, args: &[&str], waiting: &str| {
        let mut terminal = common::OnTerminal::start(project, args);
        terminal.wait_for(waiting);
        terminal.press("\u{3}");
        let (code, seen) = terminal.finish();
        assert_eq!(code, 130, "{args:?}\n{seen}");
        assert!(
            seen.contains("the test command was interrupted by SIGINT; no run was published"),
            "{args:?}\n{seen}"
        );
        let runs = project.supercov(&["runs", "--json"]).json();
        assert_eq!(
            runs["data"]["runs"].as_array().map(Vec::len),
            Some(0),
            "{runs}"
        );
    };
    if let Some(go) = go() {
        let project = Project::empty("interrupt-go");
        project.write("go.mod", "module example.com/w\n\ngo 1.22\n");
        project.write(
            "w.go",
            "package w\n\nfunc Double(n int) int { return n * 2 }\n",
        );
        project.write("w_test.go", "package w\n\nimport (\n\t\"fmt\"\n\t\"testing\"\n\t\"time\"\n)\n\nfunc TestWaits(t *testing.T) {\n\tif Double(2) != 4 {\n\t\tt.Fatal(\"double\")\n\t}\n\tfmt.Println(\"waiting now\")\n\ttime.Sleep(60 * time.Second)\n}\n");
        project.git(&["init", "-q"]);
        interrupt(&project, &["--", go, "test", "-v", "./..."], "waiting now");
    }
    let project = Project::empty("interrupt-python");
    project.write("pkg/__init__.py", "");
    project.write("pkg/w.py", "def double(n):\n    return n * 2\n");
    project.write("tests/__init__.py", "");
    project.write("tests/test_w.py", "import time\nimport unittest\nfrom pkg.w import double\n\nclass W(unittest.TestCase):\n    def test_waits(self):\n        self.assertEqual(double(2), 4)\n        print('waiting now', flush=True)\n        time.sleep(60)\n");
    project.git(&["init", "-q"]);
    interrupt(
        &project,
        &[
            "--", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".",
        ],
        "waiting now",
    );
    if let Some(ruby) = ruby() {
        let project = Project::empty("interrupt-ruby");
        project.write("lib/w.rb", "module W\n  def self.double(n) = n * 2\nend\n");
        project.write("test/w_test.rb", "require 'minitest/autorun'\nrequire 'w'\n\nclass WTest < Minitest::Test\n  def test_waits\n    assert_equal 4, W.double(2)\n    puts 'waiting now'\n    $stdout.flush\n    sleep 60\n  end\nend\n");
        project.git(&["init", "-q"]);
        interrupt(
            &project,
            &["--", &ruby, "-Ilib", "test/w_test.rb"],
            "waiting now",
        );
    }
    let maven = std::process::Command::new("mvn")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if maven {
        let project = Project::empty("interrupt-jvm");
        project.write("pom.xml", "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>example</groupId>\n  <artifactId>w</artifactId>\n  <version>1.0</version>\n  <properties>\n    <maven.compiler.source>17</maven.compiler.source>\n    <maven.compiler.target>17</maven.compiler.target>\n    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n  </properties>\n  <dependencies>\n    <dependency>\n      <groupId>org.junit.jupiter</groupId>\n      <artifactId>junit-jupiter</artifactId>\n      <version>5.10.2</version>\n      <scope>test</scope>\n    </dependency>\n  </dependencies>\n  <build>\n    <plugins>\n      <plugin>\n        <groupId>org.apache.maven.plugins</groupId>\n        <artifactId>maven-surefire-plugin</artifactId>\n        <version>3.2.5</version>\n      </plugin>\n    </plugins>\n  </build>\n</project>\n");
        project.write(
            "src/main/java/w/W.java",
            "package w;\n\npublic class W {\n    public static int twice(int n) {\n        return n * 2;\n    }\n}\n",
        );
        project.write("src/test/java/w/WTest.java", "package w;\n\nimport static org.junit.jupiter.api.Assertions.assertEquals;\n\nimport org.junit.jupiter.api.Test;\n\nclass WTest {\n    @Test\n    void waits() throws InterruptedException {\n        assertEquals(4, W.twice(2));\n        System.out.println(\"waiting now\");\n        System.out.flush();\n        Thread.sleep(60_000);\n    }\n}\n");
        project.git(&["init", "-q"]);
        interrupt(&project, &["--", "mvn", "test"], "waiting now");
    }
}

/// Each query, run once for every file operation it makes with that one
/// failing (`SUPERCOV_TEST_FAULT=fs@N`): it may refuse, but says why and never
/// panics, and the run its fault no longer reaches answers as usual.
#[test]
fn a_query_whose_file_operation_fails_says_so_and_never_panics() {
    let project = Project::cart("query-faults");
    project.supercov(&["--", "node", "--test"]).succeeds();
    for query in [
        &["runs"][..],
        &["runs", "latest"],
        &["runs", "latest", "gaps"],
        &["runs", "latest", "files"],
        &["runs", "latest", "file", "src/cart.js"],
        &["runs", "latest", "tests", "affected"],
        &["report", "--no-open", "--output", "faults.html"],
    ] {
        let mut nth = 0;
        loop {
            nth += 1;
            let fault = format!("fs@{nth}");
            let ran = project.supercov_with(query, &[("SUPERCOV_TEST_FAULT", &fault)]);
            let said = ran.stderr();
            assert!(
                !said.contains("panicked at") && ran.code() != 101,
                "{query:?} with {fault} panicked:\n{said}"
            );
            if !said.contains("[supercov] test fault at fs") {
                ran.succeeds();
                break;
            }
            assert!(nth < 2_000, "{query:?} kept reaching faults");
        }
        assert!(nth > 1, "{query:?} made no file operation");
    }
}

/// `--exact-attribution` goes before the command, and the run is the
/// command's as any other.
#[test]
fn exact_attribution_runs_the_command_after_it() {
    let project = Project::cart("exact-attribution");
    let measured = project
        .supercov(&["--exact-attribution", "--", "node", "--test"])
        .succeeds();
    contains_all(&measured, &["pass 3", "[coverage] evidence:"]);
}

/// A measured run of `command`, once for each place in the code that makes
/// a file operation, with that place's first one failing
/// (`SUPERCOV_TEST_FAULT=fs#N`): it may stop, but says why and never panics,
/// and the next run of the project, with no fault, recovers whatever the
/// stopped one left and succeeds. Four projects `write` lays out share the
/// places. The most places a run reached.
fn fail_each_place_of_a_run(label: &str, write: fn(&Project), command: &[&str]) -> usize {
    const PROJECTS: usize = 4;
    let reached = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for first in 1..=PROJECTS {
            let reached = &reached;
            scope.spawn(move || {
                let project = Project::empty(&format!("{label}-{first}"));
                write(&project);
                project.git(&["init", "-q"]);
                project.commit("fixture");
                let mut nth = first;
                loop {
                    let fault = format!("fs#{nth}");
                    let ran = project.supercov_with(command, &[("SUPERCOV_TEST_FAULT", &fault)]);
                    let said = ran.stderr();
                    assert!(
                        !said.contains("panicked at") && ran.code() != 101,
                        "{label} with {fault} panicked:\n{said}"
                    );
                    if !said.contains("[supercov] test fault at fs") {
                        break;
                    }
                    reached.fetch_max(nth, std::sync::atomic::Ordering::Relaxed);
                    nth += PROJECTS;
                }
                project.supercov(command).succeeds();
            });
        }
    });
    reached.into_inner()
}

#[test]
fn a_run_whose_file_operation_fails_anywhere_says_so_and_recovers() {
    let javascript = fail_each_place_of_a_run(
        "run-faults-js",
        |project| {
            project.write(
                "package.json",
                r#"{ "name": "cart", "private": true, "type": "module" }"#,
            );
            project.write(
                "src/cart.js",
                "export const total = (a, b) => (a && b ? a + b : 0);\n",
            );
            project.write(
                "test/cart.test.js",
                "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { total } from '../src/cart.js';\ntest('adds', () => assert.equal(total(1, 2), 3));\n",
            );
        },
        &["--", "node", "--test"],
    );
    let python = fail_each_place_of_a_run(
        "run-faults-py",
        |project| {
            project.write("pkg/__init__.py", "");
            project.write("pkg/w.py", "def double(n):\n    return n * 2 if n else 0\n");
            project.write("tests/__init__.py", "");
            project.write(
                "tests/test_w.py",
                "import unittest\nfrom pkg.w import double\n\nclass W(unittest.TestCase):\n    def test_doubles(self):\n        self.assertEqual(double(2), 4)\n",
            );
        },
        &[
            "--", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".",
        ],
    );
    assert!(javascript > 50 && python > 50, "{javascript} {python}");
}

#[cfg(unix)]
#[test]
fn a_ruby_run_whose_file_operation_fails_anywhere_says_so_and_recovers() {
    let Some(ruby) = ruby() else {
        return;
    };
    let command = ["--", ruby.as_str(), "-Ilib", "test/w_test.rb"];
    let reached = fail_each_place_of_a_run(
        "run-faults-rb",
        |project| {
            project.write(
                "lib/w.rb",
                "module W\n  def self.double(n) = n ? n * 2 : 0\nend\n",
            );
            project.write(
                "test/w_test.rb",
                "require 'minitest/autorun'\nrequire 'w'\n\nclass WTest < Minitest::Test\n  def test_doubles\n    assert_equal 4, W.double(2)\n  end\nend\n",
            );
        },
        &command,
    );
    assert!(reached > 50, "{reached}");
}

#[test]
fn a_go_run_whose_file_operation_fails_anywhere_says_so_and_recovers() {
    let Some(go) = go() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let reached = fail_each_place_of_a_run(
        "run-faults-go",
        |project| {
            project.write("go.mod", "module example.com/w\n\ngo 1.22\n");
            project.write(
                "w.go",
                "package w\n\nfunc Double(n int) int {\n\tif n == 0 {\n\t\treturn 0\n\t}\n\treturn n * 2\n}\n",
            );
            project.write(
                "w_test.go",
                "package w\n\nimport \"testing\"\n\nfunc TestDouble(t *testing.T) {\n\tif Double(2) != 4 {\n\t\tt.Fatal(\"double\")\n\t}\n}\n",
            );
        },
        &["--", go, "test", "./..."],
    );
    assert!(reached > 50, "{reached}");
}

/// A crate with a unit test and a doctest.
fn small_crate(project: &Project) {
    project.write(
        "Cargo.toml",
        "[package]\nname = \"w\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    project.write(
        "src/lib.rs",
        "/// ```\n/// assert_eq!(w::double(1), 2);\n/// ```\npub fn double(n: u32) -> u32 {\n    if n == 0 { 0 } else { n * 2 }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n        assert_eq!(super::double(2), 4);\n    }\n}\n",
    );
    project.write(".gitignore", "target/\n");
}

#[test]
fn a_rust_run_whose_file_operation_fails_anywhere_says_so_and_recovers() {
    let reached =
        fail_each_place_of_a_run("run-faults-rs", small_crate, &["--", "cargo", "test", "-q"]);
    assert!(reached > 50, "{reached}");
    // And through nextest, where it is installed.
    let nextest = std::process::Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if nextest {
        let reached = fail_each_place_of_a_run(
            "run-faults-nextest",
            small_crate,
            &["--", "cargo", "nextest", "run"],
        );
        assert!(reached > 50, "{reached}");
    }
}

/// `runs patch` reads the change from git: each git call failing in turn
/// (`SUPERCOV_TEST_FAULT=git@N`) is named and ends the command with `2`, never
/// a panic, and the call no fault reaches answers as usual.
#[test]
fn a_patch_whose_git_call_fails_says_so() {
    let project = Project::cart("patch-git-faults");
    project.edit(
        "src/cart.js",
        "  return items.map((item) => item.name).join(\", \");",
        "  const names = items.map((item) => item.name);\n  return names.join(\", \");",
    );
    project.write("src/new.js", "export const fresh = 1;\n");
    project.measure(&[]);
    for query in [
        &["runs", "patch", "--base", "HEAD", "--min-lines", "100"][..],
        &["runs", "patch", "--base", "HEAD", "--annotate", "github"],
        &["runs", "patch", "--base", "HEAD", "--json"],
    ] {
        let mut nth = 0;
        loop {
            nth += 1;
            let fault = format!("git@{nth}");
            let ran = project.supercov_with(query, &[("SUPERCOV_TEST_FAULT", &fault)]);
            let said = format!("{}{}", ran.stdout(), ran.stderr());
            assert!(
                !said.contains("panicked at") && ran.code() != 101,
                "{query:?} with {fault} panicked:\n{said}"
            );
            if !ran.stderr().contains("[supercov] test fault at git") {
                break;
            }
            assert_ne!(ran.code(), 0, "{query:?} with {fault}:\n{said}");
            assert!(said.contains("git"), "{query:?} with {fault}:\n{said}");
        }
        assert!(nth > 2, "{query:?} ran no git");
    }
}

/// A Maven project with one class and its test.
fn small_maven_project(project: &Project) {
    project.write("pom.xml", "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>example</groupId>\n  <artifactId>w</artifactId>\n  <version>1.0</version>\n  <properties>\n    <maven.compiler.source>17</maven.compiler.source>\n    <maven.compiler.target>17</maven.compiler.target>\n    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n  </properties>\n  <dependencies>\n    <dependency>\n      <groupId>org.junit.jupiter</groupId>\n      <artifactId>junit-jupiter</artifactId>\n      <version>5.10.2</version>\n      <scope>test</scope>\n    </dependency>\n  </dependencies>\n  <build>\n    <plugins>\n      <plugin>\n        <groupId>org.apache.maven.plugins</groupId>\n        <artifactId>maven-surefire-plugin</artifactId>\n        <version>3.2.5</version>\n      </plugin>\n    </plugins>\n  </build>\n</project>\n");
    project.write(
        "src/main/java/w/W.java",
        "package w;\n\npublic class W {\n    public static int twice(int n) {\n        return n == 0 ? 0 : n * 2;\n    }\n}\n",
    );
    project.write(
        "src/test/java/w/WTest.java",
        "package w;\n\nimport static org.junit.jupiter.api.Assertions.assertEquals;\n\nimport org.junit.jupiter.api.Test;\n\nclass WTest {\n    @Test\n    void twice() {\n        assertEquals(4, W.twice(2));\n    }\n}\n",
    );
    project.write(".gitignore", "target/\n");
}

#[test]
fn a_maven_run_whose_file_operation_fails_anywhere_says_so_and_recovers() {
    let maven = std::process::Command::new("mvn")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if !maven {
        eprintln!("skipping: no Maven found");
        return;
    }
    let reached = fail_each_place_of_a_run(
        "run-faults-mvn",
        small_maven_project,
        &["--", "mvn", "-q", "test"],
    );
    assert!(reached > 50, "{reached}");
}

/// A Python test whose process ends during it reported no outcome, but what
/// it ran was recorded: the test is in the run, failed, credited with that.
#[test]
fn a_python_test_whose_process_ends_during_it_is_failed_and_credited() {
    let project = Project::empty("python-dies");
    project.write("pkg/__init__.py", "");
    project.write("pkg/w.py", "def double(n):\n    return n * 2 if n else 0\n");
    project.write("tests/__init__.py", "");
    project.write(
        "tests/test_w.py",
        "import os\nimport unittest\nfrom pkg.w import double\n\nclass W(unittest.TestCase):\n    def test_a_passes(self):\n        self.assertEqual(double(2), 4)\n\n    def test_b_dies(self):\n        double(0)\n        os._exit(3)\n",
    );
    project.git(&["init", "-q"]);
    project.commit("fixture");
    project.supercov(&[
        "--", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".",
    ]);
    let dies = project
        .supercov(&["runs", "latest", "test", "test_b_dies"])
        .succeeds();
    contains_all(&dies, &["outcome failed", "line: pkg/w.py:2"]);
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["Passed      1", "Failed      1"]);
}

/// The same of a Ruby test: its process ends during it, after a test that
/// passed.
#[cfg(unix)]
#[test]
fn a_ruby_test_whose_process_ends_during_it_is_failed_and_credited() {
    let Some(ruby) = ruby() else {
        return;
    };
    let project = Project::empty("ruby-dies");
    project.write(
        "lib/w.rb",
        "module W\n  def self.double(n) = n ? n * 2 : 0\nend\n",
    );
    project.write(
        "test/w_test.rb",
        "require 'minitest/autorun'\nrequire 'w'\n\nclass WTest < Minitest::Test\n  i_suck_and_my_tests_are_order_dependent!\n\n  def test_a_passes\n    assert_equal 4, W.double(2)\n  end\n\n  def test_b_dies\n    W.double(nil)\n    exit!(3)\n  end\nend\n",
    );
    project.git(&["init", "-q"]);
    project.commit("fixture");
    project.supercov(&["--", &ruby, "-Ilib", "test/w_test.rb"]);
    // Its probes were read; its lines are not, as Ruby gives them when a
    // test ends and this one never did.
    let dies = project
        .supercov(&["runs", "latest", "test", "test_b_dies"])
        .succeeds();
    contains_all(&dies, &["outcome failed", "1 decision"]);
    let summary = project.supercov(&["runs", "latest"]).succeeds();
    contains_all(&summary, &["Passed      1", "Failed      1"]);
}

/// Started in a directory removed since, every command says it cannot
/// read where it is, not a panic or a run it never found.
#[cfg(unix)]
#[test]
fn a_command_started_in_a_removed_directory_says_so() {
    let project = Project::cart("removed-cwd");
    project.measure(&[]);
    let commands: &[&[&str]] = &[
        &["runs", "latest"],
        &["runs", "latest", "gaps"],
        &["runs", "latest", "file", "src/cart.js"],
        &["runs", "latest", "source", "src/cart.js"],
        &["runs", "latest", "assertions"],
        &["runs", "patch", "--base", "HEAD"],
        &["runs", "patch", "--base", "HEAD", "--json"],
        &["runs", "check"],
        &["runs", "check", "--lines", "50"],
        &["runs", "clean"],
        &["report", "--no-open"],
        &["report", "latest", "--no-open"],
        &["diff", "latest", "latest"],
        &["merge"],
        &["import", "coverage.lcov", "--test", "imported"],
        &["quality", "gaps"],
        &["security"],
        &["security", "--run", "latest"],
        &["quality", "--run", "latest"],
        &["tests", "affected"],
        &["--", "node", "--test"],
        &["runs", "latest", "--json"],
        &["runs", "latest", "gaps", "--json"],
    ];
    for command in commands {
        let gone = project
            .root
            .join(format!("gone-{}", command.join("-").replace('/', "_")));
        std::fs::create_dir(&gone).unwrap();
        let script = format!(
            "cd \"{}\" && rmdir \"{}\" && exec \"$0\" \"$@\"",
            gone.display(),
            gone.display()
        );
        let ran = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .arg(env!("CARGO_BIN_EXE_supercov"))
            .args(*command)
            .output()
            .unwrap();
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&ran.stdout),
            String::from_utf8_lossy(&ran.stderr)
        );
        assert!(!said.contains("panicked at"), "{command:?}: {said}");
        assert!(!ran.status.success(), "{command:?} succeeded: {said}");
    }
}

/// With every index refused, and with no room for any reply, each query
/// says what failed: a fault the engine injects stands in for an index from
/// another version and for an answer too large to send.
#[test]
fn each_query_says_so_when_its_index_or_its_reply_fails() {
    let project = Project::cart("index-faults");
    project.measure(&[]);
    let older = project.latest();
    project.measure(&[]);
    let commands: &[&[&str]] = &[
        &["runs", "latest"],
        &["runs", "latest", "--json"],
        &["runs", "latest", "gaps"],
        &["runs", "latest", "gaps", "--json"],
        &["runs", "latest", "file", "src/cart.js", "--json"],
        &["runs", "latest", "line", "src/cart.js:1", "--json"],
        &["runs", "latest", "tests", "--json"],
        &["runs", "latest", "scope", "--json"],
        &["runs", "latest", "summary", "--json"],
        &["runs", "--json"],
        &["runs", "patch", "--json"],
        &["runs", "check"],
        &["runs", "check", "--json"],
        &["runs", "check", "--lines", "100", "--json"],
        &["runs", "latest", "assertions", "--kind", "unit"],
        &[
            "runs",
            "latest",
            "assertions",
            "assess",
            "--dry-run",
            "--kind",
            "unit",
        ],
        &["tests", "affected", "--kind", "unit"],
        &["runs", "latest", "gaps", "--kind", "unit", "--json"],
        &["diff", &older, "latest", "--json"],
        &["diff", &older, "latest"],
        &["report", "--no-open"],
    ];
    for fault in ["coverage-index", "reply-too-large"] {
        let mut reached = 0;
        for command in commands {
            let ran = project.supercov_with(command, &[("SUPERCOV_TEST_FAULT", fault)]);
            let said = format!("{}{}", ran.stdout(), ran.stderr());
            assert!(!said.contains("panicked at"), "{fault} {command:?}: {said}");
            reached += usize::from(said.contains(&format!("test fault at {fault}")));
        }
        assert!(
            reached * 3 > commands.len(),
            "{fault} reached {reached} commands"
        );
    }
}

/// A line asked about after its file was cut short is past its end; after
/// the file was removed, it is answered from the run, with no source.
#[test]
fn a_line_whose_file_was_cut_or_removed_since_the_run_is_refused_or_answered_without_text() {
    let project = Project::cart("line-gone");
    project.measure(&[]);
    // Cut short, the line is past its end: no line to answer for.
    project.write("src/cart.js", "export const total = 0;\n");
    let short = project
        .supercov(&["runs", "latest", "line", "src/cart.js:3", "--json"])
        .json();
    assert_eq!(short["error"]["code"], "INVALID_ARGUMENT", "{short}");
    // Removed, a line the run kept no text of (a closing brace) has none.
    std::fs::remove_file(project.root.join("src/cart.js")).unwrap();
    let gone = project
        .supercov(&["runs", "latest", "line", "src/cart.js:5", "--json"])
        .json();
    assert_eq!(gone["ok"], true, "{gone}");
    assert!(gone["data"]["source"].is_null(), "{gone}");
}

/// `import` with nothing to import names what it needs.
#[test]
fn import_with_no_arguments_names_what_it_needs() {
    let project = Project::cart("import-nothing");
    let ran = project.supercov(&["import"]);
    assert_ne!(ran.code(), 0, "{}", ran.stderr());
    assert!(ran.stderr().contains("import"), "{}", ran.stderr());
}
