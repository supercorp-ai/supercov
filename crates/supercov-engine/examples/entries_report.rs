//! Development tool: analyse a downloaded run's evidence from the archive and
//! from its entries in memory, and say whether the two reports are the same.
//!
//! Usage: `entries_report RUN_DIRECTORY` -- it holds `run.json` and
//! `evidence.raw.gz`.

use std::{path::PathBuf, time::Instant};

use supercov_engine::{
    coverage_report::{
        ArchiveReportRequest, ExitCodeInput, analyze_coverage_archive, analyze_coverage_entries,
    },
    evidence_archive::read_archive,
    run_store::RunMetadata,
};

fn main() {
    let run = PathBuf::from(std::env::args_os().nth(1).expect("RUN_DIRECTORY"));
    let metadata: RunMetadata =
        serde_json::from_str(&std::fs::read_to_string(run.join("run.json")).expect("run.json"))
            .expect("run metadata");
    let request = ArchiveReportRequest {
        archive_path: run.join("evidence.raw.gz"),
        run_id: metadata.id.clone(),
        generated_at: metadata.started_at.clone(),
        integrity: Some(serde_json::to_value(&metadata.integrity).expect("serializes")),
        test_exit_code: ExitCodeInput::Present(metadata.test_exit_code),
        test_filter: None,
    };
    let entries = read_archive(&request.archive_path).expect("archive read");
    let started = Instant::now();
    let from_entries = analyze_coverage_entries(&request, &entries).expect("entries analysed");
    let entries_ms = started.elapsed().as_secs_f64() * 1000.0;
    drop(entries);
    let started = Instant::now();
    let from_archive = analyze_coverage_archive(&request).expect("archive analysed");
    let archive_ms = started.elapsed().as_secs_f64() * 1000.0;
    let entries = read_archive(&request.archive_path).expect("archive read");
    let same = from_archive == from_entries;
    let same_json =
        serde_json::to_vec(&from_archive).unwrap() == serde_json::to_vec(&from_entries).unwrap();
    println!(
        "entries {} | archive analysis {archive_ms:.0} ms, entries analysis {entries_ms:.0} ms | equal {same}, same JSON {same_json}",
        entries.len()
    );
    assert!(same && same_json, "the two analyses differ");
}
