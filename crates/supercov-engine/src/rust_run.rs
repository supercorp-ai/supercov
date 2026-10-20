//! Public, isolated Rust coverage run lifecycle.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    evidence_archive::write_archive,
    integrity::{ExplicitIntegrityInputs, FrontendIntegrityInputs, create_explicit_run_integrity},
    lifecycle::{
        ProjectLock, finalize_published_run, publish_run, recover_abandoned_runs,
        remove_stored_tree_deferred,
    },
    run_store::{InstrumentedBuildCache, RawEvidenceMetadata, RunMetadata, RunTimings},
    rust_build_cache::{
        read_rust_build_cache, rust_build_cache_key, rust_target_directory, write_rust_build_cache,
    },
    rust_project::{PreparedRustProject, prepare_rust_project_for},
    rust_test_runner::run_prepared_rust_tests,
    workspace::{cached_workspace_path, prepare_cached_workspace, recover_cached_workspace},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectRustRunRequest {
    pub root: PathBuf,
    pub command: Vec<String>,
    pub run_id: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectRustRunResult {
    pub run_id: String,
    pub run_directory: PathBuf,
    pub exit_code: i32,
    pub tests: usize,
    pub artifacts: usize,
    pub recovered_runs: Vec<String>,
    pub metadata: RunMetadata,
}

fn elapsed_ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

const ROOT_INPUT_EXCLUSIONS: &[&str] = &[
    ".cache",
    ".git",
    ".supercov",
    ".mcdc-pool",
    "node_modules",
    "target",
    "build",
    "dist",
    ".next",
    ".nuxt",
    ".output",
    "coverage",
    "playwright-report",
    "test-results",
];

/// An LLVM profile, written to the working directory by default: what a
/// binary built for coverage leaves, never what a build reads. Counted as
/// configuration, writing one -- to import its lines -- made every run stale.
fn is_coverage_output(name: &str) -> bool {
    let extension = Path::new(name).extension().and_then(|value| value.to_str());
    matches!(extension, Some("profraw" | "profdata"))
}

fn collect_project_inputs(
    root: &Path,
    directory: &Path,
    root_level: bool,
    regular: &mut Vec<PathBuf>,
    links: &mut Vec<String>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("Rust project contains a non-UTF-8 path: {}", path.display()))?;
        if (root_level && ROOT_INPUT_EXCLUSIONS.contains(&name.as_str()))
            // wrangler's local state (`.wrangler/`), which `wrangler dev`
            // writes beside a Worker: what it served, never what Cargo reads.
            || matches!(name.as_str(), ".supercov" | ".mcdc-pool" | ".wrangler")
            || is_coverage_output(&name)
        {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            // What a build wrote, wherever it lies: a Cargo target directory
            // of a crate in a subdirectory, the wasm wasm-bindgen generated.
            // Rebuilding is not a change to the project.
            if build_output_directory(&path) {
                continue;
            }
            collect_project_inputs(root, &path, false, regular, links)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("project input escaped root: {}", path.display()))?;
            regular.push(relative.to_owned());
        } else if file_type.is_symlink() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("project link escaped root: {}", path.display()))?;
            let target = fs::read_link(&path).map_err(|error| error.to_string())?;
            links.push(format!(
                "{}=>{}",
                relative.to_string_lossy().replace('\\', "/"),
                target.to_string_lossy().replace('\\', "/")
            ));
        } else {
            return Err(format!(
                "unsupported Rust project input: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// A Cargo target directory, told by what Cargo keeps in it, or the output
/// of wasm-bindgen, told by its `<name>_bg.wasm`.
fn build_output_directory(path: &Path) -> bool {
    if path.join(".rustc_info.json").is_file()
        || path.join(".cargo-lock").is_file()
        || fs::read(path.join("CACHEDIR.TAG"))
            .is_ok_and(|tag| tag.windows(5).any(|window| window == b"cargo"))
    {
        return true;
    }
    fs::read_dir(path).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with("_bg.wasm"))
        })
    })
}

