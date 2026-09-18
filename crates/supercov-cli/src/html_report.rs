use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Write, stdout},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::{Compression, write::GzEncoder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use supercov_engine::{
    coverage_index::{CoverageIndex, CoverageViewId, IndexedFileGap},
    coverage_query::{
        CoverageFileDetailOptions, CoverageScopeQueryOptions, CoverageSummaryQueryOptions,
        coverage_file_detail_query, coverage_scope_query, coverage_summary_query,
    },
    run_store::{StoredRun, compare_run_integrity, open_or_rebuild_query_index, select_run},
};

use crate::{current_integrity_for_run, full_suite_hints, public_run_inventory};

const REPORT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_RUNS: usize = 10;
const MAX_RUNS: usize = 20;
const MAX_REPORT_BYTES: usize = 24 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const PAYLOAD_MARKER: &str = "SUPERCOV_REPORT_PAYLOAD";
const REPORT_TEMPLATE: &str = include_str!("../assets/report.html");

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportOptions {
    selector: String,
    comparison: Option<String>,
    output: PathBuf,
    open: bool,
    runs: usize,
}

fn help() -> &'static str {
    "Usage: supercov report [run-id] [options]\n\nGenerates one private, self-contained interactive HTML file from stored runs.\nNo server or network connection is required.\n\nOptions:\n  --compare <run-id>   choose the initial comparison run\n  --runs <n>           include up to n runs (default: 10, maximum: 20)\n  --output <path>      write to this path (default: supercov-report.html)\n  --no-open            do not open the report in the default browser\n  -h, --help           show this help\n"
}

