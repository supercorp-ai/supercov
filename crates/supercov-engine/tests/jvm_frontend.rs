//! End-to-end proof that instrumented Java compiles, runs, and reports truly.
//!
//! The parser tests hold that rewritten source still parses. Only a real JDK
//! can say it compiles, that `javac -Xlint:all` is content with it, and that
//! the evidence says what actually happened. Skips without a JDK rather than
//! failing a suite the machine cannot run.

use std::path::{Path, PathBuf};
use std::process::Command;

use supercov_engine::go_evidence::read_evidence;
use supercov_engine::jvm_instrumenter::{JvmLanguage, build_jvm_obligations, rewrite};

fn tool(name: &str) -> Option<PathBuf> {
    // Homebrew's JDK is keg-only, so `/usr/bin/java` is a stub that finds no
    // runtime. A frontend that assumed PATH would fail on the common macOS
    // setup, which is why the search is explicit.
    [
        "/opt/homebrew/opt/openjdk/bin/",
        "/usr/local/opt/openjdk/bin/",
        "",
    ]
    .into_iter()
    .map(|prefix| PathBuf::from(format!("{prefix}{name}")))
    .find(|path| {
        Command::new(path)
            .arg("-version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

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
    let (Some(javac), Some(java)) = (tool("javac"), tool("java")) else {
        eprintln!("[jvm-frontend] skipped: no JDK found");
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
    let obligations = build_jvm_obligations("Classify.java", SOURCE, JvmLanguage::Java, &mut next)
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
    let (Some(kotlinc), Some(javac), Some(java)) = (kotlinc(), tool("javac"), tool("java")) else {
        eprintln!("[jvm-frontend] skipped: no Kotlin toolchain found");
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
    let obligations =
        build_jvm_obligations("Classify.kt", KOTLIN_SOURCE, JvmLanguage::Kotlin, &mut next)
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

const SUITE: &str = r#"import org.junit.jupiter.api.Test;
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
}
"#;

#[test]
fn the_platform_listener_attributes_coverage_without_touching_test_source() {
    // The test source goes in untouched and still comes out attributed, which
    // is the whole point of listening to the platform instead of rewriting
    // tests: the same mechanism covers Kotest and Spock, which declare no
    // annotated methods a rewriter could find.
    let (Some(javac), Some(java), Some(jar)) = (tool("javac"), tool("java"), junit_jar()) else {
        eprintln!("[jvm-frontend] skipped: no JDK or JUnit runner available");
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
    let obligations =
        build_jvm_obligations("Calculator.java", UNDER_TEST, JvmLanguage::Java, &mut next)
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

    std::fs::remove_dir_all(root).unwrap();
}
