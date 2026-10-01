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
