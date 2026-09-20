//! The whole Go lifecycle, on a module with more than one package.
//!
//! The frontend test proves instrumented Go compiles and reports the truth for
//! one file. This proves the parts around it: that the project is copied and
//! rewritten without touching the author's tree, that each package's test
//! binary writes evidence of its own and the run merges them, that decisions
//! numbered in different files do not collide, and that a run is published.
//!
//! Needs `go` on PATH and skips without it.

use std::path::{Path, PathBuf};
use std::process::Command;

use supercov_engine::go_run::{DirectGoRunRequest, run_direct_go};

mod common;

fn go_binary() -> Option<PathBuf> {
    // Homebrew's Go is not on a non-login shell's PATH on macOS.
    for candidate in ["go", "/opt/homebrew/bin/go", "/usr/local/go/bin/go"] {
        let path = PathBuf::from(candidate);
        if Command::new(&path)
            .arg("version")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return Some(path);
        }
    }
    None
}

fn temporary(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "supercov-go-run-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Two packages, each with a decision of its own. If decisions were numbered
/// per file they would both be decision 0 and would share condition state.
fn fixture(root: &Path) {
    write(root, "go.mod", "module example.com/app\n\ngo 1.22\n");
    write(
        root,
        "auth/auth.go",
        "package auth\n\nfunc Allow(admin bool, active bool) bool {\n\tif admin && active {\n\t\treturn true\n\t}\n\treturn false\n}\n",
    );
    write(
        root,
        "auth/auth_test.go",
        "package auth\n\nimport \"testing\"\n\nfunc TestAllows(t *testing.T) {\n\tif !Allow(true, true) {\n\t\tt.Fatal(\"admin and active should be allowed\")\n\t}\n}\n\nfunc TestDenies(t *testing.T) {\n\tif Allow(false, true) {\n\t\tt.Fatal(\"a non-admin should be denied\")\n\t}\n}\n",
    );
    write(
        root,
        "billing/billing.go",
        "package billing\n\nfunc Charge(amount int, trial bool) int {\n\tif amount > 0 || trial {\n\t\treturn amount\n\t}\n\treturn 0\n}\n",
    );
    write(
        root,
        "billing/billing_test.go",
        "package billing\n\nimport \"testing\"\n\nfunc TestCharges(t *testing.T) {\n\tif Charge(5, false) != 5 {\n\t\tt.Fatal(\"a positive amount is charged\")\n\t}\n}\n\nfunc TestSkipsWhenAsked(t *testing.T) {\n\tt.Skip(\"not today\")\n}\n",
    );
}

#[test]
fn a_multi_package_module_runs_and_publishes_what_each_test_reached() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    // The lifecycle shells out to the command as given, so `go` has to be
    // reachable by the name the command uses.
    let root = temporary("multi-package");
    fixture(&root);

    let before = std::fs::read_to_string(root.join("auth/auth.go")).unwrap();
    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-multi".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };

    // The author's tree is untouched: instrumentation happens on a copy.
    assert_eq!(
        std::fs::read_to_string(root.join("auth/auth.go")).unwrap(),
        before,
        "the project's own sources must come back exactly as they went in"
    );

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.packages, 2, "one test binary per package");
    assert_eq!(result.source_files, 2);
    assert_eq!(result.tests, 4);

    let archive = result.run_directory.join("evidence.raw.gz");
    let named = supercov_engine::evidence_archive::read_archive(&archive)
        .expect("published archive")
        .into_iter()
        .map(|entry| entry.path)
        .collect::<Vec<_>>();
    // A published run carries what the numbers mean, who produced them, the
    // obligations they are measured against, and one record per test.
    for required in ["coverage-model.json", "frontend.json", "manifest.json"] {
        assert!(named.iter().any(|path| path == required), "{named:?}");
    }
    assert_eq!(
        named
            .iter()
            .filter(|path| path.ends_with("mcdc.json"))
            .count(),
        4,
        "one record per test: {named:?}"
    );
}

