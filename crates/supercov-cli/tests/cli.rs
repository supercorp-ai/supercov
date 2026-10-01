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
            "lines 3  statements 2  functions 2  branch outcomes 1  MC/DC conditions 4",
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
            "no witness pair shows `express` independently changing the decision result",
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
    let scope = project.supercov(&["runs", "latest", "scope"]).succeeds();
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

    let impossible = project.supercov(&["runs", "latest", "minimize"]).exits(2);
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
    let guides = project.supercov(&["docs"]).succeeds();
    contains_all(
        &guides,
        &["Bundled guides", "getting-started", "assertions", "cli"],
    );
    let guide = project.supercov(&["docs", "assertions"]).succeeds();
    assert!(guide.starts_with("# Assertion coverage"), "{guide}");
    let missing = project.supercov(&["docs", "no-such-guide"]);
    assert_ne!(missing.code(), 0);
    let version = project.supercov(&["--version"]).succeeds();
    assert!(version.contains(env!("CARGO_PKG_VERSION")), "{version}");
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
            "Usage: supercov merge <run-id> <run-id>",
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
        (vec!["merge"], "Usage: supercov merge"),
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
    project.supercov(&["--", "cargo", "test"]).succeeds();
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
    // A feature is not the target's to settle, so `extra` stays counted.
    assert_eq!(lines, [16, 17], "{gaps}");
    assert!(!lines.contains(&absent));
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
            "[covered elsewhere: 22; nowhere: 12]",
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
            "Supercov could not parse this file (Expected `:` but found `decimal` at line 2, column 11)",
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
        let refused = project.supercov(&args);
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
        let refused = project.supercov(&args);
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
        let refused = project.supercov(&args);
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
            "npx supercov runs --offset 1 --limit 1".to_owned(),
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
            vec!["runs", "latest", "scope", "--limit", "1"],
            format!("runs {run} scope --offset 1 --limit 1"),
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
        for character in next.trim_start_matches("npx supercov ").chars() {
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
fn a_lock_another_process_holds_is_named_and_one_left_behind_is_taken_over() {
    let project = Project::cart("locks");
    let lock = project.root.join(".supercov/locks/active.json");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let owner = |pid: u32| {
        format!(r#"{{"runId":"run_other","pid":{pid},"startedAt":"2026-10-02T00:00:00Z"}}"#)
    };
    // This test process is alive, so its pid holds the lock.
    std::fs::write(&lock, owner(std::process::id())).unwrap();
    let held = project.supercov(&["--", "node", "--test"]);
    assert_ne!(held.code(), 0);
    assert!(
        held.stderr()
            .contains("coverage run run_other is already active in this project"),
        "{}",
        held.stderr()
    );
    // A lock still being written has no owner yet.
    std::fs::write(&lock, "{").unwrap();
    let acquiring = project.supercov(&["--", "node", "--test"]);
    assert_ne!(acquiring.code(), 0);
    assert!(
        acquiring
            .stderr()
            .contains("a coverage run is currently acquiring the project lock"),
        "{}",
        acquiring.stderr()
    );
    // One whose owner is gone, or that never got one and is old, is taken over.
    std::fs::write(&lock, owner(u32::MAX - 1)).unwrap();
    project.supercov(&["--", "node", "--test"]).succeeds();
    std::fs::write(&lock, "{").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();
    project.supercov(&["--", "node", "--test"]).succeeds();

    // The trash sweeper keeps the same rules for its own lock.
    let trash = project.root.join(".supercov/.trash");
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
            vec!["runs", "latest", "test", "total", "--limit", "0"],
            "--limit must be a positive integer",
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
        .supercov(&["runs", "latest", "scope", "--limit", "50"])
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
        let refused = project.supercov(&args);
        assert_ne!(refused.code(), 0, "{args:?}");
        assert!(
            refused.stderr().contains(message),
            "{args:?}: {}",
            refused.stderr()
        );
    }
}
