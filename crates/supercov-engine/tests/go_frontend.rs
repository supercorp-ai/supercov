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

const TESTS: &str = r#"package main

import (
	"testing"

	__supercov "example.com/probe/supercov"
)

func TestBig(t *testing.T) {
	defer __supercov.EnterTest("TestBig")()
	if got := classify(20, true); got != "big" {
		t.Fatalf("got %q", got)
	}
}

func TestZero(t *testing.T) {
	defer __supercov.EnterTest("TestZero")()
	if got := classify(0, false); got != "zero" {
		t.Fatalf("got %q", got)
	}
}

func TestMain(m *testing.M) {
	__supercov.Arm(256)
	m.Run()
	if err := __supercov.Write("evidence.bin"); err != nil {
		panic(err)
	}
}
"#;

struct Evidence {
    global: Vec<u64>,
    tests: std::collections::BTreeMap<String, std::collections::BTreeMap<usize, u64>>,
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
    for _ in 0..test_count {
        let length = cursor.u64() as usize;
        let name = cursor.text(length);
        let entries = cursor.u64() as usize;
        let mut probes = std::collections::BTreeMap::new();
        for _ in 0..entries {
            let index = cursor.u64() as usize;
            probes.insert(index, cursor.u64());
        }
        tests.insert(name, probes);
    }
    Evidence { global, tests }
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
    let obligations = build_go_obligations("classify.go", SOURCE, &mut next).expect("obligations");
    let instrumented =
        rewrite(SOURCE, &obligations.edits).replace(RUNTIME_IMPORT, "example.com/probe/supercov");
    write(&root, "classify.go", &instrumented);
    write(&root, "main_test.go", TESTS);

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
    let probe_of =
        |predicate: &dyn Fn(&supercov_engine::go_instrumenter::GoProbeTarget) -> bool| {
            obligations
                .probes
                .values()
                .find(|probe| predicate(&probe.target))
                .map(|probe| probe.id as usize)
                .expect("probe")
        };
    use supercov_engine::go_instrumenter::GoProbeTarget;
    let first_condition =
        probe_of(&|target| matches!(target, GoProbeTarget::Condition { index: 0, .. }));
    let second_condition =
        probe_of(&|target| matches!(target, GoProbeTarget::Condition { index: 1, .. }));

    let big = &evidence.tests["TestBig"];
    let zero = &evidence.tests["TestZero"];

    // The decisive one: `b` is only evaluated when `a > 10` is true. If the
    // wrapper broke short-circuiting, TestZero would have observed it too --
    // and the instrumented program would behave differently from the original.
    assert_eq!(
        big.get(&second_condition).copied(),
        Some(2),
        "TestBig saw b as true"
    );
    assert_eq!(
        zero.get(&second_condition),
        None,
        "TestZero must never evaluate b: `a > 10` was false, so `&&` short-circuits"
    );

    // Both outcomes of the first condition were seen, by different tests.
    assert_eq!(big.get(&first_condition).copied(), Some(2));
    assert_eq!(zero.get(&first_condition).copied(), Some(1));
    // Globally that condition is fully exercised, which is what the report adds up.
    assert_eq!(
        evidence.global[first_condition], 3,
        "true and false both observed"
    );

    // Attribution is exact rather than approximate: neither test claims the
    // other's evidence.
    assert!(!big.is_empty() && !zero.is_empty());
    assert_ne!(big, zero);

    std::fs::remove_dir_all(root).unwrap();
}
