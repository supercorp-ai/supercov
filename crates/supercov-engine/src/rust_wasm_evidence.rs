//! What instrumented Rust ran in wasm, published as a Rust run of its own.
//!
//! In wasm the region runtime keeps each crate's region in the module's
//! memory, and a host takes it from there: wrangler's inspector for a Worker
//! (rust_worker.rs), a JavaScript test process at each test's edges, a
//! Playwright page as its test ends. Each take is a record with an owner --
//! a window of time, a test attempt, or nobody -- and this credits the
//! records to the browser or JavaScript tests the run reported and publishes
//! them against the crate's manifest.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use supercov_contracts::{
    ExecutionModel, FrontendRunDeclaration, LANGUAGE_FRONTEND_PROTOCOL_VERSION, StructuralSource,
};

use crate::{
    coverage_report::{
        CoveragePhase, CoverageReportRequest, ExecutionScope, ExitCodeInput, RawTestResult,
        TestProvenance,
    },
    evidence_archive::write_archive,
    frontend_protocol::validate_frontend_report_request,
    lifecycle::publish_run,
    run_store::{RawEvidenceMetadata, RunIntegrity, RunMetadata},
    rust_project::PreparedRustProject,
    rust_runtime::{RustProbeEntry, parse_rust_region, parse_rust_region_cfgs},
    rust_test_runner::RustFrontendRun,
    served_tests::ServedTest,
};

/// A Cargo workspace prepared in a subdirectory of the project names its
/// files from there; the run names them from the project's root, as every
/// other run of the project does.
pub(crate) fn root_paths(project: &mut PreparedRustProject, relative: &Path) {
    if relative.as_os_str().is_empty() {
        return;
    }
    let prefix = relative.to_string_lossy().replace('\\', "/");
    let rooted = |file: &mut String| *file = format!("{prefix}/{file}");
    let manifest = &mut project.manifest;
    manifest
        .points
        .iter_mut()
        .for_each(|point| rooted(&mut point.file));
    manifest
        .assertion_sites
        .iter_mut()
        .for_each(|point| rooted(&mut point.file));
    manifest
        .decisions
        .iter_mut()
        .for_each(|decision| rooted(&mut decision.file));
    manifest
        .branches
        .iter_mut()
        .for_each(|branch| rooted(&mut branch.file));
    for limitation in &mut manifest.limitations {
        if let Some(file) = limitation.get("file").and_then(serde_json::Value::as_str) {
            limitation["file"] = serde_json::json!(format!("{prefix}/{file}"));
        }
    }
    if let Some(entries) = manifest
        .scope
        .as_mut()
        .and_then(|scope| scope.get_mut("entries"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for entry in entries {
            if let Some(file) = entry.get("file").and_then(serde_json::Value::as_str) {
                entry["file"] = serde_json::json!(format!("{prefix}/{file}"));
            }
        }
    }
    project
        .probes
        .cfgs
        .iter_mut()
        .for_each(|gate| rooted(&mut gate.file));
    project.source_files.iter_mut().for_each(rooted);
}

/// Who a take belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Owner {
    /// Taken on a clock: credited to the one test whose span it overlaps.
    Window(i64, i64),
    /// Taken at a test's edge: that attempt's.
    Attempt(String),
    /// Taken while no test was running.
    Nobody,
}

/// One take: each instrumented crate's region, by crate key.
#[derive(Debug, Clone)]
pub(crate) struct WasmRecord {
    pub owner: Owner,
    pub regions: Vec<(String, Vec<u8>)>,
}

/// The crate the records were taken from, and how its run is named.
pub(crate) struct WasmRun<'a> {
    /// Said in the line the run prints: `Rust Worker`, `Rust wasm`.
    pub language: &'static str,
    /// The archive's file name under `.supercov/work/<run>/`.
    pub archive: &'static str,
    pub project: &'a PreparedRustProject,
    pub integrity: &'a RunIntegrity,
    pub source_inputs: &'a crate::source_manifest::Inputs,
    pub source_roots: Option<Vec<String>>,
}

