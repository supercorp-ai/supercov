//! End-to-end proof that instrumented Go compiles, runs, and reports the truth.
//!
//! The parser tests hold that rewritten source still parses. Only the Go
//! toolchain can say it compiles, that `go vet` is happy with it, and that the
//! evidence it produces says what actually happened. This test needs `go` on
//! PATH and skips without it, so a machine without the toolchain does not fail
//! a suite it cannot run.

use std::path::{Path, PathBuf};
use std::process::Command;

use supercov_engine::go_instrumenter::{RUNTIME_IMPORT, build_go_obligations, rewrite};
use supercov_engine::go_test_harness::{
    instrument_test_file, probe_array_file, synthesized_harness,
};
use supercov_engine::owned_evidence::{
    OwnedRunInputs, OwnedTestOutcome, build_frontend_run, go_coverage_model, go_declaration,
    read_evidence,
};

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
        "supercov-go-{label}-{}-{}",
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

const SOURCE: &str = r#"package main

func classify(a int, b bool) string {
	if a > 10 && b {
		return "big"
	}
	for i := 0; i < a; i++ {
		_ = i
	}
	switch a {
	case 0:
		return "zero"
	}
	return "small"
}
"#;

/// Ordinary Go tests. Nothing here mentions Supercov: binding them to their
/// evidence is the harness generator's job, and if it needed help from the
/// author it would not be usable.
const TESTS: &str = r#"package main

import "testing"

func TestBig(t *testing.T) {
	if got := classify(20, true); got != "big" {
		t.Fatalf("got %q", got)
	}
}

func TestZero(t *testing.T) {
	if got := classify(0, false); got != "zero" {
		t.Fatalf("got %q", got)
	}
}

func TestSkipped(t *testing.T) {
	t.Skip("nothing to do here")
}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Vector {
    evaluated: u64,
    values: u64,
    outcome: bool,
}

struct Evidence {
    global: Vec<u64>,
    tests: std::collections::BTreeMap<String, std::collections::BTreeMap<usize, u64>>,
    statuses: std::collections::BTreeMap<String, String>,
    test_vectors: std::collections::BTreeMap<String, Vec<(usize, Vector)>>,
    decisions: Vec<(u8, Vec<Vector>)>,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn u64(&mut self) -> u64 {
        let value =
            u64::from_le_bytes(self.bytes[self.offset..self.offset + 8].try_into().unwrap());
        self.offset += 8;
        value
    }

    fn text(&mut self, length: usize) -> String {
        let value = String::from_utf8(self.bytes[self.offset..self.offset + length].to_vec())
            .expect("utf-8 test name");
        self.offset += length;
        value
    }
}

fn decode(bytes: &[u8]) -> Evidence {
    let mut cursor = Cursor { bytes, offset: 0 };
    let count = cursor.u64() as usize;
    let global = (0..count).map(|_| cursor.u64()).collect::<Vec<_>>();
    let test_count = cursor.u64() as usize;
    let mut tests = std::collections::BTreeMap::new();
    let mut statuses = std::collections::BTreeMap::new();
    let mut test_vectors = std::collections::BTreeMap::new();
    for _ in 0..test_count {
        let length = cursor.u64() as usize;
        let name = cursor.text(length);
        let status_length = cursor.u64() as usize;
        statuses.insert(name.clone(), cursor.text(status_length));
        // Which runner announced it. Go has one, so its records leave this
        // empty and the engine reads the frontend's own.
        let runner_length = cursor.u64() as usize;
        assert_eq!(runner_length, 0, "Go declares a single runner");
        let entries = cursor.u64() as usize;
        let mut probes = std::collections::BTreeMap::new();
        for _ in 0..entries {
            let index = cursor.u64() as usize;
            probes.insert(index, cursor.u64());
        }
        let vector_entries = cursor.u64() as usize;
        let mut vectors = Vec::new();
        for _ in 0..vector_entries {
            let decision = cursor.u64() as usize;
            vectors.push((decision, unpack(cursor.u64())));
        }
        tests.insert(name.clone(), probes);
        test_vectors.insert(name, vectors);
    }
    let decision_count = cursor.u64() as usize;
    let mut decisions = Vec::new();
    for _ in 0..decision_count {
        let width = cursor.u64() as u8;
        let keys = cursor.u64() as usize;
        let seen = (0..keys).map(|_| unpack(cursor.u64())).collect::<Vec<_>>();
        decisions.push((width, seen));
    }
    Evidence {
        global,
        tests,
        statuses,
        test_vectors,
        decisions,
    }
}