/// The files the project's `.gitignore` files leave in, relative to `root`;
/// None outside a git repository, where nothing is ignored.
fn kept_by_gitignore(root: &Path) -> Option<BTreeSet<PathBuf>> {
    if !root
        .ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
    {
        return None;
    }
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .git_ignore(true)
        .parents(!crate::source_discovery::ignored_by_enclosing_repository(
            root,
        ))
        .require_git(true)
        .follow_links(false)
        .build();
    Some(
        walker
            .filter_map(Result::ok)
            .filter_map(|entry| entry.path().strip_prefix(root).ok().map(Path::to_owned))
            .collect(),
    )
}

pub(crate) fn collect_integrity_inputs(
    root: &Path,
    command: &[String],
) -> Result<ExplicitIntegrityInputs, String> {
    let mut files = Vec::new();
    let mut links = Vec::new();
    collect_project_inputs(root, root, true, &mut files, &mut links)?;
    // What the project's own `.gitignore` excludes is no input a checkout
    // carries: a `.zig-cache/` cargo-zigbuild leaves, a local `.env`. Counted
    // as configuration, it made a run copied to CI read as stale. Only the
    // project's `.gitignore` files decide, inside a git repository, as for
    // source discovery: global excludes differ between machines.
    let tracked = kept_by_gitignore(root);
    files.retain(|file| tracked.as_ref().is_none_or(|kept| kept.contains(file)));
    files.sort();
    files.dedup();
    links.sort();
    links.dedup();
    let source_files = files
        .iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .cloned()
        .collect::<Vec<_>>();
    // Inline `#[cfg(test)]` modules make every Rust source file a possible test
    // input. Hashing the same file in both domains is intentional and prevents
    // stale reuse when only an inline test changes.
    let test_files = source_files.clone();
    let dependency_files = files
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| matches!(name, "Cargo.toml" | "Cargo.lock"))
        })
        .cloned()
        .collect::<Vec<_>>();
    let source_set = source_files.iter().cloned().collect::<BTreeSet<_>>();
    let dependency_set = dependency_files.iter().cloned().collect::<BTreeSet<_>>();
    let configuration_files = files
        .into_iter()
        .filter(|path| !source_set.contains(path) && !dependency_set.contains(path))
        .collect();
    let mut execution_configuration = command.join("\0").into_bytes();
    for link in links {
        execution_configuration.push(0);
        execution_configuration.extend_from_slice(link.as_bytes());
    }
    Ok(ExplicitIntegrityInputs {
        source_files,
        test_files,
        dependency_files,
        configuration_files,
        execution_configuration,
    })
}

pub fn current_rust_integrity(
    root: &Path,
    command: &[String],
    source_roots: Option<&[String]>,
) -> Result<crate::run_store::RunIntegrity, String> {
    let root = fs::canonicalize(root).map_err(|error| error.to_string())?;
    let mut inputs = collect_integrity_inputs(&root, command)?;
    crate::source_discovery::fold_roots_into_configuration(
        &mut inputs.execution_configuration,
        source_roots,
    );
    create_explicit_run_integrity(&root, &inputs, &FrontendIntegrityInputs::embedded_rust())
        .map_err(|error| error.to_string())
}

pub fn run_direct_rust(
    request: &DirectRustRunRequest,
    diagnostics: &mut dyn Write,
) -> Result<DirectRustRunResult, String> {
    run_direct_rust_timed(
        request,
        diagnostics,
        std::env::var("SUPERCOV_PHASE_TIMING").as_deref() == Ok("1"),
    )
}