#[test]
fn decisions_in_different_packages_keep_their_own_condition_state() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    // `auth` evaluates `admin && active` and `billing` evaluates
    // `amount > 0 || trial`. Numbered per file they would both be decision 0,
    // sharing one slot in the runtime's state array: the vectors recorded
    // would describe neither expression, and MC/DC would be judged from an
    // evaluation that never happened.
    let root = temporary("decision-state");
    fixture(&root);

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-decisions".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };

    let archive = result.run_directory.join("evidence.raw.gz");
    assert!(archive.exists(), "{}", archive.display());
    let entries =
        supercov_engine::evidence_archive::read_archive(&archive).expect("published archive");
    let manifest = String::from_utf8(
        entries
            .iter()
            .find(|entry| entry.path == "manifest.json")
            .expect("a published run carries its manifest")
            .contents
            .clone(),
    )
    .expect("utf-8");
    // Two decisions, one per package, each with its own two conditions.
    let decisions = manifest.matches("\"conditions\"").count();
    assert_eq!(
        decisions, 2,
        "each package's decision is its own obligation:\n{manifest}"
    );
    assert!(manifest.contains("auth/auth.go"), "{manifest}");
    assert!(manifest.contains("billing/billing.go"), "{manifest}");
}

/// A go.work repository has several modules and no module at its root. Each
/// needs a runtime of its own, because an import resolves against the module
/// that names it and one module cannot import a package inside another.
#[test]
fn every_module_of_a_workspace_is_measured_and_merged() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("workspace");
    write(
        root.as_path(),
        "go.work",
        "go 1.22\n\nuse (\n\t./core\n\t./app\n)\n",
    );
    write(
        root.as_path(),
        "core/go.mod",
        "module example.com/core\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "core/calc.go",
        "package core\n\nfunc Allow(admin, active bool) bool {\n\tif admin && active {\n\t\treturn true\n\t}\n\treturn false\n}\n",
    );
    write(
        root.as_path(),
        "core/calc_test.go",
        "package core\n\nimport \"testing\"\n\nfunc TestAllow(t *testing.T) {\n\tif !Allow(true, true) {\n\t\tt.Fatal(\"expected allow\")\n\t}\n}\n",
    );
    write(
        root.as_path(),
        "app/go.mod",
        "module example.com/app\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "app/greet.go",
        "package app\n\nfunc Hi(loud bool) string {\n\tif loud {\n\t\treturn \"HI\"\n\t}\n\treturn \"hi\"\n}\n",
    );
    // One line, brace to brace: an announcement inserted after the brace with
    // nothing following it would run into this statement and stop being Go.
    write(
        root.as_path(),
        "app/greet_test.go",
        "package app\n\nimport \"testing\"\n\nfunc TestHi(t *testing.T) { if Hi(true) != \"HI\" { t.Fatal(\"expected HI\") } }\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![
            go.display().to_string(),
            "test".into(),
            "./core/...".into(),
            "./app/...".into(),
        ],
        run_id: "run-go-workspace".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.packages, 2);
    assert_eq!(result.source_files, 2);
    assert_eq!(result.tests, 2);

    let entries = supercov_engine::evidence_archive::read_archive(
        &result.run_directory.join("evidence.raw.gz"),
    )
    .expect("published archive");
    let manifest = String::from_utf8(
        entries
            .into_iter()
            .find(|entry| entry.path == "manifest.json")
            .expect("manifest")
            .contents,
    )
    .expect("utf-8");
    assert!(manifest.contains("core/calc.go"), "{manifest}");
    assert!(manifest.contains("app/greet.go"), "{manifest}");
    std::fs::remove_dir_all(root).ok();
}

/// A directory with a go.mod that the workspace does not name is a different
/// module. `go test ./...` walks straight past it, so instrumenting it puts
/// obligations in the denominator no test can reach and writes a probe file
/// importing a runtime that module cannot resolve -- which stops a build that
/// worked before Supercov was asked to measure it.
#[test]
fn a_nested_module_does_not_break_the_build_around_it() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("nested");
    write(
        root.as_path(),
        "go.mod",
        "module example.com/root\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "top.go",
        "package root\n\nfunc Top(x bool) int {\n\tif x {\n\t\treturn 1\n\t}\n\treturn 0\n}\n",
    );
    write(
        root.as_path(),
        "top_test.go",
        "package root\n\nimport \"testing\"\n\nfunc TestTop(t *testing.T) { if Top(true) != 1 { t.Fatal(\"no\") } }\n",
    );
    write(
        root.as_path(),
        "sub/go.mod",
        "module example.com/sub\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "sub/sub.go",
        "package sub\n\nfunc Inner(x bool) int {\n\tif x {\n\t\treturn 2\n\t}\n\treturn 0\n}\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-nested".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };
    assert_eq!(result.exit_code, 0, "the build must still work");
    // Only the root module's source: the nested one is not part of this run.
    assert_eq!(result.source_files, 1);
    assert_eq!(result.tests, 1);
    std::fs::remove_dir_all(root).ok();
}

