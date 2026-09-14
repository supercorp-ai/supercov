//! The whole JVM lifecycle, through a real build system.
//!
//! The frontend test proves instrumented Java and Kotlin compile and that the
//! JUnit Platform listener attributes what they reach. This proves the parts
//! around it: that a project is copied and rewritten without touching the
//! author's tree, that Supercov's runtime, listener and configuration land
//! where the build already looks, that the build runs the suite unchanged, and
//! that a run is published and can be read back.
//!
//! Needs Maven and a JDK, and Maven needs its dependencies resolvable. Skips
//! otherwise rather than failing a suite the machine cannot run.

use std::path::{Path, PathBuf};
use std::process::Command;

use supercov_engine::jvm_project::JvmBuild;
use supercov_engine::jvm_run::{DirectJvmRunRequest, run_direct_jvm};

fn tool(name: &str) -> Option<PathBuf> {
    ["/opt/homebrew/bin/", "/usr/local/bin/", "/usr/bin/", ""]
        .into_iter()
        .map(|prefix| PathBuf::from(format!("{prefix}{name}")))
        .find(|path| {
            Command::new(path)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
}

fn temporary(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "supercov-jvm-run-{label}-{}-{}",
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

const POM: &str = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>example</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.source>17</maven.compiler.source>
    <maven.compiler.target>17</maven.compiler.target>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.10.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-surefire-plugin</artifactId>
        <version>3.2.5</version>
      </plugin>
    </plugins>
  </build>
</project>
"#;

/// `a > 10 && loud` gives two conditions, so MC/DC has something to say. The
/// suite shows one condition's independence and not the other's, which is the
/// difference between a number that measures something and one that does not.
const SOURCE: &str = r#"package app;

public class Calculator {
    public static String size(int a, boolean loud) {
        if (a > 10 && loud) {
            return "BIG";
        }
        return "small";
    }
}
"#;

const SUITE: &str = r#"package app;

import org.junit.jupiter.api.Disabled;
import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.assertEquals;

class CalculatorTest {
    @Test
    void bigWhenLoudAndLarge() {
        assertEquals("BIG", Calculator.size(20, true));
    }

    @Test
    void smallOtherwise() {
        assertEquals("small", Calculator.size(1, false));
    }

    @Test
    @Disabled("proves a skipped test is recorded as one")
    void neverRuns() {
        assertEquals("small", Calculator.size(0, true));
    }
}
"#;

fn fixture(root: &Path) {
    write(root, "pom.xml", POM);
    write(root, "src/main/java/app/Calculator.java", SOURCE);
    write(root, "src/test/java/app/CalculatorTest.java", SUITE);
}

/// Maven resolves from the network on a cold cache. A machine without it can
/// still run every other test in the suite.
fn maven_can_resolve(mvn: &Path, root: &Path) -> bool {
    fixture(root);
    Command::new(mvn)
        .args(["-q", "test"])
        .current_dir(root)
        .output()
        .is_ok_and(|out| out.status.success())
}

#[test]
fn a_maven_project_runs_through_its_own_build_and_publishes_what_each_test_reached() {
    let Some(mvn) = tool("mvn") else {
        eprintln!("[jvm-run] skipped: no Maven found");
        return;
    };
    let warmup = temporary("maven-warmup");
    if !maven_can_resolve(&mvn, &warmup) {
        eprintln!("[jvm-run] skipped: Maven cannot resolve this project's dependencies here");
        std::fs::remove_dir_all(warmup).ok();
        return;
    }
    std::fs::remove_dir_all(warmup).ok();

    let root = temporary("maven");
    fixture(&root);
    let before = std::fs::read_to_string(root.join("src/main/java/app/Calculator.java")).unwrap();

    let request = DirectJvmRunRequest {
        root: root.clone(),
        command: vec![mvn.display().to_string(), "test".into()],
        run_id: "run-jvm-maven".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_jvm(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };

    // The author's tree is untouched: instrumentation happens on a copy, and
    // nothing Supercov generates lands beside the code they wrote.
    assert_eq!(
        std::fs::read_to_string(root.join("src/main/java/app/Calculator.java")).unwrap(),
        before,
        "the project's own sources must come back exactly as they went in"
    );
    assert!(
        !root
            .join("src/main/java/com/supercorp/supercov/Supercov.java")
            .exists(),
        "Supercov's runtime belongs in the workspace, not in the author's tree"
    );

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.build, JvmBuild::Maven);
    assert_eq!(result.source_files, 1);
    // The disabled one included: a test the suite declared and did not run is
    // a fact worth reporting, not an absence worth hiding.
    assert_eq!(result.tests, 3);

    let archive = result.run_directory.join("evidence.raw.gz");
    let entries =
        supercov_engine::evidence_archive::read_archive(&archive).expect("published archive");
    let named = entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    for required in ["coverage-model.json", "frontend.json", "manifest.json"] {
        assert!(named.contains(&required), "{named:?}");
    }
    assert_eq!(
        named
            .iter()
            .filter(|path| path.ends_with("mcdc.json"))
            .count(),
        3,
        "one record per test: {named:?}"
    );

    let records = entries
        .iter()
        .filter(|entry| entry.path.ends_with("mcdc.json"))
        .map(|entry| String::from_utf8(entry.contents.clone()).expect("utf-8"))
        .collect::<Vec<_>>();
    let statuses = records
        .iter()
        .filter(|record| record.contains("\"status\":\"skipped\""))
        .count();
    assert_eq!(statuses, 1, "the disabled test is recorded as skipped");
    assert!(
        records
            .iter()
            .any(|record| record.contains("CalculatorTest#bigWhenLoudAndLarge()")),
        "tests carry the names the framework itself chose"
    );
    assert!(
        records
            .iter()
            .all(|record| record.contains("\"runner\":\"junit-platform\"")),
        "{records:?}"
    );
    std::fs::remove_dir_all(root).ok();
}

const BUILD_GRADLE: &str = r#"plugins {
    id 'java'
}

repositories {
    mavenCentral()
}

dependencies {
    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.2'
    // Gradle 9 requires every project to declare this itself, in exactly the
    // configuration its documentation recommends: on the classpath the tests
    // run with, not the one they compile against.
    testRuntimeOnly 'org.junit.platform:junit-platform-launcher'
}

test {
    useJUnitPlatform()
}
"#;

fn gradle_fixture(root: &Path) {
    write(root, "settings.gradle", "rootProject.name = 'demo'\n");
    write(root, "build.gradle", BUILD_GRADLE);
    write(root, "src/main/java/app/Calculator.java", SOURCE);
    write(root, "src/test/java/app/CalculatorTest.java", SUITE);
}

#[test]
fn a_gradle_project_runs_through_its_own_build_and_publishes_what_each_test_reached() {
    let Some(gradle) = tool("gradle") else {
        eprintln!("[jvm-run] skipped: no Gradle found");
        return;
    };
    let warmup = temporary("gradle-warmup");
    gradle_fixture(&warmup);
    let resolvable = Command::new(&gradle)
        .args(["--quiet", "--console=plain", "test"])
        .current_dir(&warmup)
        .output()
        .is_ok_and(|out| out.status.success());
    std::fs::remove_dir_all(&warmup).ok();
    if !resolvable {
        eprintln!("[jvm-run] skipped: Gradle cannot resolve this project's dependencies here");
        return;
    }

    let root = temporary("gradle");
    gradle_fixture(&root);
    let before = std::fs::read_to_string(root.join("build.gradle")).unwrap();

    let request = DirectJvmRunRequest {
        root: root.clone(),
        command: vec![
            gradle.display().to_string(),
            "--console=plain".into(),
            "test".into(),
        ],
        run_id: "run-jvm-gradle".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_jvm(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };

    // The launcher dependency goes into the copy, never into the author's own
    // build file.
    assert_eq!(
        std::fs::read_to_string(root.join("build.gradle")).unwrap(),
        before,
        "the project's own build file must come back exactly as it went in"
    );
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.build, JvmBuild::Gradle);
    assert_eq!(result.tests, 3);
    std::fs::remove_dir_all(root).ok();
}

const TESTNG_POM: &str = r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>example</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>
  <properties>
    <maven.compiler.source>17</maven.compiler.source>
    <maven.compiler.target>17</maven.compiler.target>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.testng</groupId>
      <artifactId>testng</artifactId>
      <version>7.10.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
  <build>
    <plugins>
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-surefire-plugin</artifactId>
        <version>3.2.5</version>
      </plugin>
    </plugins>
  </build>
</project>
"#;

/// TestNG's own idioms: a data provider that runs one method several times,
/// and a skip. Neither has a JUnit Platform equivalent to fall back on.
const TESTNG_SUITE: &str = r#"package app;

import org.testng.SkipException;
import org.testng.annotations.DataProvider;
import org.testng.annotations.Test;
import static org.testng.Assert.assertEquals;

public class CalculatorTest {
    @DataProvider(name = "sizes")
    public Object[][] sizes() {
        return new Object[][] {{20, true, "BIG"}, {1, false, "small"}};
    }

    @Test(dataProvider = "sizes")
    public void sizesAreNamed(int a, boolean loud, String expected) {
        assertEquals(Calculator.size(a, loud), expected);
    }

    @Test
    public void notToday() {
        throw new SkipException("nothing to do here");
    }
}
"#;

fn testng_fixture(root: &Path) {
    write(root, "pom.xml", TESTNG_POM);
    write(root, "src/main/java/app/Calculator.java", SOURCE);
    write(root, "src/test/java/app/CalculatorTest.java", TESTNG_SUITE);
}

#[test]
fn a_testng_suite_is_attributed_through_its_own_lifecycle() {
    // TestNG is the one framework the JUnit Platform does not report, so it
    // needs a listener of its own. Kotest and Spock are platform engines and
    // need nothing extra.
    let Some(mvn) = tool("mvn") else {
        eprintln!("[jvm-run] skipped: no Maven found");
        return;
    };
    let warmup = temporary("testng-warmup");
    testng_fixture(&warmup);
    let resolvable = Command::new(&mvn)
        .args(["-q", "test"])
        .current_dir(&warmup)
        .output()
        .is_ok_and(|out| out.status.success());
    std::fs::remove_dir_all(&warmup).ok();
    if !resolvable {
        eprintln!("[jvm-run] skipped: Maven cannot resolve TestNG here");
        return;
    }

    let root = temporary("testng");
    testng_fixture(&root);
    let request = DirectJvmRunRequest {
        root: root.clone(),
        command: vec![mvn.display().to_string(), "-q".into(), "test".into()],
        run_id: "run-jvm-testng".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_jvm(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };

    // Two data-provider invocations and the skip. The invocations are separate
    // tests because they reach different code, which is the point of a data
    // provider: collapsing them would credit one with what the other proved.
    assert_eq!(
        result.tests,
        3,
        "{diagnostics:?}",
        diagnostics = String::from_utf8_lossy(&diagnostics)
    );

    let archive = result.run_directory.join("evidence.raw.gz");
    let records = supercov_engine::evidence_archive::read_archive(&archive)
        .expect("published archive")
        .into_iter()
        .filter(|entry| entry.path.ends_with("mcdc.json"))
        .map(|entry| String::from_utf8(entry.contents).expect("utf-8"))
        .collect::<Vec<_>>();
    assert!(
        records
            .iter()
            .any(|record| record.contains("CalculatorTest#sizesAreNamed()")),
        "{records:?}"
    );
    assert!(
        records
            .iter()
            .any(|record| record.contains("CalculatorTest#sizesAreNamed()[1]")),
        "a second invocation is its own test: {records:?}"
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.contains("\"status\":\"skipped\""))
            .count(),
        1,
        "the skipped test is recorded as one: {records:?}"
    );
    // Attributed by the lifecycle that actually saw it. A project can run both
    // frameworks in one JVM, so a result naming the platform here would claim
    // it was announced by something that never saw it.
    assert!(
        records
            .iter()
            .all(|record| record.contains("\"runner\":\"testng\"")),
        "{records:?}"
    );
    std::fs::remove_dir_all(root).ok();
}

const KOTLIN_BUILD_GRADLE: &str = r#"plugins {
    id 'org.jetbrains.kotlin.jvm' version '2.2.20'
}

repositories {
    mavenCentral()
}

// Pinned so the Java and Kotlin compilers agree on a target; Gradle refuses
// the build otherwise, and what is under test here is Supercov, not a
// toolchain mismatch.
java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

dependencies {
    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.2'
    testImplementation 'org.jetbrains.kotlin:kotlin-test'
    testRuntimeOnly 'org.junit.platform:junit-platform-launcher'
}

test {
    useJUnitPlatform()
}
"#;

const KOTLIN_SOURCE: &str = r#"package app

object Calculator {
    fun size(a: Int, loud: Boolean): String {
        if (a > 10 && loud) {
            return "BIG"
        }
        return "small"
    }
}
"#;

const KOTLIN_SUITE: &str = r#"package app

import kotlin.test.Test
import kotlin.test.assertEquals

class CalculatorTest {
    @Test
    fun bigWhenLoudAndLarge() {
        assertEquals("BIG", Calculator.size(20, true))
    }

    @Test
    fun smallOtherwise() {
        assertEquals("small", Calculator.size(1, false))
    }
}
"#;

fn kotlin_fixture(root: &Path) {
    write(root, "settings.gradle", "rootProject.name = 'demo'\n");
    write(root, "build.gradle", KOTLIN_BUILD_GRADLE);
    write(root, "src/main/kotlin/app/Calculator.kt", KOTLIN_SOURCE);
    write(root, "src/test/kotlin/app/CalculatorTest.kt", KOTLIN_SUITE);
}

#[test]
fn a_kotlin_project_is_measured_like_any_other_jvm_one() {
    // Kotlin is instrumented by the same rewriter and attributed by the same
    // listener; what differs is only the grammar the obligations come from.
    let Some(gradle) = tool("gradle") else {
        eprintln!("[jvm-run] skipped: no Gradle found");
        return;
    };
    let warmup = temporary("kotlin-warmup");
    kotlin_fixture(&warmup);
    let resolvable = Command::new(&gradle)
        .args(["--quiet", "--console=plain", "test"])
        .current_dir(&warmup)
        .output()
        .is_ok_and(|out| out.status.success());
    std::fs::remove_dir_all(&warmup).ok();
    if !resolvable {
        eprintln!("[jvm-run] skipped: Gradle cannot build this Kotlin project here");
        return;
    }

    let root = temporary("kotlin");
    kotlin_fixture(&root);
    let request = DirectJvmRunRequest {
        root: root.clone(),
        command: vec![
            gradle.display().to_string(),
            "--console=plain".into(),
            "test".into(),
        ],
        run_id: "run-jvm-kotlin".into(),
        started_at: "2026-01-01T00:00:00.000Z".into(),
    };
    let mut diagnostics = Vec::new();
    let result = match run_direct_jvm(&request, &mut diagnostics) {
        Ok(result) => result,
        Err(error) => panic!(
            "run failed: {error}\n--- diagnostics ---\n{}",
            String::from_utf8_lossy(&diagnostics)
        ),
    };
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.tests, 2);
    assert_eq!(result.source_files, 1);

    // The decision the Kotlin source declares is in the published manifest,
    // which is what the numbers are measured against.
    let entries =
        supercov_engine::evidence_archive::read_archive(&result.run_directory.join("evidence.raw.gz"))
            .expect("published archive");
    let manifest = String::from_utf8(
        entries
            .into_iter()
            .find(|entry| entry.path == "manifest.json")
            .expect("manifest")
            .contents,
    )
    .expect("utf-8");
    assert!(manifest.contains("Calculator.kt"), "{manifest}");
    assert_eq!(manifest.matches("\"conditions\"").count(), 1, "{manifest}");
    std::fs::remove_dir_all(root).ok();
}
