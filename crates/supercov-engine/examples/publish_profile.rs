//! Development tool: publish a downloaded run's evidence again into a scratch
//! root, timing each part, so publication can be profiled away from CI.
//!
//! Usage: `publish_profile ROOT RUN_DIRECTORY [COUNT] [handed]` -- ROOT gets
//! `.supercov/runs/<id>`; RUN_DIRECTORY holds `run.json` and `evidence.raw.gz`.
//! With `handed`, the evidence is analysed first and the analysis handed to
//! publication, as the JavaScript frontend does; only publication is timed.

use std::{path::PathBuf, time::Instant};

use supercov_engine::{
    lifecycle::{Archived, publish_archived_run, publish_run},
    run_store::{RunMetadata, StoredRun, analyze_stored_run},
};

fn main() {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let root = args.next().expect("ROOT");
    let run = args.next().expect("RUN_DIRECTORY");
    let times = args
        .next()
        .map_or(1, |count| count.to_string_lossy().parse().expect("COUNT"));
    let handed = args.next().is_some_and(|mode| mode.as_os_str() == "handed");
    let text = std::fs::read_to_string(run.join("run.json")).expect("run.json");
    let metadata: RunMetadata = serde_json::from_str(&text).expect("run metadata");
    let destination = root.join(".supercov/runs").join(&metadata.id);
    for _ in 0..times {
        let _ = std::fs::remove_dir_all(&destination);
        let _ = std::fs::remove_dir_all(root.join(".supercov/work"));
        let evidence = run.join("evidence.raw.gz");
        let report = handed.then(|| {
            analyze_stored_run(&StoredRun {
                id: metadata.id.clone(),
                directory: run.clone(),
                evidence_path: evidence.clone(),
                metadata_path: run.join("run.json"),
                query_index_path: run.join("unused-query-index.bin"),
                metadata: metadata.clone(),
            })
            .expect("analysed")
        });
        let started = Instant::now();
        match report {
            Some(report) => publish_archived_run(
                &root,
                &metadata,
                &evidence,
                Archived {
                    report: Some(report),
                    inputs: None,
                },
            ),
            None => publish_run(&root, &metadata, &evidence),
        }
        .expect("published");
        eprintln!(
            "publication {:.1}ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}
