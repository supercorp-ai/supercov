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
            "Security: 1 of 1 files flagged, 0 clean; 1 confirmed at a line.",
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
            "nosuchdir: No such file or directory",
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
fn a_fixture_imported_by_a_tsconfig_alias_is_shown_with_the_test() {
    let project = Project::empty("alias-fixture");
    project.write(
        "package.json",
        r##"{ "name": "alias", "type": "module", "imports": { "#f/*": "./test/fixtures/*" } }"##,
    );
    // tsconfig.json allows comments; the alias is read past them.
    project.write(
        "tsconfig.json",
        "{\n  // fixtures are imported by alias\n  \"compilerOptions\": { \"paths\": { \"#f/*\": [\"./test/fixtures/*\"] } }\n}\n",
    );
    project.write(
        "src/cart.js",
        "export function total(items) {\n  return items.reduce((sum, item) => sum + item.price, 0);\n}\n",
    );
    project.write(
        "test/fixtures/cart.js",
        "export const items = [{ price: 2 }, { price: 3 }]; // the fixture\n",
    );
    project.write(
        "test/cart.test.js",
        "import test from \"node:test\";\nimport assert from \"node:assert/strict\";\nimport { total } from \"../src/cart.js\";\nimport { items } from \"#f/cart.js\";\n\ntest(\"totals the fixture\", () => assert.equal(total(items), 5));\n",
    );
    project.git(&["init", "-q"]);
    let run = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let helpers = |seen: &common::Seen| {
        seen.lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(_, _, request)| request["state"]["test"]["name"] == "totals the fixture")
            .map(|(_, _, request)| request["state"]["helpers"].clone())
            .expect("the test was asked about")
    };
    let shown = helpers(&seen);
    assert!(
        shown["test/fixtures/cart.js"]
            .as_str()
            .is_some_and(|text| text.contains("[{ price: 2 }, { price: 3 }]")),
        "{shown}"
    );
    // Without the alias the import names nothing Supercov can find.
    std::fs::remove_file(project.root.join("tsconfig.json")).unwrap();
    let run = project.measure(&[]);
    let (base, seen) = gateway(MODEL, answer_yes);
    project
        .supercov_with(&["runs", &run, "assertions", "assess"], &through(&base))
        .succeeds();
    let shown = helpers(&seen);
    assert!(shown.get("test/fixtures/cart.js").is_none(), "{shown}");
}
