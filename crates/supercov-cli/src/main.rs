use std::{io::Read, process::ExitCode};

use serde::Deserialize;
use supercov_engine::js_instrumenter::instrument_candidate;

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
        Some(command) => {
            eprintln!(
                "[supercov] Rust engine candidate is not ready for `{command}`; the TypeScript reference remains authoritative"
            );
            ExitCode::from(2)
        }
    }
}

#[derive(Deserialize)]
struct InstrumentCase {
    file: String,
    source: String,
}

/// Private migration protocol. It intentionally accepts a whole batch so the
/// Node shim never pays one process launch per source file.
fn instrument_js() -> ExitCode {
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("[supercov] failed to read Rust instrumenter input: {error}");
        return ExitCode::from(2);
    }
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