fn parse_options(arguments: &[String]) -> Result<ReportOptions, String> {
    let mut selector = None;
    let mut comparison = None;
    let mut output = PathBuf::from("supercov-report.html");
    let mut open = true;
    let mut runs = DEFAULT_RUNS;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--compare" => {
                index += 1;
                comparison = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--compare requires a run ID".to_owned())?
                        .clone(),
                );
            }
            "--output" | "-o" => {
                index += 1;
                output = PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--output requires a path".to_owned())?,
                );
            }
            "--runs" => {
                index += 1;
                runs = arguments
                    .get(index)
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|value| (1..=MAX_RUNS).contains(value))
                    .ok_or_else(|| format!("--runs must be between 1 and {MAX_RUNS}"))?;
            }
            "--no-open" => open = false,
            "--help" | "-h" => return Err(help().into()),
            value if value.starts_with('-') => {
                return Err(format!("Unknown report option: {value}"));
            }
            value if selector.is_none() => selector = Some(value.to_owned()),
            value => return Err(format!("Unexpected report argument: {value}")),
        }
        index += 1;
    }
    if output.as_os_str().is_empty() {
        return Err("--output requires a non-empty path".into());
    }
    Ok(ReportOptions {
        selector: selector.unwrap_or_else(|| "latest".into()),
        comparison,
        output,
        open,
        runs,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportBundle {
    schema_version: u32,
    project: String,
    selected_run_id: String,
    comparison_run_id: Option<String>,
    runs: Vec<ReportRun>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportRun {
    id: String,
    started_at: String,
    duration_ms: f64,
    command: Vec<String>,
    test_exit_code: Option<i32>,
    stale: bool,
    stale_reasons: Vec<String>,
    source_mode: &'static str,
    omitted_sources: Vec<String>,
    summary: serde_json::Value,
    files: Vec<IndexedFileGap>,
    file_details: BTreeMap<String, serde_json::Value>,
    decisions: serde_json::Value,
    tests: Vec<ReportTest>,
    lines: Vec<ReportLine>,
    scope: Option<serde_json::Value>,
    sources: BTreeMap<String, ReportSource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportSource {
    sha256: String,
    contents: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportLine {
    file: String,
    line: usize,
    covered: bool,
    confidence: String,
    tests: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportTest {
    id: String,
    name: String,
    file: Option<String>,
    title: Option<String>,
    outcome: String,
    role: String,
    runner: String,
    kind: String,
    project: Option<String>,
    retries: Vec<usize>,
    attempts: serde_json::Value,
    lines: serde_json::Value,
    total_lines: usize,
    total_hits: usize,
    total_decisions: usize,
}

fn selected_runs<'a>(
    inventory: &'a supercov_engine::run_store::RunInventory,
    options: &ReportOptions,
) -> Result<Vec<&'a StoredRun>, String> {
    let selected =
        select_run(inventory, Some(&options.selector)).map_err(|error| error.to_string())?;
    let mut runs = vec![selected];
    let mut ids = BTreeSet::from([selected.id.as_str()]);
    if let Some(selector) = options.comparison.as_deref() {
        let comparison =
            select_run(inventory, Some(selector)).map_err(|error| error.to_string())?;
        if ids.insert(comparison.id.as_str()) {
            runs.push(comparison);
        }
    }
    let selected_position = inventory
        .runs
        .iter()
        .position(|run| run.id == selected.id)
        .expect("selected run belongs to inventory");
    for run in inventory.runs.iter().skip(selected_position + 1) {
        if runs.len()
            >= options
                .runs
                .max(usize::from(options.comparison.is_some()) + 1)
        {
            break;
        }
        if ids.insert(run.id.as_str()) {
            runs.push(run);
        }
    }
    Ok(runs)
}

fn safe_relative_source(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let root = fs::canonicalize(root).ok()?;
    let source = fs::canonicalize(root.join(relative)).ok()?;
    source.starts_with(&root).then_some(source)
}

fn collect_sources(
    root: &Path,
    files: &[IndexedFileGap],
    exact: bool,
) -> (BTreeMap<String, ReportSource>, Vec<String>) {
    if !exact {
        return (BTreeMap::new(), Vec::new());
    }
    let mut sources = BTreeMap::new();
    let mut omitted = Vec::new();
    let mut total = 0_usize;
    for file in files {
        let Some(path) = safe_relative_source(root, &file.file) else {
            omitted.push(file.file.clone());
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            omitted.push(file.file.clone());
            continue;
        };
        let Ok(length) = usize::try_from(metadata.len()) else {
            omitted.push(file.file.clone());
            continue;
        };
        if !metadata.is_file()
            || length > MAX_SOURCE_BYTES
            || total.saturating_add(length) > MAX_TOTAL_SOURCE_BYTES
        {
            omitted.push(file.file.clone());
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            omitted.push(file.file.clone());
            continue;
        };
        total += contents.len();
        sources.insert(
            file.file.clone(),
            ReportSource {
                sha256: format!("{:x}", Sha256::digest(contents.as_bytes())),
                contents,
            },
        );
    }
    (sources, omitted)
}

fn project_name(root: &Path) -> String {
    fs::read(root.join("package.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|package| package.get("name")?.as_str().map(str::to_owned))
        .filter(|name| !name.trim().is_empty() && name.len() <= 200)
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "project".into())
}

fn build_run(root: &Path, run: &StoredRun) -> Result<ReportRun, String> {
    let current_integrity = current_integrity_for_run(root, run);
    let source_exact = current_integrity.as_ref().is_some_and(|current| {
        current.fingerprint.source == run.metadata.integrity.fingerprint.source
    });
    let comparison = current_integrity
        .as_ref()
        .map(|current| compare_run_integrity(Some(&run.metadata.integrity), current));
    let stale = comparison.as_ref().is_some_and(|value| value.stale)
        || run.metadata.integrity.stale.unwrap_or(false);
    let stale_reasons = comparison
        .as_ref()
        .map(|value| value.reasons.clone())
        .or_else(|| run.metadata.integrity.stale_reasons.clone())
        .unwrap_or_default();
    let container = open_or_rebuild_query_index(run)
        .map_err(|error| format!("could not open run {}: {error}", run.id))?;
    let index = CoverageIndex::new(&container)
        .map_err(|error| format!("could not read run {}: {error}", run.id))?;
    let mut summary = coverage_summary_query(
        &index,
        CoverageSummaryQueryOptions {
            run: &run.id,
            view: CoverageViewId::All,
            kind: None,
            runner: None,
            valid: run.metadata.test_exit_code == Some(0),
            test_exit_code: run.metadata.test_exit_code,
            stale,
            stale_reasons: stale_reasons.clone(),
        },
    )
    .map_err(|error| format!("could not summarize run {}: {error:?}", run.id))?;
    summary.command.clone_from(&run.metadata.command);
    let observed_kinds = summary
        .coverage_by_kind
        .iter()
        .filter_map(|entry| entry.kind.clone())
        .collect::<Vec<_>>();
    summary.hints = full_suite_hints(root, &run.metadata.command, &observed_kinds);

    let files = index
        .file_gaps(CoverageViewId::All, None, None)
        .map_err(|error| format!("could not read files for run {}: {error}", run.id))?;
    let mut file_details = BTreeMap::new();
    for file in files
        .iter()
        .filter(|file| file.score > 0 || file.measurement_limitations > 0)
    {
        let (detail, _) = coverage_file_detail_query(
            &index,
            CoverageFileDetailOptions {
                run: &run.id,
                view: CoverageViewId::All,
                kind: None,
                runner: None,
                selector: &file.file,
                metric: supercov_engine::coverage_query::MinimizeMetric::All,
                offset: 0,
                limit: usize::MAX,
            },
        )
        .map_err(|error| format!("could not read {} in run {}: {error:?}", file.file, run.id))?;
        file_details.insert(
            file.file.clone(),
            serde_json::to_value(detail).map_err(|error| error.to_string())?,
        );
    }

    let decisions = index
        .decision_details(CoverageViewId::All)
        .map_err(|error| format!("could not read decisions for run {}: {error}", run.id))?;
    let tests = index
        .test_details(CoverageViewId::All)
        .map_err(|error| format!("could not read tests for run {}: {error}", run.id))?
        .into_iter()
        .map(|test| {
            let total_lines = test.lines.len();
            let total_hits = test.hits.len();
            let total_decisions = test.decisions.len();
            Ok(ReportTest {
                id: test.summary.id,
                name: test.summary.name,
                file: test.summary.file,
                title: test.summary.title,
                outcome: test.summary.outcome,
                role: test.summary.role,
                runner: test.summary.provenance.runner,
                kind: test.summary.provenance.kind,
                project: test.summary.provenance.project,
                retries: test.retries,
                attempts: serde_json::to_value(test.attempts).map_err(|error| error.to_string())?,
                lines: serde_json::to_value(test.lines).map_err(|error| error.to_string())?,
                total_lines,
                total_hits,
                total_decisions,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let lines = index
        .lines(CoverageViewId::All)
        .map_err(|error| format!("could not read lines for run {}: {error}", run.id))?
        .into_iter()
        .map(|line| ReportLine {
            file: line.file,
            line: line.line,
            covered: line.covered,
            confidence: line.confidence.level,
            tests: line.tests,
        })
        .collect();
    let scope = match coverage_scope_query(
        &index,
        CoverageScopeQueryOptions {
            run: &run.id,
            view: CoverageViewId::All,
            kind: None,
            runner: None,
            offset: 0,
            limit: usize::MAX,
        },
    ) {
        Ok((scope, _)) => Some(serde_json::to_value(scope).map_err(|error| error.to_string())?),
        Err(supercov_engine::coverage_query::QueryError::ScopeUnavailable) => None,
        Err(error) => {
            return Err(format!(
                "could not read scope for run {}: {error:?}",
                run.id
            ));
        }
    };
    let (sources, omitted_sources) = collect_sources(root, &files, source_exact);

    Ok(ReportRun {
        id: run.id.clone(),
        started_at: run.metadata.started_at.clone(),
        duration_ms: run.metadata.duration_ms,
        command: run.metadata.command.clone(),
        test_exit_code: run.metadata.test_exit_code,
        stale,
        stale_reasons,
        source_mode: if source_exact { "exact" } else { "snippets" },
        omitted_sources,
        summary: serde_json::to_value(summary).map_err(|error| error.to_string())?,
        files,
        file_details,
        decisions: serde_json::to_value(decisions).map_err(|error| error.to_string())?,
        tests,
        lines,
        scope,
        sources,
    })
}

fn render_html(bundle: &ReportBundle) -> Result<Vec<u8>, String> {
    let payload = serde_json::to_vec(bundle).map_err(|error| error.to_string())?;
    let mut gzip = GzEncoder::new(Vec::new(), Compression::best());
    gzip.write_all(&payload)
        .map_err(|error| error.to_string())?;
    let compressed = gzip.finish().map_err(|error| error.to_string())?;
    let encoded = STANDARD.encode(compressed);
    if REPORT_TEMPLATE.matches(PAYLOAD_MARKER).count() != 1 {
        return Err("embedded report template has an invalid payload marker".into());
    }
    Ok(REPORT_TEMPLATE
        .replacen(PAYLOAD_MARKER, &encoded, 1)
        .into_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{} is not a file path", path.display()))?
        .to_string_lossy();
    let mut temporary = None;
    for attempt in 0..100_u32 {
        let candidate = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            attempt
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{}: {error}", candidate.display())),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        format!(
            "could not allocate a temporary file beside {}",
            path.display()
        )
    })?;
    let result = (|| -> Result<(), String> {
        file.write_all(bytes)
            .map_err(|error| format!("{}: {error}", temporary_path.display()))?;
        file.sync_all()
            .map_err(|error| format!("{}: {error}", temporary_path.display()))?;
        drop(file);
        fs::rename(&temporary_path, path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn open_report(path: &Path) -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(path);
        command
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command.arg(path);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub fn report_command(arguments: Vec<String>) -> ExitCode {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print!("{}", help());
        return ExitCode::SUCCESS;
    }
    let options = match parse_options(&arguments) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let result = (|| -> Result<(PathBuf, usize), String> {
        let inventory = public_run_inventory(&root).map_err(|error| error.to_string())?;
        let selected = selected_runs(&inventory, &options)?;
        let selected_run_id = selected[0].id.clone();
        let comparison_run_id = options
            .comparison
            .as_deref()
            .map(|selector| select_run(&inventory, Some(selector)).map(|run| run.id.clone()))
            .transpose()
            .map_err(|error| error.to_string())?
            .or_else(|| selected.get(1).map(|run| run.id.clone()));
        let mut runs = Vec::with_capacity(selected.len());
        for (position, run) in selected.iter().enumerate() {
            eprint!(
                "\r[supercov] preparing report run {}/{}: {}",
                position + 1,
                selected.len(),
                run.id
            );
            let _ = std::io::stderr().flush();
            runs.push(build_run(&root, run)?);
        }
        eprint!("\r{}\r", " ".repeat(96));
        let _ = std::io::stderr().flush();
        let bundle = ReportBundle {
            schema_version: REPORT_SCHEMA_VERSION,
            project: project_name(&root),
            selected_run_id,
            comparison_run_id,
            runs,
        };
        let html = render_html(&bundle)?;
        if html.len() > MAX_REPORT_BYTES {
            return Err(format!(
                "report is {:.1} MB, above the 24 MB PR-attachment target; retry with --runs 1",
                html.len() as f64 / 1024.0 / 1024.0
            ));
        }
        let output = if options.output.is_absolute() {
            options.output.clone()
        } else {
            root.join(&options.output)
        };
        write_atomic(&output, &html)?;
        Ok((output, html.len()))
    })();
    let (output, bytes) = match result {
        Ok(result) => result,
        Err(error) => {
            eprint!("\r{}\r", " ".repeat(96));
            eprintln!("[supercov] report failed: {error}");
            return ExitCode::from(2);
        }
    };
    println!(
        "[supercov] wrote {} ({:.1} MB, self-contained and private)",
        output.display(),
        bytes as f64 / 1024.0 / 1024.0
    );
    if options.open {
        match open_report(&output) {
            Ok(()) => println!("[supercov] opened the report in your browser"),
            Err(error) => eprintln!(
                "[supercov] could not open the browser ({error}); open {} manually",
                output.display()
            ),
        }
    }
    let _ = stdout().flush();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_portable_report_options() {
        assert_eq!(parse_options(&[]).unwrap().runs, 10);
        let options = parse_options(&[
            "run_123".into(),
            "--compare".into(),
            "run_122".into(),
            "--runs".into(),
            "4".into(),
            "--output".into(),
            "artifacts/coverage.html".into(),
            "--no-open".into(),
        ])
        .unwrap();
        assert_eq!(options.selector, "run_123");
        assert_eq!(options.comparison.as_deref(), Some("run_122"));
        assert_eq!(options.runs, 4);
        assert_eq!(options.output, PathBuf::from("artifacts/coverage.html"));
        assert!(!options.open);
    }

    #[test]
    fn rejects_ambiguous_or_unbounded_report_arguments() {
        assert!(parse_options(&["one".into(), "two".into()]).is_err());
        assert!(parse_options(&["--runs".into(), "0".into()]).is_err());
        assert!(parse_options(&["--runs".into(), "21".into()]).is_err());
        assert!(parse_options(&["--wat".into()]).is_err());
    }

    #[test]
    fn source_paths_cannot_escape_the_project() {
        let root =
            std::env::temp_dir().join(format!("supercov-report-paths-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/app.py"), "answer = 42\n").unwrap();
        assert!(safe_relative_source(&root, "src/app.py").is_some());
        assert!(safe_relative_source(&root, "../secret").is_none());
        assert!(safe_relative_source(&root, "/etc/passwd").is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
