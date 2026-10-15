//! A server a browser suite drove, measured by a runtime Supercov owns: Go
//! and the JVM. Their probes store into one array, which cannot tell two
//! requests apart, so the runtime sweeps it on a clock into windows, and each
//! window is credited, by time, to the one browser test whose span it
//! overlaps. What no single test's window holds is no test's.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use supercov_contracts::FrontendRunDeclaration;

use crate::{
    coverage_report::{CoverageManifest, CoverageModelDeclaration},
    evidence_archive::write_archive,
    frontend_protocol::validate_frontend_report_request,
    go_instrumenter::GoProbe,
    lifecycle::{note_kept_evidence, publish_run, remove_stored_tree_deferred},
    owned_evidence::{
        OwnedEvidence, OwnedRunInputs, OwnedTestEvidence, OwnedTestOutcome, PackedVector,
        build_frontend_run,
    },
    run_store::{RawEvidenceMetadata, RunIntegrity, RunMetadata},
    served_tests::ServedTest,
};

/// One sweep of a server's probes, as the runtime appended it.
#[derive(Deserialize)]
struct ServerWindow {
    start: i64,
    end: i64,
    /// Pairs: probe id, mask.
    probes: Vec<u64>,
    /// Pairs: decision, packed vector.
    vectors: Vec<u64>,
}

/// What publishing a server's run needs from its frontend.
pub struct OwnedServer<'a> {
    /// `Go`, `Java/Kotlin`: said in the line the run prints.
    pub language: &'static str,
    /// The frontend's own declaration; its first runner becomes the browser
    /// runner the tests came from.
    pub declaration: FrontendRunDeclaration,
    pub environment: &'a str,
    pub coverage_model: CoverageModelDeclaration,
    pub manifest: &'a CoverageManifest,
    pub probes: &'a BTreeMap<u64, GoProbe>,
    pub widths: &'a [u8],
    pub integrity: &'a RunIntegrity,
    pub source_inputs: &'a crate::source_manifest::Inputs,
    pub source_roots: Option<Vec<String>>,
    /// `.supercov/work/<run>/<language>`, holding `evidence/`.
    pub directory: &'a Path,
    pub source_files: usize,
}

fn read_windows(directory: &Path) -> Result<Vec<ServerWindow>, String> {
    let mut files = fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "windows")
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    let mut windows = Vec::new();
    for file in &files {
        let text =
            fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
        // A line cut short by the server being killed is the one window it
        // was writing.
        windows.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<ServerWindow>(line).ok()),
        );
    }
    Ok(windows)
}

