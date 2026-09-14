//! Public, isolated JVM coverage run lifecycle, for Java and Kotlin.
//!
//! Supercov owns the probes, so measuring a project means rewriting its
//! sources. That happens on a copy in an isolated workspace; the tree the
//! author edits is never touched.
//!
//! The build system runs the tests, and Supercov puts three things where that
//! build will find them without being reconfigured:
//!
//! - `Supercov` in the main source set, because instrumented product code
//!   stores into its probe array;
//! - `SupercovListener` and a generated `SupercovConfig` in the test source
//!   set, because attribution comes from the JUnit Platform and only the test
//!   classpath has it;
//! - a services file registering the listener, and a
//!   `junit-platform.properties` that turns parallel execution off.
//!
//! That last one is prevention rather than detection. Two tests running at
//! once share one probe array, so the runtime cannot attribute either, and it
//! notices and says so — but the better outcome is that it never happens, and
//! Supercov owns the workspace, so it can simply make sure of it.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    evidence_archive::write_archive,
    frontend_protocol::validate_frontend_report_request,
    integrity::{FrontendIntegrityInputs, create_explicit_run_integrity},
    jvm_project::{
        JvmBuild, PreparedJvmProject, detect_build, jvm_integrity_inputs, prepare_jvm_project,
    },
    lifecycle::{
        ProjectLock, finalize_published_run, publish_run, recover_abandoned_runs,
        remove_stored_tree_deferred,
    },
    orchestration::{ExecutionPhase, ExecutionPlan, PhaseKind, execute_plan},
    owned_evidence::{
        OwnedRunInputs, OwnedTestOutcome, build_frontend_run, jvm_coverage_model, jvm_declaration,
        read_evidence,
    },
    process_supervision::{CommandSpec, SupervisionOptions},
    run_store::{RawEvidenceMetadata, RunMetadata, RunTimings},
    workspace::{canonicalize_simplified, prepare_cached_workspace, recover_cached_workspace},
};

const RUNTIME_SOURCE: &str =
    include_str!("../runtime-assets/jvm/com/supercorp/supercov/Supercov.java");
const LISTENER_SOURCE: &str =
    include_str!("../runtime-assets/jvm/com/supercorp/supercov/SupercovListener.java");

const TESTNG_LISTENER_SOURCE: &str =
    include_str!("../runtime-assets/jvm/com/supercorp/supercov/SupercovTestNGListener.java");

const PACKAGE_DIRECTORY: &str = "com/supercorp/supercov";

/// Where the JUnit Platform looks for listeners to register.
const SERVICES_FILE: &str = "META-INF/services/org.junit.platform.launcher.TestExecutionListener";

const LISTENER_CLASS: &str = "com.supercorp.supercov.SupercovListener";

/// Where TestNG looks for listeners to register.
const TESTNG_SERVICES_FILE: &str = "META-INF/services/org.testng.ITestNGListener";

const TESTNG_LISTENER_CLASS: &str = "com.supercorp.supercov.SupercovTestNGListener";

/// Which test frameworks a project actually depends on.
///
/// This decides which listeners are written, and it has to: each is compiled
/// from the project's own test sources, so one whose framework is absent would
/// fail on imports the project never asked for. A project that names neither
/// gets the platform listener, which is what nearly every JVM suite runs on
/// and what Kotest and Spock report through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frameworks {
    platform: bool,
    testng: bool,
}