/// A Go test that calls `t.Parallel()` is deliberately left unattributed:
/// probes are a store into one shared array, so what it reaches while others
/// run beside it cannot be credited to it. The declaration says its coverage
/// still counts run-wide — and until there was a record to carry it, nothing
/// did. A suite written the way Go suites are written measured almost nothing
/// and was told nothing about why: samber/lo calls `t.Parallel()` 1606 times
/// across 548 tests, and reported 0.26% of its statements.
#[test]
fn a_parallel_tests_coverage_counts_even_though_no_test_can_claim_it() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("parallel");
    write(
        root.as_path(),
        "go.mod",
        "module example.com/par\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "lib.go",
        "package par\n\nfunc Serial(x bool) int {\n\tif x {\n\t\treturn 1\n\t}\n\treturn 0\n}\n\nfunc Parallel(x bool) int {\n\tif x {\n\t\treturn 2\n\t}\n\treturn 0\n}\n\nfunc Never(x bool) int {\n\tif x {\n\t\treturn 3\n\t}\n\treturn 0\n}\n",
    );
    write(
        root.as_path(),
        "lib_test.go",
        "package par\n\nimport \"testing\"\n\nfunc TestSerial(t *testing.T) {\n\tif Serial(true) != 1 {\n\t\tt.Fatal(\"serial\")\n\t}\n}\n\nfunc TestParallel(t *testing.T) {\n\tt.Parallel()\n\tif Parallel(true) != 2 {\n\t\tt.Fatal(\"parallel\")\n\t}\n}\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-parallel".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };
    assert_eq!(result.exit_code, 0);
    // Only the serial one can be named.
    assert_eq!(result.tests, 1);

    let records = supercov_engine::evidence_archive::read_archive(
        &result.run_directory.join("evidence.raw.gz"),
    )
    .expect("published archive")
    .into_iter()
    .filter(|entry| entry.path.ends_with("mcdc.json"))
    .map(|entry| String::from_utf8(entry.contents).expect("utf-8"))
    .collect::<Vec<_>>();

    let background = records
        .iter()
        .find(|record| record.contains("\"role\":\"background\""))
        .expect("the run carries what no test could claim");
    // What the parallel test reached is in it, and what nothing reached is not.
    assert!(background.contains("go:statement:"), "{background}");
    // It counts as coverage only because the run passed. A failing run cannot
    // say which of this came from the test that failed, so it would not.
    assert!(background.contains("\"status\":\"passed\""), "{background}");
    // And the serial test still claims its own, rather than everything being
    // swept into the background record.
    assert!(
        records
            .iter()
            .any(|record| record.contains("TestSerial") && record.contains("go:statement:")),
        "{records:?}"
    );
    std::fs::remove_dir_all(root).ok();
}