/// `run_direct_rust`, saying where preparing the workspace went when
/// `phase_timing` (`SUPERCOV_PHASE_TIMING=1`).
fn run_direct_rust_timed(
    request: &DirectRustRunRequest,
    diagnostics: &mut dyn Write,
    phase_timing: bool,
) -> Result<DirectRustRunResult, String> {
    if request.command.is_empty() {
        return Err("test command must not be empty".into());
    }
    // The shared probe runtime maps its evidence file and hooks thread and
    // process creation on macOS, Linux and Windows; on any other host every
    // probe is a no-op, so a run there would report zero coverage without a
    // word of explanation. Refuse plainly instead.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err(
        "Rust suites are not supported on this platform: the probe transport has no implementation here, so a run would measure nothing"
            .into(),
    );
    let total_started = Instant::now();
    let initialization_started = Instant::now();
    let root = fs::canonicalize(&request.root)
        .map_err(|error| format!("{}: {error}", request.root.display()))?;
    let mut lock = ProjectLock::acquire(&root, &request.run_id, &request.started_at)
        .map_err(|error| error.to_string())?;
    let initialization_ms = elapsed_ms(initialization_started);
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

        // `wasm-pack test` is learned once and then run as the `cargo test` it
        // stands for; the run keeps the command it was given.
        let learned = if crate::rust_wasm_pack::is_wasm_pack_test(&request.command) {
            Some(crate::rust_wasm_pack::learn(
                &root,
                &request.command,
                &root.join(".supercov/work").join(&request.run_id),
            )?)
        } else {
            None
        };
        let command: &[String] = learned
            .as_ref()
            .map_or(&request.command, |learned| &learned.command);
        let runner_environment = learned
            .as_ref()
            .map(|learned| learned.runner_environment.clone())
            .unwrap_or_default();
        let adapter_started = Instant::now();
        let source_roots = crate::source_discovery::configured_source_roots(
            &std::env::vars().collect::<std::collections::BTreeMap<_, _>>(),
        );
        let mut integrity_inputs = collect_integrity_inputs(&root, &request.command)?;
        crate::source_discovery::fold_roots_into_configuration(
            &mut integrity_inputs.execution_configuration,
            source_roots.as_deref(),
        );
        let source_inputs =
            crate::source_capture::capture(&root, "rust", integrity_inputs.assertion_paths())?;
        let integrity = create_explicit_run_integrity(
            &root,
            &integrity_inputs,
            &FrontendIntegrityInputs::embedded_rust(),
        )
        .map_err(|error| error.to_string())?;
        let build_cache_key = rust_build_cache_key(&integrity, &request.command)
            .map_err(|error| error.to_string())?;

        let workspace_started = Instant::now();
        recover_cached_workspace(&root, &lock).map_err(|error| error.to_string())?;
        let workspace = cached_workspace_path(&root, &lock).map_err(|error| error.to_string())?;
        if let Some(planted) = crate::workspace::planted_cargo_configuration(&root, &workspace) {
            return Err(format!(
                "{} would change how Supercov builds the project but not how `cargo test` does; remove it",
                planted.display()
            ));
        }
        let target_directory = rust_target_directory(&root);
        let cache_started = Instant::now();
        let cached = read_rust_build_cache(&workspace, &target_directory, &build_cache_key);
        let cache_read_ms = elapsed_ms(cache_started);
        let mut copy_ms = 0.0;
        let reused_build = cached.is_some();
        let mut project = if let Some(cached) = cached {
            writeln!(
                diagnostics,
                "[supercov] detected Rust; reusing authenticated instrumented workspace {}",
                workspace.display()
            )
            .map_err(|error| error.to_string())?;
            PreparedRustProject {
                workspace_root: workspace.clone(),
                target_directory: target_directory.clone(),
                source_files: cached.source_files,
                crate_roots: Vec::new(),
                runtime_module: String::new(),
                manifest: cached.manifest,
                probes: cached.probes,
                preparation: Default::default(),
            }
        } else {
            let copy_started = Instant::now();
            let workspace =
                prepare_cached_workspace(&root, &lock, &[]).map_err(|error| error.to_string())?;
            copy_ms = elapsed_ms(copy_started);
            writeln!(
                diagnostics,
                "[supercov] detected Rust; instrumenting isolated Cargo workspace {}",
                workspace.display()
            )
            .map_err(|error| error.to_string())?;
            let ambient = std::env::vars().collect::<std::collections::BTreeMap<_, _>>();
            let roots = crate::source_discovery::ExplicitSourceRoots::from_environment_in(
                &root, &workspace, &ambient,
            )
            .map_err(|error| error.to_string())?;
            // The target the command builds for decides which `#[cfg]` items
            // exist; when it cannot be read, every item is measured.
            let cfg = crate::rust_test_runner::cargo_invocation(&workspace, command)
                .ok()
                .and_then(|invocation| {
                    crate::rust_cargo_configuration::active_target_cfg(&workspace, &invocation)
                })
                .map(|values| crate::rust_instrumenter::TargetCfg::new(&values));
            prepare_rust_project_for(&workspace, roots.as_ref(), cfg.as_ref())
                .map_err(|error| error.to_string())?
        };
        project.target_directory = target_directory;
        fs::create_dir_all(&project.target_directory).map_err(|error| error.to_string())?;
        let workspace_preparation_ms = elapsed_ms(workspace_started);
        let adapter_setup_ms = (elapsed_ms(adapter_started) - workspace_preparation_ms).max(0.0);
        if phase_timing {
            let preparation = &project.preparation;
            writeln!(
                diagnostics,
                "[supercov] workspace timings cache-check={cache_read_ms:.1}ms copy={copy_ms:.1}ms metadata={:.1}ms discovery={:.1}ms instrument={:.1}ms runtime={:.1}ms",
                preparation.metadata_ms,
                preparation.discovery_ms,
                preparation.instrument_ms,
                preparation.runtime_ms,
            )
            .map_err(|error| error.to_string())?;
        }

        let nextest = request
            .command
            .windows(2)
            .any(|pair| pair == ["nextest", "run"]);
        writeln!(
            diagnostics,
            "{}",
            if nextest {
                "[supercov] running cargo nextest with Supercov as its target runner, each attempt in its own process"
            } else {
                "[supercov] building once and running each libtest case and doctest in its own process"
            }
        )
        .map_err(|error| error.to_string())?;
        // What is in the copy now is Supercov's; what the command adds to
        // it or changes is the command's, and belongs in the project.
        // A test that starts a Rust Worker with `wrangler dev` finds
        // Supercov's `wrangler` first, which reads what the Worker ran into
        // that test's evidence. Prepared before the copy is taken stock of:
        // the configurations it points at Supercov's entry are Supercov's.
        let rust_workers = crate::rust_worker::CargoTestWorkers::prepare(
            &workspace,
            &workspace
                .join(".supercov/rust-evidence")
                .join(&request.run_id)
                .join("wrangler"),
        )?;
        let before = crate::workspace::workspace_output_baseline(&workspace)
            .map_err(|error| error.to_string())?;
        let run = run_prepared_rust_tests(
            &project,
            command,
            &request.run_id,
            &request.started_at,
            rust_workers.as_ref(),
            &runner_environment,
            diagnostics,
        )
        .map_err(|error| error.to_string())?;
        // A snapshot a test accepted, a report: they stayed in the copy, and
        // the project had the old ones or none.
        let outputs = crate::workspace::sync_outputs_of(
            crate::workspace::CommandOutputs::Rust,
            &root,
            &workspace,
            &before,
            &BTreeSet::new(),
        )
        .map_err(|error| error.to_string())?;
        // What stayed is the lockfile and the sources, which nobody asked
        // for: only what flowed back is said.
        for line in crate::workspace::describe_command_outputs(&outputs)
            .into_iter()
            .filter(|line| !line.contains("stayed in the isolated workspace"))
        {
            writeln!(diagnostics, "{line}").map_err(|error| error.to_string())?;
        }
        write_rust_build_cache(
            &root,
            &workspace,
            &build_cache_key,
            &request.started_at,
            &project.source_files,
            // The manifest the run reported: pruned to what the build
            // compiled, so a reused build reuses the same denominator.
            &run.request.manifest,
            &project.probes,
            &run.artifact_files,
        )?;

        let publication_started = Instant::now();
        let archive_path = root
            .join(".supercov/work")
            .join(&request.run_id)
            .join("evidence.raw.gz");
        let raw = write_archive(
            crate::source_capture::append(
                run.archive_entries().map_err(|error| error.to_string())?,
                &source_inputs,
            )?,
            &archive_path,
        )
        .map_err(|error| error.to_string())?;
        remove_stored_tree_deferred(
            &root,
            &workspace
                .join(".supercov/rust-evidence")
                .join(&request.run_id),
        )
        .map_err(|error| error.to_string())?;
        let evidence_publication_ms = elapsed_ms(publication_started);
        let timings = RunTimings {
            initialization_ms,
            workspace_preparation_ms,
            adapter_setup_ms,
            instrumented_build_ms: (run.build_ms * 10.0).round() / 10.0,
            test_command_ms: (run.execution_ms * 10.0).round() / 10.0,
            evidence_publication_ms,
        };
        let metadata = RunMetadata {
            id: request.run_id.clone(),
            started_at: request.started_at.clone(),
            duration_ms: elapsed_ms(total_started),
            command: request.command.clone(),
            test_exit_code: Some(run.exit_code),
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
            instrumented_build_cache: Some(InstrumentedBuildCache {
                key: build_cache_key,
                reused: reused_build,
            }),
            timings: Some(timings),
            merged: None,
            parents: None,
            source_roots: source_roots.clone(),
            copy: Some(lock.copy()),
            merged_across: None,
            recorded_by: None,
            companions: None,
        };
        let run_directory =
            publish_run(&root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        finalize_published_run(&root, &request.run_id).map_err(|error| error.to_string())?;
        Ok(DirectRustRunResult {
            run_id: request.run_id.clone(),
            run_directory,
            exit_code: run.exit_code,
            // Tests, not attempts: a retried test is still one test.
            tests: run
                .request
                .raw_results
                .iter()
                .map(|result| result.test.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            artifacts: run.artifacts,
            recovered_runs,
            metadata,
        })
    })();
    if result.is_err() {
        let _ =
            remove_stored_tree_deferred(&root, &root.join(".supercov/work").join(&request.run_id));
        if let Ok(workspace) = cached_workspace_path(&root, &lock) {
            let _ = remove_stored_tree_deferred(
                &root,
                &workspace
                    .join(".supercov/rust-evidence")
                    .join(&request.run_id),
            );
        }
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

    fn project(name: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-inputs-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".zig-cache/o")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("rust-toolchain.toml"), "").unwrap();
        fs::write(root.join(".zig-cache/o/build.o"), "x").unwrap();
        fs::write(root.join(".gitignore"), ".zig-cache/\n").unwrap();
        root
    }

    /// What a build or a coverage run wrote is no input: a profile, and a
    /// directory Cargo marks by its info file, its lock or its cache tag. A
    /// link is an input by where it points; a FIFO is no input Supercov
    /// knows how to fingerprint.
    #[cfg(unix)]
    #[test]
    fn build_output_profiles_and_links_are_told_from_inputs() {
        let root = project("outputs");
        for (file, text) in [
            ("default.profraw", "x"),
            ("sub/merged.profdata", "x"),
            ("crate/target/.rustc_info.json", "{}"),
            ("crate/target/debug/a", "x"),
            ("locked/.cargo-lock", ""),
            ("locked/a", "x"),
            (
                "tagged/CACHEDIR.TAG",
                "Signature: 8a477f597d28d172789f06886806bc55\n# cargo\n",
            ),
            ("tagged/a", "x"),
            (
                "other/CACHEDIR.TAG",
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            ),
        ] {
            fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            fs::write(root.join(file), text).unwrap();
        }
        std::os::unix::fs::symlink("src/lib.rs", root.join("alias.rs")).unwrap();
        let (mut files, mut links) = (Vec::new(), Vec::new());
        collect_project_inputs(&root, &root, true, &mut files, &mut links).unwrap();
        let files = files
            .iter()
            .map(|file| file.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();
        for left_out in [
            "default.profraw",
            "sub/merged.profdata",
            "crate/target/debug/a",
            "locked/a",
            "tagged/a",
        ] {
            assert!(
                !files.iter().any(|file| file == left_out),
                "{left_out}: {files:?}"
            );
        }
        assert!(
            files.contains(&"other/CACHEDIR.TAG".to_owned()),
            "{files:?}"
        );
        assert_eq!(links, ["alias.rs=>src/lib.rs"]);
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("pipe"))
                .status()
                .unwrap()
                .success()
        );
        let refused = collect_project_inputs(&root, &root, true, &mut Vec::new(), &mut Vec::new())
            .unwrap_err();
        assert!(
            refused.contains("unsupported Rust project input"),
            "{refused}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A build cache the project's `.gitignore` excludes is no configuration:
    /// a run copied to CI, where it is absent, read as stale. Outside a git
    /// repository nothing is ignored.
    #[test]
    fn what_the_project_ignores_in_git_is_no_input() {
        let inside = project("git");
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&inside)
                .status()
                .unwrap()
                .success()
        );
        let inputs = collect_integrity_inputs(&inside, &["cargo".into(), "test".into()]).unwrap();
        let configuration = inputs
            .configuration_files
            .iter()
            .map(|file| file.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();
        assert!(
            configuration.contains(&"rust-toolchain.toml".to_owned()),
            "{configuration:?}"
        );
        assert!(
            !configuration
                .iter()
                .any(|file| file.starts_with(".zig-cache")),
            "{configuration:?}"
        );
        let outside = project("plain");
        let inputs = collect_integrity_inputs(&outside, &["cargo".into(), "test".into()]).unwrap();
        assert!(
            inputs
                .configuration_files
                .iter()
                .any(|file| file.starts_with(".zig-cache")),
            "outside git nothing is ignored"
        );
        fs::remove_dir_all(&inside).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    fn rust_request(root: &Path, command: &[&str]) -> DirectRustRunRequest {
        DirectRustRunRequest {
            root: root.to_owned(),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            run_id: "2026-01-02T00-00-00-000Z".into(),
            started_at: "2026-01-02T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_rust_run_refuses_an_empty_command() {
        let root = std::env::temp_dir().join("supercov-rust-empty-command-never-created");
        let error = run_direct_rust(&rust_request(&root, &[]), &mut Vec::new()).unwrap_err();
        assert_eq!(error, "test command must not be empty");
        assert!(!root.exists());
    }

    /// A run recovers the runs abandoned before it, says where preparing
    /// the workspace went when asked, and a command Cargo refuses leaves
    /// nothing of the run behind.
    #[test]
    fn a_run_says_what_it_recovered_and_how_long_preparing_took() {
        let root = project("recovered");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::remove_file(root.join("rust-toolchain.toml")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn one() -> u8 {\n    1\n}\n").unwrap();
        let abandoned = "2026-01-01T00-00-00-000Z";
        fs::create_dir_all(root.join(".supercov/work").join(abandoned)).unwrap();
        crate::lifecycle::write_run_state(
            &root,
            &crate::lifecycle::RunState {
                id: abandoned.into(),
                pid: u32::MAX,
                root: root.display().to_string(),
                workspace: root.display().to_string(),
                started_at: "start".into(),
                updated_at: "update".into(),
                status: crate::lifecycle::RunStateStatus::Testing,
                signal: None,
                error: None,
            },
        )
        .unwrap();
        let request = rust_request(&root, &["cargo", "test", "--no-such-flag"]);
        let mut diagnostics = Vec::new();
        assert!(run_direct_rust_timed(&request, &mut diagnostics, true).is_err());
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains(&format!("recovered abandoned run(s): {abandoned}")),
            "{diagnostics}"
        );
        assert!(
            diagnostics.contains("[supercov] workspace timings cache-check="),
            "{diagnostics}"
        );
        assert!(!root.join(".supercov/work").join(&request.run_id).exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