fn frameworks(build_file: &str) -> Frameworks {
    let testng = build_file.contains("testng");
    let platform = build_file.contains("junit")
        || build_file.contains("kotest")
        || build_file.contains("spock");
    Frameworks {
        // A TestNG-only project would fail to compile a platform listener, so
        // the fallback applies only when nothing at all was recognised.
        platform: platform || !testng,
        testng,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectJvmRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectJvmRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub exit_code: i32,
    pub tests: usize,
    pub source_files: usize,
    pub build: JvmBuild,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

fn elapsed_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    fs::write(path, contents).map_err(|error| format!("{}: {error}", path.display()))
}

/// The source root Supercov's own Java lands in for a given source set.
///
/// Maven, Gradle and a plain tree have meant the same thing by these paths for
/// long enough that the convention is more reliable than reading a build file,
/// and a Kotlin project compiles Java from here too.
fn source_root(source_set: &str) -> String {
    format!("src/{source_set}/java")
}

/// Java source escaping for a string literal, so a Windows path or a name with
/// a quote in it cannot end the literal early.
fn java_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// What the listener reads to know the shape of the run it is recording.
fn configuration(probe_count: usize, widths: &[u8], evidence: &Path) -> String {
    let widths = widths
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "// Code generated by Supercov. DO NOT EDIT.\npackage com.supercorp.supercov;\n\npublic final class SupercovConfig {{\n  private SupercovConfig() {{}}\n\n  public static final int PROBES = {probe_count};\n  public static final int[] WIDTHS = new int[] {{{widths}}};\n  public static final String EVIDENCE = {};\n}}\n",
        java_literal(&evidence.to_string_lossy())
    )
}