/// `t.Parallel()` is idiomatic, and a package where every test calls it is
/// ordinary Go. Every such test is deliberately unattributed — what it reached
/// while others ran beside it cannot be credited to it — and the run was then
/// discarded for having no attributed test, so nothing was published at all:
/// not the lines, not the branches, not the MC/DC. The evidence existed the
/// whole time; it simply had no owner. One reporter's codebase calls
/// `t.Parallel()` in 935 of 1144 test files, leaving 2.7% of its tests
/// measurable.
#[test]
fn a_package_whose_tests_are_all_parallel_still_publishes_what_it_measured() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("all-parallel");
    write(
        root.as_path(),
        "go.mod",
        "module example.com/allpar\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "lib.go",
        "package allpar\n\nfunc Classify(n int) string {\n\tif n > 0 {\n\t\treturn \"positive\"\n\t}\n\treturn \"non-positive\"\n}\n\nfunc Unreached() string {\n\treturn \"never\"\n}\n",
    );
    write(
        root.as_path(),
        "lib_test.go",
        "package allpar\n\nimport \"testing\"\n\nfunc TestPositive(t *testing.T) {\n\tt.Parallel()\n\tif Classify(1) != \"positive\" {\n\t\tt.Fatal(\"want positive\")\n\t}\n}\n\nfunc TestNonPositive(t *testing.T) {\n\tt.Parallel()\n\tif Classify(-1) != \"non-positive\" {\n\t\tt.Fatal(\"want non-positive\")\n\t}\n}\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-all-parallel".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "a run that measured a whole package was discarded: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };
    assert_eq!(result.exit_code, 0);
    // Nothing can be attributed, and the run says so rather than implying the
    // suite is empty: two tests ran and neither could be named for what it
    // reached.
    assert_eq!(result.tests, 0);
    assert_eq!(
        result.unattributed, 2,
        "both tests ran and neither is named"
    );

    let records = supercov_engine::evidence_archive::read_archive(
        &result.run_directory.join("evidence.raw.gz"),
    )
    .expect("published archive")
    .into_iter()
    .filter(|entry| entry.path.ends_with("mcdc.json"))
    .map(|entry| String::from_utf8(entry.contents).expect("utf-8"))
    .collect::<Vec<_>>();
    let background = records
        .iter()
        .find(|record| record.contains("\"role\":\"background\""))
        .unwrap_or_else(|| panic!("the run carries what no test could claim: {records:?}"));
    assert!(background.contains("go:statement:"), "{background}");
    assert!(background.contains("\"status\":\"passed\""), "{background}");
    std::fs::remove_dir_all(root).ok();
}

/// A suite that runs but reaches nothing is still a run that measured nothing,
/// and publishing it would report a floor it never established. The parallel
/// case is published because the evidence exists without an owner; this one
/// has no evidence at all.
#[test]
fn a_suite_that_reaches_no_measured_code_is_still_refused() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("reaches-nothing");
    write(
        root.as_path(),
        "go.mod",
        "module example.com/nothing\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "lib.go",
        "package nothing\n\nfunc Unreached() string {\n\treturn \"never\"\n}\n",
    );
    write(
        root.as_path(),
        "lib_test.go",
        "package nothing\n\nimport \"testing\"\n\nfunc TestNothing(t *testing.T) {\n\tt.Parallel()\n}\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-reaches-nothing".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let error = run_direct_go(&request, &mut diagnostics)
        .expect_err("a run with nothing in it is not published");
    assert!(error.contains("measured nothing"), "{error}");
    std::fs::remove_dir_all(root).ok();
}

/// Go 1.26 let `new` take a value. tree-sitter-go cannot express that, so
/// every source file holding one was dropped from the denominator with a
/// single warning — and the run then reported success over the files that were
/// left. `Go coverage: 1 test(s) across 0 source file(s)` with exit 0 is the
/// worst shape a wrong number can take, because an agent loop reads it as a
/// pass.
#[test]
fn a_source_file_using_go_1_26_new_is_measured_like_any_other() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("new-value");
    write(
        root.as_path(),
        "go.mod",
        "module example.com/newvalue\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "lib.go",
        "package newvalue\n\nfunc Plain(n int) *int {\n\tif n > 0 {\n\t\treturn &n\n\t}\n\treturn new(int)\n}\n",
    );
    // The value form only compiles on Go 1.26 and the suite has to run on the
    // toolchain that is here, so it goes in a file the toolchain skips and
    // discovery does not. What is being proved is the half that is broken on
    // every version: that Supercov can read the construct, and that the file
    // holding it is in the denominator rather than dropped out of it.
    write(
        root.as_path(),
        "boxed.go",
        "//go:build ignore\n\npackage newvalue\n\nfunc Boxed() *string {\n\treturn new(\"hello\")\n}\n\nfunc Sum(a, b int) *int {\n\treturn new(a + b)\n}\n",
    );
    write(
        root.as_path(),
        "lib_test.go",
        "package newvalue\n\nimport \"testing\"\n\nfunc TestPlain(t *testing.T) {\n\tif *Plain(1) != 1 {\n\t\tt.Fatal(\"plain\")\n\t}\n}\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-new-value".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };
    assert_eq!(result.exit_code, 0);
    assert_eq!(
        result.source_files, 2,
        "both files are in the denominator, the one using the construct included"
    );
    assert_eq!(result.unparseable_sources, 0);
    let diagnostics = String::from_utf8_lossy(&diagnostics);
    assert!(!diagnostics.contains("could not parse"), "{diagnostics}");
    std::fs::remove_dir_all(root).ok();
}

