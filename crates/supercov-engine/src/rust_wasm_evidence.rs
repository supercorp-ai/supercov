//! What instrumented Rust ran in wasm, published as a Rust run of its own.
//!
//! In wasm the region runtime keeps each crate's region in the module's
//! memory, and a host takes it from there: wrangler's inspector for a Worker
//! (rust_worker.rs), a JavaScript test process at each test's edges, a
//! Playwright page as its test ends. Each take is a record with an owner --
//! a window of time, a test attempt, or nobody -- and this credits the
//! records to the browser or JavaScript tests the run reported and publishes
//! them against the crate's manifest.

use crate::fault_fs as fs;
use std::{
    collections::{BTreeMap, BTreeSet},
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

/// A test's provenance, naming Playwright when its journal names no runner:
/// a runner the run does not declare fails the whole run's validation.
fn provenance_of(test: &ServedTest) -> TestProvenance {
    let mut provenance = test.provenance.clone();
    if provenance.runner.is_empty() {
        provenance.runner = "playwright".into();
    }
    provenance
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
    // own runner's process or page, named by its attempt or its time. There
    // is at least one test, so at least one runner.
    let names = served
        .iter()
        .map(|test| provenance_of(test).runner)
        .collect::<BTreeSet<_>>();
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
            provenance: provenance_of(test),
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
    let work = root.join(".supercov/work").join(&metadata.id);
    fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let archive_path = work.join(wasm.archive);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage_analysis::PointKind;
    use crate::coverage_report::PointMeta;
    use crate::run_store::RawEvidenceMetadata;

    /// A crate below the project's root names its files from the project's
    /// root: its points, limitations and scope entries. A limitation or an
    /// entry that names no file is left as it is, as is a manifest with no
    /// scope; a crate at the root changes nothing.
    #[test]
    fn a_crate_below_the_root_names_its_files_from_the_root() {
        let mut project = PreparedRustProject::empty(PathBuf::from("/nonexistent"));
        project.source_files = vec!["src/lib.rs".into()];
        project.manifest.points.push(PointMeta {
            id: "p".into(),
            kind: PointKind::Statement,
            file: "src/lib.rs".into(),
            line: 1,
            column: 1,
            source: "x".into(),
            label: None,
        });
        project.manifest.limitations = vec![
            serde_json::json!({"id": "a", "file": "src/lib.rs"}),
            serde_json::json!({"id": "b"}),
        ];
        root_paths(&mut project, Path::new(""));
        assert_eq!(project.source_files, ["src/lib.rs"]);
        root_paths(&mut project, Path::new("worker"));
        assert_eq!(project.source_files, ["worker/src/lib.rs"]);
        assert_eq!(project.manifest.points[0].file, "worker/src/lib.rs");
        assert_eq!(
            project.manifest.limitations,
            [
                serde_json::json!({"id": "a", "file": "worker/src/lib.rs"}),
                serde_json::json!({"id": "b"}),
            ]
        );
        assert!(project.manifest.scope.is_none());
        project.manifest.scope = Some(serde_json::json!({
            "entries": [{"file": "src/lib.rs", "status": "included"}, {"status": "excluded"}]
        }));
        root_paths(&mut project, Path::new("crates"));
        assert_eq!(
            project.manifest.scope,
            Some(serde_json::json!({
                "entries": [
                    {"file": "crates/src/lib.rs", "status": "included"},
                    {"status": "excluded"}
                ]
            }))
        );
    }

    const TOKEN: &str = "0123456789ab";

    /// A region of one hit probe, `id`, recorded first by thread 1.
    fn region_with_one_hit(probes: &crate::rust_instrumenter::RustProbeLayout) -> Vec<u8> {
        let mut bytes = vec![0u8; 256 + 4096];
        bytes[..8].copy_from_slice(b"SCVRGN01");
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&128u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&(probes.hits.len() as u32).to_le_bytes());
        bytes[32..36].copy_from_slice(&256u32.to_le_bytes());
        bytes[56..56 + TOKEN.len()].copy_from_slice(TOKEN.as_bytes());
        // The first page touched, and the first hit at sequence 1.
        bytes[128] = 1;
        bytes[256..264].copy_from_slice(&((1u64 << 32) | 1).to_le_bytes());
        bytes
    }

    fn metadata(id: &str, exit: i32) -> RunMetadata {
        let digest = |character: char| std::iter::repeat_n(character, 64).collect::<String>();
        RunMetadata {
            id: id.into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            duration_ms: 1.0,
            command: vec!["npx".into(), "playwright".into(), "test".into()],
            test_exit_code: Some(exit),
            integrity: RunIntegrity {
                schema_version: 2,
                instrumenter_version: "test".into(),
                git: None,
                fingerprint: crate::run_store::RunFingerprint {
                    algorithm: "sha256".into(),
                    source: digest('a'),
                    tests: digest('b'),
                    dependencies: digest('c'),
                    configuration: digest('d'),
                    instrumenter: digest('e'),
                    execution: digest('f'),
                    combined: digest('0'),
                    source_files: 1,
                    test_files: 1,
                },
                stale: None,
                stale_reasons: None,
            },
            raw_evidence: RawEvidenceMetadata {
                schema_version: 2,
                format: "supercov-evidence-archive".into(),
                file: "evidence.raw.gz".into(),
                files: 0,
                uncompressed_bytes: 0,
                compressed_bytes: 0,
            },
            isolated_build: None,
            instrumented_build_cache: None,
            timings: None,
            merged: None,
            parents: None,
            source_roots: None,
            copy: None,
            merged_across: None,
            recorded_by: None,
            companions: None,
        }
    }

    fn served(index: usize, window: (i64, i64), runner: &str) -> ServedTest {
        ServedTest {
            worker: "playwright:0".into(),
            test_id: format!("t{index}"),
            retry: 0,
            name: format!("test {index}"),
            title: format!("test {index}"),
            file: "e2e/app.spec.ts".into(),
            status: "passed".into(),
            expected_status: "passed".into(),
            provenance: TestProvenance {
                runner: runner.into(),
                kind: "e2e".into(),
                project: None,
                source: "playwright".into(),
                tags: Vec::new(),
            },
            window: Some(window),
            attempt_id: None,
        }
    }

    /// Work taken while two tests ran is neither's: it is the run's, and it
    /// counts as passed only if the run did. A test whose journal names no
    /// runner is Playwright's. Without a test or a take there is no run to
    /// publish.
    #[test]
    fn work_two_tests_overlap_is_the_run_s_and_fails_with_it() {
        let root = std::env::temp_dir().join(format!(
            "supercov-wasm-publish-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let id = "rs:statement:000000000000000000000000";
        let mut project = PreparedRustProject::empty(root.clone());
        project.source_files = vec!["src/lib.rs".into()];
        project.manifest.points.push(PointMeta {
            id: id.into(),
            kind: PointKind::Statement,
            file: "src/lib.rs".into(),
            line: 1,
            column: 0,
            source: "work();".into(),
            label: None,
        });
        project.probes.token = TOKEN.into();
        project.probes.hits = vec![id.into()];
        let integrity = metadata("x", 0).integrity;
        let inputs = crate::source_manifest::Inputs {
            language: "rust".into(),
            files: Default::default(),
        };
        let wasm = || WasmRun {
            language: "Rust wasm",
            archive: "rust-wasm-evidence.raw.gz",
            project: &project,
            integrity: &integrity,
            source_inputs: &inputs,
            source_roots: None,
        };
        let records = || {
            vec![WasmRecord {
                owner: Owner::Window(6, 7),
                regions: vec![("crate".into(), region_with_one_hit(&project.probes))],
            }]
        };
        // The second test's journal names no runner.
        let tests = [served(0, (0, 10), "playwright"), served(1, (5, 15), "")];
        let run_id = "2026-01-01T00-00-00-000Z";
        let mut diagnostics = Vec::new();
        assert_eq!(
            publish(
                &root,
                wasm(),
                records(),
                metadata(run_id, 1),
                &[],
                &mut diagnostics
            ),
            Ok(None)
        );
        assert_eq!(
            publish(
                &root,
                wasm(),
                Vec::new(),
                metadata(run_id, 1),
                &tests,
                &mut diagnostics
            ),
            Ok(None)
        );
        assert!(diagnostics.is_empty());
        let (directory, _) = publish(
            &root,
            wasm(),
            records(),
            metadata(run_id, 1),
            &tests,
            &mut diagnostics,
        )
        .unwrap()
        .unwrap();
        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics
                .contains("2 test(s), 0 credited, across 1 source file(s), credited by time"),
            "{diagnostics}"
        );
        assert!(
            diagnostics.contains("2 of those test(s) had its work only while another test ran too"),
            "{diagnostics}"
        );
        let archive = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().ends_with(".raw.gz"))
            .unwrap();
        let results = crate::evidence_archive::read_archive(&archive)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.path.ends_with("/mcdc.json"))
            .map(|entry| serde_json::from_slice::<serde_json::Value>(&entry.contents).unwrap())
            .map(|result| {
                (
                    result["role"].as_str().unwrap().to_owned(),
                    result["status"].as_str().unwrap().to_owned(),
                    result["provenance"]["runner"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let named = |role: &str, status: &str| {
            (role.to_owned(), status.to_owned(), "playwright".to_owned())
        };
        assert_eq!(
            results,
            [
                named("test", "passed"),
                named("test", "passed"),
                named("background", "failed")
            ]
        );
        // Each file operation failing in turn, a region that is not one, and
        // a stream that refuses either line: each ends with an error, never
        // a panic.
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let fresh = || {
            let other = root.join(format!(
                "attempt-{}",
                UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&other).unwrap();
            other
        };
        let broken = vec![WasmRecord {
            owner: Owner::Window(6, 7),
            regions: vec![("crate".into(), b"not a region".to_vec())],
        }];
        assert!(
            publish(
                &fresh(),
                wasm(),
                broken,
                metadata(run_id, 1),
                &tests,
                &mut Vec::new()
            )
            .is_err()
        );
        struct Refusing(&'static str);
        impl Write for Refusing {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                if String::from_utf8_lossy(buffer).contains(self.0) {
                    return Err(std::io::Error::other("closed"));
                }
                Ok(buffer.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for refused in ["coverage:", "of those test(s)"] {
            assert!(
                publish(
                    &fresh(),
                    wasm(),
                    records(),
                    metadata(run_id, 1),
                    &tests,
                    &mut Refusing(refused)
                )
                .is_err(),
                "{refused}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
