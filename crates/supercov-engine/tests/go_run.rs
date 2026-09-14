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