/// A source file Supercov cannot read is a hole in the denominator. One that
/// leaves nothing readable at all is not a hole, it is the whole floor: the
/// run would report coverage of zero obligations, which satisfies every
/// threshold, with exit 0. So it is refused, the way a project with no test
/// package already is.
#[test]
fn a_project_whose_sources_none_parse_is_refused_rather_than_reported_green() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("no-source-parses");
    write(
        root.as_path(),
        "go.mod",
        "module example.com/unreadable\n\ngo 1.22\n",
    );
    // Unparseable to Supercov and to the Go compiler alike; what matters is
    // that the run does not report success over a denominator of nothing.
    write(
        root.as_path(),
        "lib.go",
        "package unreadable\n\nfunc f( {\n",
    );
    write(
        root.as_path(),
        "lib_test.go",
        "package unreadable\n\nimport \"testing\"\n\nfunc TestNothing(t *testing.T) {}\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-unreadable".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let error = run_direct_go(&request, &mut diagnostics)
        .expect_err("a run with nothing in its denominator is not published");
    assert!(
        error.contains("denominator of nothing"),
        "and it names the file it could not read: {error}"
    );
    assert!(error.contains("lib.go"), "{error}");
    std::fs::remove_dir_all(root).ok();
}

/// One file that does not parse among several that do is a hole, not a floor:
/// the run is published, and the hole travels with the number it was taken out
/// of. It reaches the stored run as a blocking limitation, and the count comes
/// back with the result so the line that reports success can say it.
#[test]
fn an_unparseable_file_among_readable_ones_is_published_and_counted() {
    let Some(go) = go_binary() else {
        common::skip("go", "no Go toolchain found");
        return;
    };
    let root = temporary("partial-hole");
    write(
        root.as_path(),
        "go.mod",
        "module example.com/partial\n\ngo 1.22\n",
    );
    write(
        root.as_path(),
        "good.go",
        "package partial\n\nfunc Classify(n int) string {\n\tif n > 0 {\n\t\treturn \"positive\"\n\t}\n\treturn \"non-positive\"\n}\n",
    );
    // A build constraint the toolchain honours and discovery does not, which
    // is how a file can be unreadable to Supercov without the package failing
    // to build — the shape a generated or platform-specific file takes.
    write(
        root.as_path(),
        "gen.go",
        "//go:build ignore\n\npackage partial\n\nfunc Broken( {\n",
    );
    write(
        root.as_path(),
        "lib_test.go",
        "package partial\n\nimport \"testing\"\n\nfunc TestPositive(t *testing.T) {\n\tif Classify(1) != \"positive\" {\n\t\tt.Fatal(\"want positive\")\n\t}\n}\n",
    );

    let request = DirectGoRunRequest {
        root: root.clone(),
        command: vec![go.display().to_string(), "test".into(), "./...".into()],
        run_id: "run-go-partial-hole".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_go(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "one hole is not a reason to discard a measured run: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.source_files, 1, "the readable file is measured");
    assert_eq!(result.unparseable_sources, 1, "and the hole is counted");

    // Counted in the result is not enough on its own: the run has to carry it,
    // so `supercov runs latest` can still name the file long after the build
    // log is gone.
    let declaration = supercov_engine::evidence_archive::read_archive(
        &result.run_directory.join("evidence.raw.gz"),
    )
    .expect("published archive")
    .into_iter()
    .find(|entry| entry.path.ends_with("manifest.json"))
    .map(|entry| String::from_utf8(entry.contents).expect("utf-8"))
    .expect("the run declares what it measured against");
    assert!(declaration.contains("file-does-not-parse"), "{declaration}");
    assert!(declaration.contains("gen.go"), "{declaration}");
    std::fs::remove_dir_all(root).ok();
}
