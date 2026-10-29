//! A server a browser suite drove, measured by a runtime Supercov owns: Go
//! and the JVM. Their probes store into one array, which cannot tell two
//! requests apart, so the runtime sweeps it on a clock into windows, and each
//! window is credited, by time, to the one browser test whose span it
//! overlaps. What no single test's window holds is no test's.

use crate::fault_fs as fs;
use std::{
    collections::{BTreeMap, BTreeSet},
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

/// Whether the server whose work is in `directory` has a run to publish: an
/// instrumented server ran, and a browser test reported. A server without
/// one, or whose evidence cannot be read, has its work cleared.
pub fn has_run(
    root: &Path,
    directory: &Path,
    environment: &str,
    served: &[ServedTest],
) -> Result<bool, String> {
    let evidence_directory = directory.join("evidence");
    let windows = read_windows(&evidence_directory);
    if let Ok(windows) = &windows
        && !served.is_empty()
        && !windows.is_empty()
    {
        return Ok(true);
    }
    let result = windows.map(|_| false).map_err(|error| {
        note_kept_evidence(
            root,
            &evidence_directory,
            &kept_name(directory, environment),
            error,
        )
    });
    let _ = remove_stored_tree_deferred(root, directory);
    result
}

/// The name a server's kept evidence goes by: its run's, then its
/// environment's.
fn kept_name(directory: &Path, environment: &str) -> String {
    let run = directory
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{run}-{environment}")
}

/// Publish what the server ran as a run of its own, credited by time, once
/// [`has_run`] has said it has one.
pub fn publish(
    root: &Path,
    server: OwnedServer,
    metadata: RunMetadata,
    served: &[ServedTest],
    diagnostics: &mut dyn Write,
) -> Result<(PathBuf, RunMetadata), String> {
    let evidence_directory = server.directory.join("evidence");
    let result = (|| {
        let windows = read_windows(&evidence_directory)?;
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
        let runner = declaration
            .runners
            .first_mut()
            .expect("a frontend declares a runner");
        runner.runner = "playwright".into();
        runner.execution_model = supercov_contracts::ExecutionModel::ParallelContextPropagated;
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
        // Never none: there are tests, and a result for each.
        run.request
            .raw_results
            .iter_mut()
            .zip(served.iter().enumerate())
            .for_each(|(raw, (index, test))| {
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
            });
        validate_frontend_report_request(&run.declaration, &run.request)
            .map_err(|error| error.to_string())?;
        let work = root.join(".supercov/work").join(&metadata.id);
        fs::create_dir_all(&work).map_err(|error| error.to_string())?;
        let archive_path = work.join(format!("{}-evidence.raw.gz", server.environment));
        let raw = write_archive(
            crate::source_capture::append(run.archive_entries(), server.source_inputs),
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
        // The run is published: a line about it that cannot be written takes
        // nothing from it, as for a Python or Ruby server.
        let _ = writeln!(
            diagnostics,
            "[supercov] {} server coverage: {} test(s) across {} source file(s), credited by time",
            server.language,
            served.len(),
            server.source_files
        );
        // Parallel workers share one server, and its probes cannot tell
        // their requests apart. Say what it cost, and how to have it all.
        if !overlapped.is_empty() {
            let _ = writeln!(
                diagnostics,
                "[supercov] {} of those test(s) had the server's work only while another test ran too, so it is the run's and none of theirs; with `--workers=1` each test's is its own",
                overlapped.len()
            );
        }
        Ok((run_directory, metadata))
    })();
    let result = result.map_err(|error| {
        note_kept_evidence(
            root,
            &evidence_directory,
            &kept_name(server.directory, server.environment),
            error,
        )
    });
    let _ = remove_stored_tree_deferred(root, server.directory);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage_report::TestProvenance;

    fn served(name: &str, status: &str, window: (i64, i64)) -> ServedTest {
        ServedTest {
            worker: "playwright:w0".into(),
            test_id: name.into(),
            retry: 0,
            name: name.into(),
            title: name.into(),
            file: "tests/a.spec.js".into(),
            status: status.into(),
            expected_status: "passed".into(),
            provenance: TestProvenance::default(),
            window: Some(window),
            attempt_id: None,
        }
    }

    fn window(start: i64, end: i64, probes: &[u64], vectors: &[u64]) -> ServerWindow {
        ServerWindow {
            start,
            end,
            probes: probes.to_vec(),
            vectors: vectors.to_vec(),
        }
    }

    /// A window one test's span overlaps is that test's, each probe and
    /// vector once; one several tests overlap is only the run's, and those
    /// tests are named when it held work; one no test overlaps is the run's.
    /// A probe or decision the server does not declare reaches no total.
    #[test]
    fn each_window_is_credited_to_the_one_test_running_or_to_the_run() {
        let tests = [
            served("one", "passed", (100, 200)),
            served("two", "timedOut", (300, 400)),
            served("three", "skipped", (340, 500)),
        ];
        let windows = [
            window(100, 150, &[0, 1, 99, 1], &[0, 5, 0, 5, 7, 1]),
            window(350, 360, &[], &[]),
            window(380, 390, &[1, 2], &[]),
            window(1000, 1001, &[1, 4], &[0, 9]),
        ];
        // No window and no test credit nothing.
        let (_, nothing, none, neither) = credit(&[], &[], 2, &[1]);
        assert!(nothing.is_empty() && none.is_empty() && neither.is_empty());
        let (evidence, outcomes, credited, overlapped) = credit(&windows, &tests, 2, &[1]);
        assert_eq!(evidence.global, [1, 6]);
        // A vector no test's span holds still reaches the run's totals.
        assert_eq!(evidence.decision_vectors, [vec![5, 9]]);
        assert_eq!(
            evidence.tests[0]
                .probes
                .iter()
                .map(|(probe, mask)| (*probe, *mask))
                .collect::<Vec<_>>(),
            [(0, 1), (99, 1)]
        );
        assert_eq!(
            evidence.tests[0].vectors,
            [
                PackedVector {
                    decision: 0,
                    key: 5
                },
                PackedVector {
                    decision: 7,
                    key: 1
                }
            ]
        );
        assert!(evidence.tests[1].probes.is_empty());
        assert_eq!(credited, BTreeSet::from([0]));
        assert_eq!(overlapped, BTreeSet::from([1, 2]));
        assert_eq!(
            outcomes
                .iter()
                .map(|outcome| outcome.status.as_str())
                .collect::<Vec<_>>(),
            ["passed", "failed", "skipped"]
        );
    }

    /// A server no browser test reported on, or one that swept nothing, has
    /// no run, and its work directory is cleared either way.
    #[test]
    fn a_server_without_tests_or_windows_has_no_run() {
        let root = std::env::temp_dir().join(format!(
            "supercov-owned-server-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = crate::test_directory::TestDirectory::adopt(root);
        let directory = root.join(".supercov/work/run_server/go");
        let tests = [served("one", "passed", (100, 200))];
        for (served, windows) in [(&tests[..0], true), (&tests[..], false)] {
            fs::create_dir_all(directory.join("evidence")).unwrap();
            if windows {
                fs::write(
                    directory.join("evidence/1.windows"),
                    "{\"start\":100,\"end\":150,\"probes\":[],\"vectors\":[]}\n",
                )
                .unwrap();
            }
            assert_eq!(has_run(&root, &directory, "go", served), Ok(false));
            assert!(!directory.exists());
        }
    }

    /// Writes its first `lines` lines, then fails.
    struct Lines(usize);

    impl std::io::Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("closed"));
            }
            self.0 -= 1;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A browser test the server's window ran under makes a run of the
    /// server's own; each step of publishing it failing -- the windows read,
    /// the work directory, the archive, the run already there, a line said
    /// -- keeps the evidence and says so.
    #[test]
    fn a_server_window_a_test_ran_under_is_published_and_each_failure_kept() {
        let root = std::env::temp_dir().join(format!(
            "supercov-owned-server-published-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = crate::test_directory::TestDirectory::adopt(root);
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: Vec::new(),
            points: Vec::new(),
            branches: Vec::new(),
            limitations: Vec::new(),
            scope: None,
            assertion_sites: Vec::new(),
        };
        let probes = BTreeMap::new();
        let inputs = crate::source_manifest::Inputs {
            language: "go".into(),
            files: BTreeMap::new(),
        };
        let browser = |name: &str, window: (i64, i64)| {
            let mut test = served(name, "passed", window);
            test.provenance.runner = "playwright".into();
            test.provenance.kind = "browser".into();
            test
        };
        let one = [browser("one", (100, 200))];
        // Two tests at once: the window is the run's and neither's.
        let both = [browser("one", (100, 200)), browser("two", (100, 200))];
        let publishing = |id: &str, tests: &[ServedTest], fault: &str, said: &mut dyn Write| {
            let directory = root.join(format!(".supercov/work/{id}/go"));
            fs::create_dir_all(directory.join("evidence")).unwrap();
            fs::write(
                directory.join("evidence/1.windows"),
                "{\"start\":100,\"end\":150,\"probes\":[],\"vectors\":[]}\n",
            )
            .unwrap();
            let metadata = crate::lifecycle::test_run_metadata(id, 0);
            crate::fault::during(fault, || {
                publish(
                    &root,
                    OwnedServer {
                        language: "Go",
                        declaration: crate::owned_evidence::go_declaration(),
                        environment: "go",
                        coverage_model: crate::owned_evidence::go_coverage_model(),
                        manifest: &manifest,
                        probes: &probes,
                        widths: &[],
                        integrity: &metadata.integrity,
                        source_inputs: &inputs,
                        source_roots: None,
                        directory: &directory,
                        source_files: 0,
                    },
                    metadata.clone(),
                    tests,
                    said,
                )
            })
        };
        let mut said = Vec::new();
        publishing("run_one", &one, "", &mut said).unwrap();
        assert!(
            String::from_utf8_lossy(&said).contains("Go server coverage: 1 test(s)"),
            "{}",
            String::from_utf8_lossy(&said)
        );
        let again = publishing("run_one", &one, "", &mut Vec::new()).unwrap_err();
        assert!(again.contains("coverage run already exists"), "{again}");
        for (id, tests, fault, lines) in [
            (
                "run_read",
                &one[..],
                "fs.read_to_string#owned_server.rs:79",
                9,
            ),
            (
                "run_work",
                &one[..],
                "fs.create_dir_all#owned_server.rs:295",
                9,
            ),
            (
                "run_archive",
                &one[..],
                "fs.rename#evidence_archive.rs:471",
                9,
            ),
        ] {
            let failed = publishing(id, tests, fault, &mut Lines(lines)).unwrap_err();
            assert!(failed.contains("their evidence is kept"), "{id}: {failed}");
        }
        // A line about a published run that cannot be written takes nothing
        // from the run.
        for (id, tests, lines) in [("run_said", &one[..], 0), ("run_overlap", &both[..], 1)] {
            publishing(id, tests, "", &mut Lines(lines))
                .unwrap_or_else(|error| panic!("{id}: {error}"));
        }
    }
}
