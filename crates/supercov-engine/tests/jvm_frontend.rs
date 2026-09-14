//! End-to-end proof that instrumented Java compiles, runs, and reports truly.
//!
//! The parser tests hold that rewritten source still parses. Only a real JDK
//! can say it compiles, that `javac -Xlint:all` is content with it, and that
//! the evidence says what actually happened. Skips without a JDK rather than
//! failing a suite the machine cannot run.

use std::path::{Path, PathBuf};
use std::process::Command;

use supercov_engine::jvm_instrumenter::{JvmLanguage, build_jvm_obligations, rewrite};
use supercov_engine::owned_evidence::{
    OwnedRunInputs, OwnedTestOutcome, build_frontend_run, jvm_coverage_model, jvm_declaration,
    read_evidence,
};

mod common;

fn temporary(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "supercov-jvm-{label}-{}-{}",
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

const SOURCE: &str = r#"public class Classify {
    public static String classify(int a, boolean b) {
        if (a > 10 && b) {
            return "big";
        }
        if (a == 0) return "zero";
        return "small";
    }
}
"#;

#[test]
fn instrumented_java_compiles_and_reports_what_actually_ran() {
    let (Some(javac), Some(java)) = (common::tool("javac"), common::tool("java")) else {
        common::skip("jvm", "no JDK found");
        return;
    };
    let root = temporary("frontend");
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/jvm/com/supercorp/supercov/Supercov.java");
    write(
        &root,
        "com/supercorp/supercov/Supercov.java",
        &std::fs::read_to_string(runtime).expect("runtime source"),
    );

    let mut next = 0;
    let mut decisions = 0;
    let obligations = build_jvm_obligations(
        "Classify.java",
        SOURCE,
        JvmLanguage::Java,
        &mut next,
        &mut decisions,
    )
    .expect("obligations");
    write(&root, "Classify.java", &rewrite(SOURCE, &obligations.edits));

    // The harness a generator will produce; written by hand here so the
    // instrumenter is what is under test.
    let probes = obligations.probes.len() + 1;
    let widths = obligations
        .decision_widths
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    write(
        &root,
        "Harness.java",
        &format!(
            r#"import com.supercorp.supercov.Supercov;

public class Harness {{
    public static void main(String[] args) throws Exception {{
        Supercov.arm({probes}, new int[] {{{widths}}});
        Supercov.enterTest("testZero");
        Classify.classify(0, false);
        Supercov.exitTest();
        Supercov.enterTest("testBig");
        Classify.classify(20, true);
        Supercov.exitTest();
        Supercov.write("evidence.bin");
    }}
}}
"#
        ),
    );

    // -Xlint:all is stricter than the compiler's default and rejects shapes a
    // Java author would not accept in their tree.
    let compile = Command::new(&javac)
        .args([
            "-Xlint:all",
            "-d",
            ".",
            "com/supercorp/supercov/Supercov.java",
            "Classify.java",
            "Harness.java",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "javac rejected instrumented source:\n{}\n--- source ---\n{}",
        String::from_utf8_lossy(&compile.stderr),
        rewrite(SOURCE, &obligations.edits)
    );
    let run = Command::new(&java)
        .args(["-cp", ".", "Harness"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "the instrumented program failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    // The JVM transport is the same shape as Go's, so the same reader decodes
    // it. One format for every language is what keeps the report honest.
    let evidence = read_evidence(&std::fs::read(root.join("evidence.bin")).expect("evidence"))
        .expect("decode");
    assert_eq!(evidence.tests.len(), 2);
    assert_eq!(evidence.widths, [2]);

    let vectors = &evidence.decision_vectors[0];
    let unpack = |key: u64| {
        (
            key & 0xFF_FFFF,
            (key >> 24) & 0xFF_FFFF,
            (key >> 48) & 1 == 1,
        )
    };
    let seen = vectors.iter().map(|key| unpack(*key)).collect::<Vec<_>>();
    // testZero: `a > 10` false, so `b` was never evaluated. If wrapping had
    // broken short-circuiting, bit 1 would be set and the instrumented program
    // would have computed something the original never would.
    assert!(seen.contains(&(0b01, 0b00, false)), "{seen:?}");
    // testBig: both evaluated, both true.
    assert!(seen.contains(&(0b11, 0b11, true)), "{seen:?}");

    // Attribution is exact: neither test claims the other's evidence.
    let zero = &evidence
        .tests
        .iter()
        .find(|t| t.name == "testZero")
        .expect("testZero")
        .probes;
    let big = &evidence
        .tests
        .iter()
        .find(|t| t.name == "testBig")
        .expect("testBig")
        .probes;
    assert!(!zero.is_empty() && !big.is_empty());
    assert_ne!(zero, big);

    std::fs::remove_dir_all(root).unwrap();
}

const KOTLIN_SOURCE: &str = r#"object Classify {
    @JvmStatic
    fun classify(a: Int, b: Boolean): String {
        if (a > 10 && b) {
            return "big"
        }
        if (a == 0) return "zero"
        return "small"
    }
}
"#;

fn kotlinc() -> Option<PathBuf> {
    ["/opt/homebrew/bin/kotlinc", "kotlinc"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| {
            Command::new(path)
                .arg("-version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
}

#[test]
fn instrumented_kotlin_compiles_and_reports_what_actually_ran() {
    // Parsing correctly and compiling correctly are different claims, and only
    // the Kotlin compiler can settle the second.
    let (Some(kotlinc), Some(javac), Some(java)) =
        (kotlinc(), common::tool("javac"), common::tool("java"))
    else {
        common::skip("jvm", "no Kotlin toolchain found");
        return;
    };
    let root = temporary("kotlin");
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/jvm/com/supercorp/supercov/Supercov.java");
    write(
        &root,
        "com/supercorp/supercov/Supercov.java",
        &std::fs::read_to_string(runtime).expect("runtime source"),
    );
    let compiled = Command::new(&javac)
        .args(["-d", "classes", "com/supercorp/supercov/Supercov.java"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let mut next = 0;
    let mut decisions = 0;
    let obligations = build_jvm_obligations(
        "Classify.kt",
        KOTLIN_SOURCE,
        JvmLanguage::Kotlin,
        &mut next,
        &mut decisions,
    )
    .expect("obligations");
    let instrumented = rewrite(KOTLIN_SOURCE, &obligations.edits);
    write(&root, "Classify.kt", &instrumented);

    let probes = obligations.probes.len() + 1;
    let widths = obligations
        .decision_widths
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    write(
        &root,
        "Harness.kt",
        &format!(
            r#"import com.supercorp.supercov.Supercov

fun main() {{
    Supercov.arm({probes}, intArrayOf({widths}))
    Supercov.enterTest("testZero")
    Classify.classify(0, false)
    Supercov.exitTest()
    Supercov.enterTest("testBig")
    Classify.classify(20, true)
    Supercov.exitTest()
    Supercov.write("evidence.bin")
}}
"#
        ),
    );

    let compile = Command::new(&kotlinc)
        .args([
            "-cp",
            "classes",
            "-d",
            "classes",
            "Classify.kt",
            "Harness.kt",
            "-nowarn",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "kotlinc rejected instrumented source:\n{}\n--- source ---\n{instrumented}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&java)
        .args(["-cp", "classes", "HarnessKt"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "the instrumented program failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let evidence = read_evidence(&std::fs::read(root.join("evidence.bin")).expect("evidence"))
        .expect("decode");
    assert_eq!(evidence.widths, [2]);
    let seen = evidence.decision_vectors[0]
        .iter()
        .map(|key| {
            (
                key & 0xFF_FFFF,
                (key >> 24) & 0xFF_FFFF,
                (key >> 48) & 1 == 1,
            )
        })
        .collect::<Vec<_>>();
    // Kotlin short-circuits `&&` exactly as Java does, and the wrapper must
    // not change that: `b` is unevaluated when `a > 10` is false.
    assert!(seen.contains(&(0b01, 0b00, false)), "{seen:?}");
    assert!(seen.contains(&(0b11, 0b11, true)), "{seen:?}");
    assert_eq!(evidence.tests.len(), 2);

    std::fs::remove_dir_all(root).unwrap();
}

/// The JUnit 5 standalone runner, if one is already on this machine.
///
/// Deliberately not downloaded by the suite. A test that reaches the network
/// fails on a plane, fails behind a proxy, and turns an unrelated outage into
/// a red build. To enable this test, put the jar somewhere it looks:
///
/// ```text
/// curl -sSLo /tmp/junit5/junit-platform-console-standalone.jar \
///   https://repo1.maven.org/maven2/org/junit/platform/\
///   junit-platform-console-standalone/1.11.4/\
///   junit-platform-console-standalone-1.11.4.jar
/// ```
fn junit_jar() -> Option<PathBuf> {
    ["JUNIT_CONSOLE_JAR"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(PathBuf::from)
        .chain(
            ["/tmp/junit5/junit-platform-console-standalone.jar"]
                .into_iter()
                .map(PathBuf::from),
        )
        .find(|path| path.is_file())
}

const UNDER_TEST: &str = r#"public class Calculator {
    public static String size(int a, boolean loud) {
        if (a > 10 && loud) {
            return "BIG";
        }
        if (a == 0) return "zero";
        return "small";
    }
}
"#;

const SUITE: &str = r#"import org.junit.jupiter.api.Disabled;
import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.assertEquals;

class CalculatorTest {
    @Test
    void zeroIsNamed() {
        assertEquals("zero", Calculator.size(0, false));
    }

    @Test
    void loudAndLargeIsBig() {
        assertEquals("BIG", Calculator.size(20, true));
    }

    @Test
    @Disabled("proves a skipped test is recorded as one")
    void neverRuns() {
        assertEquals("small", Calculator.size(1, false));
    }
}
"#;

#[test]
fn the_platform_listener_attributes_coverage_without_touching_test_source() {
    // The test source goes in untouched and still comes out attributed, which
    // is the whole point of listening to the platform instead of rewriting
    // tests: the same mechanism covers Kotest and Spock, which declare no
    // annotated methods a rewriter could find.
    let (Some(javac), Some(java), Some(jar)) =
        (common::tool("javac"), common::tool("java"), junit_jar())
    else {
        common::skip("jvm", "no JDK or JUnit runner available");
        return;
    };
    let root = temporary("junit");
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/jvm/com/supercorp/supercov/Supercov.java");
    write(
        &root,
        "com/supercorp/supercov/Supercov.java",
        &std::fs::read_to_string(runtime).expect("runtime source"),
    );
    let listener = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/jvm/com/supercorp/supercov/SupercovListener.java");
    write(
        &root,
        "com/supercorp/supercov/SupercovListener.java",
        &std::fs::read_to_string(listener).expect("listener source"),
    );

    let mut next = 0;
    let mut decisions = 0;
    let obligations = build_jvm_obligations(
        "Calculator.java",
        UNDER_TEST,
        JvmLanguage::Java,
        &mut next,
        &mut decisions,
    )
    .expect("obligations");
    write(
        &root,
        "Calculator.java",
        &rewrite(UNDER_TEST, &obligations.edits),
    );

    // The test source goes in exactly as its author wrote it. Attribution
    // comes from the JUnit Platform listener, which sees every engine's tests
    // — including Kotest's and Spock's, which declare no annotated methods for
    // a rewriter to find.
    write(&root, "CalculatorTest.java", SUITE);

    let probes = obligations.probes.len() + 1;
    let widths = obligations
        .decision_widths
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    write(
        &root,
        "com/supercorp/supercov/SupercovConfig.java",
        &format!(
            r#"package com.supercorp.supercov;

public final class SupercovConfig {{
    public static final int PROBES = {probes};
    public static final int[] WIDTHS = new int[] {{{widths}}};
    public static final String EVIDENCE = "evidence.bin";
}}
"#
        ),
    );
    write(
        &root,
        "META-INF/services/org.junit.platform.launcher.TestExecutionListener",
        "com.supercorp.supercov.SupercovListener\n",
    );

    let classpath = format!("{}:.", jar.display());
    let compile = Command::new(&javac)
        .args([
            "-cp",
            &classpath,
            "-d",
            ".",
            "com/supercorp/supercov/Supercov.java",
            "com/supercorp/supercov/SupercovListener.java",
            "com/supercorp/supercov/SupercovConfig.java",
            "Calculator.java",
            "CalculatorTest.java",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "javac rejected the instrumented suite:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&java)
        .args([
            "-jar",
            &jar.display().to_string(),
            "execute",
            "--class-path",
            ".",
            "--select-class",
            "CalculatorTest",
            "--details=none",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "JUnit reported failures:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let evidence = read_evidence(&std::fs::read(root.join("evidence.bin")).expect("evidence"))
        .expect("decode");
    let named = evidence
        .tests
        .iter()
        .map(|t| t.name.clone())
        .collect::<Vec<_>>();
    // The names are the framework's own — class then method, as JUnit reports
    // them — so a reader matching this against a test report does not have to
    // translate between two naming schemes.
    assert!(
        named.contains(&"CalculatorTest#zeroIsNamed()".to_owned()),
        "{named:?}"
    );
    assert!(
        named.contains(&"CalculatorTest#loudAndLargeIsBig()".to_owned()),
        "{named:?}"
    );

    // How each test ended travels with what it covered, because a failing or
    // skipped test's coverage is not evidence that anything works. A disabled
    // test never starts, so the platform reports it separately: it is recorded
    // as having covered nothing rather than left out of the run entirely.
    let status = |needle: &str| {
        evidence
            .tests
            .iter()
            .find(|t| t.name.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing from {named:?}"))
            .status
            .clone()
    };
    assert_eq!(status("zeroIsNamed"), "passed");
    assert_eq!(status("loudAndLargeIsBig"), "passed");
    assert_eq!(status("neverRuns"), "skipped");
    let skipped = evidence
        .tests
        .iter()
        .find(|t| t.name.contains("neverRuns"))
        .unwrap();
    assert!(
        skipped.probes.is_empty(),
        "a test that never ran covers nothing: {:?}",
        skipped.probes
    );

    let zero = evidence
        .tests
        .iter()
        .find(|t| t.name.contains("zeroIsNamed"))
        .unwrap();
    let big = evidence
        .tests
        .iter()
        .find(|t| t.name.contains("loudAndLargeIsBig"))
        .unwrap();
    // Each test reached different code, and neither claims the other's.
    assert_ne!(zero.probes, big.probes);
    // Only the test that passed a large, loud value evaluated the second
    // condition; the other short-circuited before reaching it.
    assert!(
        big.vectors.iter().any(|v| v.key & 0b11 == 0b11),
        "the big case evaluated both conditions: {:?}",
        big.vectors
    );
    assert!(
        zero.vectors.iter().all(|v| v.key & 0b10 == 0),
        "the zero case must never evaluate the second condition: {:?}",
        zero.vectors
    );

    // And the same engine path Go uses turns it into a report. One transport
    // and one reader across languages is what keeps two frontends from
    // disagreeing about what an evaluation was.
    let outcomes = evidence
        .tests
        .iter()
        .map(|test| OwnedTestOutcome {
            name: test.name.clone(),
            runner: String::new(),
            package: "CalculatorTest".into(),
            file: Some("CalculatorTest.java".into()),
            status: "passed".into(),
        })
        .collect::<Vec<_>>();
    let run = build_frontend_run(OwnedRunInputs {
        declaration: jvm_declaration(),
        environment: "jvm",
        manifest: &obligations.manifest,
        probes: &obligations.probes,
        evidence: &evidence,
        outcomes: &outcomes,
        run_id: "run_jvm",
        generated_at: "now",
        test_exit_code: 0,
        coverage_model: jvm_coverage_model(),
    })
    .expect("frontend run");
    let report =
        supercov_engine::coverage_report::analyze_coverage_results(&run.request).expect("report");
    let summary = &report.filters.passed.summary;
    assert_eq!(summary.conditions, 2, "one decision of two conditions");
    assert!(summary.lines.covered > 0);
    // `size` returns "small" only for a value neither test passes, so a report
    // claiming everything covered would be the wrong answer.
    assert!(
        summary.lines.covered < summary.lines.total,
        "{}/{}",
        summary.lines.covered,
        summary.lines.total
    );

    std::fs::remove_dir_all(root).unwrap();
}

/// Per-test MC/DC is what credits an obligation to the test that discharges
/// it, so two tests producing the same vector must both record it. The inline
/// recent-key cache that keeps a looping decision off the runtime's monitor
/// once outlived the test whose vectors it stood in for, and the second test
/// to reach a vector was told it had already been recorded — the obligation
/// was then credited only to whichever test happened to run first.
///
/// Driven through the real runtime and the real transport rather than a
/// reimplementation of either, because the bug lived in the seam between them.
#[test]
fn every_test_records_a_vector_it_produces() {
    let (Some(javac), Some(java)) = (common::tool("javac"), common::tool("java")) else {
        common::skip("jvm", "no JDK found");
        return;
    };
    let root = temporary("vector-attribution");
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/jvm/com/supercorp/supercov/Supercov.java");
    write(
        &root,
        "com/supercorp/supercov/Supercov.java",
        &std::fs::read_to_string(runtime).expect("runtime"),
    );
    write(
        &root,
        "Driver.java",
        r#"import com.supercorp.supercov.Supercov;

public class Driver {
    public static void main(String[] args) throws Exception {
        Supercov.arm(4, new int[] {2});
        for (String name : new String[] {"first", "second", "third"}) {
            Supercov.enterTest(name);
            // `a && b` with both operands true: one vector, the same each time.
            Supercov.bd(1, 2, 0, Supercov.c(0, 0, true) && Supercov.c(0, 1, true));
            Supercov.exitTest();
        }
        Supercov.write(args[0]);
    }
}
"#,
    );
    let compile = Command::new(&javac)
        .args([
            "-d",
            ".",
            "com/supercorp/supercov/Supercov.java",
            "Driver.java",
        ])
        .current_dir(&root)
        .output()
        .expect("javac");
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let evidence_path = root.join("evidence.bin");
    let run = Command::new(&java)
        .args(["-cp", ".", "Driver", evidence_path.to_str().unwrap()])
        .current_dir(&root)
        .output()
        .expect("java");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let evidence = read_evidence(&std::fs::read(&evidence_path).expect("evidence"))
        .expect("the runtime's own transport");
    let recorded: Vec<(&str, usize)> = evidence
        .tests
        .iter()
        .map(|test| (test.name.as_str(), test.vectors.len()))
        .collect();
    assert_eq!(
        recorded,
        vec![("first", 1), ("second", 1), ("third", 1)],
        "every test that produced the vector should carry it"
    );
    std::fs::remove_dir_all(root).ok();
}

/// Probes are a store into one shared array, which is what makes them cost a
/// single instruction, and attribution is a sweep of that array at each test
/// boundary. Two tests running at once share the array, so the sweep credits
/// hits to whichever test happens to be current. The runtime must notice and
/// stop attributing rather than emit records that look measured and are not.
///
/// Probe hits survive, because every write stores the same constant and no
/// interleaving changes the result — so a parallel suite still reports what it
/// reached, it just cannot say which test reached it. Condition vectors do
/// not: their state is read-modify-write, so concurrent evaluations describe
/// an evaluation that never happened and they are dropped rather than shown.
#[test]
fn concurrent_tests_lose_attribution_rather_than_get_it_wrong() {
    let (Some(javac), Some(java)) = (common::tool("javac"), common::tool("java")) else {
        common::skip("jvm", "no JDK found");
        return;
    };
    let root = temporary("overlap");
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/jvm/com/supercorp/supercov/Supercov.java");
    write(
        &root,
        "com/supercorp/supercov/Supercov.java",
        &std::fs::read_to_string(runtime).expect("runtime"),
    );
    write(
        &root,
        "Driver.java",
        r#"import com.supercorp.supercov.Supercov;
import java.util.concurrent.CountDownLatch;

public class Driver {
    public static void main(String[] args) throws Exception {
        Supercov.arm(4, new int[] {2});
        // Both tests are open at once: the latch makes the overlap certain
        // rather than a race the test hopes to lose.
        CountDownLatch bothStarted = new CountDownLatch(2);
        Runnable body = () -> {
            Supercov.enterTest(Thread.currentThread().getName());
            Supercov.bd(1, 2, 0, Supercov.c(0, 0, true) && Supercov.c(0, 1, true));
            bothStarted.countDown();
            try {
                bothStarted.await();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            Supercov.exitTest();
        };
        Thread first = new Thread(body, "first");
        Thread second = new Thread(body, "second");
        first.start();
        second.start();
        first.join();
        second.join();
        Supercov.write(args[0]);
    }
}
"#,
    );
    let compile = Command::new(&javac)
        .args([
            "-d",
            ".",
            "com/supercorp/supercov/Supercov.java",
            "Driver.java",
        ])
        .current_dir(&root)
        .output()
        .expect("javac");
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let evidence_path = root.join("evidence.bin");
    let run = Command::new(&java)
        .args(["-cp", ".", "Driver", evidence_path.to_str().unwrap()])
        .current_dir(&root)
        .output()
        .expect("java");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let complaint = String::from_utf8_lossy(&run.stderr);
    assert!(
        complaint.contains("cannot be attributed to individual tests"),
        "the run should say why attribution is missing, got: {complaint}"
    );

    let evidence = read_evidence(&std::fs::read(&evidence_path).expect("evidence"))
        .expect("the runtime's own transport");
    assert!(
        evidence.tests.is_empty(),
        "attribution nobody can trust should be absent, not present: {:?}",
        evidence.tests
    );
    assert!(
        evidence.global.iter().any(|&mask| mask != 0),
        "run-wide totals are a union and should survive the overlap"
    );
    assert_eq!(
        evidence.decision_vectors[0].len(),
        0,
        "condition vectors a race can corrupt should be dropped, not reported"
    );
    std::fs::remove_dir_all(root).ok();
}

/// Instrumented code must run correctly when nothing ever arms the runtime.
///
/// A probe is a bare store into the probe array and nothing checks its bounds,
/// which is what makes it cost one instruction. If arming were the only thing
/// that sized the array, then any run where the listener did not start — a
/// suite in a language Supercov does not parse, a build that never reaches the
/// test task, a framework nobody wrote a listener for — would not merely lose
/// coverage. The first instrumented line would throw
/// ArrayIndexOutOfBoundsException, and Supercov would have turned a passing
/// suite into a failing one. Losing a measurement is acceptable; breaking the
/// thing being measured is not.
#[test]
fn instrumented_code_runs_correctly_when_nothing_arms_the_runtime() {
    let (Some(javac), Some(java)) = (common::tool("javac"), common::tool("java")) else {
        common::skip("jvm", "no JDK found");
        return;
    };
    let root = temporary("unarmed");
    let mut next = 0;
    let mut decisions = 0;
    let obligations = build_jvm_obligations(
        "Classify.java",
        SOURCE,
        JvmLanguage::Java,
        &mut next,
        &mut decisions,
    )
    .expect("obligations");
    write(&root, "Classify.java", &rewrite(SOURCE, &obligations.edits));

    // The runtime exactly as a workspace gets it, sized for this project, and
    // no listener and no configuration anywhere.
    let probes = obligations.probes.len() + 1;
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("runtime-assets/jvm/com/supercorp/supercov/Supercov.java"),
    )
    .expect("runtime");
    let sized = source.replace(
        "static final int PROBE_COUNT = 0; // supercov:probe-count",
        &format!("static final int PROBE_COUNT = {probes}; // supercov:probe-count"),
    );
    assert_ne!(
        sized, source,
        "the runtime must declare a substitutable probe count"
    );
    write(&root, "com/supercorp/supercov/Supercov.java", &sized);
    write(
        &root,
        "Main.java",
        r#"public class Main {
    public static void main(String[] args) {
        // The answers the uninstrumented program would give.
        if (!"big".equals(Classify.classify(20, true))) { throw new AssertionError("big"); }
        if (!"zero".equals(Classify.classify(0, false))) { throw new AssertionError("zero"); }
        if (!"small".equals(Classify.classify(1, false))) { throw new AssertionError("small"); }
        System.out.println("ok");
    }
}
"#,
    );
    let compile = Command::new(&javac)
        .args([
            "-d",
            ".",
            "com/supercorp/supercov/Supercov.java",
            "Classify.java",
            "Main.java",
        ])
        .current_dir(&root)
        .output()
        .expect("javac");
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&java)
        .args(["-cp", ".", "Main"])
        .current_dir(&root)
        .output()
        .expect("java");
    assert!(
        run.status.success(),
        "instrumented code must not break the program it measures:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    std::fs::remove_dir_all(root).ok();
}