/// The project's JUnit Platform configuration with parallel execution turned
/// off, keeping whatever else it already said.
///
/// Attribution is a sweep of one shared probe array at each test boundary, so
/// two tests running at once cannot both be credited. The runtime notices and
/// degrades honestly; this is the half that means it never has to.
fn sequential_properties(existing: Option<&str>) -> String {
    let mut out = String::new();
    for line in existing.unwrap_or("").lines() {
        if line
            .trim_start()
            .starts_with("junit.jupiter.execution.parallel.enabled")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(
        "# Set by Supercov: probes are a store into one shared array, so two tests running\n\
         # at once cannot both be credited with what they reached.\n\
         junit.jupiter.execution.parallel.enabled=false\n",
    );
    out
}

/// The JUnit Platform artifact the listener is written against.
///
/// `junit-jupiter` does not bring it: Maven's surefire and Gradle's test task
/// both put it on the test *runtime* classpath because they need it
/// themselves, but neither offers it at compile time, so a listener declared
/// in the project's own test sources will not compile without it. Supercov
/// owns the workspace copy, so it adds the dependency there — the same line a
/// person would add, in a tree the author never sees.
const LAUNCHER_ARTIFACT: &str = "junit-platform-launcher";

/// The launcher version to ask for.
///
/// JUnit numbers the platform 1.N alongside Jupiter 5.N, so a project that
/// pins its Jupiter version tells us exactly which launcher agrees with the
/// engine it will run on. A project using the BOM has already said how to
/// version every JUnit artifact, and naming a version there would override the
/// answer it gave.
fn launcher_version(build_file: &str) -> Option<String> {
    if build_file.contains("junit-bom") {
        return None;
    }
    let jupiter = build_file.find("junit-jupiter")?;
    let rest = &build_file[jupiter..];
    let mut digits = rest.match_indices("5.").filter_map(|(at, _)| {
        let tail = &rest[at + 2..];
        let minor = tail
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        let patch = tail[minor.len()..]
            .strip_prefix('.')?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        (!minor.is_empty() && !patch.is_empty()).then(|| format!("1.{minor}.{patch}"))
    });
    digits
        .next()
        // A project that says nothing gets a launcher new enough to drive an
        // older engine, which is the direction that works.
        .or_else(|| Some(DEFAULT_LAUNCHER_VERSION.to_owned()))
}

const DEFAULT_LAUNCHER_VERSION: &str = "1.10.2";

/// `pom.xml` with the launcher among its test dependencies.
fn maven_with_launcher(pom: &str) -> Option<String> {
    if pom.contains(LAUNCHER_ARTIFACT) {
        return None;
    }
    let version = launcher_version(pom)
        .map(|version| format!("\n      <version>{version}</version>"))
        .unwrap_or_default();
    let dependency = format!(
        "    <dependency>\n      <groupId>org.junit.platform</groupId>\n      <artifactId>{LAUNCHER_ARTIFACT}</artifactId>{version}\n      <scope>test</scope>\n    </dependency>\n"
    );
    match pom.rfind("</dependencies>") {
        Some(at) => Some(format!("{}{dependency}{}", &pom[..at], &pom[at..])),
        // A project with no dependencies block at all still needs one.
        None => pom.rfind("</project>").map(|at| {
            format!(
                "{}  <dependencies>\n{dependency}  </dependencies>\n{}",
                &pom[..at],
                &pom[at..]
            )
        }),
    }
}

/// Whether a Gradle script already puts the launcher where test *sources* can
/// see it.
///
/// Presence is not enough. Gradle 9 requires every project to declare the
/// launcher itself, and the configuration its own documentation recommends is
/// `testRuntimeOnly` — which puts the artifact on the classpath the tests run
/// with and not the one they compile against. A project following that advice
/// has the artifact and still cannot compile a listener, so the question is
/// which configuration declares it, not whether one does.
fn declares_launcher_for_compilation(build_file: &str) -> bool {
    build_file.lines().any(|line| {
        line.contains(LAUNCHER_ARTIFACT)
            && ["testImplementation", "testCompileOnly", "testApi"]
                .iter()
                .any(|configuration| line.contains(configuration))
    })
}

/// A Gradle build file with the launcher among its test dependencies.
///
/// Appended as its own `dependencies` block rather than edited into the
/// existing one: Gradle merges them, and finding the right brace in a Groovy
/// or Kotlin script by hand is the kind of parsing that works until it does
/// not.
fn gradle_with_launcher(build_file: &str, kotlin: bool) -> Option<String> {
    if declares_launcher_for_compilation(build_file) {
        return None;
    }
    let coordinate = match launcher_version(build_file) {
        Some(version) => format!("org.junit.platform:{LAUNCHER_ARTIFACT}:{version}"),
        None => format!("org.junit.platform:{LAUNCHER_ARTIFACT}"),
    };
    let line = if kotlin {
        format!("    testImplementation(\"{coordinate}\")")
    } else {
        format!("    testImplementation '{coordinate}'")
    };
    Some(format!(
        "{build_file}\n// Added by Supercov: the JUnit Platform listener that attributes coverage\n// to each test is compiled from this project's test sources, and the launcher\n// API it implements is on the test runtime classpath but not the compile one.\ndependencies {{\n{line}\n}}\n"
    ))
}

/// Make the build run its tests again rather than reporting a cached result.
///
/// A test that does not run records nothing, and a run that measured half a
/// suite without saying so is worse than one that took longer.
fn command_with_fresh_results(
    build: JvmBuild,
    command: &[String],
) -> (Vec<String>, Option<String>) {
    let mut updated = command.to_vec();
    match build {
        JvmBuild::Gradle => {
            if updated
                .iter()
                .any(|argument| argument == "--rerun-tasks" || argument == "--rerun")
            {
                return (updated, None);
            }
            updated.push("--rerun-tasks".into());
            (
                updated,
                Some(
                    "added --rerun-tasks: Gradle skips a test task it considers up to date, and a task that does not run records no coverage"
                        .into(),
                ),
            )
        }
        // Maven's default lifecycle re-runs surefire every time, and Gradle's
        // build cache has no Maven equivalent worth defeating here.
        JvmBuild::Maven | JvmBuild::Plain => (updated, None),
    }
}

struct InstrumentedWorkspace {
    project: PreparedJvmProject,
    build: JvmBuild,
    evidence: PathBuf,
    /// Which file declared each test class, so a result can point at a source.
    declared_in: BTreeMap<String, String>,
    /// The build file the launcher dependency was added to, if it was.
    added_launcher: Option<&'static str>,
}

/// The class a test's reported name belongs to, as JUnit names it:
/// `CalculatorTest#zeroIsNamed()`, or a DSL framework's own wording.
fn class_of(test_name: &str) -> Option<&str> {
    test_name.split('#').next().filter(|name| !name.is_empty())
}

fn instrument_workspace(
    workspace: &Path,
    evidence_directory: &Path,
) -> Result<InstrumentedWorkspace, String> {
    let project = prepare_jvm_project(workspace)?;
    let build = detect_build(workspace);
    let evidence = evidence_directory.join("evidence.bin");

    for (relative, instrumented) in &project.instrumented {
        write(&workspace.join(relative), instrumented)?;
    }

    let probe_count = project
        .probes
        .keys()
        .max()
        .map_or(0, |highest| *highest as usize + 1);

    // The runtime goes in the main source set: instrumented product code
    // stores into its array, so it has to compile with the product.
    let main = workspace.join(source_root("main")).join(PACKAGE_DIRECTORY);
    write(&main.join("Supercov.java"), RUNTIME_SOURCE)?;

    // The listener and its configuration go in the test source set, because
    // only the test classpath has the JUnit Platform to listen to.
    // Which listeners can be compiled at all depends on what the project
    // depends on, so read the build file before writing any of them.
    let build_file = ["pom.xml", "build.gradle.kts", "build.gradle"]
        .iter()
        .find_map(|name| fs::read_to_string(workspace.join(name)).ok())
        .unwrap_or_default();
    let frameworks = frameworks(&build_file);

    let test = workspace.join(source_root("test")).join(PACKAGE_DIRECTORY);
    write(
        &test.join("SupercovConfig.java"),
        &configuration(probe_count, &project.decision_widths, &evidence),
    )?;
    if frameworks.platform {
        write(&test.join("SupercovListener.java"), LISTENER_SOURCE)?;
    }
    if frameworks.testng {
        write(
            &test.join("SupercovTestNGListener.java"),
            TESTNG_LISTENER_SOURCE,
        )?;
    }

    // The platform listener is compiled from the project's own test sources,
    // so the launcher API it implements has to be on the compile classpath. A
    // TestNG-only project needs none of that: it already depends on the
    // framework its own listener implements.
    let mut added_launcher = None;
    match if frameworks.platform {
        build
    } else {
        JvmBuild::Plain
    } {
        JvmBuild::Maven => {
            let pom = workspace.join("pom.xml");
            if let Ok(existing) = fs::read_to_string(&pom)
                && let Some(updated) = maven_with_launcher(&existing)
            {
                write(&pom, &updated)?;
                added_launcher = Some("pom.xml");
            }
        }
        JvmBuild::Gradle => {
            for name in ["build.gradle.kts", "build.gradle"] {
                let path = workspace.join(name);
                let Ok(existing) = fs::read_to_string(&path) else {
                    continue;
                };
                if let Some(updated) = gradle_with_launcher(&existing, name.ends_with(".kts")) {
                    write(&path, &updated)?;
                    added_launcher = Some(if name.ends_with(".kts") {
                        "build.gradle.kts"
                    } else {
                        "build.gradle"
                    });
                }
                break;
            }
        }
        // Nothing resolves dependencies for a plain tree; whoever compiles it
        // supplies the classpath.
        JvmBuild::Plain => {}
    }

    let resources = workspace.join("src/test/resources");
    if frameworks.platform {
        write(
            &resources.join(SERVICES_FILE),
            &format!("{LISTENER_CLASS}\n"),
        )?;
        let properties = resources.join("junit-platform.properties");
        let existing = fs::read_to_string(&properties).ok();
        write(&properties, &sequential_properties(existing.as_deref()))?;
    }
    if frameworks.testng {
        write(
            &resources.join(TESTNG_SERVICES_FILE),
            &format!("{TESTNG_LISTENER_CLASS}\n"),
        )?;
    }

    // A test class's file, so a result can name where it came from. Matched on
    // the class rather than the test, because the name a framework reports for
    // a test is its own and need not be a method at all.
    let mut declared_in = BTreeMap::new();
    for (relative, _) in &project.files.tests {
        if let Some(stem) = relative
            .rsplit('/')
            .next()
            .and_then(|name| name.split('.').next())
        {
            declared_in.insert(stem.to_owned(), relative.clone());
        }
    }

    Ok(InstrumentedWorkspace {
        project,
        build,
        evidence,
        declared_in,
        added_launcher,
    })
}

/// The fingerprint a later query compares against the stored run.
pub fn current_jvm_integrity(
    root: &Path,
    command: &[String],
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = canonicalize_simplified(root).map_err(|error| error.to_string())?;
    let files = crate::jvm_project::discover_jvm_files(&root)?;
    create_explicit_run_integrity(
        &root,
        &jvm_integrity_inputs(&files, command),
        &FrontendIntegrityInputs::embedded_jvm(),
    )
    .map_err(|error| error.to_string())
}

pub fn run_direct_jvm(
    request: &DirectJvmRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<DirectJvmRunResult, String> {
    if request.command.is_empty() {
        return Err("test command must not be empty".into());
    }
    let total_started = Instant::now();
    let initialization_started = Instant::now();
    let root = canonicalize_simplified(&request.root)
        .map_err(|error| format!("{}: {error}", request.root.display()))?;
    let mut lock = ProjectLock::acquire(&root, &request.run_id, &request.started_at)
        .map_err(|error| error.to_string())?;
    let initialization_ms = elapsed_ms(initialization_started);
    let work_directory = root.join(".supercov/work").join(&request.run_id);
    let result = (|| {
        let recovered_runs = recover_abandoned_runs(&root, &request.started_at)
            .map_err(|error| error.to_string())?;
        if !recovered_runs.is_empty() {
            writeln!(
                diagnostics,
                "[supercov] recovered abandoned run(s): {}",
                recovered_runs.join(", ")
            )
            .map_err(|error| error.to_string())?;
        }

        let adapter_started = Instant::now();
        let files = crate::jvm_project::discover_jvm_files(&root)?;
        let integrity_inputs = jvm_integrity_inputs(&files, &request.command);
        let assertion_inputs =
            crate::assertion_inputs::capture(&root, "jvm", integrity_inputs.assertion_paths())?;
        let integrity = create_explicit_run_integrity(
            &root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_jvm(),
        )
        .map_err(|error| error.to_string())?;

        let workspace_started = Instant::now();
        recover_cached_workspace(&root, &lock).map_err(|error| error.to_string())?;
        let workspace =
            prepare_cached_workspace(&root, &lock, &[]).map_err(|error| error.to_string())?;
        let evidence_directory = work_directory.join("jvm");
        fs::create_dir_all(&evidence_directory).map_err(|error| error.to_string())?;
        let instrumented = instrument_workspace(&workspace, &evidence_directory)?;
        let workspace_preparation_ms = elapsed_ms(workspace_started);
        let adapter_setup_ms = (elapsed_ms(adapter_started) - workspace_preparation_ms).max(0.0);
        writeln!(
            diagnostics,
            "[supercov] detected {}; instrumenting {} source file(s) in isolated workspace {}",
            match instrumented.build {
                JvmBuild::Maven => "a Maven project",
                JvmBuild::Gradle => "a Gradle project",
                JvmBuild::Plain => "Java/Kotlin sources",
            },
            instrumented.project.instrumented.len(),
            workspace.display()
        )
        .map_err(|error| error.to_string())?;
        if let Some(build_file) = instrumented.added_launcher {
            writeln!(
                diagnostics,
                "[supercov] added a test-scoped {LAUNCHER_ARTIFACT} to the workspace's {build_file}: per-test attribution comes from a JUnit Platform listener, and the API it implements is on the test runtime classpath but not the compile one. Your own {build_file} is untouched."
            )
            .map_err(|error| error.to_string())?;
        }
        for (file, reason) in &instrumented.project.unparseable {
            writeln!(
                diagnostics,
                "[supercov] could not parse {file}: {reason}; it carries no obligations"
            )
            .map_err(|error| error.to_string())?;
        }

        let (command, note) = command_with_fresh_results(instrumented.build, &request.command);
        if let Some(note) = note {
            writeln!(diagnostics, "[supercov] {note}").map_err(|error| error.to_string())?;
        }
        let test_started = Instant::now();
        let plan = ExecutionPlan {
            preparation: Vec::new(),
            test: ExecutionPhase {
                name: "test".into(),
                kind: PhaseKind::Test,
                command: CommandSpec {
                    program: command[0].clone().into(),
                    arguments: command[1..].iter().map(OsString::from).collect(),
                    cwd: workspace.clone(),
                    environment: None,
                    captured_output: None,
                },
            },
        };
        let options = SupervisionOptions::from_environment().map_err(|error| error.to_string())?;
        let execution = execute_plan(&plan, options, diagnostics, |_, _| Ok(()))
            .map_err(|error| error.to_string())?;
        let test_command_ms = elapsed_ms(test_started);
        if let Some(signal) = execution.interrupted_signal {
            return Err(format!(
                "the test command was interrupted by {signal:?}; no run was published"
            ));
        }
        let exit_code = execution.exit_code;

        let publication_started = Instant::now();
        let bytes = fs::read(&instrumented.evidence).map_err(|_| {
            format!(
                "the test run wrote no coverage evidence (the command exited {exit_code}). Supercov listens through the JUnit Platform, so the suite has to run on it: a TestNG-only suite is not measured this way."
            )
        })?;
        let evidence = read_evidence(&bytes)
            .map_err(|error| format!("{}: {error}", instrumented.evidence.display()))?;
        let outcomes = evidence
            .tests
            .iter()
            .map(|test| OwnedTestOutcome {
                name: test.name.clone(),
                runner: test.runner.clone(),
                // The class is the unit a JVM suite reports under, and it is
                // what a reader looks for when matching a coverage report
                // against a test report.
                package: class_of(&test.name).unwrap_or("tests").to_owned(),
                file: class_of(&test.name)
                    .and_then(|class| instrumented.declared_in.get(class))
                    .cloned(),
                status: test.status.clone(),
            })
            .collect::<Vec<_>>();
        if outcomes.is_empty() {
            return Err(format!(
                "no JVM test recorded evidence (the command exited {exit_code}); a run that measured nothing is not published"
            ));
        }
        let run = build_frontend_run(OwnedRunInputs {
            declaration: jvm_declaration(),
            environment: "jvm",
            manifest: &instrumented.project.manifest,
            probes: &instrumented.project.probes,
            evidence: &evidence,
            outcomes: &outcomes,
            run_id: &request.run_id,
            generated_at: &request.started_at,
            test_exit_code: exit_code,
            coverage_model: jvm_coverage_model(),
        })
        .map_err(|error| error.to_string())?;
        validate_frontend_report_request(&run.declaration, &run.request)
            .map_err(|error| error.to_string())?;
        let archive_path = work_directory.join("evidence.raw.gz");
        let raw = write_archive(
            crate::assertion_inputs::append(
                run.archive_entries().map_err(|error| error.to_string())?,
                &assertion_inputs,
            )?,
            &archive_path,
        )
        .map_err(|error| error.to_string())?;
        let evidence_publication_ms = elapsed_ms(publication_started);

        let timings = RunTimings {
            initialization_ms,
            workspace_preparation_ms,
            adapter_setup_ms,
            instrumented_build_ms: 0.0,
            test_command_ms,
            evidence_publication_ms,
        };
        let metadata = RunMetadata {
            id: request.run_id.clone(),
            started_at: request.started_at.clone(),
            duration_ms: elapsed_ms(total_started),
            command: request.command.clone(),
            test_exit_code: Some(exit_code),
            integrity,
            raw_evidence: RawEvidenceMetadata {
                schema_version: raw.schema_version,
                format: raw.format.into(),
                file: raw.file.into(),
                files: raw.files,
                uncompressed_bytes: raw.uncompressed_bytes,
                compressed_bytes: raw.compressed_bytes,
            },
            isolated_build: Some(true),
            instrumented_build_cache: None,
            timings: Some(timings),
            merged: None,
            parents: None,
        };
        let run_directory =
            publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        Ok(DirectJvmRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code,
            tests: outcomes
                .iter()
                .map(|outcome| outcome.name.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            source_files: instrumented.project.instrumented.len(),
            build: instrumented.build,
            recovered_runs,
            metadata,
        })
    })();
    if result.is_err() {
        let _ = remove_stored_tree_deferred(&root, &work_directory);
    }
    let release = lock.release().map_err(|error| error.to_string());
    match (result, release) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_execution_is_turned_off_without_discarding_what_else_was_set() {
        // Two tests running at once share one probe array, so neither can be
        // credited with what it reached. The runtime notices and says so; this
        // is the half that means it never has to.
        let existing = "junit.jupiter.testinstance.lifecycle.default=per_class\n\
                        junit.jupiter.execution.parallel.enabled=true\n\
                        junit.jupiter.displayname.generator.default=org.junit.jupiter.api.DisplayNameGenerator$ReplaceUnderscores\n";
        let updated = sequential_properties(Some(existing));
        assert!(updated.contains("junit.jupiter.execution.parallel.enabled=false"));
        assert!(
            !updated.contains("parallel.enabled=true"),
            "the project's own setting must not survive:\n{updated}"
        );
        // Everything the project chose for reasons of its own stays.
        assert!(updated.contains("testinstance.lifecycle.default=per_class"));
        assert!(updated.contains("displayname.generator.default"));

        // And a project with no properties at all still gets the setting.
        assert!(
            sequential_properties(None).contains("junit.jupiter.execution.parallel.enabled=false")
        );
    }

    #[test]
    fn gradle_is_told_to_run_the_tests_again() {
        // Gradle skips a test task it considers up to date, and a task that
        // does not run records nothing.
        let (command, note) = command_with_fresh_results(
            JvmBuild::Gradle,
            &["./gradlew".to_owned(), "test".to_owned()],
        );
        assert_eq!(command, ["./gradlew", "test", "--rerun-tasks"]);
        assert!(note.is_some());

        // An author who already said so means it.
        let (command, note) = command_with_fresh_results(
            JvmBuild::Gradle,
            &[
                "./gradlew".to_owned(),
                "test".to_owned(),
                "--rerun".to_owned(),
            ],
        );
        assert_eq!(command, ["./gradlew", "test", "--rerun"]);
        assert!(note.is_none());

        // Maven's lifecycle re-runs surefire every time; nothing to defeat.
        let (command, note) =
            command_with_fresh_results(JvmBuild::Maven, &["mvn".to_owned(), "test".to_owned()]);
        assert_eq!(command, ["mvn", "test"]);
        assert!(note.is_none());
    }

    #[test]
    fn a_configuration_literal_survives_a_path_java_would_have_read_as_escapes() {
        // A Windows path is full of backslashes, and one of them landing
        // before a `t` or a `"` would change the path or end the literal.
        let configuration = configuration(7, &[2, 3], Path::new(r"C:\tmp\runs\evidence.bin"));
        assert!(
            configuration.contains(r#""C:\\tmp\\runs\\evidence.bin""#),
            "{configuration}"
        );
        assert!(configuration.contains("PROBES = 7"));
        assert!(configuration.contains("new int[] {2, 3}"));
    }

    #[test]
    fn a_tests_class_is_read_from_the_name_the_framework_chose() {
        // JUnit reports `CalculatorTest#zeroIsNamed()`; a DSL framework
        // reports its own wording under the same class.
        assert_eq!(
            class_of("CalculatorTest#zeroIsNamed()"),
            Some("CalculatorTest")
        );
        assert_eq!(
            class_of("CalculatorSpec#a sum adds its parts"),
            Some("CalculatorSpec")
        );
        assert_eq!(class_of("Standalone"), Some("Standalone"));
        assert_eq!(class_of(""), None);
    }

    #[test]
    fn the_launcher_version_follows_whatever_junit_the_project_chose() {
        // JUnit numbers the platform 1.N alongside Jupiter 5.N, so a pinned
        // Jupiter says exactly which launcher agrees with the engine that will
        // run. Guessing instead could pair a launcher with an engine it does
        // not understand.
        assert_eq!(
            launcher_version("<artifactId>junit-jupiter</artifactId><version>5.10.2</version>")
                .as_deref(),
            Some("1.10.2")
        );
        assert_eq!(
            launcher_version("testImplementation 'org.junit.jupiter:junit-jupiter:5.13.1'")
                .as_deref(),
            Some("1.13.1")
        );
        // A project using the BOM has already said how every JUnit artifact is
        // versioned; naming one would override the answer it gave.
        assert_eq!(
            launcher_version("<artifactId>junit-bom</artifactId><version>5.11.0</version>"),
            None
        );
        // And one that says nothing gets a launcher new enough to drive an
        // older engine, which is the direction that works.
        assert_eq!(
            launcher_version("<artifactId>junit-jupiter</artifactId>").as_deref(),
            Some(DEFAULT_LAUNCHER_VERSION)
        );
    }

    #[test]
    fn the_launcher_is_added_once_and_only_where_it_is_missing() {
        let pom = "<project>\n  <dependencies>\n    <dependency>\n      <groupId>org.junit.jupiter</groupId>\n      <artifactId>junit-jupiter</artifactId>\n      <version>5.10.2</version>\n      <scope>test</scope>\n    </dependency>\n  </dependencies>\n</project>\n";
        let updated = maven_with_launcher(pom).expect("the launcher is missing");
        assert!(updated.contains("junit-platform-launcher"), "{updated}");
        assert!(updated.contains("<version>1.10.2</version>"), "{updated}");
        // Inside the existing block, not after it.
        assert!(
            updated.find("junit-platform-launcher") < updated.find("</dependencies>"),
            "{updated}"
        );
        // A project that already has it is left exactly as it is.
        assert_eq!(maven_with_launcher(&updated), None);

        // And one with no dependencies block at all still gets a valid pom.
        let bare = "<project>\n  <artifactId>demo</artifactId>\n</project>\n";
        let updated = maven_with_launcher(bare).expect("a block is created");
        assert!(updated.contains("<dependencies>"), "{updated}");
        assert!(
            updated.find("</dependencies>") < updated.find("</project>"),
            "{updated}"
        );
    }

    #[test]
    fn gradle_gets_the_launcher_in_the_dialect_its_script_is_written_in() {
        let groovy =
            "dependencies {\n    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.2'\n}\n";
        let updated = gradle_with_launcher(groovy, false).expect("the launcher is missing");
        assert!(
            updated
                .contains("testImplementation 'org.junit.platform:junit-platform-launcher:1.10.2'"),
            "{updated}"
        );
        // The project's own block survives: Gradle merges what we append.
        assert!(updated.contains("junit-jupiter:5.10.2"), "{updated}");
        assert_eq!(gradle_with_launcher(&updated, false), None);

        // Gradle 9 makes every project declare the launcher, and the
        // configuration its own documentation recommends is testRuntimeOnly,
        // which the tests run with but do not compile against. A project
        // following that advice has the artifact and still cannot compile a
        // listener, so it gets a compile-visible declaration alongside.
        let runtime_only = "dependencies {\n    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.2'\n    testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n}\n";
        let updated = gradle_with_launcher(runtime_only, false)
            .expect("a runtime-only declaration does not reach the compiler");
        assert!(
            updated.contains("testImplementation 'org.junit.platform:junit-platform-launcher"),
            "{updated}"
        );
        assert!(
            updated.contains("testRuntimeOnly 'org.junit.platform:junit-platform-launcher'"),
            "the project's own declaration stays:\n{updated}"
        );

        let kotlin = "dependencies {\n    testImplementation(\"org.junit.jupiter:junit-jupiter:5.10.2\")\n}\n";
        let updated = gradle_with_launcher(kotlin, true).expect("the launcher is missing");
        assert!(
            updated.contains(
                "testImplementation(\"org.junit.platform:junit-platform-launcher:1.10.2\")"
            ),
            "{updated}"
        );
    }

    #[test]
    fn only_the_listeners_a_project_can_compile_are_written() {
        // Each listener is compiled from the project's own test sources, so
        // one whose framework is absent would fail on imports the project
        // never asked for.
        let junit = frameworks("<artifactId>junit-jupiter</artifactId>");
        assert!(junit.platform && !junit.testng);

        let testng = frameworks("<artifactId>testng</artifactId>");
        assert!(testng.testng && !testng.platform);

        // A migration in progress runs both, and both listeners fire in one
        // JVM against one runtime.
        let both = frameworks("testng ... junit-jupiter");
        assert!(both.platform && both.testng);

        // Kotest and Spock are platform engines, so the platform listener
        // reports their tests without either being named.
        assert!(frameworks("io.kotest:kotest-runner-junit5").platform);
        assert!(frameworks("org.spockframework:spock-core").platform);

        // And a project naming nothing recognisable gets the platform, which
        // is what nearly every JVM suite runs on.
        let unknown = frameworks("<artifactId>demo</artifactId>");
        assert!(unknown.platform && !unknown.testng);
    }
}
