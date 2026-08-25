use std::{io::Read, path::PathBuf, process::ExitCode, time::Instant};

use serde::{Deserialize, Serialize};
use supercov_engine::{
    evidence_archive::{
        EvidenceArchiveEntry, EvidenceArchiveSource, collect_sources, write_archive,
    },
    js_instrumenter::instrument_candidate,
};

const HELP: &str = "Rust candidate for the frozen Supercov engine contract v1.\n\
This binary is a contract shell, not yet a coverage engine.\n\
\n\
Reference-engine UX:\n\
  supercov -- <test command>\n\
  supercov runs <run-id> coverage [resource] [--json]\n\
  supercov diff <older-run> <newer-run> [--json]\n\
  supercov merge <run-id> <run-id> [...]\n\
  supercov prune|clean [--keep N] [--dry-run]\n";

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None | Some("help" | "--help" | "-h") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!(
                "supercov {} (rust contract v{})",
                supercov_engine::version(),
                supercov_contracts::CONTRACT_VERSION
            );
            ExitCode::SUCCESS
        }
        Some("__instrument-js") => instrument_js(),
        Some("__benchmark-js-transform") => benchmark_js_transform(),
        Some("__pack-evidence") => pack_evidence(),
        Some(command) => {
            eprintln!(
                "[supercov] Rust engine candidate is not ready for `{command}`; use the currently shipped engine while the Rust contract gates are incomplete"
            );
            ExitCode::from(2)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransformBenchmarkResult {
    files: usize,
    duration_ns: u128,
}

/// Development-only measurement boundary for the frozen Phase 3 transform
/// gate. Input decoding and output transport are measured separately by the
/// caller; this reports only parse -> transform -> codegen engine time.
fn benchmark_js_transform() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let cases: Vec<InstrumentCase> = match serde_json::from_str(&input) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("[supercov] invalid Rust benchmark input: {error}");
            return ExitCode::from(2);
        }
    };
    let files = cases.len();
    let started = Instant::now();
    for case in cases {
        if let Err(error) = instrument_candidate(&case.source, &case.file) {
            eprintln!("[supercov] {}: {error:?}", case.file);
            return ExitCode::from(2);
        }
    }
    let result = TransformBenchmarkResult {
        files,
        duration_ns: started.elapsed().as_nanos(),
    };
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &result) {
        eprintln!("[supercov] failed to write Rust benchmark output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn stdin() -> Result<String, String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("failed to read Rust engine input: {error}"))?;
    Ok(input)
}

#[derive(Deserialize)]
struct InstrumentCase {
    file: String,
    source: String,
}

/// Private migration protocol. It intentionally accepts a whole batch so the
/// Node shim never pays one process launch per source file.
fn instrument_js() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let cases: Vec<InstrumentCase> = match serde_json::from_str(&input) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("[supercov] invalid Rust instrumenter input: {error}");
            return ExitCode::from(2);
        }
    };
    let mut outputs = Vec::with_capacity(cases.len());
    for case in cases {
        match instrument_candidate(&case.source, &case.file) {
            Ok(output) => outputs.push(output),
            Err(error) => {
                eprintln!("[supercov] {}: {error:?}", case.file);
                return ExitCode::from(2);
            }
        }
    }
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &outputs) {
        eprintln!("[supercov] failed to write Rust instrumenter output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackEvidenceRequest {
    destination: PathBuf,
    #[serde(default)]
    sources: Vec<PackEvidenceSource>,
    #[serde(default)]
    entries: Vec<PackEvidenceEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackEvidenceSource {
    directory: Option<PathBuf>,
    prefix: Option<String>,
    file: Option<PathBuf>,
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackEvidenceEntry {
    path: String,
    contents: String,
}

fn pack_evidence() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let request: PackEvidenceRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid Rust evidence request: {error}");
            return ExitCode::from(2);
        }
    };
    if !request.sources.is_empty() && !request.entries.is_empty() {
        eprintln!("[supercov] Rust evidence request cannot mix sources and entries");
        return ExitCode::from(2);
    }
    let entries = if request.entries.is_empty() {
        let mut sources = Vec::with_capacity(request.sources.len());
        for source in request.sources {
            match (source.directory, source.file, source.path) {
                (Some(directory), None, None) => sources.push(EvidenceArchiveSource::Directory {
                    directory,
                    prefix: source.prefix,
                }),
                (None, Some(file), Some(path)) if source.prefix.is_none() => {
                    sources.push(EvidenceArchiveSource::File { file, path });
                }
                _ => {
                    eprintln!(
                        "[supercov] each evidence source must be one directory with an optional prefix or one file with an archive path"
                    );
                    return ExitCode::from(2);
                }
            }
        }
        match collect_sources(&sources) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("[supercov] failed to collect evidence: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        request
            .entries
            .into_iter()
            .map(|entry| EvidenceArchiveEntry {
                path: entry.path,
                contents: entry.contents.into_bytes(),
            })
            .collect()
    };
    let metadata = match write_archive(entries, &request.destination) {
        Ok(metadata) => metadata,
        Err(error) => {
            eprintln!("[supercov] failed to pack evidence: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = serde_json::to_writer(std::io::stdout(), &metadata) {
        eprintln!("[supercov] failed to serialize evidence metadata: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_is_explicitly_not_a_false_coverage_implementation() {
        assert!(HELP.contains("not yet a coverage engine"));
        assert_eq!(
            supercov_engine::READINESS,
            supercov_engine::EngineReadiness::ContractShell
        );
    }
}