/// The run's evidence from its windows: run-wide totals from every window,
/// and each browser test's from the windows that overlap its span and no
/// other test's. Returns the tests that were credited any, and those whose
/// server work was all done while another test ran too.
fn credit(
    windows: &[ServerWindow],
    served: &[ServedTest],
    probe_count: usize,
    widths: &[u8],
) -> (
    OwnedEvidence,
    Vec<OwnedTestOutcome>,
    BTreeSet<usize>,
    BTreeSet<usize>,
) {
    let mut global = vec![0_u32; probe_count];
    let mut decision_vectors = vec![Vec::<u64>::new(); widths.len()];
    let mut by_test = BTreeMap::<usize, OwnedTestEvidence>::new();
    let mut overlapped = BTreeSet::new();
    for window in windows {
        let running = served
            .iter()
            .enumerate()
            .filter(|(_, test)| {
                test.window
                    .is_some_and(|(start, end)| start <= window.end && window.start <= end)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let owner = match running.as_slice() {
            [index] => Some(*index),
            _ => None,
        };
        if running.len() > 1 && !window.probes.is_empty() {
            overlapped.extend(running);
        }
        for pair in window.probes.chunks_exact(2) {
            let (index, mask) = (pair[0] as usize, pair[1] as u32);
            if let Some(total) = global.get_mut(index) {
                *total |= mask;
            }
            if let Some(owner) = owner {
                *by_test
                    .entry(owner)
                    .or_default()
                    .probes
                    .entry(index as u32)
                    .or_default() |= mask;
            }
        }
        for pair in window.vectors.chunks_exact(2) {
            let (decision, key) = (pair[0] as u32, pair[1]);
            if let Some(seen) = decision_vectors.get_mut(decision as usize)
                && !seen.contains(&key)
            {
                seen.push(key);
            }
            if let Some(owner) = owner {
                let test = by_test.entry(owner).or_default();
                let vector = PackedVector { decision, key };
                if !test.vectors.contains(&vector) {
                    test.vectors.push(vector);
                }
            }
        }
    }
    let credited = by_test.keys().copied().collect::<BTreeSet<_>>();
    let overlapped = overlapped.difference(&credited).copied().collect();
    let mut tests = Vec::new();
    let mut outcomes = Vec::new();
    for (index, test) in served.iter().enumerate() {
        let status = match test.status.as_str() {
            "passed" => "passed",
            "skipped" => "skipped",
            _ => "failed",
        };
        let mut evidence = by_test.remove(&index).unwrap_or_default();
        evidence.name = test.name.clone();
        evidence.status = status.into();
        evidence.runner = test.provenance.runner.clone();
        tests.push(evidence);
        outcomes.push(OwnedTestOutcome {
            name: test.name.clone(),
            runner: test.provenance.runner.clone(),
            package: test.worker.clone(),
            file: Some(test.file.clone()),
            status: status.into(),
            attributed: true,
        });
    }
    (
        OwnedEvidence {
            global,
            tests,
            widths: widths.to_vec(),
            decision_vectors,
        },
        outcomes,
        credited,
        overlapped,
    )
}

/// Publish what the server ran as a run of its own, credited by time. None
/// when no instrumented server ran, or no browser test reported.
pub fn publish(
    root: &Path,
    server: OwnedServer,
    metadata: RunMetadata,
    served: &[ServedTest],
    diagnostics: &mut dyn Write,
) -> Result<Option<(PathBuf, RunMetadata)>, String> {
    let evidence_directory = server.directory.join("evidence");
    let result = (|| {
        let windows = read_windows(&evidence_directory)?;
        if served.is_empty() || windows.is_empty() {
            return Ok(None);
        }
        let probe_count = server
            .probes
            .keys()
            .max()
            .map_or(0, |highest| *highest as usize + 1);
        let (evidence, outcomes, credited, overlapped) =
            credit(&windows, served, probe_count, server.widths);
        // The tests came from the browser runner, whichever runner the
        // frontend declares for its own suites.
        let mut declaration = server.declaration;
        declaration.runners.truncate(1);
        for runner in &mut declaration.runners {
            runner.runner = "playwright".into();
            runner.execution_model = supercov_contracts::ExecutionModel::ParallelContextPropagated;
        }
        let mut run = build_frontend_run(OwnedRunInputs {
            declaration,
            environment: server.environment,
            manifest: server.manifest,
            probes: server.probes,
            evidence: &evidence,
            outcomes: &outcomes,
            run_id: &metadata.id,
            generated_at: &metadata.started_at,
            test_exit_code: metadata.test_exit_code.unwrap_or_default(),
            coverage_model: server.coverage_model,
        })
        .map_err(|error| error.to_string())?;
        for (raw, (index, test)) in run
            .request
            .raw_results
            .iter_mut()
            .zip(served.iter().enumerate())
        {
            raw.title = Some(test.title.clone());
            raw.expected_status = Some(test.expected_status.clone());
            raw.status = Some(test.status.clone());
            raw.provenance = crate::coverage_report::TestProvenance {
                source: raw.provenance.source.clone(),
                ..test.provenance.clone()
            };
            if credited.contains(&index) {
                raw.attribution = crate::coverage_report::ATTRIBUTION_BY_TIME.into();
            }
        }
        validate_frontend_report_request(&run.declaration, &run.request)
            .map_err(|error| error.to_string())?;
        let archive_path = root
            .join(".supercov/work")
            .join(&metadata.id)
            .join(format!("{}-evidence.raw.gz", server.environment));
        if let Some(parent) = archive_path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let raw = write_archive(
            crate::source_capture::append(
                run.archive_entries().map_err(|error| error.to_string())?,
                server.source_inputs,
            )?,
            &archive_path,
        )
        .map_err(|error| error.to_string())?;
        let metadata = RunMetadata {
            integrity: server.integrity.clone(),
            raw_evidence: RawEvidenceMetadata {
                schema_version: raw.schema_version,
                format: raw.format.into(),
                file: raw.file.into(),
                files: raw.files,
                uncompressed_bytes: raw.uncompressed_bytes,
                compressed_bytes: raw.compressed_bytes,
            },
            source_roots: server.source_roots.clone(),
            ..metadata
        };
        let run_directory =
            publish_run(root, &metadata, &archive_path).map_err(|error| error.to_string())?;
        writeln!(
            diagnostics,
            "[supercov] {} server coverage: {} test(s) across {} source file(s), credited by time",
            server.language,
            served.len(),
            server.source_files
        )
        .map_err(|error| error.to_string())?;
        // Parallel workers share one server, and its probes cannot tell
        // their requests apart. Say what it cost, and how to have it all.
        if !overlapped.is_empty() {
            writeln!(
                diagnostics,
                "[supercov] {} of those test(s) had the server's work only while another test ran too, so it is the run's and none of theirs; with `--workers=1` each test's is its own",
                overlapped.len()
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(Some((run_directory, metadata)))
    })();
    let result = result.map_err(|error| {
        let run = server
            .directory
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        note_kept_evidence(
            root,
            &evidence_directory,
            &format!("{run}-{}", server.environment),
            error,
        )
    });
    let _ = remove_stored_tree_deferred(root, server.directory);
    result
}
