use std::process::ExitCode;

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
        Some(command) => {
            eprintln!(
                "[supercov] Rust engine candidate is not ready for `{command}`; the TypeScript reference remains authoritative"
            );
            ExitCode::from(2)
        }
    }
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