/// The runtime packs one evaluation into a word: evaluated mask, then the
/// values shifted above it, then the outcome.
fn unpack(key: u64) -> Vector {
    Vector {
        evaluated: key & 0xFF_FFFF,
        values: (key >> 24) & 0xFF_FFFF,
        outcome: (key >> 48) & 1 == 1,
    }
}

#[test]
fn instrumented_go_compiles_and_reports_what_actually_ran() {
    let Some(go) = go_binary() else {
        eprintln!("[go-frontend] skipped: no Go toolchain on PATH");
        return;
    };
    let root = temporary("frontend");
    let runtime =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/go/supercov/supercov.go");
    write(&root, "go.mod", "module example.com/probe\n\ngo 1.22\n");
    write(
        &root,
        "supercov/supercov.go",
        &std::fs::read_to_string(runtime).expect("runtime source"),
    );

    let mut next = 0;
    let mut decisions = 0;
    let obligations = build_go_obligations("classify.go", SOURCE, &mut next, &mut decisions)
        .expect("obligations");
    let local = "example.com/probe/supercov";
    let instrumented = rewrite(SOURCE, &obligations.edits).replace(RUNTIME_IMPORT, local);
    write(&root, "classify.go", &instrumented);
    write(
        &root,
        "supercov_probes.go",
        &probe_array_file("main", "__supercov", local, 256),
    );

    // The test file goes in as the author wrote it and comes out bound to its
    // evidence, which is the whole point of the harness generator.
    let harness = instrument_test_file(TESTS, "__supercov", "evidence.bin").expect("harness");
    assert_eq!(harness.tests, ["TestBig", "TestZero", "TestSkipped"]);
    assert!(!harness.declares_test_main);
    write(
        &root,
        "main_test.go",
        &rewrite(TESTS, &harness.edits).replace(RUNTIME_IMPORT, local),
    );
    write(
        &root,
        "supercov_generated_test.go",
        &synthesized_harness(
            "main",
            "__supercov",
            local,
            256,
            &obligations.decision_widths,
            "evidence.bin",
            false,
        ),
    );

    // `go vet` is stricter than the compiler and catches shapes that compile
    // but that no Go author would accept in their tree.
    let vet = Command::new(&go)
        .arg("vet")
        .arg("./...")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        vet.status.success(),
        "go vet rejected instrumented source:\n{}\n--- source ---\n{instrumented}",
        String::from_utf8_lossy(&vet.stderr)
    );
    let test = Command::new(&go)
        .arg("test")
        .arg("./...")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        test.status.success(),
        "go test failed:\n{}{}",
        String::from_utf8_lossy(&test.stdout),
        String::from_utf8_lossy(&test.stderr)
    );

    let evidence = decode(&std::fs::read(root.join("evidence.bin")).expect("evidence"));

    // How a test ended travels with what it covered. Without it the report
    // cannot tell a passing test's coverage from a failing one's, and a
    // failing test's coverage is not evidence that anything works.
    assert_eq!(evidence.statuses["TestBig"], "passed");
    assert_eq!(evidence.statuses["TestZero"], "passed");
    assert_eq!(evidence.statuses["TestSkipped"], "skipped");

    let big = &evidence.tests["TestBig"];
    let zero = &evidence.tests["TestZero"];

    // Attribution is exact rather than approximate: neither test claims the
    // other's evidence.
    assert!(!big.is_empty() && !zero.is_empty());
    assert_ne!(big, zero);

    // MC/DC needs vectors, not per-condition bits. The decisive distinction is
    // that a short-circuited operand is *unevaluated*, which is a different
    // fact from being false -- and the only thing that lets independence be
    // judged at all.
    assert_eq!(
        obligations.decision_widths,
        [2],
        "one decision of two conditions"
    );
    let (width, vectors) = &evidence.decisions[0];
    assert_eq!(*width, 2);

    // TestZero: `a > 10` was false, so `b` was never evaluated. If wrapping had
    // broken short-circuiting, bit 1 would be set here and the instrumented
    // program would have computed something the original never would.
    let short_circuited = Vector {
        evaluated: 0b01,
        values: 0b00,
        outcome: false,
    };
    // TestBig: both evaluated, both true.
    let both_true = Vector {
        evaluated: 0b11,
        values: 0b11,
        outcome: true,
    };
    assert!(vectors.contains(&short_circuited), "{vectors:?}");
    assert!(vectors.contains(&both_true), "{vectors:?}");
    // Two evaluations, two distinct vectors: recording de-duplicates rather
    // than accumulating one entry per loop iteration.
    assert_eq!(vectors.len(), 2, "{vectors:?}");

    // And each vector belongs to the test that established it. MC/DC evidence
    // that cannot name the test which proved independence is far less use.
    assert_eq!(evidence.test_vectors["TestBig"], [(0, both_true)]);
    assert_eq!(evidence.test_vectors["TestZero"], [(0, short_circuited)]);

    // The run-wide totals are the union of the per-test records, so a probe any
    // test reached is reached run-wide, and the union invents nothing of its
    // own.
    let reached = big
        .keys()
        .chain(zero.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    for probe in &reached {
        assert_eq!(evidence.global[*probe], 2, "probe {probe} was reached");
    }
    assert_eq!(
        evidence.global.iter().filter(|value| **value != 0).count(),
        reached.len()
    );

    // And the engine's own decoder turns that transport into a coverage report
    // whose numbers match the obligations the manifest declared. A frontend
    // that records correctly but cannot be read is not yet a frontend.
    let raw = std::fs::read(root.join("evidence.bin")).expect("evidence");
    let decoded = read_evidence(&raw).expect("decode");
    let run = build_frontend_run(OwnedRunInputs {
        declaration: go_declaration(),
        environment: "go",
        manifest: &obligations.manifest,
        probes: &obligations.probes,
        evidence: &decoded,
        outcomes: &[
            OwnedTestOutcome {
                name: "TestBig".into(),
                runner: String::new(),
                package: "example.com/probe".into(),
                file: Some("main_test.go".into()),
                status: "passed".into(),
            },
            OwnedTestOutcome {
                name: "TestZero".into(),
                runner: String::new(),
                package: "example.com/probe".into(),
                file: Some("main_test.go".into()),
                status: "passed".into(),
            },
        ],
        run_id: "run_go",
        generated_at: "now",
        test_exit_code: 0,
        coverage_model: go_coverage_model(),
    })
    .expect("frontend run");
    assert_eq!(run.tests, 2);
    let report =
        supercov_engine::coverage_report::analyze_coverage_results(&run.request).expect("report");
    let summary = &report.filters.passed.summary;

    // Every obligation the manifest declared is in the denominator, and the
    // covered counts are the ones the two tests actually reached.
    // Every measured statement the manifest declared is in the denominator,
    // and only what the two tests reached is in the numerator.
    assert!(summary.lines.total > 0);
    assert!(summary.lines.covered > 0);
    assert!(
        summary.lines.covered < summary.lines.total,
        "neither test enters the loop body, so something must stay uncovered: {}/{}",
        summary.lines.covered,
        summary.lines.total
    );
    assert_eq!(summary.conditions, 2, "one decision of two conditions");
    assert!(
        summary.branches.total > 0,
        "the if, the loop and the switch all declared arms"
    );
    // TestBig returns before the switch and TestZero never enters the loop, so
    // some arms are genuinely untaken: a report that showed everything covered
    // would be the wrong answer.
    assert!(
        summary.branches.covered < summary.branches.total,
        "{}/{}",
        summary.branches.covered,
        summary.branches.total
    );

    std::fs::remove_dir_all(root).unwrap();
}