/// Publish the records as a Rust run, each credited to its test. None when
/// there is nothing to publish or no test to credit.
pub(crate) fn publish(
    root: &Path,
    wasm: WasmRun,
    records: Vec<WasmRecord>,
    metadata: RunMetadata,
    served: &[ServedTest],
    diagnostics: &mut dyn Write,
) -> Result<Option<(PathBuf, RunMetadata)>, String> {
    if served.is_empty() || records.is_empty() {
        return Ok(None);
    }
    let probes = &wasm.project.probes;
    let by_time = records
        .iter()
        .any(|record| matches!(record.owner, Owner::Window(..)));
    let mut settled = vec![0_u8; probes.cfgs.len()];
    let mut by_test = BTreeMap::<usize, Vec<Vec<RustProbeEntry>>>::new();
    let mut background = Vec::<Vec<RustProbeEntry>>::new();
    let mut overlapped = BTreeSet::new();
    for record in &records {
        let running = match &record.owner {
            Owner::Window(start, end) => served
                .iter()
                .enumerate()
                .filter(|(_, test)| {
                    test.window
                        .is_some_and(|(first, last)| first <= *end && *start <= last)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            Owner::Attempt(attempt) => served
                .iter()
                .position(|test| test.attempt_id.as_deref() == Some(attempt.as_str()))
                .into_iter()
                .collect(),
            Owner::Nobody => Vec::new(),
        };
        let mut entries_of_record = Vec::new();
        for (_, bytes) in &record.regions {
            let entries = parse_rust_region(bytes, None, &probes.token, probes)
                .map_err(|error| error.to_string())?;
            for (slot, bits) in settled
                .iter_mut()
                .zip(parse_rust_region_cfgs(bytes, &probes.token, probes).unwrap_or_default())
            {
                *slot |= bits;
            }
            if !entries.is_empty() {
                entries_of_record.push(entries);
            }
        }
        if entries_of_record.is_empty() {
            continue;
        }
        match running.as_slice() {
            [owner] => by_test.entry(*owner).or_default().extend(entries_of_record),
            _ => {
                if running.len() > 1 {
                    overlapped.extend(running);
                }
                background.extend(entries_of_record);
            }
        }
    }
    let manifest = crate::rust_test_runner::settle_cfg_gates_with(
        &wasm.project.manifest,
        probes,
        &settled,
        diagnostics,
    )
    .map_err(|error| error.to_string())?;
    // The runners the tests came from: Playwright for a browser suite, or
    // whichever runs a Node suite. Each test's work reached the module in its
    // own runner's process or page, named by its attempt or its time.
    let mut names = served
        .iter()
        .map(|test| test.provenance.runner.clone())
        .filter(|runner| !runner.is_empty())
        .collect::<BTreeSet<_>>();
    if names.is_empty() {
        names.insert("playwright".into());
    }
    let background_runner = names.iter().next().cloned().unwrap_or_default();
    let run_id = &metadata.id;
    let mut raw_results = Vec::new();
    for (index, test) in served.iter().enumerate() {
        let attempt_id = format!("{run_id}:worker:{index:08}");
        let credited = by_test.remove(&index);
        let attributed = credited.is_some();
        let evidence = crate::rust_test_runner::snapshot_regions(
            &manifest,
            credited.unwrap_or_default(),
            &attempt_id,
        )
        .map_err(|error| error.to_string())?;
        let test_id = format!("{}::{}", test.file, test.test_id);
        raw_results.push(RawTestResult {
            test_id: Some(test_id.clone()),
            scope: Some(ExecutionScope {
                version: 1,
                run_id: run_id.clone(),
                worker_id: test.worker.clone(),
                test_id: test_id.clone(),
                test_key: test_id.clone(),
                retry: test.retry,
                attempt_id,
            }),
            test: test.name.clone(),
            test_file: Some(test.file.clone()),
            title: Some(test.title.clone()),
            retry: Some(test.retry),
            status: Some(test.status.clone()),
            expected_status: Some(test.expected_status.clone()),
            flaky: false,
            span: None,
            provenance: test.provenance.clone(),
            role: "test".into(),
            attribution: if attributed && by_time {
                crate::coverage_report::ATTRIBUTION_BY_TIME
            } else if attributed {
                crate::coverage_report::ATTRIBUTION_EXACT
            } else {
                crate::coverage_report::ATTRIBUTION_RUN_WIDE
            }
            .into(),
            phases: evidence.phases,
            runtime: vec![evidence.snapshot],
            browser: Vec::new(),
            server: Vec::new(),
        });
    }
    if !background.is_empty() {
        // Whether it counts turns on whether what produced it passed, as
        // for every test: the producers cannot be named, so the run is
        // asked.
        let status = if metadata.test_exit_code.unwrap_or_default() == 0 {
            "passed"
        } else {
            "failed"
        };
        let attempt_id = format!("{run_id}:worker:background");
        let evidence =
            crate::rust_test_runner::snapshot_regions(&manifest, background, &attempt_id)
                .map_err(|error| error.to_string())?;
        let test_id = "rust::background".to_owned();
        raw_results.push(RawTestResult {
            test_id: Some(test_id.clone()),
            scope: Some(ExecutionScope {
                version: 1,
                run_id: run_id.clone(),
                worker_id: "background".into(),
                test_id: test_id.clone(),
                test_key: test_id.clone(),
                retry: 0,
                attempt_id,
            }),
            test: "execution no test could be credited with".into(),
            test_file: None,
            title: None,
            retry: Some(0),
            status: Some(status.into()),
            expected_status: None,
            flaky: false,
            span: None,
            provenance: TestProvenance {
                runner: background_runner.clone(),
                kind: "background".into(),
                project: None,
                source: "rust-owned-v1".into(),
                tags: Vec::new(),
            },
            role: "background".into(),
            attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
            phases: vec![CoveragePhase {
                id: format!("{test_id}:background"),
                kind: "background".into(),
                operation: "execution outside any test".into(),
                source: None,
                caused_by_phase_id: None,
                started_at_ms: 0,
                ended_at_ms: Some(0),
                status: Some(status.into()),
                error: None,
            }]
            .into_iter()
            .chain(evidence.phases)
            .collect(),
            runtime: vec![evidence.snapshot],
            browser: Vec::new(),
            server: Vec::new(),
        });
    }
    let runners = names
        .into_iter()
        .map(|name| {
            // Limitation IDs carry the runner's name, in their own alphabet.
            let label = name
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() {
                        character.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            let mut runner = crate::rust_test_runner::rust_runner_declaration(&label);
            runner.runner = name;
            runner.execution_model = ExecutionModel::ParallelContextPropagated;
            runner
        })
        .collect::<Vec<_>>();
    let declaration = FrontendRunDeclaration {
        protocol_version: LANGUAGE_FRONTEND_PROTOCOL_VERSION,
        frontend_id: "rust".into(),
        frontend_version: "rust-owned-v1".into(),
        language: "rust".into(),
        structural_source: StructuralSource::OwnedProbes,
        runners,
        structural_limitations: crate::rust_test_runner::structural_limitations(&manifest),
    };
    let run = RustFrontendRun {
        declaration,
        request: CoverageReportRequest {
            run_id: run_id.clone(),
            manifest,
            raw_results,
            generated_at: metadata.started_at.clone(),
            coverage_model: Some(crate::rust_test_runner::rust_coverage_model()),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(
                metadata.test_exit_code.unwrap_or_default(),
            )),
        },
        exit_code: metadata.test_exit_code.unwrap_or_default(),
        artifacts: 0,
        artifact_files: Vec::new(),
        build_ms: 0.0,
        execution_ms: 0.0,
    };
    validate_frontend_report_request(&run.declaration, &run.request)
        .map_err(|error| error.to_string())?;
    let archive_path = root
        .join(".supercov/work")
        .join(&metadata.id)
        .join(wasm.archive);
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let raw = write_archive(
        crate::source_capture::append(
            run.archive_entries().map_err(|error| error.to_string())?,
            wasm.source_inputs,
        )?,
        &archive_path,
    )
    .map_err(|error| error.to_string())?;
    let credited = served.len().saturating_sub(
        run.request
            .raw_results
            .iter()
            .filter(|result| result.attribution == crate::coverage_report::ATTRIBUTION_RUN_WIDE)
            .count(),
    );
    let metadata = RunMetadata {
        integrity: wasm.integrity.clone(),
        raw_evidence: RawEvidenceMetadata {
            schema_version: raw.schema_version,
            format: raw.format.into(),
            file: raw.file.into(),
            files: raw.files,
            uncompressed_bytes: raw.uncompressed_bytes,
            compressed_bytes: raw.compressed_bytes,
        },
        source_roots: wasm.source_roots.clone(),
        ..metadata
    };
    let run_directory =
        publish_run(root, &metadata, &archive_path).map_err(|error| error.to_string())?;
    writeln!(
        diagnostics,
        "[supercov] {} coverage: {} test(s), {credited} credited, across {} source file(s){}",
        wasm.language,
        served.len(),
        wasm.project.source_files.len(),
        if by_time { ", credited by time" } else { "" }
    )
    .map_err(|error| error.to_string())?;
    let overlapped = overlapped
        .into_iter()
        .filter(|index| {
            run.request.raw_results.get(*index).is_some_and(|result| {
                result.attribution == crate::coverage_report::ATTRIBUTION_RUN_WIDE
            })
        })
        .count();
    if overlapped > 0 {
        writeln!(
            diagnostics,
            "[supercov] {overlapped} of those test(s) had its work only while another test ran too, so it is the run's and none of theirs; with `--workers=1` each test's is its own"
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(Some((run_directory, metadata)))
}
